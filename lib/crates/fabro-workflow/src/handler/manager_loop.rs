use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use fabro_graphviz::graph::{AttrValue, Graph, Node};
use fabro_store::{ArtifactStore, Database};
use fabro_types::settings::SettingsLayer;
use object_store::memory::InMemory;
use tokio::time::{sleep, timeout};

use super::{EngineServices, Handler};
use crate::artifact_upload::ArtifactSink;
use crate::condition::evaluate_condition;
use crate::context::{Context, WorkflowContext, keys};
use crate::error::Error;
use crate::operations::{ValidateInput, WorkflowInput, validate};
use crate::outcome::{Outcome, OutcomeExt, StageStatus};
use crate::pipeline;
use crate::pipeline::types::Initialized;
use crate::run_dir::visit_from_context;
use crate::run_options::RunOptions;

/// Orchestrates a child workflow engine, polling for completion or stop
/// conditions.
pub struct SubWorkflowHandler;

struct ParsedChildWorkflow {
    graph: Graph,
    workflow_path: Option<PathBuf>,
}

/// Parse a duration string like "45s", "200ms", "5m" into a Duration.
/// Falls back to 45 seconds on parse failure.
fn parse_duration_str(s: &str) -> Duration {
    let s = s.trim();
    if let Some(secs) = s.strip_suffix('s') {
        if let Some(ms) = secs.strip_suffix('m') {
            // "ms" suffix
            if let Ok(val) = ms.parse::<u64>() {
                return Duration::from_millis(val);
            }
        } else if let Ok(val) = secs.parse::<u64>() {
            return Duration::from_secs(val);
        }
    }
    if let Some(mins) = s.strip_suffix('m') {
        if let Ok(val) = mins.parse::<u64>() {
            return Duration::from_secs(val * 60);
        }
    }
    Duration::from_secs(45)
}

/// Parse a child workflow graph from node attributes: inline
/// `stack.child_dot_source` (no file inlining), or file path
/// `stack.child_workflow` / `stack.child_dotfile` (with file inlining).
/// `stack.child_workflow` is preferred; `stack.child_dotfile` is kept for
/// backward compatibility.
fn parse_child_graph(node: &Node, services: &EngineServices) -> Result<ParsedChildWorkflow, Error> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    if let Some(dot) = node
        .attrs
        .get("stack.child_dot_source")
        .and_then(|v| v.as_str())
    {
        let validated = validate(ValidateInput {
            workflow: WorkflowInput::DotSource {
                source: dot.to_string(),
                base_dir: None,
            },
            settings: SettingsLayer::default(),
            cwd: cwd.clone(),
            custom_transforms: Vec::new(),
        })?;
        validated.raise_on_errors()?;
        let (graph, _, _) = validated.into_parts();
        return Ok(ParsedChildWorkflow {
            graph,
            workflow_path: None,
        });
    }
    if let Some(path) = node
        .attrs
        .get("stack.child_workflow")
        .or_else(|| node.attrs.get("stack.child_dotfile"))
        .and_then(|v| v.as_str())
    {
        let workflow = match (&services.workflow_bundle, &services.workflow_path) {
            (Some(bundle), Some(current_workflow_path)) => WorkflowInput::Bundled(
                bundle
                    .resolve_child(current_workflow_path, path)
                    .cloned()
                    .ok_or_else(|| {
                        Error::handler(format!(
                            "child workflow is not present in the persisted bundle: {path}"
                        ))
                    })?,
            ),
            (Some(_), None) => {
                return Err(Error::engine(
                    "workflow bundle is missing the current workflow path".to_string(),
                ));
            }
            (None, _) => WorkflowInput::Path(PathBuf::from(path)),
        };
        let workflow_path = match &workflow {
            WorkflowInput::Bundled(workflow) => Some(workflow.logical_path.clone()),
            WorkflowInput::Path(path) => Some(path.clone()),
            WorkflowInput::DotSource { .. } => None,
        };
        let validated = validate(ValidateInput {
            workflow,
            settings: SettingsLayer::default(),
            cwd,
            custom_transforms: Vec::new(),
        })?;
        validated.raise_on_errors()?;
        let (graph, _, _) = validated.into_parts();
        return Ok(ParsedChildWorkflow {
            graph,
            workflow_path,
        });
    }
    Err(Error::handler("No child workflow source".to_string()))
}

/// Compute the context diff: keys that changed or were added relative to
/// `before`.
fn context_diff(
    before: &HashMap<String, serde_json::Value>,
    after: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    let mut diff = HashMap::new();
    for (key, value) in after {
        if before.get(key) != Some(value) {
            diff.insert(key.clone(), value.clone());
        }
    }
    diff
}

#[async_trait]
impl Handler for SubWorkflowHandler {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        _graph: &Graph,
        run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, Error> {
        let poll_interval = node
            .attrs
            .get("manager.poll_interval")
            .and_then(AttrValue::as_duration)
            .unwrap_or_else(|| {
                let raw = node
                    .attrs
                    .get("manager.poll_interval")
                    .and_then(|v| v.as_str())
                    .unwrap_or("45s");
                parse_duration_str(raw)
            });

        let max_cycles = node
            .attrs
            .get("manager.max_cycles")
            .and_then(AttrValue::as_i64)
            .unwrap_or(1000);
        let max_cycles = u64::try_from(max_cycles).unwrap_or(1000).max(1);

        let stop_condition = node
            .attrs
            .get("manager.stop_condition")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Read and parse child workflow graph
        let ParsedChildWorkflow {
            graph: child_graph,
            workflow_path: child_workflow_path,
        } = match parse_child_graph(node, services) {
            Ok(g) => g,
            Err(e) => {
                return Ok(Outcome::fail_classify(format!(
                    "Failed to parse child pipeline: {e}"
                )));
            }
        };

        // Build child RunOptions
        let visit = visit_from_context(context) as u64;
        let child_logs = run_dir.join(format!("stages/{}@{visit}/child", node.id));
        let _ = std::fs::create_dir_all(&child_logs);

        let cancel_token = Arc::new(AtomicBool::new(false));
        let child_cancel = Arc::clone(&cancel_token);

        let child_run_options = RunOptions {
            settings: SettingsLayer::default(),
            run_dir: child_logs,
            cancel_token: Some(cancel_token),
            // Child workflows are part of the parent run's event stream.
            run_id: services.emitter.run_id(),
            labels: HashMap::new(),
            workflow_slug: None,
            github_app: None,
            base_branch: None,
            display_base_sha: None,
            host_repo_path: None,
            git: None,
        };

        // Clone parent context for child; inject parent preamble
        let child_context = context.fork();
        let parent_preamble = context.preamble();
        if !parent_preamble.is_empty() {
            child_context.set(
                keys::INTERNAL_PARENT_PREAMBLE,
                serde_json::json!(parent_preamble),
            );
        }
        let before_snapshot = context.snapshot();

        let emitter = Arc::clone(&services.emitter);
        let sandbox = Arc::clone(&services.sandbox);
        let registry = Arc::clone(&services.registry);
        let hook_runner = services.hook_runner.clone();
        let env = services.env.clone();
        let inputs = services.inputs.clone();
        let dry_run = services.dry_run;
        let workflow_bundle = services.workflow_bundle.clone();
        let object_store = Arc::new(InMemory::new());
        let store = Arc::new(Database::new(
            object_store.clone(),
            "",
            Duration::from_millis(1),
            None,
        ));
        let run_store = store
            .create_run(&child_run_options.run_id)
            .await
            .map_err(|err| Error::engine(err.to_string()))?;
        let artifact_store = ArtifactStore::new(object_store, "artifacts");

        // Spawn child engine
        let mut child_handle = tokio::spawn(async move {
            let initialized = Initialized {
                graph: child_graph,
                source: String::new(),
                inputs,
                run_options: child_run_options,
                workflow_path: child_workflow_path,
                workflow_bundle,
                run_store: run_store.into(),
                checkpoint: None,
                seed_context: Some(child_context),
                emitter,
                sandbox,
                registry,
                on_node: None,
                artifact_sink: Some(ArtifactSink::Store(artifact_store)),
                run_control: None,
                hook_runner,
                env,
                dry_run,
                llm_client: None,
                model: String::new(),
                provider: fabro_llm::Provider::Anthropic,
            };
            let executed = pipeline::execute(initialized).await;
            Ok::<_, Error>((executed.outcome?, executed.final_context))
        });

        // Poll loop
        for cycle in 1..=max_cycles {
            tokio::select! {
                result = &mut child_handle => {
                    // Child finished
                    let (child_outcome, child_final_context) = match result {
                        Ok(Ok(pair)) => pair,
                        Ok(Err(e)) => return Ok(Outcome::fail_classify(format!("Child engine error: {e}"))),
                        Err(e) => return Ok(Outcome::fail_classify(format!("Child task panicked: {e}"))),
                    };

                    // Compute context diff, filtering engine-internal keys
                    let after_snapshot = child_final_context.snapshot();
                    let raw_diff = context_diff(&before_snapshot, &after_snapshot);
                    let diff: HashMap<String, serde_json::Value> = raw_diff
                        .into_iter()
                        .filter(|(key, _)| !keys::is_engine_internal_key(key))
                        .collect();

                    tracing::debug!(
                        node = %node.id,
                        propagated_keys = ?diff.keys(),
                        "Sub-workflow context diff filtered"
                    );

                    let mut outcome = Outcome {
                        status: child_outcome.status.clone(),
                        notes: Some(format!("Child completed at cycle {cycle}")),
                        context_updates: diff,
                        ..Outcome::success()
                    };

                    if child_outcome.status == StageStatus::Fail {
                        outcome.failure.clone_from(&child_outcome.failure);
                    }

                    return Ok(outcome);
                }
                () = sleep(poll_interval) => {
                    // Check stop condition
                    if !stop_condition.is_empty() {
                        let dummy_outcome = Outcome::success();
                        if evaluate_condition(stop_condition, &dummy_outcome, context) {
                            child_cancel.store(true, Ordering::Relaxed);
                            // Give child a moment to wind down
                            let _ = timeout(
                                Duration::from_millis(100),
                                &mut child_handle,
                            ).await;
                            return Ok(Outcome {
                                status: StageStatus::Success,
                                notes: Some(format!("Stop condition satisfied at cycle {cycle}")),
                                ..Outcome::success()
                            });
                        }
                    }
                }
            }
        }

        // Max cycles exceeded — cancel child
        child_cancel.store(true, Ordering::Relaxed);
        let _ = timeout(Duration::from_millis(100), &mut child_handle).await;

        Ok(Outcome::fail_classify(format!(
            "Max cycles ({max_cycles}) exceeded for manager loop node: {}",
            node.id
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use fabro_graphviz::graph::AttrValue;

    use super::*;
    use crate::handler::HandlerRegistry;
    use crate::handler::exit::ExitHandler;
    use crate::handler::start::StartHandler;
    use crate::workflow_bundle::{BundledWorkflow, WorkflowBundle};

    fn make_services() -> EngineServices {
        let mut services = EngineServices::test_default();
        let mut registry = HandlerRegistry::new(Box::new(StartHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        services.registry = std::sync::Arc::new(registry);
        services
    }

    fn child_dot_succeeds() -> &'static str {
        "digraph Child { start [shape=Mdiamond]; exit [shape=Msquare]; start -> exit }"
    }

    #[tokio::test]
    async fn child_pipeline_succeeds() {
        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs.insert(
            "stack.child_dot_source".to_string(),
            AttrValue::String(child_dot_succeeds().to_string()),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(10)),
        );

        let context = Context::new();
        let graph = Graph::new("test");
        let dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        assert!(
            outcome
                .notes
                .as_deref()
                .unwrap()
                .contains("Child completed")
        );
        assert!(
            dir.path().join("stages/manager@1/child").exists(),
            "child logs should default to first-visit directory naming"
        );
    }

    #[tokio::test]
    async fn no_dot_source_fails() {
        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(10));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(1)),
        );

        let context = Context::new();
        let graph = Graph::new("test");
        let dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
        assert!(
            outcome
                .failure_reason()
                .unwrap()
                .contains("No child workflow source")
        );
    }

    #[tokio::test]
    async fn invalid_dot_source_fails() {
        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs.insert(
            "stack.child_dot_source".to_string(),
            AttrValue::String("not valid dot!!!".to_string()),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(10));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(1)),
        );

        let context = Context::new();
        let graph = Graph::new("test");
        let dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
        assert!(
            outcome
                .failure_reason()
                .unwrap()
                .contains("Failed to parse child pipeline")
        );
    }

    #[tokio::test]
    async fn context_flows_parent_to_child_and_back() {
        // Register a handler that reads parent context and sets a result
        struct ContextEchoHandler;

        #[async_trait]
        impl Handler for ContextEchoHandler {
            async fn execute(
                &self,
                _node: &Node,
                context: &Context,
                _graph: &Graph,
                _run_dir: &Path,
                _services: &EngineServices,
            ) -> Result<Outcome, Error> {
                let target = context.get_string("review.target", "");
                let mut outcome = Outcome::success();
                outcome
                    .context_updates
                    .insert("review.result".to_string(), serde_json::json!("approved"));
                outcome
                    .context_updates
                    .insert("review.echo".to_string(), serde_json::json!(target));
                Ok(outcome)
            }
        }

        let mut registry = HandlerRegistry::new(Box::new(ContextEchoHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        let mut services = EngineServices::test_default();
        services.registry = std::sync::Arc::new(registry);

        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        // Child pipeline with a "work" node (default handler = ContextEchoHandler)
        node.attrs.insert(
            "stack.child_dot_source".to_string(),
            AttrValue::String(
                "digraph Child { start [shape=Mdiamond]; work [shape=box]; exit [shape=Msquare]; start -> work -> exit }"
                    .to_string(),
            ),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(10)),
        );

        // Parent sets a context value the child should be able to read
        let context = Context::new();
        context.set("review.target", serde_json::json!("src/main.rs"));

        let graph = Graph::new("test");
        let dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        assert_eq!(
            outcome.context_updates.get("review.result"),
            Some(&serde_json::json!("approved"))
        );
        assert_eq!(
            outcome.context_updates.get("review.echo"),
            Some(&serde_json::json!("src/main.rs"))
        );
    }

    #[tokio::test]
    async fn child_dotfile_reads_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let dot_path = dir.path().join("child.dot");
        std::fs::write(&dot_path, child_dot_succeeds()).unwrap();

        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs.insert(
            "stack.child_dotfile".to_string(),
            AttrValue::String(dot_path.to_string_lossy().to_string()),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(10)),
        );

        let context = Context::new();
        let graph = Graph::new("test");

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn child_workflow_reads_from_bundle_when_present() {
        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs.insert(
            "stack.child_workflow".to_string(),
            AttrValue::String("./children/review.fabro".to_string()),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(10)),
        );

        let mut services = make_services();
        services.workflow_path = Some(PathBuf::from("workflow.fabro"));
        services.workflow_bundle = Some(Arc::new(WorkflowBundle::new(HashMap::from([(
            PathBuf::from("children/review.fabro"),
            BundledWorkflow {
                logical_path: PathBuf::from("children/review.fabro"),
                source: child_dot_succeeds().to_string(),
                files: HashMap::new(),
            },
        )]))));

        let context = Context::new();
        let graph = Graph::new("test");
        let dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn child_workflow_missing_from_bundle_does_not_fall_back_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let dot_path = dir.path().join("child.fabro");
        std::fs::write(&dot_path, child_dot_succeeds()).unwrap();

        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs.insert(
            "stack.child_workflow".to_string(),
            AttrValue::String(dot_path.to_string_lossy().to_string()),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(10)),
        );

        let mut services = make_services();
        services.workflow_path = Some(PathBuf::from("workflow.fabro"));
        services.workflow_bundle = Some(Arc::new(WorkflowBundle::new(HashMap::new())));

        let context = Context::new();
        let graph = Graph::new("test");

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
        assert!(
            outcome
                .failure_reason()
                .unwrap()
                .contains("child workflow is not present in the persisted bundle")
        );
    }

    #[tokio::test]
    async fn max_cycles_exceeded_cancels_child() {
        // Use a child that takes a long time (many nodes with sleep won't work, so use
        // a child that succeeds quickly but set max_cycles=1 and very short
        // poll) Actually, to test max cycles exceeded we need a child that runs
        // longer than max_cycles * poll_interval. Use a child dot that's valid
        // but we set max_cycles=1 with poll_interval=1ms so the child likely
        // won't finish in time.
        //
        // But a simple start->exit child is almost instant. So we need a handler that
        // sleeps to make the child slow.
        struct SlowHandler;

        #[async_trait]
        impl Handler for SlowHandler {
            async fn execute(
                &self,
                _node: &Node,
                _context: &Context,
                _graph: &Graph,
                _run_dir: &Path,
                _services: &EngineServices,
            ) -> Result<Outcome, Error> {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(Outcome::success())
            }
        }

        let mut registry = HandlerRegistry::new(Box::new(SlowHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        let mut services = EngineServices::test_default();
        services.registry = std::sync::Arc::new(registry);

        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs.insert(
            "stack.child_dot_source".to_string(),
            AttrValue::String(
                "digraph Child { start [shape=Mdiamond]; slow [shape=box]; exit [shape=Msquare]; start -> slow -> exit }"
                    .to_string(),
            ),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(2));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(1)),
        );

        let context = Context::new();
        let graph = Graph::new("test");
        let dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
        assert!(outcome.failure_reason().unwrap().contains("Max cycles"));
    }

    #[tokio::test]
    async fn stop_condition_cancels_child() {
        struct SlowHandler;

        #[async_trait]
        impl Handler for SlowHandler {
            async fn execute(
                &self,
                _node: &Node,
                _context: &Context,
                _graph: &Graph,
                _run_dir: &Path,
                _services: &EngineServices,
            ) -> Result<Outcome, Error> {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(Outcome::success())
            }
        }

        let mut registry = HandlerRegistry::new(Box::new(SlowHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        let mut services = EngineServices::test_default();
        services.registry = std::sync::Arc::new(registry);

        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs.insert(
            "stack.child_dot_source".to_string(),
            AttrValue::String(
                "digraph Child { start [shape=Mdiamond]; slow [shape=box]; exit [shape=Msquare]; start -> slow -> exit }"
                    .to_string(),
            ),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(1)),
        );
        node.attrs.insert(
            "manager.stop_condition".to_string(),
            AttrValue::String("context.done=true".to_string()),
        );

        // Pre-set the stop condition so it fires on first poll
        let context = Context::new();
        context.set("done", serde_json::json!("true"));

        let graph = Graph::new("test");
        let dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        assert!(
            outcome
                .notes
                .as_deref()
                .unwrap()
                .contains("Stop condition satisfied")
        );
    }

    #[test]
    fn parse_duration_str_seconds() {
        assert_eq!(parse_duration_str("45s"), Duration::from_secs(45));
    }

    #[test]
    fn parse_duration_str_milliseconds() {
        assert_eq!(parse_duration_str("200ms"), Duration::from_millis(200));
    }

    #[test]
    fn parse_duration_str_minutes() {
        assert_eq!(parse_duration_str("5m"), Duration::from_secs(300));
    }

    #[test]
    fn parse_duration_str_invalid_fallback() {
        assert_eq!(parse_duration_str("bad"), Duration::from_secs(45));
    }

    #[test]
    fn context_diff_detects_additions() {
        let before = HashMap::new();
        let mut after = HashMap::new();
        after.insert("key".to_string(), serde_json::json!("value"));
        let diff = context_diff(&before, &after);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff.get("key"), Some(&serde_json::json!("value")));
    }

    #[test]
    fn context_diff_detects_changes() {
        let mut before = HashMap::new();
        before.insert("key".to_string(), serde_json::json!("old"));
        let mut after = HashMap::new();
        after.insert("key".to_string(), serde_json::json!("new"));
        let diff = context_diff(&before, &after);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff.get("key"), Some(&serde_json::json!("new")));
    }

    #[test]
    fn context_diff_ignores_unchanged() {
        let mut before = HashMap::new();
        before.insert("key".to_string(), serde_json::json!("same"));
        let mut after = HashMap::new();
        after.insert("key".to_string(), serde_json::json!("same"));
        let diff = context_diff(&before, &after);
        assert!(diff.is_empty());
    }

    #[test]
    fn context_diff_ignores_deletions() {
        let mut before = HashMap::new();
        before.insert("removed".to_string(), serde_json::json!("gone"));
        let after = HashMap::new();
        let diff = context_diff(&before, &after);
        assert!(diff.is_empty());
    }

    #[test]
    fn context_diff_excludes_engine_internal_keys() {
        let before = HashMap::new();
        let mut after = HashMap::new();
        after.insert("graph.goal".to_string(), serde_json::json!("child goal"));
        after.insert(
            "internal.run_id".to_string(),
            serde_json::json!("child-run"),
        );
        after.insert(
            "thread.main.current_node".to_string(),
            serde_json::json!("exit"),
        );
        after.insert("current_node".to_string(), serde_json::json!("exit"));
        after.insert("response.plan".to_string(), serde_json::json!("the plan"));
        after.insert("review.result".to_string(), serde_json::json!("approved"));

        let raw_diff = context_diff(&before, &after);
        let filtered: HashMap<String, serde_json::Value> = raw_diff
            .into_iter()
            .filter(|(key, _)| !keys::is_engine_internal_key(key))
            .collect();

        assert_eq!(filtered.len(), 2);
        assert!(filtered.contains_key("response.plan"));
        assert!(filtered.contains_key("review.result"));
    }

    #[tokio::test]
    async fn context_flows_parent_to_child_and_back_excludes_internals() {
        struct ContextEchoHandler;

        #[async_trait]
        impl Handler for ContextEchoHandler {
            async fn execute(
                &self,
                _node: &Node,
                context: &Context,
                _graph: &Graph,
                _run_dir: &Path,
                _services: &EngineServices,
            ) -> Result<Outcome, Error> {
                let target = context.get_string("review.target", "");
                let mut outcome = Outcome::success();
                outcome
                    .context_updates
                    .insert("review.result".to_string(), serde_json::json!("approved"));
                outcome
                    .context_updates
                    .insert("review.echo".to_string(), serde_json::json!(target));
                Ok(outcome)
            }
        }

        let mut registry = HandlerRegistry::new(Box::new(ContextEchoHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        let mut services = EngineServices::test_default();
        services.registry = std::sync::Arc::new(registry);

        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs.insert(
            "stack.child_dot_source".to_string(),
            AttrValue::String(
                "digraph Child { start [shape=Mdiamond]; work [shape=box]; exit [shape=Msquare]; start -> work -> exit }"
                    .to_string(),
            ),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(10)),
        );

        let context = Context::new();
        context.set("review.target", serde_json::json!("src/main.rs"));

        let graph = Graph::new("test");
        let dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &services)
            .await
            .unwrap();

        assert_eq!(outcome.status, StageStatus::Success);
        // User-defined keys propagate
        assert_eq!(
            outcome.context_updates.get("review.result"),
            Some(&serde_json::json!("approved"))
        );
        // Engine-internal keys do NOT propagate
        assert!(!outcome.context_updates.contains_key("internal.run_id"));
        assert!(!outcome.context_updates.contains_key("graph.goal"));
        assert!(
            !outcome
                .context_updates
                .keys()
                .any(|k| k.starts_with("thread."))
        );
        assert!(
            !outcome
                .context_updates
                .keys()
                .any(|k| k.starts_with("current"))
        );
    }

    #[tokio::test]
    async fn child_receives_parent_preamble() {
        struct PreambleEchoHandler;

        #[async_trait]
        impl Handler for PreambleEchoHandler {
            async fn execute(
                &self,
                _node: &Node,
                context: &Context,
                _graph: &Graph,
                _run_dir: &Path,
                _services: &EngineServices,
            ) -> Result<Outcome, Error> {
                let parent_preamble = context.get_string(keys::INTERNAL_PARENT_PREAMBLE, "");
                let mut outcome = Outcome::success();
                outcome.context_updates.insert(
                    "echo.parent_preamble".to_string(),
                    serde_json::json!(parent_preamble),
                );
                Ok(outcome)
            }
        }

        let mut registry = HandlerRegistry::new(Box::new(PreambleEchoHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        let mut services = EngineServices::test_default();
        services.registry = std::sync::Arc::new(registry);

        let handler = SubWorkflowHandler;
        let mut node = Node::new("manager");
        node.attrs.insert(
            "stack.child_dot_source".to_string(),
            AttrValue::String(
                "digraph Child { start [shape=Mdiamond]; work [shape=box]; exit [shape=Msquare]; start -> work -> exit }"
                    .to_string(),
            ),
        );
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(10)),
        );

        // Set a preamble on the parent context
        let context = Context::new();
        context.set(
            keys::CURRENT_PREAMBLE,
            serde_json::json!("Parent did step A and step B"),
        );

        let graph = Graph::new("test");
        let dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, dir.path(), &services)
            .await
            .unwrap();

        assert_eq!(outcome.status, StageStatus::Success);
        let echoed = outcome
            .context_updates
            .get("echo.parent_preamble")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            echoed.contains("Parent did step A and step B"),
            "Child should receive the parent preamble, got: {echoed}"
        );
    }
}
