use std::path::Path;
use std::sync::Arc;

use arc_agent::ExecutionEnvironment;
use async_trait::async_trait;

use crate::context::Context;
use crate::error::ArcError;
use crate::event::EventEmitter;
use crate::graph::{CodergenMode, Graph, Node};
use crate::outcome::{Outcome, StageUsage};

use super::{EngineServices, Handler};

/// Result from a `CodergenBackend` invocation.
pub enum CodergenResult {
    Text {
        text: String,
        usage: Option<StageUsage>,
        files_touched: Vec<String>,
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
        execution_env: &Arc<dyn ExecutionEnvironment>,
    ) -> Result<CodergenResult, ArcError>;

    /// Run a single LLM call with no tools (one_shot mode).
    async fn one_shot(
        &self,
        _node: &Node,
        _prompt: &str,
        _stage_dir: &Path,
    ) -> Result<CodergenResult, ArcError> {
        Err(ArcError::Validation(
            "one_shot mode not supported by this backend".into(),
        ))
    }
}

/// The default handler for LLM task nodes.
pub struct CodergenHandler {
    backend: Option<Box<dyn CodergenBackend>>,
}

impl CodergenHandler {
    #[must_use]
    pub fn new(backend: Option<Box<dyn CodergenBackend>>) -> Self {
        Self { backend }
    }
}

/// Expand `$goal` in text using the graph goal.
fn expand_variables(text: &str, graph: &Graph) -> String {
    text.replace("$goal", graph.goal())
}

/// Status fields that indicate a JSON object contains routing directives.
const STATUS_FIELDS: &[&str] = &[
    "preferred_next_label",
    "outcome",
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
fn extract_status_fields(text: &str, outcome: &mut Outcome) {
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

    let Some(value) = parsed else { return };
    let Some(obj) = value.as_object() else { return };

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

    if let Some(updates) = obj.get("context_updates").and_then(|v| v.as_object()) {
        for (key, val) in updates {
            outcome.context_updates.insert(key.clone(), val.clone());
        }
    }
}

/// Truncate a string to at most `max_chars` characters.
fn truncate(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        s
    } else {
        &s[..max_chars]
    }
}

/// Resolve a tool hook command from node attributes, falling back to graph attributes.
fn resolve_hook(node: &Node, graph: &Graph, key: &str) -> Option<String> {
    node.attrs
        .get(key)
        .and_then(|v| v.as_str())
        .or_else(|| graph.attrs.get(key).and_then(|v| v.as_str()))
        .map(String::from)
}

/// Execute a tool hook shell command. Returns true if the command succeeded (exit 0).
fn run_hook(command: &str, node_id: &str, work_dir: Option<&Path>) -> bool {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command).env("ARC_NODE_ID", node_id);
    if let Some(wd) = work_dir {
        cmd.current_dir(wd);
    }
    match cmd.output() {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

#[async_trait]
impl Handler for CodergenHandler {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        logs_root: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, ArcError> {
        // 1. Build prompt (prepend fidelity preamble if present)
        let raw_prompt = node
            .prompt()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| node.label());
        let expanded = expand_variables(raw_prompt, graph);
        let preamble = context.get_string("current.preamble", "");
        let prompt = if preamble.is_empty() {
            expanded
        } else {
            format!("{preamble}\n\n{expanded}")
        };

        // 2. Write prompt to logs
        let visit = crate::engine::visit_from_context(context);
        let stage_dir = crate::engine::node_dir(logs_root, &node.id, visit);
        tokio::fs::create_dir_all(&stage_dir).await?;
        tokio::fs::write(stage_dir.join("prompt.md"), &prompt).await?;

        // Resolve work_dir from context for hooks
        let work_dir_str = context
            .get("internal.work_dir")
            .and_then(|v| v.as_str().map(String::from));
        let work_dir = work_dir_str.as_deref().map(Path::new);

        // 3. Execute pre-hook (spec 9.7)
        if let Some(pre_hook) = resolve_hook(node, graph, "tool_hooks.pre") {
            if !run_hook(&pre_hook, &node.id, work_dir) {
                let mut outcome = Outcome::skipped();
                outcome.notes = Some("pre-hook returned non-zero, tool call skipped".to_string());
                return Ok(outcome);
            }
        }

        // 4. Call LLM backend
        let mode = node.codergen_mode()?;
        let thread_id = context
            .get("internal.thread_id")
            .and_then(|v| v.as_str().map(String::from));
        let (response_text, stage_usage, backend_files_touched) =
            if let Some(backend) = &self.backend {
                let result = match mode {
                    CodergenMode::AgentLoop => {
                        backend
                            .run(
                                node,
                                &prompt,
                                context,
                                thread_id.as_deref(),
                                &services.emitter,
                                &stage_dir,
                                &services.execution_env,
                            )
                            .await
                    }
                    CodergenMode::OneShot => backend.one_shot(node, &prompt, &stage_dir).await,
                };
                match result {
                    Ok(CodergenResult::Full(outcome)) => {
                        let status_json = serde_json::to_string_pretty(&outcome)
                            .unwrap_or_else(|_| "{}".to_string());
                        tokio::fs::write(stage_dir.join("status.json"), &status_json).await?;
                        return Ok(outcome);
                    }
                    Ok(CodergenResult::Text {
                        text,
                        usage,
                        files_touched,
                    }) => (text, usage, files_touched),
                    Err(e) if e.is_retryable() => {
                        return Err(e);
                    }
                    Err(e) => {
                        return Ok(Outcome::fail(e.to_string()));
                    }
                }
            } else {
                (
                    format!("[Simulated] Response for stage: {}", node.id),
                    None,
                    Vec::new(),
                )
            };

        // 5. Execute post-hook (spec 9.7)
        if let Some(post_hook) = resolve_hook(node, graph, "tool_hooks.post") {
            if !run_hook(&post_hook, &node.id, work_dir) {
                context.append_log(format!("post-hook failed for node {}, continuing", node.id));
            }
        }

        // 6. Write response to logs
        tokio::fs::write(stage_dir.join("response.md"), &response_text).await?;

        // 7. Build and write status
        let mut outcome = Outcome::success();
        outcome.notes = Some(format!("Stage completed: {}", node.id));
        outcome
            .context_updates
            .insert("last_stage".to_string(), serde_json::json!(node.id));
        outcome.context_updates.insert(
            "last_response".to_string(),
            serde_json::json!(truncate(&response_text, 200)),
        );
        outcome.context_updates.insert(
            format!("response.{}", node.id),
            serde_json::json!(&response_text),
        );

        // 7b. Parse routing directives from response text
        extract_status_fields(&response_text, &mut outcome);
        outcome.usage = stage_usage;
        outcome.files_touched = backend_files_touched;

        let status_json =
            serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".to_string());
        tokio::fs::write(stage_dir.join("status.json"), &status_json).await?;

        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventEmitter;
    use crate::graph::AttrValue;
    use crate::handler::start::StartHandler;
    use crate::handler::HandlerRegistry;
    use tempfile::TempDir;

    fn make_services() -> EngineServices {
        EngineServices {
            registry: std::sync::Arc::new(HandlerRegistry::new(Box::new(StartHandler))),
            emitter: std::sync::Arc::new(EventEmitter::new()),
            execution_env: std::sync::Arc::new(arc_agent::LocalExecutionEnvironment::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )),
        }
    }

    #[tokio::test]
    async fn codergen_handler_simulation_mode() {
        let handler = CodergenHandler::new(None);
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Plan the implementation".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert_eq!(outcome.notes.as_deref(), Some("Stage completed: plan"));

        // Check files were written
        let prompt_path = tmp.path().join("nodes").join("plan").join("prompt.md");
        assert!(prompt_path.exists());
        let prompt_content = std::fs::read_to_string(&prompt_path).unwrap();
        assert_eq!(prompt_content, "Plan the implementation");

        let response_path = tmp.path().join("nodes").join("plan").join("response.md");
        assert!(response_path.exists());
        let response_content = std::fs::read_to_string(&response_path).unwrap();
        assert!(response_content.contains("[Simulated]"));

        let status_path = tmp.path().join("nodes").join("plan").join("status.json");
        assert!(status_path.exists());
    }

    #[tokio::test]
    async fn codergen_handler_variable_expansion() {
        let handler = CodergenHandler::new(None);
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Achieve: $goal".to_string()),
        );
        let context = Context::new();
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
        let handler = CodergenHandler::new(None);
        let mut node = Node::new("work");
        node.attrs.insert(
            "label".to_string(),
            AttrValue::String("Do work".to_string()),
        );
        let context = Context::new();
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
        let handler = CodergenHandler::new(None);
        let node = Node::new("step");
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        assert_eq!(
            outcome.context_updates.get("last_stage"),
            Some(&serde_json::json!("step"))
        );
        assert!(outcome.context_updates.contains_key("last_response"));
        assert_eq!(
            outcome.context_updates.get("response.step"),
            Some(&serde_json::json!("[Simulated] Response for stage: step"))
        );
    }

    #[test]
    fn expand_variables_replaces_goal() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );
        let result = expand_variables("Goal is: $goal, do it", &graph);
        assert_eq!(result, "Goal is: Fix bugs, do it");
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
    async fn codergen_handler_pre_hook_failure_skips_backend() {
        let handler = CodergenHandler::new(None);
        let mut node = Node::new("step");
        node.attrs.insert(
            "tool_hooks.pre".to_string(),
            AttrValue::String("exit 1".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Skipped);
        assert!(outcome.notes.as_deref().unwrap().contains("pre-hook"));
    }

    #[tokio::test]
    async fn codergen_handler_pre_hook_success_continues() {
        let handler = CodergenHandler::new(None);
        let mut node = Node::new("step");
        node.attrs.insert(
            "tool_hooks.pre".to_string(),
            AttrValue::String("exit 0".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
    }

    #[tokio::test]
    async fn codergen_handler_post_hook_failure_logs_warning() {
        let handler = CodergenHandler::new(None);
        let mut node = Node::new("step");
        node.attrs.insert(
            "tool_hooks.post".to_string(),
            AttrValue::String("exit 1".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        // Post-hook failure should not fail the node
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
    }

    #[test]
    fn resolve_hook_from_node_attr() {
        let mut node = Node::new("step");
        node.attrs.insert(
            "tool_hooks.pre".to_string(),
            AttrValue::String("echo node".to_string()),
        );
        let graph = Graph::new("test");
        assert_eq!(
            resolve_hook(&node, &graph, "tool_hooks.pre"),
            Some("echo node".to_string())
        );
    }

    #[test]
    fn resolve_hook_falls_back_to_graph() {
        let node = Node::new("step");
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "tool_hooks.pre".to_string(),
            AttrValue::String("echo graph".to_string()),
        );
        assert_eq!(
            resolve_hook(&node, &graph, "tool_hooks.pre"),
            Some("echo graph".to_string())
        );
    }

    #[test]
    fn resolve_hook_none_when_missing() {
        let node = Node::new("step");
        let graph = Graph::new("test");
        assert_eq!(resolve_hook(&node, &graph, "tool_hooks.pre"), None);
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
                _execution_env: &Arc<dyn ExecutionEnvironment>,
            ) -> Result<CodergenResult, ArcError> {
                *self.captured_thread_id.lock().unwrap() = Some(thread_id.map(String::from));
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                })
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let backend = ThreadCapturingBackend {
            captured_thread_id: captured.clone(),
        };
        let handler = CodergenHandler::new(Some(Box::new(backend)));

        let node = Node::new("work");
        let context = Context::new();
        // Simulate what the engine stores in internal.thread_id
        context.set("internal.thread_id", serde_json::json!("main"));
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
                _execution_env: &Arc<dyn ExecutionEnvironment>,
            ) -> Result<CodergenResult, ArcError> {
                *self.captured_thread_id.lock().unwrap() = Some(thread_id.map(String::from));
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                })
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let backend = ThreadCapturingBackend {
            captured_thread_id: captured.clone(),
        };
        let handler = CodergenHandler::new(Some(Box::new(backend)));

        let node = Node::new("work");
        let context = Context::new();
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
                _execution_env: &Arc<dyn ExecutionEnvironment>,
            ) -> Result<CodergenResult, ArcError> {
                Err(ArcError::Handler("Request timed out".to_string()))
            }
        }

        let handler = CodergenHandler::new(Some(Box::new(FailingBackend)));
        let node = Node::new("step");
        let context = Context::new();
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
    fn extract_status_fields_uses_last_match() {
        let text = r#"{"preferred_next_label": "first"}
Some text in between.
{"preferred_next_label": "second"}"#;
        let mut outcome = Outcome::success();
        extract_status_fields(text, &mut outcome);
        assert_eq!(outcome.preferred_label.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn codergen_handler_one_shot_dispatches_to_backend() {
        struct OneShotBackend;

        #[async_trait]
        impl CodergenBackend for OneShotBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &Path,
                _execution_env: &Arc<dyn ExecutionEnvironment>,
            ) -> Result<CodergenResult, ArcError> {
                panic!("run() should not be called in one_shot mode");
            }

            async fn one_shot(
                &self,
                _node: &Node,
                _prompt: &str,
                _stage_dir: &Path,
            ) -> Result<CodergenResult, ArcError> {
                Ok(CodergenResult::Text {
                    text: "one-shot response".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                })
            }
        }

        let handler = CodergenHandler::new(Some(Box::new(OneShotBackend)));
        let mut node = Node::new("classify");
        node.attrs.insert(
            "codergen_mode".to_string(),
            AttrValue::String("one_shot".to_string()),
        );
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);

        let prompt_content =
            std::fs::read_to_string(tmp.path().join("nodes").join("classify").join("prompt.md"))
                .unwrap();
        assert_eq!(prompt_content, "Classify this");

        let response_content = std::fs::read_to_string(
            tmp.path()
                .join("nodes")
                .join("classify")
                .join("response.md"),
        )
        .unwrap();
        assert_eq!(response_content, "one-shot response");
    }

    #[tokio::test]
    async fn codergen_handler_one_shot_simulation_mode() {
        let handler = CodergenHandler::new(None);
        let mut node = Node::new("classify");
        node.attrs.insert(
            "codergen_mode".to_string(),
            AttrValue::String("one_shot".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);

        let response_content = std::fs::read_to_string(
            tmp.path()
                .join("nodes")
                .join("classify")
                .join("response.md"),
        )
        .unwrap();
        assert!(response_content.contains("[Simulated]"));
    }

    #[tokio::test]
    async fn codergen_handler_invalid_mode_returns_error() {
        let handler = CodergenHandler::new(None);
        let mut node = Node::new("step");
        node.attrs.insert(
            "codergen_mode".to_string(),
            AttrValue::String("bogus".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let result = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("bogus"));
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
                _execution_env: &Arc<dyn ExecutionEnvironment>,
            ) -> Result<CodergenResult, ArcError> {
                Err(ArcError::Validation("bad config".to_string()))
            }
        }

        let handler = CodergenHandler::new(Some(Box::new(ValidationFailBackend)));
        let node = Node::new("step");
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
        assert!(outcome.failure_reason.unwrap().contains("bad config"));
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
                _execution_env: &Arc<dyn ExecutionEnvironment>,
            ) -> Result<CodergenResult, ArcError> {
                *self.captured_prompt.lock().unwrap() = Some(prompt.to_string());
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                })
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let backend = PromptCapturingBackend {
            captured_prompt: captured.clone(),
        };
        let handler = CodergenHandler::new(Some(Box::new(backend)));

        let mut node = Node::new("report");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Summarize the results".to_string()),
        );
        let context = Context::new();
        context.set(
            "current.preamble",
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
                _execution_env: &Arc<dyn ExecutionEnvironment>,
            ) -> Result<CodergenResult, ArcError> {
                *self.captured_prompt.lock().unwrap() = Some(prompt.to_string());
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                })
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let backend = PromptCapturingBackend {
            captured_prompt: captured.clone(),
        };
        let handler = CodergenHandler::new(Some(Box::new(backend)));

        let mut node = Node::new("report");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Summarize the results".to_string()),
        );
        let context = Context::new();
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
    async fn codergen_handler_one_shot_prepends_preamble() {
        use std::sync::{Arc, Mutex};

        struct OneShotCapturingBackend {
            captured_prompt: Arc<Mutex<Option<String>>>,
        }

        #[async_trait]
        impl CodergenBackend for OneShotCapturingBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _stage_dir: &std::path::Path,
                _execution_env: &Arc<dyn ExecutionEnvironment>,
            ) -> Result<CodergenResult, ArcError> {
                panic!("run() should not be called in one_shot mode");
            }

            async fn one_shot(
                &self,
                _node: &Node,
                prompt: &str,
                _stage_dir: &std::path::Path,
            ) -> Result<CodergenResult, ArcError> {
                *self.captured_prompt.lock().unwrap() = Some(prompt.to_string());
                Ok(CodergenResult::Text {
                    text: "classified".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                })
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let backend = OneShotCapturingBackend {
            captured_prompt: captured.clone(),
        };
        let handler = CodergenHandler::new(Some(Box::new(backend)));

        let mut node = Node::new("classify");
        node.attrs.insert(
            "codergen_mode".to_string(),
            AttrValue::String("one_shot".to_string()),
        );
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        let context = Context::new();
        context.set("current.preamble", serde_json::json!("Prior output here"));
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let prompt = captured.lock().unwrap().clone().unwrap();
        assert!(
            prompt.starts_with("Prior output here"),
            "one_shot prompt should start with preamble, got: {prompt}"
        );
        assert!(prompt.ends_with("Classify this"));
    }

    #[tokio::test]
    async fn codergen_handler_preamble_written_to_prompt_md() {
        let handler = CodergenHandler::new(None);
        let mut node = Node::new("report");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Summarize".to_string()),
        );
        let context = Context::new();
        context.set(
            "current.preamble",
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
