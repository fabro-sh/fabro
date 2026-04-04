use crate::config::{SessionOptions, ToolHookCallback, ToolHookDecision};
use crate::event::Emitter;
use crate::sandbox::Sandbox;
use crate::tool_registry::{RegisteredTool, ToolContext, ToolRegistry};
use crate::truncation::truncate_tool_output;
use crate::types::AgentEvent;
use fabro_llm::types::{ToolCall, ToolResult};
use futures::future;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Execute tool calls, choosing parallel or sequential based on `parallel` flag.
#[allow(clippy::too_many_arguments)]
pub async fn execute_tool_calls(
    tool_calls: &[ToolCall],
    parallel: bool,
    registry: &ToolRegistry,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: &CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    tool_env: Option<&HashMap<String, String>>,
) -> Vec<ToolResult> {
    if parallel && tool_calls.len() > 1 {
        execute_tool_calls_parallel(
            tool_calls,
            registry,
            env,
            tool_hooks,
            cancel_token,
            config,
            emitter,
            session_id,
            tool_env,
        )
        .await
    } else {
        execute_tool_calls_sequential(
            tool_calls,
            registry,
            env,
            tool_hooks,
            cancel_token,
            config,
            emitter,
            session_id,
            tool_env,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_calls_sequential(
    tool_calls: &[ToolCall],
    registry: &ToolRegistry,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: &CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    tool_env: Option<&HashMap<String, String>>,
) -> Vec<ToolResult> {
    let mut results = Vec::new();
    for tc in tool_calls {
        if cancel_token.is_cancelled() {
            results.push(ToolResult::error(tc.id.clone(), "Cancelled"));
            continue;
        }

        let result = execute_and_emit_one_tool(
            tc,
            registry,
            env.clone(),
            tool_hooks,
            cancel_token.child_token(),
            config,
            emitter,
            session_id,
            tool_env,
        )
        .await;
        results.push(result);
    }
    results
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_calls_parallel(
    tool_calls: &[ToolCall],
    registry: &ToolRegistry,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: &CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    tool_env: Option<&HashMap<String, String>>,
) -> Vec<ToolResult> {
    let tool_env = tool_env.cloned();
    let futures: Vec<_> = tool_calls
        .iter()
        .map(|tc| {
            let emitter = emitter.clone();
            let env = env.clone();
            let config = config.clone();
            let cancel_token = cancel_token.clone();
            let tc = tc.clone();
            let session_id = session_id.to_owned();
            let tool_hooks = tool_hooks.cloned();
            let tool_env = tool_env.clone();
            // Look up the tool before spawning since ToolRegistry is not Send.
            let registered_tool = registry.get(&tc.name).cloned();
            async move {
                execute_and_emit_one_tool_with_lookup(
                    &tc,
                    registered_tool.as_ref(),
                    env,
                    tool_hooks.as_ref(),
                    cancel_token.child_token(),
                    &config,
                    &emitter,
                    &session_id,
                    tool_env.as_ref(),
                )
                .await
            }
        })
        .collect();

    future::join_all(futures).await
}

/// Execute a single tool call with event emission and output truncation.
#[allow(clippy::too_many_arguments)]
pub async fn execute_and_emit_one_tool(
    tc: &ToolCall,
    registry: &ToolRegistry,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    tool_env: Option<&HashMap<String, String>>,
) -> ToolResult {
    execute_and_emit_one_tool_with_lookup(
        tc,
        registry.get(&tc.name),
        env,
        tool_hooks,
        cancel_token,
        config,
        emitter,
        session_id,
        tool_env,
    )
    .await
}

/// Execute a single tool call with event emission, using a pre-looked-up tool reference.
#[allow(clippy::too_many_arguments)]
async fn execute_and_emit_one_tool_with_lookup(
    tc: &ToolCall,
    registered_tool: Option<&RegisteredTool>,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    tool_env: Option<&HashMap<String, String>>,
) -> ToolResult {
    emitter.emit(
        session_id.to_owned(),
        AgentEvent::ToolCallStarted {
            tool_name: tc.name.clone(),
            tool_call_id: tc.id.clone(),
            arguments: tc.arguments.clone(),
        },
    );

    // Pre-tool-use hook
    if let Some(hooks) = tool_hooks {
        debug!(tool = %tc.name, hook_event = "pre_tool_use", "Calling tool hook");
        let start = std::time::Instant::now();
        let decision = hooks.pre_tool_use(&tc.name, &tc.arguments).await;
        let elapsed = u64::try_from(start.elapsed().as_millis()).unwrap();
        debug!(tool = %tc.name, hook_event = "pre_tool_use", ?decision, duration_ms = elapsed, "Tool hook complete");

        if let ToolHookDecision::Block { reason } = decision {
            let result = ToolResult::error(&tc.id, &reason);

            emitter.emit(
                session_id.to_owned(),
                AgentEvent::ToolCallOutputDelta {
                    delta: result.content.to_string(),
                },
            );
            emitter.emit(
                session_id.to_owned(),
                AgentEvent::ToolCallCompleted {
                    tool_name: tc.name.clone(),
                    tool_call_id: tc.id.clone(),
                    output: result.content.clone(),
                    is_error: true,
                },
            );

            return truncate_tool_result(&result, &tc.name, config);
        }
    }

    let result = execute_one_tool(tc, registered_tool, env, cancel_token, tool_env).await;

    emitter.emit(
        session_id.to_owned(),
        AgentEvent::ToolCallOutputDelta {
            delta: result.content.to_string(),
        },
    );

    emitter.emit(
        session_id.to_owned(),
        AgentEvent::ToolCallCompleted {
            tool_name: tc.name.clone(),
            tool_call_id: tc.id.clone(),
            output: result.content.clone(),
            is_error: result.is_error,
        },
    );

    // Post-tool-use hooks
    if let Some(hooks) = tool_hooks {
        let fallback;
        let content_str = if let Some(s) = result.content.as_str() {
            s
        } else {
            fallback = result.content.to_string();
            &fallback
        };
        if result.is_error {
            debug!(tool = %tc.name, hook_event = "post_tool_use_failure", "Calling tool hook");
            hooks
                .post_tool_use_failure(&tc.name, &tc.id, content_str)
                .await;
            debug!(tool = %tc.name, hook_event = "post_tool_use_failure", "Tool hook complete");
        } else {
            debug!(tool = %tc.name, hook_event = "post_tool_use", "Calling tool hook");
            hooks.post_tool_use(&tc.name, &tc.id, content_str).await;
            debug!(tool = %tc.name, hook_event = "post_tool_use", "Tool hook complete");
        }
    }

    truncate_tool_result(&result, &tc.name, config)
}

/// Execute a single tool call: argument validation and execution.
async fn execute_one_tool(
    tc: &ToolCall,
    registered_tool: Option<&RegisteredTool>,
    env: Arc<dyn Sandbox>,
    cancel_token: CancellationToken,
    tool_env: Option<&HashMap<String, String>>,
) -> ToolResult {
    match registered_tool {
        Some(tool) => {
            if let Err(validation_error) =
                validate_tool_args(&tool.definition.parameters, &tc.arguments)
            {
                return ToolResult::error(&tc.id, validation_error);
            }

            let ctx = ToolContext {
                env,
                cancel: cancel_token,
                tool_env: tool_env.cloned(),
            };
            match (tool.executor)(tc.arguments.clone(), ctx).await {
                Ok(output) => ToolResult::success(&tc.id, serde_json::json!(output)),
                Err(err) => ToolResult::error(&tc.id, err),
            }
        }
        None => ToolResult::error(&tc.id, format!("Unknown tool: {}", tc.name)),
    }
}

/// Truncate tool output for history storage while preserving identity fields.
fn truncate_tool_result(
    result: &ToolResult,
    tool_name: &str,
    config: &SessionOptions,
) -> ToolResult {
    let truncated_content = match &result.content {
        serde_json::Value::String(s) => {
            serde_json::json!(truncate_tool_output(s, tool_name, config))
        }
        other => other.clone(),
    };

    ToolResult {
        tool_call_id: result.tool_call_id.clone(),
        content: truncated_content,
        is_error: result.is_error,
        image_data: result.image_data.clone(),
        image_media_type: result.image_media_type.clone(),
    }
}

pub fn validate_tool_args(
    schema: &serde_json::Value,
    args: &serde_json::Value,
) -> Result<(), String> {
    // Skip validation for empty/trivial schemas
    if schema.is_null() {
        return Ok(());
    }
    if let Some(obj) = schema.as_object() {
        if obj.is_empty() {
            return Ok(());
        }
    }

    let validator =
        jsonschema::validator_for(schema).map_err(|e| format!("Invalid tool schema: {e}"))?;

    let errors: Vec<String> = validator.iter_errors(args).map(|e| e.to_string()).collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Tool argument validation failed: {}",
            errors.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToolHookCallback, ToolHookDecision};
    use crate::event::Emitter;
    use crate::local_sandbox::LocalSandbox;
    use crate::read_before_write_sandbox::ReadBeforeWriteSandbox;
    use crate::test_support::MutableMockSandbox;
    use crate::tool_registry::{RegisteredTool, ToolContext, ToolRegistry};
    use crate::tools::{
        make_edit_file_tool, make_grep_tool, make_read_file_tool, make_write_file_tool,
    };
    use fabro_llm::types::{ToolCall, ToolDefinition};
    use std::sync::Mutex;

    fn make_echo_tool() -> RegisteredTool {
        RegisteredTool {
            definition: ToolDefinition {
                name: "echo".to_string(),
                description: "Echo input".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"]
                }),
            },
            executor: Arc::new(|args: serde_json::Value, _ctx: ToolContext| {
                Box::pin(async move {
                    let text = args["text"].as_str().unwrap_or("").to_string();
                    Ok(format!("echo: {text}"))
                })
            }),
        }
    }

    fn make_fail_tool() -> RegisteredTool {
        RegisteredTool {
            definition: ToolDefinition {
                name: "fail_tool".to_string(),
                description: "Always fails".to_string(),
                parameters: serde_json::json!({}),
            },
            executor: Arc::new(|_args: serde_json::Value, _ctx: ToolContext| {
                Box::pin(async move { Err("tool failed".to_string()) })
            }),
        }
    }

    fn make_tool_call(name: &str, id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            tool_type: "function".to_string(),
            arguments: args,
            raw_arguments: None,
            provider_metadata: None,
        }
    }

    struct MockHookCallback {
        pre_decision: ToolHookDecision,
        post_calls: Arc<Mutex<Vec<(String, String, String)>>>,
        post_failure_calls: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    impl MockHookCallback {
        fn new(decision: ToolHookDecision) -> Self {
            Self {
                pre_decision: decision,
                post_calls: Arc::new(Mutex::new(Vec::new())),
                post_failure_calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolHookCallback for MockHookCallback {
        async fn pre_tool_use(
            &self,
            _tool_name: &str,
            _tool_input: &serde_json::Value,
        ) -> ToolHookDecision {
            self.pre_decision.clone()
        }

        async fn post_tool_use(&self, tool_name: &str, tool_call_id: &str, tool_output: &str) {
            self.post_calls.lock().unwrap().push((
                tool_name.to_string(),
                tool_call_id.to_string(),
                tool_output.to_string(),
            ));
        }

        async fn post_tool_use_failure(&self, tool_name: &str, tool_call_id: &str, error: &str) {
            self.post_failure_calls.lock().unwrap().push((
                tool_name.to_string(),
                tool_call_id.to_string(),
                error.to_string(),
            ));
        }
    }

    fn make_sandbox() -> Arc<dyn Sandbox> {
        Arc::new(LocalSandbox::new(std::env::current_dir().unwrap()))
    }

    #[tokio::test]
    async fn pre_tool_use_hook_blocks_execution() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let hooks: Arc<dyn ToolHookCallback> =
            Arc::new(MockHookCallback::new(ToolHookDecision::Block {
                reason: "blocked by hook".to_string(),
            }));

        let tc = make_tool_call("echo", "call_1", serde_json::json!({"text": "hello"}));
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        let result = execute_and_emit_one_tool(
            &tc,
            &registry,
            make_sandbox(),
            Some(&hooks),
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        assert!(result.is_error);
        let content = result.content.as_str().unwrap();
        assert!(content.contains("blocked by hook"));
    }

    #[tokio::test]
    async fn pre_tool_use_hook_proceeds() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let hooks: Arc<dyn ToolHookCallback> =
            Arc::new(MockHookCallback::new(ToolHookDecision::Proceed));

        let tc = make_tool_call("echo", "call_1", serde_json::json!({"text": "hello"}));
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        let result = execute_and_emit_one_tool(
            &tc,
            &registry,
            make_sandbox(),
            Some(&hooks),
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        assert!(!result.is_error);
        let content = result.content.to_string();
        assert!(content.contains("echo: hello"));
    }

    #[tokio::test]
    async fn post_tool_use_hook_fires_on_success() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let mock = Arc::new(MockHookCallback::new(ToolHookDecision::Proceed));
        let hooks: Arc<dyn ToolHookCallback> = mock.clone();

        let tc = make_tool_call("echo", "call_1", serde_json::json!({"text": "hello"}));
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        execute_and_emit_one_tool(
            &tc,
            &registry,
            make_sandbox(),
            Some(&hooks),
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        let calls = mock.post_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "echo");
        assert_eq!(calls[0].1, "call_1");
        assert!(calls[0].2.contains("echo: hello"));

        let failure_calls = mock.post_failure_calls.lock().unwrap();
        assert!(failure_calls.is_empty());
    }

    #[tokio::test]
    async fn post_tool_use_failure_hook_fires_on_error() {
        let mut registry = ToolRegistry::new();
        registry.register(make_fail_tool());

        let mock = Arc::new(MockHookCallback::new(ToolHookDecision::Proceed));
        let hooks: Arc<dyn ToolHookCallback> = mock.clone();

        let tc = make_tool_call("fail_tool", "call_1", serde_json::json!({}));
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        execute_and_emit_one_tool(
            &tc,
            &registry,
            make_sandbox(),
            Some(&hooks),
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        let failure_calls = mock.post_failure_calls.lock().unwrap();
        assert_eq!(failure_calls.len(), 1);
        assert_eq!(failure_calls[0].0, "fail_tool");
        assert_eq!(failure_calls[0].1, "call_1");
        assert!(failure_calls[0].2.contains("tool failed"));

        let calls = mock.post_calls.lock().unwrap();
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn no_hooks_skips_all_callbacks() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let tc = make_tool_call("echo", "call_1", serde_json::json!({"text": "hello"}));
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        let result = execute_and_emit_one_tool(
            &tc,
            &registry,
            make_sandbox(),
            None,
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        assert!(!result.is_error);
        let content = result.content.to_string();
        assert!(content.contains("echo: hello"));
    }

    // --- ReadBeforeWriteSandbox e2e tests ---

    fn make_guarded_sandbox(files: HashMap<String, String>) -> Arc<dyn Sandbox> {
        Arc::new(ReadBeforeWriteSandbox::new(Arc::new(
            MutableMockSandbox::new(files),
        )))
    }

    #[tokio::test]
    async fn write_to_unread_file_blocked() {
        let mut registry = ToolRegistry::new();
        registry.register(make_write_file_tool());

        let sandbox = make_guarded_sandbox(HashMap::from([("a.ts".into(), "content".into())]));
        let tc = make_tool_call(
            "write_file",
            "call_1",
            serde_json::json!({"file_path": "a.ts", "content": "new"}),
        );
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        let result = execute_and_emit_one_tool(
            &tc,
            &registry,
            sandbox,
            None,
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        assert!(result.is_error);
        assert!(result.content.to_string().contains("has not been read"));
    }

    #[tokio::test]
    async fn read_then_write_succeeds() {
        let mut registry = ToolRegistry::new();
        registry.register(make_read_file_tool());
        registry.register(make_write_file_tool());

        let sandbox = make_guarded_sandbox(HashMap::from([("a.ts".into(), "content".into())]));
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        // First read the file
        let read_tc = make_tool_call(
            "read_file",
            "call_1",
            serde_json::json!({"file_path": "a.ts"}),
        );
        let read_result = execute_and_emit_one_tool(
            &read_tc,
            &registry,
            sandbox.clone(),
            None,
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;
        assert!(!read_result.is_error);

        // Then write should succeed
        let write_tc = make_tool_call(
            "write_file",
            "call_2",
            serde_json::json!({"file_path": "a.ts", "content": "new"}),
        );
        let write_result = execute_and_emit_one_tool(
            &write_tc,
            &registry,
            sandbox,
            None,
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        assert!(!write_result.is_error);
    }

    #[tokio::test]
    async fn grep_then_write_succeeds() {
        let mut registry = ToolRegistry::new();
        registry.register(make_grep_tool());
        registry.register(make_write_file_tool());

        let sandbox = make_guarded_sandbox(HashMap::from([("a.ts".into(), "content".into())]));
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        // Grep matching a.ts
        let grep_tc = make_tool_call("grep", "call_1", serde_json::json!({"pattern": "content"}));
        let grep_result = execute_and_emit_one_tool(
            &grep_tc,
            &registry,
            sandbox.clone(),
            None,
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;
        assert!(!grep_result.is_error);

        // Then write should succeed
        let write_tc = make_tool_call(
            "write_file",
            "call_2",
            serde_json::json!({"file_path": "a.ts", "content": "new"}),
        );
        let write_result = execute_and_emit_one_tool(
            &write_tc,
            &registry,
            sandbox,
            None,
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        assert!(!write_result.is_error);
    }

    #[tokio::test]
    async fn edit_unread_file_blocked() {
        let mut registry = ToolRegistry::new();
        registry.register(make_edit_file_tool());

        let sandbox = make_guarded_sandbox(HashMap::from([("a.ts".into(), "content".into())]));
        let tc = make_tool_call(
            "edit_file",
            "call_1",
            serde_json::json!({"file_path": "a.ts", "old_string": "content", "new_string": "updated"}),
        );
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        let result = execute_and_emit_one_tool(
            &tc,
            &registry,
            sandbox,
            None,
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        assert!(result.is_error);
        assert!(result.content.to_string().contains("has not been read"));
    }

    #[tokio::test]
    async fn write_new_file_succeeds() {
        let mut registry = ToolRegistry::new();
        registry.register(make_write_file_tool());

        let sandbox = make_guarded_sandbox(HashMap::new());
        let tc = make_tool_call(
            "write_file",
            "call_1",
            serde_json::json!({"file_path": "new.ts", "content": "hello"}),
        );
        let emitter = Emitter::new();
        let config = SessionOptions::default();

        let result = execute_and_emit_one_tool(
            &tc,
            &registry,
            sandbox,
            None,
            CancellationToken::new(),
            &config,
            &emitter,
            "test-session",
            None,
        )
        .await;

        assert!(!result.is_error);
    }
}
