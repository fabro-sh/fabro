use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_agent::Sandbox;
use fabro_types::RunId;

use crate::context::keys;
use crate::context::{Context, WorkflowContext};
use crate::error::FabroError;
use crate::event::EventEmitter;
use crate::outcome::{
    FailureCategory, FailureDetail, Outcome, OutcomeExt, StageStatus, StageUsage,
};
use crate::run_dir::{node_dir, visit_from_context};
use crate::vars::expand_vars;
use fabro_graphviz::graph::{Graph, Node};
use tokio::fs;

use super::{EngineServices, Handler};

/// Result from a `CodergenBackend` invocation.
pub enum CodergenResult {
    Text {
        text: String,
        usage: Option<StageUsage>,
        files_touched: Vec<String>,
        last_file_touched: Option<String>,
    },
    Full(Outcome),
}

/// Backend interface for LLM execution in codergen nodes.
#[allow(clippy::too_many_arguments)]
#[async_trait]
pub trait CodergenBackend: Send + Sync {
    /// Run a multi-turn agent loop (the default codergen mode).
    async fn run(
        &self,
        node: &Node,
        prompt: &str,
        context: &Context,
        thread_id: Option<&str>,
        emitter: &Arc<EventEmitter>,
        stage_dir: &Path,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<CodergenResult, FabroError>;

    /// Run a single LLM call with no tools (one_shot mode).
    async fn one_shot(
        &self,
        _node: &Node,
        _prompt: &str,
        _system_prompt: Option<&str>,
        _stage_dir: &Path,
    ) -> Result<CodergenResult, FabroError> {
        Err(FabroError::Validation(
            "one_shot mode not supported by this backend".into(),
        ))
    }
}

/// The default handler for LLM task nodes.
pub struct AgentHandler {
    backend: Option<Box<dyn CodergenBackend>>,
}

impl AgentHandler {
    #[must_use]
    pub fn new(backend: Option<Box<dyn CodergenBackend>>) -> Self {
        Self { backend }
    }
}

/// Expand `$variable` placeholders in text using graph attributes.
///
/// Known variables are built from graph attributes (e.g. `$goal`). Any
/// `$identifier` not in the map produces an error, catching typos like
/// `$gaol` at runtime.
pub(crate) fn expand_variables(text: &str, graph: &Graph) -> Result<String, FabroError> {
    let vars = HashMap::from([("goal".to_string(), graph.goal().to_string())]);
    expand_vars(text, &vars).map_err(|e| FabroError::Validation(e.to_string()))
}

/// Status fields that indicate a JSON object contains routing directives.
const STATUS_FIELDS: &[&str] = &[
    "preferred_next_label",
    "outcome",
    "failure_reason",
    "suggested_next_ids",
    "context_updates",
];

/// Find all balanced `{...}` JSON object substrings in the text.
fn find_json_objects(text: &str) -> Vec<&str> {
    let mut results = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let start = i;
            let mut depth = 0;
            let mut in_string = false;
            let mut escape = false;
            let mut j = i;
            while j < bytes.len() {
                let c = bytes[j];
                if escape {
                    escape = false;
                } else if c == b'\\' && in_string {
                    escape = true;
                } else if c == b'"' {
                    in_string = !in_string;
                } else if !in_string {
                    if c == b'{' {
                        depth += 1;
                    } else if c == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            results.push(&text[start..=j]);
                            break;
                        }
                    }
                }
                j += 1;
            }
        }
        i += 1;
    }
    results
}

/// Extract routing directives from LLM response text.
///
/// Searches for the last JSON object in the response that contains at least
/// one status field (`preferred_next_label`, `outcome`, `suggested_next_ids`,
/// `context_updates`). Merges extracted fields into the outcome.
pub(crate) fn extract_status_fields(text: &str, outcome: &mut Outcome) -> bool {
    let candidates = find_json_objects(text);

    let parsed = candidates.iter().rev().find_map(|candidate| {
        let value: serde_json::Value = serde_json::from_str(candidate).ok()?;
        if let Some(obj) = value.as_object() {
            if STATUS_FIELDS.iter().any(|f| obj.contains_key(*f)) {
                return Some(value);
            }
        }
        None
    });

    let Some(value) = parsed else { return false };
    let Some(obj) = value.as_object() else {
        return false;
    };

    if let Some(label) = obj.get("preferred_next_label").and_then(|v| v.as_str()) {
        outcome.preferred_label = Some(label.to_string());
    }

    if let Some(ids) = obj.get("suggested_next_ids").and_then(|v| v.as_array()) {
        let string_ids: Vec<String> = ids
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if !string_ids.is_empty() {
            outcome.suggested_next_ids = string_ids;
        }
    }

    if let Some(status_str) = obj.get("outcome").and_then(|v| v.as_str()) {
        if let Ok(status) = status_str.parse::<StageStatus>() {
            outcome.status = status;
            if outcome.status == StageStatus::Fail {
                if let Some(reason) = obj.get("failure_reason").and_then(|v| v.as_str()) {
                    outcome.failure =
                        Some(FailureDetail::new(reason, FailureCategory::Deterministic));
                }
            }
        }
    }

    if let Some(updates) = obj.get("context_updates").and_then(|v| v.as_object()) {
        for (key, val) in updates {
            outcome.context_updates.insert(key.clone(), val.clone());
        }
    }

    true
}

/// Truncate a string to at most `max_chars` characters (char-boundary safe).
pub(crate) fn truncate(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        s
    } else {
        &s[..fabro_agent::floor_char_boundary(s, max_chars)]
    }
}

/// Shared simulate implementation for LLM-backed handlers (agent & prompt).
/// Produces a simulated outcome with standard context updates.
pub(crate) fn simulate_llm_handler(node: &Node) -> Outcome {
    let simulated_text = format!("[Simulated] Response for stage: {}", node.id);
    let mut outcome = Outcome::simulated(&node.id);
    outcome
        .context_updates
        .insert(keys::LAST_STAGE.to_string(), serde_json::json!(node.id));
    outcome.context_updates.insert(
        keys::LAST_RESPONSE.to_string(),
        serde_json::json!(truncate(&simulated_text, 200)),
    );
    outcome.context_updates.insert(
        keys::response_key(&node.id),
        serde_json::json!(&simulated_text),
    );
    outcome
}

#[async_trait]
impl Handler for AgentHandler {
    async fn simulate(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        Ok(simulate_llm_handler(node))
    }

    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        // 1. Build prompt (prepend fidelity preamble if present)
        let raw_prompt = node
            .prompt()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| node.label());
        let expanded = expand_variables(raw_prompt, graph)?;
        let preamble = context.preamble();
        let prompt = if preamble.is_empty() {
            expanded
        } else {
            format!("{preamble}\n\n{expanded}")
        };

        // 2. Write prompt to logs
        let visit = visit_from_context(context);
        let stage_dir = node_dir(run_dir, &node.id, visit);
        fs::create_dir_all(&stage_dir).await?;
        fs::write(stage_dir.join("prompt.md"), &prompt).await?;

        // 3. Call LLM backend (agent loop)
        let thread_id = context.thread_id();
        let run_id = context
            .run_id()
            .parse::<RunId>()
            .map_err(|err| FabroError::handler(format!("invalid internal run_id: {err}")))?;
        let tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>> =
            services.hook_runner.as_ref().map(|hr| {
                Arc::new(fabro_hooks::WorkflowToolHookCallback {
                    hook_runner: Arc::clone(hr),
                    sandbox: Arc::clone(&services.sandbox),
                    run_id,
                    workflow_name: graph.name.clone(),
                    work_dir: None,
                    node_id: node.id.clone(),
                }) as Arc<dyn fabro_agent::ToolHookCallback>
            });
        let (response_text, stage_usage, backend_files_touched, last_file_touched) =
            if let Some(backend) = &self.backend {
                let result = backend
                    .run(
                        node,
                        &prompt,
                        context,
                        thread_id.as_deref(),
                        &services.emitter,
                        &stage_dir,
                        &services.sandbox,
                        tool_hooks,
                    )
                    .await;
                match result {
                    Ok(CodergenResult::Full(outcome)) => return Ok(outcome),
                    Ok(CodergenResult::Text {
                        text,
                        usage,
                        files_touched,
                        last_file_touched,
                    }) => (text, usage, files_touched, last_file_touched),
                    Err(e) if e.is_retryable() => {
                        return Err(e);
                    }
                    Err(e) => {
                        return Ok(e.to_fail_outcome());
                    }
                }
            } else {
                (
                    format!("[Simulated] Response for stage: {}", node.id),
                    None,
                    Vec::new(),
                    None,
                )
            };

        // 4. Write response to logs
        fs::write(stage_dir.join("response.md"), &response_text).await?;
        // 7. Build and write status
        let mut outcome = Outcome::success();
        outcome.notes = Some(format!("Stage completed: {}", node.id));
        outcome
            .context_updates
            .insert(keys::LAST_STAGE.to_string(), serde_json::json!(node.id));
        outcome.context_updates.insert(
            keys::LAST_RESPONSE.to_string(),
            serde_json::json!(truncate(&response_text, 200)),
        );
        outcome.context_updates.insert(
            keys::response_key(&node.id),
            serde_json::json!(&response_text),
        );

        // 7b. Parse routing directives from response text, falling back to
        //     status.json written by the agent into the sandbox CWD, then to
        //     the last file the agent wrote.
        let found_in_response = extract_status_fields(&response_text, &mut outcome);
        if !found_in_response {
            let mut found_in_status_json = false;
            if let Ok(result) = services
                .sandbox
                .exec_command("cat status.json", 5_000, None, None, None)
                .await
            {
                if result.exit_code == 0 {
                    found_in_status_json = extract_status_fields(&result.stdout, &mut outcome);
                }
            }
            if !found_in_status_json {
                if let Some(ref path) = last_file_touched {
                    let quoted = shlex::try_quote(path).unwrap_or_else(|_| path.into());
                    let cmd = format!("cat {quoted}");
                    if let Ok(result) = services
                        .sandbox
                        .exec_command(&cmd, 5_000, None, None, None)
                        .await
                    {
                        if result.exit_code == 0 {
                            extract_status_fields(&result.stdout, &mut outcome);
                        }
                    }
                }
            }
        }
        outcome.usage = stage_usage;
        outcome.files_touched = backend_files_touched;

        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventEmitter;
    use fabro_graphviz::graph::AttrValue;
    use fabro_store::{InMemoryStore, NodeVisitRef, RunStore, Store};
    use fabro_types::fixtures;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    async fn make_services_with_run_store() -> (
        EngineServices,
        Arc<dyn RunStore>,
        crate::event::StoreProgressLogger,
    ) {
        let store = InMemoryStore::default();
        let run_store = store
            .create_run(&fixtures::RUN_1, chrono::Utc::now(), None)
            .await
            .unwrap();
        let services = EngineServices {
            run_store: Arc::clone(&run_store),
            ..EngineServices::test_default()
        };
        let logger = crate::event::StoreProgressLogger::new(Arc::clone(&run_store));
        logger.register(services.emitter.as_ref());
        (services, run_store, logger)
    }

    fn test_context() -> Context {
        let context = Context::new();
        context.set(
            crate::context::keys::INTERNAL_RUN_ID,
            serde_json::json!(fixtures::RUN_1.to_string()),
        );
        context
    }

    #[tokio::test]
    async fn codergen_handler_simulate() {
        let handler = AgentHandler::new(None);
        let node = Node::new("plan");
        let context = test_context();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .simulate(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert_eq!(outcome.notes.as_deref(), Some("[Simulated] plan"));
        assert_eq!(
            outcome.context_updates.get(keys::LAST_STAGE),
            Some(&serde_json::json!("plan"))
        );
        assert!(outcome.context_updates.contains_key(keys::LAST_RESPONSE));
        assert_eq!(
            outcome.context_updates.get(&keys::response_key("plan")),
            Some(&serde_json::json!("[Simulated] Response for stage: plan"))
        );
    }

    #[tokio::test]
    async fn codergen_handler_variable_expansion() {
        let handler = AgentHandler::new(None);
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Achieve: $goal".to_string()),
        );
        let context = test_context();
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Build a feature".to_string()),
        );
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let prompt_content =
            std::fs::read_to_string(tmp.path().join("nodes").join("plan").join("prompt.md"))
                .unwrap();
        assert_eq!(prompt_content, "Achieve: Build a feature");
    }

    #[tokio::test]
    async fn codergen_handler_falls_back_to_label() {
        let handler = AgentHandler::new(None);
        let mut node = Node::new("work");
        node.attrs.insert(
            "label".to_string(),
            AttrValue::String("Do work".to_string()),
        );
        let context = test_context();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let prompt_content =
            std::fs::read_to_string(tmp.path().join("nodes").join("work").join("prompt.md"))
                .unwrap();
        assert_eq!(prompt_content, "Do work");
    }

    #[tokio::test]
    async fn codergen_handler_context_updates() {
        let handler = AgentHandler::new(None);
        let node = Node::new("step");
        let context = test_context();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        assert_eq!(
            outcome.context_updates.get(keys::LAST_STAGE),
            Some(&serde_json::json!("step"))
        );
        assert!(outcome.context_updates.contains_key(keys::LAST_RESPONSE));
        assert_eq!(
            outcome.context_updates.get(&keys::response_key("step")),
            Some(&serde_json::json!("[Simulated] Response for stage: step"))
        );
    }

    #[tokio::test]
    async fn codergen_handler_falls_back_to_status_json_in_sandbox() {
        // Simulation mode returns text with no JSON directives, so the
        // handler should fall back to reading status.json from the sandbox CWD.
        let sandbox_dir = TempDir::new().unwrap();
        std::fs::write(
            sandbox_dir.path().join("status.json"),
            r#"{"outcome": "fail", "failure_reason": "tests failed"}"#,
        )
        .unwrap();

        let handler = AgentHandler::new(None);
        let node = Node::new("step");
        let context = test_context();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let mut services = EngineServices::test_default();
        services.sandbox = std::sync::Arc::new(fabro_agent::LocalSandbox::new(
            sandbox_dir.path().to_path_buf(),
        ));

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();

        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
        assert_eq!(outcome.failure_reason(), Some("tests failed"));
    }

    #[tokio::test]
    async fn codergen_handler_prefers_response_text_over_status_json() {
        use std::sync::Arc;

        // Backend returns response text with routing directives — status.json
        // in the sandbox should be ignored.
        struct DirectiveBackend;

        #[async_trait]
        impl CodergenBackend for DirectiveBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &Path,
                _sandbox: &Arc<dyn fabro_agent::Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                Ok(CodergenResult::Text {
                    text: r#"Done. {"outcome": "success", "preferred_next_label": "approve"}"#
                        .to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                    last_file_touched: None,
                })
            }
        }

        let sandbox_dir = TempDir::new().unwrap();
        std::fs::write(
            sandbox_dir.path().join("status.json"),
            r#"{"outcome": "fail", "failure_reason": "should be ignored"}"#,
        )
        .unwrap();

        let handler = AgentHandler::new(Some(Box::new(DirectiveBackend)));
        let node = Node::new("step");
        let context = test_context();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let mut services = EngineServices::test_default();
        services.sandbox = std::sync::Arc::new(fabro_agent::LocalSandbox::new(
            sandbox_dir.path().to_path_buf(),
        ));

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();

        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert_eq!(outcome.preferred_label.as_deref(), Some("approve"));
        assert!(outcome.failure.is_none());
    }

    #[tokio::test]
    async fn codergen_handler_extracts_status_from_last_file_touched() {
        use std::sync::Arc;

        struct LastFileBackend;

        #[async_trait]
        impl CodergenBackend for LastFileBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &Path,
                _sandbox: &Arc<dyn fabro_agent::Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                Ok(CodergenResult::Text {
                    text: "Done writing results.".to_string(),
                    usage: None,
                    files_touched: vec!["results.md".to_string()],
                    last_file_touched: Some("results.md".to_string()),
                })
            }
        }

        let sandbox_dir = TempDir::new().unwrap();
        // Write status fields into the file the agent "touched" — no status.json
        std::fs::write(
            sandbox_dir.path().join("results.md"),
            r#"# Results
{"context_updates": {"verified": "true"}}
"#,
        )
        .unwrap();

        let handler = AgentHandler::new(Some(Box::new(LastFileBackend)));
        let node = Node::new("step");
        let context = test_context();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let mut services = EngineServices::test_default();
        services.sandbox = std::sync::Arc::new(fabro_agent::LocalSandbox::new(
            sandbox_dir.path().to_path_buf(),
        ));

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();

        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert_eq!(
            outcome.context_updates.get("verified"),
            Some(&serde_json::json!("true")),
        );
    }

    #[tokio::test]
    async fn codergen_handler_projects_provider_used_from_agent_session_events() {
        struct ProviderEventBackend;

        #[async_trait]
        impl CodergenBackend for ProviderEventBackend {
            async fn run(
                &self,
                node: &Node,
                _prompt: &str,
                context: &Context,
                _thread_id: Option<&str>,
                emitter: &Arc<EventEmitter>,
                _stage_dir: &Path,
                _sandbox: &Arc<dyn fabro_agent::Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                emitter.emit(&crate::event::WorkflowRunEvent::Agent {
                    stage: node.id.clone(),
                    visit: crate::run_dir::visit_from_context(context) as u32,
                    event: fabro_agent::AgentEvent::SessionStarted {
                        provider: Some("openai".to_string()),
                        model: Some("gpt-5.4".to_string()),
                    },
                    session_id: Some("session_123".to_string()),
                    parent_session_id: None,
                });
                Ok(CodergenResult::Text {
                    text: "done".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                    last_file_touched: None,
                })
            }
        }

        let handler = AgentHandler::new(Some(Box::new(ProviderEventBackend)));
        let node = Node::new("step");
        let context = test_context();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();
        logger.flush().await;

        let snapshot = run_store
            .get_node(&NodeVisitRef {
                node_id: "step",
                visit: 1,
            })
            .await
            .unwrap();
        assert_eq!(snapshot.provider_used.unwrap()["provider"], "openai");
    }

    #[test]
    fn expand_variables_replaces_goal() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );
        let result = expand_variables("Goal is: $goal, do it", &graph).unwrap();
        assert_eq!(result, "Goal is: Fix bugs, do it");
    }

    #[test]
    fn expand_variables_errors_on_unknown_variable() {
        let graph = Graph::new("test");
        let err = expand_variables("Do $foo now", &graph).unwrap_err();
        assert!(
            err.to_string().contains("Undefined variable: $foo"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn expand_variables_allows_bare_dollar() {
        let graph = Graph::new("test");
        let result = expand_variables("costs $5", &graph).unwrap();
        assert_eq!(result, "costs $5");
    }

    #[test]
    fn expand_variables_allows_dollar_alone() {
        let graph = Graph::new("test");
        let result = expand_variables("just a $ sign", &graph).unwrap();
        assert_eq!(result, "just a $ sign");
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 200), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let long = "a".repeat(300);
        assert_eq!(truncate(&long, 200).len(), 200);
    }

    #[tokio::test]
    async fn codergen_handler_passes_thread_id_to_backend() {
        use std::sync::{Arc, Mutex};

        struct ThreadCapturingBackend {
            captured_thread_id: Arc<Mutex<Option<Option<String>>>>,
        }

        #[async_trait]
        impl CodergenBackend for ThreadCapturingBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &Path,
                _sandbox: &Arc<dyn Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                *self.captured_thread_id.lock().unwrap() = Some(thread_id.map(String::from));
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                    last_file_touched: None,
                })
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let backend = ThreadCapturingBackend {
            captured_thread_id: captured.clone(),
        };
        let handler = AgentHandler::new(Some(Box::new(backend)));

        let node = Node::new("work");
        let context = test_context();
        // Simulate what the engine stores in internal.thread_id
        context.set(keys::INTERNAL_THREAD_ID, serde_json::json!("main"));
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let result = captured.lock().unwrap().clone();
        assert_eq!(result, Some(Some("main".to_string())));
    }

    #[tokio::test]
    async fn codergen_handler_passes_none_thread_id_when_absent() {
        use std::sync::{Arc, Mutex};

        struct ThreadCapturingBackend {
            captured_thread_id: Arc<Mutex<Option<Option<String>>>>,
        }

        #[async_trait]
        impl CodergenBackend for ThreadCapturingBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &Path,
                _sandbox: &Arc<dyn Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                *self.captured_thread_id.lock().unwrap() = Some(thread_id.map(String::from));
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                    last_file_touched: None,
                })
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let backend = ThreadCapturingBackend {
            captured_thread_id: captured.clone(),
        };
        let handler = AgentHandler::new(Some(Box::new(backend)));

        let node = Node::new("work");
        let context = test_context();
        // No thread context set
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let result = captured.lock().unwrap().clone();
        assert_eq!(result, Some(None));
    }

    #[tokio::test]
    async fn codergen_handler_propagates_retryable_backend_error() {
        struct FailingBackend;

        #[async_trait]
        impl CodergenBackend for FailingBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &Path,
                _sandbox: &Arc<dyn Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                Err(FabroError::handler("Request timed out".to_string()))
            }
        }

        let handler = AgentHandler::new(Some(Box::new(FailingBackend)));
        let node = Node::new("step");
        let context = test_context();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let result = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await;
        let err = result.unwrap_err();
        assert!(err.is_retryable());
        assert!(err.to_string().contains("Request timed out"));
    }

    #[test]
    fn extract_status_fields_from_fenced_code_block() {
        let text = r#"Here is my analysis of the code.

```json
{"preferred_next_label": "fix", "outcome": "success"}
```

That's it."#;
        let mut outcome = Outcome::success();
        extract_status_fields(text, &mut outcome);
        assert_eq!(outcome.preferred_label.as_deref(), Some("fix"));
    }

    #[test]
    fn extract_status_fields_from_bare_json() {
        let text = r#"I recommend routing to fix.
{"preferred_next_label": "fix_batch"}"#;
        let mut outcome = Outcome::success();
        extract_status_fields(text, &mut outcome);
        assert_eq!(outcome.preferred_label.as_deref(), Some("fix_batch"));
    }

    #[test]
    fn extract_status_fields_no_json() {
        let text = "Just some plain text response with no JSON at all.";
        let mut outcome = Outcome::success();
        extract_status_fields(text, &mut outcome);
        assert!(outcome.preferred_label.is_none());
        assert!(outcome.suggested_next_ids.is_empty());
    }

    #[test]
    fn extract_status_fields_json_without_status_fields() {
        let text = r#"Here is some data: {"name": "test", "count": 42}"#;
        let mut outcome = Outcome::success();
        extract_status_fields(text, &mut outcome);
        assert!(outcome.preferred_label.is_none());
        assert!(outcome.suggested_next_ids.is_empty());
    }

    #[test]
    fn extract_status_fields_context_updates_and_suggested_ids() {
        let text = r#"```json
{
  "preferred_next_label": "review",
  "suggested_next_ids": ["node_a", "node_b"],
  "context_updates": {"fix.files_changed": 3, "fix.summary": "patched"}
}
```"#;
        let mut outcome = Outcome::success();
        outcome
            .context_updates
            .insert("existing_key".to_string(), serde_json::json!("keep"));
        extract_status_fields(text, &mut outcome);
        assert_eq!(outcome.preferred_label.as_deref(), Some("review"));
        assert_eq!(outcome.suggested_next_ids, vec!["node_a", "node_b"]);
        assert_eq!(
            outcome.context_updates.get("fix.files_changed"),
            Some(&serde_json::json!(3))
        );
        assert_eq!(
            outcome.context_updates.get("fix.summary"),
            Some(&serde_json::json!("patched"))
        );
        // Existing keys preserved
        assert_eq!(
            outcome.context_updates.get("existing_key"),
            Some(&serde_json::json!("keep"))
        );
    }

    #[test]
    fn extract_status_fields_outcome_fail_with_reason() {
        let text = r#"{"outcome": "fail", "failure_reason": "tests failed"}"#;
        let mut outcome = Outcome::success();
        extract_status_fields(text, &mut outcome);
        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
        assert_eq!(outcome.failure_reason(), Some("tests failed"));
    }

    #[test]
    fn extract_status_fields_outcome_success() {
        let text = r#"{"outcome": "success"}"#;
        let mut outcome = Outcome::success();
        extract_status_fields(text, &mut outcome);
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert!(outcome.failure.is_none());
    }

    #[test]
    fn extract_status_fields_outcome_fail_without_reason() {
        let text = r#"{"outcome": "fail"}"#;
        let mut outcome = Outcome::success();
        extract_status_fields(text, &mut outcome);
        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
        assert!(outcome.failure.is_none());
    }

    #[test]
    fn extract_status_fields_uses_last_match() {
        let text = r#"{"preferred_next_label": "first"}
Some text in between.
{"preferred_next_label": "second"}"#;
        let mut outcome = Outcome::success();
        extract_status_fields(text, &mut outcome);
        assert_eq!(outcome.preferred_label.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn codergen_handler_returns_fail_outcome_for_non_retryable_backend_error() {
        struct ValidationFailBackend;

        #[async_trait]
        impl CodergenBackend for ValidationFailBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &Path,
                _sandbox: &Arc<dyn Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                Err(FabroError::Validation("bad config".to_string()))
            }
        }

        let handler = AgentHandler::new(Some(Box::new(ValidationFailBackend)));
        let node = Node::new("step");
        let context = test_context();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
        assert!(outcome.failure_reason().unwrap().contains("bad config"));
    }

    #[tokio::test]
    async fn codergen_handler_prepends_preamble_to_prompt() {
        use std::sync::{Arc, Mutex};

        struct PromptCapturingBackend {
            captured_prompt: Arc<Mutex<Option<String>>>,
        }

        #[async_trait]
        impl CodergenBackend for PromptCapturingBackend {
            async fn run(
                &self,
                _node: &Node,
                prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &std::path::Path,
                _sandbox: &Arc<dyn Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                *self.captured_prompt.lock().unwrap() = Some(prompt.to_string());
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                    last_file_touched: None,
                })
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let backend = PromptCapturingBackend {
            captured_prompt: captured.clone(),
        };
        let handler = AgentHandler::new(Some(Box::new(backend)));

        let mut node = Node::new("report");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Summarize the results".to_string()),
        );
        let context = test_context();
        context.set(
            keys::CURRENT_PREAMBLE,
            serde_json::json!("## Test Output\n10 passed, 0 failed"),
        );
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let prompt = captured.lock().unwrap().clone().unwrap();
        assert!(
            prompt.starts_with("## Test Output\n10 passed, 0 failed"),
            "prompt should start with preamble, got: {prompt}"
        );
        assert!(
            prompt.ends_with("Summarize the results"),
            "prompt should end with original prompt, got: {prompt}"
        );
        assert!(
            prompt.contains("\n\nSummarize"),
            "preamble and prompt should be separated by blank line"
        );
    }

    #[tokio::test]
    async fn codergen_handler_no_preamble_when_empty() {
        use std::sync::{Arc, Mutex};

        struct PromptCapturingBackend {
            captured_prompt: Arc<Mutex<Option<String>>>,
        }

        #[async_trait]
        impl CodergenBackend for PromptCapturingBackend {
            async fn run(
                &self,
                _node: &Node,
                prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &std::path::Path,
                _sandbox: &Arc<dyn Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                *self.captured_prompt.lock().unwrap() = Some(prompt.to_string());
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                    last_file_touched: None,
                })
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let backend = PromptCapturingBackend {
            captured_prompt: captured.clone(),
        };
        let handler = AgentHandler::new(Some(Box::new(backend)));

        let mut node = Node::new("report");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Summarize the results".to_string()),
        );
        let context = test_context();
        // No preamble set -- context.get_string returns ""
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let prompt = captured.lock().unwrap().clone().unwrap();
        assert_eq!(prompt, "Summarize the results");
    }

    #[tokio::test]
    async fn codergen_handler_preamble_written_to_prompt_md() {
        let handler = AgentHandler::new(None);
        let mut node = Node::new("report");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Summarize".to_string()),
        );
        let context = test_context();
        context.set(
            keys::CURRENT_PREAMBLE,
            serde_json::json!("## Script Output\nAll tests passed"),
        );
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let prompt_content =
            std::fs::read_to_string(tmp.path().join("nodes").join("report").join("prompt.md"))
                .unwrap();
        assert!(
            prompt_content.contains("## Script Output\nAll tests passed"),
            "prompt.md should contain preamble"
        );
        assert!(
            prompt_content.contains("Summarize"),
            "prompt.md should contain original prompt"
        );
    }
}
