use std::path::Path;

use async_trait::async_trait;
use fabro_agent::CommandOutputCallback;
use fabro_graphviz::graph::{Graph, Node};
use fabro_types::{CommandTermination, StageTiming};

use super::{EngineServices, Handler, NodeTimeoutPolicy};
use crate::command_log::CommandLogRecorder;
use crate::context::{Context, keys};
use crate::error::Error;
use crate::event::{Event, StageScope};
use crate::outcome::{Outcome, OutcomeExt};

fn timeout_ms(node: &Node) -> Option<u64> {
    node.timeout().map(crate::millis_u64)
}

/// Shell-escape a string using `shlex::try_quote` (POSIX-safe).
fn shell_quote(s: &str) -> String {
    shlex::try_quote(s).map_or_else(
        |_| format!("'{}'", s.replace('\'', "'\\''")),
        |q| q.to_string(),
    )
}

/// Executes an external script configured via node attributes.
pub struct CommandHandler;

#[async_trait]
impl Handler for CommandHandler {
    async fn simulate(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, Error> {
        let script = node
            .attrs
            .get("script")
            .or_else(|| node.attrs.get("tool_command"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let mut outcome = Outcome::simulated(&node.id);
        outcome.notes = Some(format!("[Simulated] Command skipped: {script}"));
        outcome
            .context_updates
            .insert(keys::COMMAND_OUTPUT.to_string(), serde_json::json!(""));
        Ok(outcome)
    }

    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        _graph: &Graph,
        run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, Error> {
        let script = node
            .attrs
            .get("script")
            .or_else(|| node.attrs.get("tool_command"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if script.is_empty() {
            return Ok(Outcome::fail_classify("No script specified"));
        }

        let language = node
            .attrs
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("shell");

        if language != "shell" && language != "python" {
            return Ok(Outcome::fail_classify(format!(
                "Invalid language: {language:?} (expected \"shell\" or \"python\")"
            )));
        }

        let command = if language == "python" {
            format!("python3 -c {}", shell_quote(script))
        } else {
            script.to_string()
        };
        let command = format!("exec 2>&1\n{command}");
        let stage_scope = StageScope::for_handler(context, &node.id);
        services.run.emitter.emit_scoped(
            &Event::CommandStarted {
                node_id:    node.id.clone(),
                script:     script.to_string(),
                command:    command.clone(),
                language:   language.to_string(),
                timeout_ms: timeout_ms(node),
            },
            &stage_scope,
        );

        let timeout_ms = node.timeout().map_or(600_000, crate::millis_u64);
        let env = services
            .env_for_stage()
            .await
            .map_err(|err| Error::handler_with_anyhow("Failed to resolve stage env", err))?;
        let env_vars = if env.is_empty() { None } else { Some(&env) };
        let cancel_token = services.run.cancel_token().child_token();
        let stage_id = stage_scope.stage_id();
        let recorder = CommandLogRecorder::create(run_dir, &stage_id).await?;
        let output_callback: CommandOutputCallback = {
            let recorder = recorder.clone();
            std::sync::Arc::new(move |_stream, bytes| {
                let recorder = recorder.clone();
                Box::pin(async move {
                    recorder
                        .append(&bytes)
                        .await
                        .map_err(|err| fabro_sandbox::Error::message(err.to_string()))
                })
            })
        };

        let result = services
            .run
            .sandbox
            .exec_command_streaming(
                &command,
                Some(timeout_ms),
                None,
                env_vars,
                Some(cancel_token.clone()),
                output_callback,
            )
            .await;
        cancel_token.cancel();
        let streaming = match result {
            Ok(streaming) => streaming,
            Err(err) => {
                recorder.discard().await?;
                return Err(Error::handler_with_source("Failed to spawn script", err));
            }
        };
        let result = streaming.result;
        let finalized = recorder.finalize(&services.run.run_store).await?;

        services.run.emitter.emit_scoped(
            &Event::CommandCompleted {
                node_id:        node.id.clone(),
                output:         finalized.output_ref.clone(),
                exit_code:      result.exit_code,
                duration_ms:    result.duration_ms,
                termination:    result.termination,
                output_bytes:   finalized.output_bytes,
                live_streaming: streaming.live_streaming,
            },
            &stage_scope,
        );

        if result.termination == CommandTermination::TimedOut {
            let mut reason = format!("Script timed out after {timeout_ms}ms: {script}");
            append_output_tail(&mut reason, &finalized.output_text);
            return Err(Error::handler(reason));
        }

        if result.termination == CommandTermination::Cancelled {
            let mut reason = format!("Script cancelled: {script}");
            append_output_tail(&mut reason, &finalized.output_text);
            return Err(Error::handler(reason));
        }

        if result.exit_code == Some(0) {
            let mut outcome = Outcome::success();
            outcome.context_updates.insert(
                keys::COMMAND_OUTPUT.to_string(),
                serde_json::json!(finalized.output_ref),
            );
            outcome.notes = Some(format!("Script completed: {script}"));
            outcome.timing = Some(StageTiming::active_only(0, result.duration_ms));
            Ok(outcome)
        } else {
            let mut reason = format!(
                "Script failed with exit code: {}",
                result.exit_code.unwrap_or(-1)
            );
            append_output_tail(&mut reason, &finalized.output_text);
            let mut outcome = Outcome::fail_classify(reason);
            outcome.context_updates.insert(
                keys::COMMAND_OUTPUT.to_string(),
                serde_json::json!(finalized.output_ref),
            );
            outcome.timing = Some(StageTiming::active_only(0, result.duration_ms));
            Ok(outcome)
        }
    }

    fn node_timeout_policy(&self, _node: &Node) -> NodeTimeoutPolicy {
        NodeTimeoutPolicy::HandlerManaged
    }
}

fn append_output_tail(reason: &mut String, output: &str) {
    let output_tail = tail_bytes(output, 4096);
    if !output_tail.trim().is_empty() {
        reason.push_str("\n\n## output\n");
        reason.push_str(&output_tail);
    }
}

fn tail_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use fabro_graphviz::graph::AttrValue;
    use fabro_store::{Database, RunDatabase, StageId};
    use fabro_types::{Graph, RunProjection, RunSpec, WorkflowSettings, fixtures};
    use object_store::memory::InMemory;
    use tokio::sync::Mutex;

    use super::*;
    use crate::command_log::command_log_path;
    use crate::outcome::StageOutcome;
    use crate::runtime_store::{RunStoreBackend, RunStoreHandle};

    #[derive(Default)]
    struct MemoryRunStoreBackend {
        blobs: Mutex<std::collections::HashMap<fabro_types::RunBlobId, Bytes>>,
    }

    #[async_trait::async_trait]
    impl RunStoreBackend for MemoryRunStoreBackend {
        async fn load_state(&self) -> anyhow::Result<fabro_store::RunProjection> {
            Ok(RunProjection::new(
                "Test run".to_string(),
                RunSpec {
                    run_id:           fixtures::RUN_1,
                    settings:         WorkflowSettings::default(),
                    graph:            Graph::new("test"),
                    graph_source:     None,
                    workflow_slug:    None,
                    automation:       None,
                    source_directory: None,
                    labels:           std::collections::HashMap::default(),
                    provenance:       None,
                    manifest_blob:    None,
                    definition_blob:  None,
                    git:              None,
                    fork_source_ref:  None,
                },
                chrono::Utc::now(),
            ))
        }

        async fn list_events(&self) -> anyhow::Result<Vec<fabro_store::EventEnvelope>> {
            Ok(Vec::new())
        }

        async fn append_run_event(&self, _event: &fabro_types::RunEvent) -> anyhow::Result<()> {
            Ok(())
        }

        async fn write_blob(&self, data: &[u8]) -> anyhow::Result<fabro_types::RunBlobId> {
            let blob_id = fabro_types::RunBlobId::new(data);
            self.blobs
                .lock()
                .await
                .insert(blob_id, Bytes::copy_from_slice(data));
            Ok(blob_id)
        }

        async fn read_blob(&self, id: &fabro_types::RunBlobId) -> anyhow::Result<Option<Bytes>> {
            Ok(self.blobs.lock().await.get(id).cloned())
        }

        async fn read_run_log(&self) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(None)
        }
    }

    fn make_services() -> EngineServices {
        let mut services = EngineServices::test_default();
        services.run = services.run.with_run_store(RunStoreHandle::new(Arc::new(
            MemoryRunStoreBackend::default(),
        )));
        services
    }

    async fn command_text(services: &EngineServices, value: &serde_json::Value) -> String {
        crate::artifact::resolve_text_or_blob_ref(value, &services.run.run_store)
            .await
            .unwrap()
    }

    async fn command_log_text(services: &EngineServices, value: &str) -> String {
        crate::command_log::read_json_string_blob(&services.run.run_store, value)
            .await
            .unwrap()
            .unwrap_or_else(|| value.to_string())
    }

    fn test_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    async fn make_services_with_run_store() -> (
        EngineServices,
        RunDatabase,
        crate::event::StoreProgressLogger,
    ) {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        seed_created(&run_store).await;
        let mut services = EngineServices::test_default();
        services.run = services
            .run
            .with_emitter(Arc::new(crate::event::Emitter::new(fixtures::RUN_1)))
            .with_run_store(run_store.clone().into());
        let logger = crate::event::StoreProgressLogger::new(run_store.clone());
        logger.register(services.run.emitter.as_ref());
        (services, run_store, logger)
    }

    async fn seed_created(run_store: &RunDatabase) {
        crate::event::append_event(
            run_store,
            &fixtures::RUN_1,
            &crate::event::Event::RunCreated {
                run_id:           fixtures::RUN_1,
                title:            None,
                settings:         serde_json::to_value(WorkflowSettings::default()).unwrap(),
                graph:            serde_json::to_value(Graph::new("test")).unwrap(),
                workflow_source:  None,
                workflow_config:  None,
                labels:           std::collections::BTreeMap::default(),
                run_dir:          "/tmp".to_string(),
                source_directory: None,
                workflow_slug:    None,
                automation:       None,
                db_prefix:        None,
                provenance:       None,
                manifest_blob:    None,
                git:              None,
                fork_source_ref:  None,
                retried_from:     None,
                parent_id:        None,
                web_url:          None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn script_handler_no_script() {
        let handler = CommandHandler;
        let node = Node::new("script_node");
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let services = make_services();
        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Failed {
            retry_requested: false,
        });
        assert_eq!(outcome.failure_reason(), Some("No script specified"));
    }

    #[tokio::test]
    async fn simulate_skips_execution() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .simulate(&node, &context, &graph, run_dir.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Succeeded);
        assert!(outcome.notes.as_deref().unwrap().contains("[Simulated]"));
        assert!(outcome.notes.as_deref().unwrap().contains("echo hello"));
        assert_eq!(
            outcome.context_updates.get(keys::COMMAND_OUTPUT),
            Some(&serde_json::json!(""))
        );
        assert!(!outcome.context_updates.contains_key("command.stderr"));
    }

    #[tokio::test]
    async fn dispatch_routes_to_simulate_in_dry_run() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let mut services = make_services();
        services.dry_run = true;

        let outcome = crate::handler::dispatch_handler(
            &handler,
            &node,
            &context,
            &graph,
            run_dir.path(),
            &services,
        )
        .await
        .unwrap();
        assert_eq!(outcome.status, StageOutcome::Succeeded);
        assert!(outcome.notes.as_deref().unwrap().contains("[Simulated]"));
    }

    #[tokio::test]
    async fn script_handler_echo_command() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let services = make_services();
        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Succeeded);
        assert!(outcome.notes.as_deref().unwrap().contains("echo hello"));
        let command_output = outcome.context_updates.get(keys::COMMAND_OUTPUT).unwrap();
        assert!(
            command_text(&services, command_output)
                .await
                .contains("hello")
        );
        assert!(!outcome.context_updates.contains_key("command.stderr"));
    }

    #[tokio::test]
    async fn script_handler_reports_command_duration_as_tool_timing() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("sleep 0.05; echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let services = make_services();
        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();

        let timing = outcome.timing.expect("command outcome should carry timing");
        assert_eq!(timing.inference_time_ms, 0);
        assert!(
            timing.tool_time_ms >= 25,
            "expected command duration to be reported as tool time, got {timing:?}"
        );
        assert_eq!(timing.active_time_ms, timing.tool_time_ms);
    }

    #[tokio::test]
    async fn script_handler_failing_command() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs
            .insert("script".to_string(), AttrValue::String("false".to_string()));
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let services = make_services();
        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Failed {
            retry_requested: false,
        });
    }

    #[tokio::test]
    async fn script_handler_timeout() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("sleep 60".to_string()),
        );
        node.attrs.insert(
            "timeout".to_string(),
            AttrValue::Duration(Duration::from_millis(50)),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let err = handler
            .execute(&node, &context, &graph, run_dir.path(), &make_services())
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("timed out"),
            "expected timeout message, got: {msg}"
        );
    }

    #[tokio::test]
    async fn writes_script_invocation_json() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        logger.flush().await;

        let snapshot = run_store.state().await.unwrap();
        let node_state = snapshot.stage(&StageId::new("script_node", 1)).unwrap();
        let json = node_state.script_invocation.as_ref().unwrap();
        assert_eq!(json["command"], "exec 2>&1\necho hello");
        assert_eq!(json["language"], "shell");
        assert_eq!(json["timeout_ms"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn writes_script_invocation_json_with_timeout() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        node.attrs.insert(
            "timeout".to_string(),
            AttrValue::Duration(Duration::from_secs(5)),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        logger.flush().await;

        let snapshot = run_store.state().await.unwrap();
        let node_state = snapshot.stage(&StageId::new("script_node", 1)).unwrap();
        let json = node_state.script_invocation.as_ref().unwrap();
        assert_eq!(json["command"], "exec 2>&1\necho hello");
        assert_eq!(json["language"], "shell");
        assert_eq!(json["timeout_ms"], 5000);
    }

    #[tokio::test]
    async fn writes_output_log() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        logger.flush().await;

        let snapshot = run_store.state().await.unwrap();
        let node_state = snapshot.stage(&StageId::new("script_node", 1)).unwrap();
        let output = node_state.output.as_deref().unwrap();
        assert_eq!(command_log_text(&services, output).await.trim(), "hello");
        assert_eq!(node_state.output_bytes, Some(6));
        assert_eq!(node_state.live_streaming, Some(true));
    }

    #[tokio::test]
    async fn writes_stderr_to_output_log_on_failure() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo oops >&2 && false".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        logger.flush().await;

        let snapshot = run_store.state().await.unwrap();
        let node_state = snapshot.stage(&StageId::new("script_node", 1)).unwrap();
        let output = node_state.output.as_deref().unwrap();
        assert_eq!(command_log_text(&services, output).await.trim(), "oops");
    }

    #[tokio::test]
    async fn writes_script_timing_json_on_success() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        logger.flush().await;

        let snapshot = run_store.state().await.unwrap();
        let node_state = snapshot.stage(&StageId::new("script_node", 1)).unwrap();
        let json = node_state.script_timing.as_ref().unwrap();
        assert!(json["duration_ms"].is_u64());
        assert_eq!(json["exit_code"], 0);
        assert_eq!(json["termination"], "exited");
    }

    #[tokio::test]
    async fn writes_script_timing_json_on_failure() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs
            .insert("script".to_string(), AttrValue::String("false".to_string()));
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        logger.flush().await;

        let snapshot = run_store.state().await.unwrap();
        let node_state = snapshot.stage(&StageId::new("script_node", 1)).unwrap();
        let json = node_state.script_timing.as_ref().unwrap();
        assert_eq!(json["exit_code"], 1);
        assert_eq!(json["termination"], "exited");
    }

    #[tokio::test]
    async fn writes_script_timing_json_on_timeout() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("sleep 60".to_string()),
        );
        node.attrs.insert(
            "timeout".to_string(),
            AttrValue::Duration(Duration::from_millis(50)),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        let _err = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap_err();
        logger.flush().await;

        let snapshot = run_store.state().await.unwrap();
        let node_state = snapshot.stage(&StageId::new("script_node", 1)).unwrap();
        let json = node_state.script_timing.as_ref().unwrap();
        assert!(json["duration_ms"].is_u64());
        assert_eq!(json["exit_code"], serde_json::Value::Null);
        assert_eq!(json["termination"], "timed_out");
    }

    #[tokio::test]
    async fn stores_script_invocation_and_timing_in_run_store() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        logger.flush().await;

        let snapshot = run_store.state().await.unwrap();
        let node = snapshot
            .stage(&StageId::new("script_node", 1))
            .cloned()
            .unwrap();

        assert_eq!(node.script_invocation.unwrap()["script"], "echo hello");
        assert_eq!(node.script_timing.unwrap()["exit_code"], 0);
    }

    #[tokio::test]
    async fn script_handler_python_echo() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("print('hello from python')".to_string()),
        );
        node.attrs.insert(
            "language".to_string(),
            AttrValue::String("python".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let services = make_services();
        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Succeeded);
        let command_output = outcome.context_updates.get(keys::COMMAND_OUTPUT).unwrap();
        assert!(
            command_text(&services, command_output)
                .await
                .contains("hello from python")
        );
    }

    #[tokio::test]
    async fn script_handler_python_failure() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("raise Exception('boom')".to_string()),
        );
        node.attrs.insert(
            "language".to_string(),
            AttrValue::String("python".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Failed {
            retry_requested: false,
        });
    }

    #[tokio::test]
    async fn script_handler_invalid_language() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        node.attrs.insert(
            "language".to_string(),
            AttrValue::String("ruby".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Failed {
            retry_requested: false,
        });
        assert!(
            outcome
                .failure_reason()
                .unwrap()
                .contains("Invalid language")
        );
    }

    #[tokio::test]
    async fn tool_command_attribute_fallback() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "tool_command".to_string(),
            AttrValue::String("echo legacy".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let services = make_services();
        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Succeeded);
        let command_output = outcome.context_updates.get(keys::COMMAND_OUTPUT).unwrap();
        assert!(
            command_text(&services, command_output)
                .await
                .contains("legacy")
        );
    }

    #[tokio::test]
    async fn script_handler_merges_stderr_into_output() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo out && echo err >&2".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let services = make_services();
        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Succeeded);
        let command_output = outcome.context_updates.get(keys::COMMAND_OUTPUT).unwrap();
        assert!(
            command_text(&services, command_output)
                .await
                .contains("err"),
            "command.output should contain 'err', got: {:?}",
            command_output
        );
    }

    /// A sandbox that returns a canned `ExecResult` and captures the command,
    /// proving that `CommandHandler` delegates to the sandbox rather than
    /// spawning a host process.
    struct SpySandbox {
        exec_result:           fabro_agent::sandbox::ExecResult,
        exec_error:            Option<String>,
        captured_command:      std::sync::Mutex<Option<String>>,
        captured_env_vars:     std::sync::Mutex<Option<std::collections::HashMap<String, String>>>,
        captured_cancel_token: std::sync::Mutex<Option<bool>>,
    }

    impl SpySandbox {
        fn new(exec_result: fabro_agent::sandbox::ExecResult) -> Self {
            Self {
                exec_result,
                exec_error: None,
                captured_command: std::sync::Mutex::new(None),
                captured_env_vars: std::sync::Mutex::new(None),
                captured_cancel_token: std::sync::Mutex::new(None),
            }
        }

        fn fail(message: impl Into<String>) -> Self {
            Self {
                exec_result:           fabro_agent::sandbox::ExecResult {
                    stdout:      String::new(),
                    stderr:      String::new(),
                    exit_code:   Some(1),
                    termination: CommandTermination::Exited,
                    duration_ms: 0,
                },
                exec_error:            Some(message.into()),
                captured_command:      std::sync::Mutex::new(None),
                captured_env_vars:     std::sync::Mutex::new(None),
                captured_cancel_token: std::sync::Mutex::new(None),
            }
        }

        fn captured_command(&self) -> Option<String> {
            self.captured_command.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl fabro_agent::sandbox::Sandbox for SpySandbox {
        async fn read_file_bytes(&self, _: &str) -> fabro_sandbox::Result<Vec<u8>> {
            unimplemented!()
        }
        async fn write_file(&self, _: &str, _: &str) -> fabro_sandbox::Result<()> {
            unimplemented!()
        }
        async fn delete_file(&self, _: &str) -> fabro_sandbox::Result<()> {
            unimplemented!()
        }
        async fn file_exists(&self, _: &str) -> fabro_sandbox::Result<bool> {
            unimplemented!()
        }
        async fn list_directory(
            &self,
            _: &str,
            _: Option<usize>,
        ) -> fabro_sandbox::Result<Vec<fabro_agent::sandbox::DirEntry>> {
            unimplemented!()
        }
        async fn exec_command(
            &self,
            command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            env_vars: Option<&std::collections::HashMap<String, String>>,
            cancel_token: Option<tokio_util::sync::CancellationToken>,
        ) -> fabro_sandbox::Result<fabro_agent::sandbox::ExecResult> {
            *self.captured_command.lock().unwrap() = Some(command.to_string());
            *self.captured_env_vars.lock().unwrap() = env_vars.cloned();
            *self.captured_cancel_token.lock().unwrap() = Some(cancel_token.is_some());
            if let Some(message) = self.exec_error.as_ref() {
                return Err(fabro_sandbox::Error::message(message.clone()));
            }
            Ok(self.exec_result.clone())
        }
        async fn grep(
            &self,
            _: &str,
            _: &str,
            _: &fabro_agent::sandbox::GrepOptions,
        ) -> fabro_sandbox::Result<Vec<String>> {
            unimplemented!()
        }
        async fn glob(&self, _: &str, _: Option<&str>) -> fabro_sandbox::Result<Vec<String>> {
            unimplemented!()
        }
        async fn download_file_to_local(
            &self,
            _: &str,
            _: &std::path::Path,
        ) -> fabro_sandbox::Result<()> {
            unimplemented!()
        }
        async fn upload_file_from_local(
            &self,
            _: &std::path::Path,
            _: &str,
        ) -> fabro_sandbox::Result<()> {
            unimplemented!()
        }
        async fn initialize(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn cleanup(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        fn working_directory(&self) -> &str {
            "/mock"
        }
        fn platform(&self) -> &str {
            "linux"
        }
        fn os_version(&self) -> String {
            "Mock".into()
        }
    }

    fn make_spy_services(sandbox: std::sync::Arc<SpySandbox>) -> EngineServices {
        let mut services = make_services();
        services.run = services.run.with_sandbox(sandbox);
        services
    }

    struct RefreshingMinter {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::github_token_source::IatMinter for RefreshingMinter {
        async fn mint(&self) -> anyhow::Result<fabro_github::InstallationToken> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            Ok(fabro_github::InstallationToken {
                token:      format!("ghs_{call}"),
                expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
            })
        }
    }

    #[tokio::test]
    async fn executes_script_via_sandbox() {
        let spy = std::sync::Arc::new(SpySandbox::new(fabro_agent::sandbox::ExecResult {
            stdout:      "SANDBOX_MARKER\n".into(),
            stderr:      String::new(),
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 5,
        }));

        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let services = make_spy_services(spy.clone());
        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();

        assert_eq!(outcome.status, StageOutcome::Succeeded);
        let command_output = outcome.context_updates.get(keys::COMMAND_OUTPUT).unwrap();
        assert_eq!(
            command_text(&services, command_output).await,
            "SANDBOX_MARKER\n",
            "CommandHandler must delegate to the sandbox, not spawn a host process"
        );
        assert_eq!(
            spy.captured_command().as_deref(),
            Some("exec 2>&1\necho hello"),
            "sandbox should receive the wrapped script as the command"
        );
    }

    #[tokio::test]
    async fn executes_python_script_via_sandbox() {
        let spy = std::sync::Arc::new(SpySandbox::new(fabro_agent::sandbox::ExecResult {
            stdout:      "PYTHON_SANDBOX\n".into(),
            stderr:      String::new(),
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 5,
        }));

        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("print('hi')".to_string()),
        );
        node.attrs.insert(
            "language".to_string(),
            AttrValue::String("python".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(
                &node,
                &context,
                &graph,
                run_dir.path(),
                &make_spy_services(spy.clone()),
            )
            .await
            .unwrap();

        assert_eq!(outcome.status, StageOutcome::Succeeded);
        let captured = spy.captured_command().unwrap();
        assert!(
            captured.starts_with("exec 2>&1\npython3 -c ") && captured.contains("print"),
            "sandbox command should invoke python3 with the script, got: {captured}"
        );
    }

    #[tokio::test]
    async fn passes_env_vars_to_sandbox() {
        let spy = std::sync::Arc::new(SpySandbox::new(fabro_agent::sandbox::ExecResult {
            stdout:      String::new(),
            stderr:      String::new(),
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 5,
        }));

        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs
            .insert("script".to_string(), AttrValue::String("true".to_string()));
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let mut services = make_spy_services(spy.clone());
        services
            .base_env
            .insert("MY_VAR".to_string(), "my_value".to_string());

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();

        let captured_env = spy.captured_env_vars.lock().unwrap().clone().unwrap();
        assert_eq!(
            captured_env.get("MY_VAR").map(String::as_str),
            Some("my_value")
        );
    }

    #[tokio::test]
    async fn refreshes_github_token_for_each_command_stage_when_near_expiry() {
        let spy = std::sync::Arc::new(SpySandbox::new(fabro_agent::sandbox::ExecResult {
            stdout:      String::new(),
            stderr:      String::new(),
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 5,
        }));
        let minter = std::sync::Arc::new(RefreshingMinter {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut services = make_spy_services(spy.clone());
        services.github_token = Some(std::sync::Arc::new(
            crate::github_token_source::GitHubTokenSource::mintable(minter.clone()),
        ));

        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs
            .insert("script".to_string(), AttrValue::String("true".to_string()));
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(
            spy.captured_env_vars
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|env| env.get("GITHUB_TOKEN"))
                .map(String::as_str),
            Some("ghs_1")
        );

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(
            spy.captured_env_vars
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|env| env.get("GITHUB_TOKEN"))
                .map(String::as_str),
            Some("ghs_2")
        );
        assert_eq!(minter.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn passes_run_cancellation_to_sandbox() {
        let spy = std::sync::Arc::new(SpySandbox::new(fabro_agent::sandbox::ExecResult {
            stdout:      String::new(),
            stderr:      String::new(),
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 5,
        }));

        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs
            .insert("script".to_string(), AttrValue::String("true".to_string()));
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let mut services = make_spy_services(spy.clone());
        services.run = services
            .run
            .with_cancel_token(tokio_util::sync::CancellationToken::new());

        handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();

        assert_eq!(*spy.captured_cancel_token.lock().unwrap(), Some(true));
    }

    #[tokio::test]
    async fn script_handler_timeout_error_includes_output_tails() {
        let spy = std::sync::Arc::new(SpySandbox::new(fabro_agent::sandbox::ExecResult {
            stdout:      "partial stdout\n".into(),
            stderr:      "partial stderr\n".into(),
            exit_code:   None,
            termination: CommandTermination::TimedOut,
            duration_ms: 50,
        }));

        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("sleep 10".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let err = handler
            .execute(
                &node,
                &context,
                &graph,
                run_dir.path(),
                &make_spy_services(spy),
            )
            .await
            .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("timed out"), "got: {message}");
        assert!(
            message.contains("partial stdout"),
            "timeout error should include output tail, got: {message}"
        );
        assert!(
            message.contains("partial stderr"),
            "timeout error should include merged output tail, got: {message}"
        );
    }

    #[tokio::test]
    async fn tool_output_context_key_not_emitted() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo dual".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Succeeded);
        assert!(outcome.context_updates.contains_key(keys::COMMAND_OUTPUT));
        assert!(
            !outcome.context_updates.contains_key("tool.output"),
            "tool.output should not be emitted"
        );
    }

    #[tokio::test]
    async fn script_handler_failure_includes_output() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String(r#"echo "build output" && echo "oops" >&2 && exit 1"#.to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();

        let services = make_services();
        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Failed {
            retry_requested: false,
        });
        let reason = outcome.failure_reason().unwrap();
        assert!(
            reason.contains("build output"),
            "failure_reason should contain output, got: {reason}"
        );
        assert!(
            reason.contains("oops"),
            "failure_reason should contain merged stderr, got: {reason}"
        );
        assert!(
            reason.contains("exit code: 1"),
            "failure_reason should contain exit code, got: {reason}"
        );
    }

    #[tokio::test]
    async fn script_handler_spawn_failure() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hello".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let services = make_spy_services(std::sync::Arc::new(SpySandbox::fail("No such file")));

        let err = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("Failed to spawn script"));
        let stage_id = StageId::new("script_node", 1);
        assert!(
            !command_log_path(run_dir.path(), &stage_id).exists(),
            "spawn failure should remove pre-created output scratch log"
        );
    }

    #[tokio::test]
    async fn script_handler_failure_sets_command_output() {
        let handler = CommandHandler;
        let mut node = Node::new("script_node");
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String(r#"echo "build output" && exit 1"#.to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = tempfile::tempdir().unwrap();
        let services = make_services();

        let outcome = handler
            .execute(&node, &context, &graph, run_dir.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageOutcome::Failed {
            retry_requested: false,
        });
        let command_output = outcome
            .context_updates
            .get(keys::COMMAND_OUTPUT)
            .expect("command.output should be set on failure");
        assert!(
            command_text(&services, command_output)
                .await
                .contains("build output"),
            "command.output should contain output, got: {command_output:?}"
        );
    }
}
