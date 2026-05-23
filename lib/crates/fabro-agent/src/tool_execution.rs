use std::sync::Arc;

use fabro_llm::types::{ToolCall, ToolResult};
use futures::future;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::config::{SessionOptions, ToolHookCallback, ToolHookDecision};
use crate::event::{Emitter, SessionBoundEmitter};
use crate::question_tools::{self, AgentToolRuntime, is_question_tool};
use crate::sandbox::Sandbox;
use crate::session::ToolEnvProvider;
use crate::tool_registry::{AgentEventEmitter, RegisteredTool, ToolContext, ToolRegistry};
use crate::truncation::truncate_tool_output;
use crate::types::AgentEvent;

/// Execute tool calls, choosing parallel or sequential based on `parallel`
/// flag.
#[allow(
    clippy::too_many_arguments,
    reason = "Tool dispatch needs the shared runtime handles and call list together."
)]
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
    root_session_id: &str,
    tool_env_provider: Option<&Arc<dyn ToolEnvProvider>>,
    agent_tool_runtime: &AgentToolRuntime,
) -> Vec<ToolResult> {
    if tool_calls.iter().any(|tc| is_question_tool(&tc.name)) {
        return execute_question_tool_round(
            tool_calls,
            registry,
            env,
            tool_hooks,
            cancel_token,
            config,
            emitter,
            session_id,
            root_session_id,
            tool_env_provider,
            agent_tool_runtime,
        )
        .await;
    }

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
            root_session_id,
            tool_env_provider,
            agent_tool_runtime,
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
            root_session_id,
            tool_env_provider,
            agent_tool_runtime,
        )
        .await
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Sequential execution threads the runtime handles through each tool call."
)]
async fn execute_tool_calls_sequential(
    tool_calls: &[ToolCall],
    registry: &ToolRegistry,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: &CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    root_session_id: &str,
    tool_env_provider: Option<&Arc<dyn ToolEnvProvider>>,
    agent_tool_runtime: &AgentToolRuntime,
) -> Vec<ToolResult> {
    let mut results = Vec::new();
    for tc in tool_calls {
        if cancel_token.is_cancelled() {
            results.push(ToolResult::error(tc.id.clone(), "Cancelled"));
            continue;
        }

        let result = execute_and_emit_one_tool_with_runtime(
            tc,
            registry,
            env.clone(),
            tool_hooks,
            cancel_token.child_token(),
            config,
            emitter,
            session_id,
            root_session_id,
            tool_env_provider,
            agent_tool_runtime,
        )
        .await;
        results.push(result);
    }
    results
}

#[allow(
    clippy::too_many_arguments,
    reason = "Parallel execution threads the runtime handles into each spawned tool task."
)]
async fn execute_tool_calls_parallel(
    tool_calls: &[ToolCall],
    registry: &ToolRegistry,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: &CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    root_session_id: &str,
    tool_env_provider: Option<&Arc<dyn ToolEnvProvider>>,
    agent_tool_runtime: &AgentToolRuntime,
) -> Vec<ToolResult> {
    let tool_env_provider = tool_env_provider.cloned();
    let agent_tool_runtime = agent_tool_runtime.clone();
    let futures: Vec<_> = tool_calls
        .iter()
        .map(|tc| {
            let emitter = emitter.clone();
            let env = env.clone();
            let config = config.clone();
            let cancel_token = cancel_token.clone();
            let tc = tc.clone();
            let session_id = session_id.to_owned();
            let root_session_id = root_session_id.to_owned();
            let tool_hooks = tool_hooks.cloned();
            let tool_env_provider = tool_env_provider.clone();
            let agent_tool_runtime = agent_tool_runtime.clone();
            let access_denial = config.tool_access_denial_reason(&tc.name);
            // Look up the tool before spawning since ToolRegistry is not Send.
            let registered_tool = if access_denial.is_none() {
                registry.get(&tc.name).cloned()
            } else {
                None
            };
            async move {
                execute_and_emit_one_tool_with_lookup(
                    &tc,
                    registered_tool.as_ref(),
                    access_denial,
                    env,
                    tool_hooks.as_ref(),
                    cancel_token.child_token(),
                    &config,
                    &emitter,
                    &session_id,
                    &root_session_id,
                    tool_env_provider.as_ref(),
                    &agent_tool_runtime,
                )
                .await
            }
        })
        .collect();

    future::join_all(futures).await
}

#[allow(
    clippy::too_many_arguments,
    reason = "Question-tool round handling needs the same execution context as normal tool dispatch."
)]
async fn execute_question_tool_round(
    tool_calls: &[ToolCall],
    registry: &ToolRegistry,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: &CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    root_session_id: &str,
    tool_env_provider: Option<&Arc<dyn ToolEnvProvider>>,
    agent_tool_runtime: &AgentToolRuntime,
) -> Vec<ToolResult> {
    let first_question_index = tool_calls
        .iter()
        .position(|tc| is_question_tool(&tc.name))
        .expect("question-tool round should contain a question tool");
    let mut results = Vec::with_capacity(tool_calls.len());

    for (index, tc) in tool_calls.iter().enumerate() {
        if cancel_token.is_cancelled() {
            results.push(ToolResult::error(tc.id.clone(), "Cancelled"));
            continue;
        }

        if index == first_question_index {
            results.push(
                execute_and_emit_one_tool_with_runtime(
                    tc,
                    registry,
                    env.clone(),
                    tool_hooks,
                    cancel_token.child_token(),
                    config,
                    emitter,
                    session_id,
                    root_session_id,
                    tool_env_provider,
                    agent_tool_runtime,
                )
                .await,
            );
        } else if is_question_tool(&tc.name) {
            results.push(error_tool_result_with_events(
                tc,
                emitter,
                session_id,
                config,
                "Only one human-question tool call may be used in a tool round. Combine all questions into a single questions[] batch and call the question tool once.",
            ));
        } else {
            results.push(error_tool_result_with_events(
                tc,
                emitter,
                session_id,
                config,
                "This tool call was not executed because human-question tools must run alone in a tool round. Retry non-question tools in a later round after the user answers.",
            ));
        }
    }

    results
}

fn error_tool_result_with_events(
    tc: &ToolCall,
    emitter: &Emitter,
    session_id: &str,
    config: &SessionOptions,
    message: &str,
) -> ToolResult {
    emit_tool_call_started(emitter, session_id, tc);
    let result = ToolResult::error(&tc.id, message);
    emit_tool_call_result(emitter, session_id, tc, &result);
    truncate_tool_result(&result, &tc.name, config)
}

fn emit_tool_call_started(emitter: &Emitter, session_id: &str, tc: &ToolCall) {
    emitter.emit(session_id.to_owned(), AgentEvent::ToolCallStarted {
        tool_name:    tc.name.clone(),
        tool_call_id: tc.id.clone(),
        arguments:    tc.arguments.clone(),
    });
}

fn emit_tool_call_result(emitter: &Emitter, session_id: &str, tc: &ToolCall, result: &ToolResult) {
    emitter.emit(session_id.to_owned(), AgentEvent::ToolCallOutputDelta {
        delta: result.content.to_string(),
    });
    emitter.emit(session_id.to_owned(), AgentEvent::ToolCallCompleted {
        tool_name:    tc.name.clone(),
        tool_call_id: tc.id.clone(),
        output:       result.content.clone(),
        is_error:     result.is_error,
    });
}

/// Execute a single tool call with event emission and output truncation.
#[allow(
    clippy::too_many_arguments,
    reason = "Single-tool execution needs the tool, runtime handles, and emission context."
)]
pub async fn execute_and_emit_one_tool(
    tc: &ToolCall,
    registry: &ToolRegistry,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    root_session_id: &str,
    tool_env_provider: Option<&Arc<dyn ToolEnvProvider>>,
) -> ToolResult {
    execute_and_emit_one_tool_with_runtime(
        tc,
        registry,
        env,
        tool_hooks,
        cancel_token,
        config,
        emitter,
        session_id,
        root_session_id,
        tool_env_provider,
        &AgentToolRuntime::default(),
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "Single-tool execution needs the tool, runtime handles, and emission context."
)]
async fn execute_and_emit_one_tool_with_runtime(
    tc: &ToolCall,
    registry: &ToolRegistry,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    root_session_id: &str,
    tool_env_provider: Option<&Arc<dyn ToolEnvProvider>>,
    agent_tool_runtime: &AgentToolRuntime,
) -> ToolResult {
    let access_denial = config.tool_access_denial_reason(&tc.name);
    let registered_tool = if access_denial.is_none() {
        registry.get(&tc.name)
    } else {
        None
    };
    execute_and_emit_one_tool_with_lookup(
        tc,
        registered_tool,
        access_denial,
        env,
        tool_hooks,
        cancel_token,
        config,
        emitter,
        session_id,
        root_session_id,
        tool_env_provider,
        agent_tool_runtime,
    )
    .await
}

/// Execute a single tool call with event emission, using a pre-looked-up tool
/// reference.
#[allow(
    clippy::too_many_arguments,
    reason = "The looked-up execution path still needs the tool, runtime handles, and emission context."
)]
async fn execute_and_emit_one_tool_with_lookup(
    tc: &ToolCall,
    registered_tool: Option<&RegisteredTool>,
    access_denial: Option<String>,
    env: Arc<dyn Sandbox>,
    tool_hooks: Option<&Arc<dyn ToolHookCallback>>,
    cancel_token: CancellationToken,
    config: &SessionOptions,
    emitter: &Emitter,
    session_id: &str,
    root_session_id: &str,
    tool_env_provider: Option<&Arc<dyn ToolEnvProvider>>,
    agent_tool_runtime: &AgentToolRuntime,
) -> ToolResult {
    emit_tool_call_started(emitter, session_id, tc);

    if let Some(reason) = access_denial {
        let result = ToolResult::error(&tc.id, &reason);
        emit_tool_call_result(emitter, session_id, tc, &result);
        return truncate_tool_result(&result, &tc.name, config);
    }

    // Pre-tool-use hook
    if let Some(hooks) = tool_hooks {
        debug!(tool = %tc.name, hook_event = "pre_tool_use", "Calling tool hook");
        let start = std::time::Instant::now();
        let decision = hooks.pre_tool_use(&tc.name, &tc.arguments).await;
        let elapsed = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        debug!(tool = %tc.name, hook_event = "pre_tool_use", ?decision, duration_ms = elapsed, "Tool hook complete");

        if let ToolHookDecision::Block { reason } = decision {
            let result = ToolResult::error(&tc.id, &reason);
            emit_tool_call_result(emitter, session_id, tc, &result);
            return truncate_tool_result(&result, &tc.name, config);
        }
    }

    let result = execute_one_tool(
        tc,
        registered_tool,
        env,
        cancel_token,
        emitter,
        session_id,
        root_session_id,
        tool_env_provider,
        agent_tool_runtime,
    )
    .await;

    emit_tool_call_result(emitter, session_id, tc, &result);

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
#[allow(
    clippy::too_many_arguments,
    reason = "Single-tool execution threads session identity plus runtime handles to populate ToolContext."
)]
async fn execute_one_tool(
    tc: &ToolCall,
    registered_tool: Option<&RegisteredTool>,
    env: Arc<dyn Sandbox>,
    cancel_token: CancellationToken,
    emitter: &Emitter,
    session_id: &str,
    root_session_id: &str,
    tool_env_provider: Option<&Arc<dyn ToolEnvProvider>>,
    agent_tool_runtime: &AgentToolRuntime,
) -> ToolResult {
    match registered_tool {
        Some(tool) => {
            if tc.tool_type != "custom" {
                if let Err(validation_error) =
                    validate_tool_args(&tool.definition.parameters, &tc.arguments)
                {
                    return ToolResult::error(&tc.id, validation_error);
                }
            }

            let agent_event_emitter: Option<Arc<dyn AgentEventEmitter>> =
                Some(Arc::new(SessionBoundEmitter {
                    emitter:      emitter.clone(),
                    session_id:   session_id.to_owned(),
                    tool_call_id: Some(tc.id.clone()),
                }));
            let ctx = ToolContext {
                env,
                cancel: cancel_token,
                tool_env_provider: tool_env_provider.cloned(),
                session_id: Some(session_id.to_owned()),
                root_session_id: Some(root_session_id.to_owned()),
                tool_call_id: Some(tc.id.clone()),
                agent_event_emitter,
            };
            let execution = (tool.executor)(tc.arguments.clone(), ctx);
            match question_tools::scope_agent_tool_runtime(agent_tool_runtime.clone(), execution)
                .await
            {
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
        tool_call_id:     result.tool_call_id.clone(),
        content:          truncated_content,
        is_error:         result.is_error,
        image_data:       result.image_data.clone(),
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
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use fabro_llm::types::{ToolCall, ToolDefinition};
    use fabro_model::AgentProfileKind;

    use super::*;
    use crate::config::{
        ToolAccess, ToolAccessPolicy, ToolExposureMode, ToolHookCallback, ToolHookDecision,
    };
    use crate::event::Emitter;
    use crate::local_sandbox::LocalSandbox;
    use crate::question_tools::{
        AgentQuestion, AgentQuestionAnswer, AgentQuestionAnswerStatus, AgentQuestionRuntime,
        AgentToolRuntime, register_question_tools,
    };
    use crate::read_before_write_sandbox::ReadBeforeWriteSandbox;
    use crate::test_support::MutableMockSandbox;
    use crate::tool_registry::{RegisteredTool, ToolContext, ToolRegistry, ToolSource};
    use crate::tools::{
        make_edit_file_tool, make_grep_tool, make_read_file_tool, make_write_file_tool,
    };

    struct NamedPolicy {
        decisions: HashMap<String, ToolAccess>,
    }

    impl NamedPolicy {
        fn new(decisions: impl IntoIterator<Item = (&'static str, ToolAccess)>) -> Self {
            Self {
                decisions: decisions
                    .into_iter()
                    .map(|(name, access)| (name.to_string(), access))
                    .collect(),
            }
        }
    }

    impl ToolAccessPolicy for NamedPolicy {
        fn access_for_tool(&self, tool_name: &str) -> ToolAccess {
            self.decisions
                .get(tool_name)
                .copied()
                .unwrap_or(ToolAccess::Denied)
        }
    }

    fn make_echo_tool() -> RegisteredTool {
        RegisteredTool {
            definition: ToolDefinition {
                name:        "echo".to_string(),
                description: "Echo input".to_string(),
                parameters:  serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"]
                }),
            },
            executor:   Arc::new(|args: serde_json::Value, _ctx: ToolContext| {
                Box::pin(async move {
                    let text = args["text"].as_str().unwrap_or("").to_string();
                    Ok(format!("echo: {text}"))
                })
            }),
            source:     ToolSource::Native,
        }
    }

    fn make_fail_tool() -> RegisteredTool {
        RegisteredTool {
            definition: ToolDefinition {
                name:        "fail_tool".to_string(),
                description: "Always fails".to_string(),
                parameters:  serde_json::json!({}),
            },
            executor:   Arc::new(|_args: serde_json::Value, _ctx: ToolContext| {
                Box::pin(async move { Err("tool failed".to_string()) })
            }),
            source:     ToolSource::Native,
        }
    }

    fn make_tool_call(name: &str, id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id:                id.to_string(),
            name:              name.to_string(),
            tool_type:         "function".to_string(),
            arguments:         args,
            raw_arguments:     None,
            provider_metadata: None,
        }
    }

    struct StubQuestionRuntime;

    #[async_trait]
    impl AgentQuestionRuntime for StubQuestionRuntime {
        async fn ask_questions(
            &self,
            _tool_call_id: &str,
            questions: Vec<AgentQuestion>,
            _cancel_token: CancellationToken,
        ) -> Result<Vec<AgentQuestionAnswer>, String> {
            Ok(questions
                .into_iter()
                .map(|question| AgentQuestionAnswer {
                    original_id:       question.original_id,
                    original_question: question.original_question,
                    answers:           vec!["Ship".to_string()],
                    status:            AgentQuestionAnswerStatus::Answered,
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn question_tool_round_rejects_non_question_peers_and_preserves_order() {
        let mut registry = ToolRegistry::new();
        register_question_tools(AgentProfileKind::OpenAi, &mut registry);
        registry.register(make_echo_tool());
        let tool_calls = vec![
            make_tool_call(
                "request_user_input",
                "call_question",
                serde_json::json!({
                    "questions": [{
                        "id": "q1",
                        "header": "Decision",
                        "question": "Ship it?",
                        "options": [{ "label": "Ship" }]
                    }]
                }),
            ),
            make_tool_call("echo", "call_echo", serde_json::json!({"text": "hello"})),
        ];
        let runtime = AgentToolRuntime::with_question_runtime(Arc::new(StubQuestionRuntime));

        let results = execute_tool_calls(
            &tool_calls,
            true,
            &registry,
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap())),
            None,
            &CancellationToken::new(),
            &SessionOptions::default(),
            &Emitter::new(),
            "root",
            "root",
            None,
            &runtime,
        )
        .await;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].tool_call_id, "call_question");
        assert!(!results[0].is_error);
        assert_eq!(results[1].tool_call_id, "call_echo");
        assert!(results[1].is_error);
        assert!(
            results[1]
                .content
                .as_str()
                .unwrap()
                .contains("human-question tools must run alone")
        );
    }

    #[tokio::test]
    async fn multiple_question_tool_calls_execute_only_first() {
        let mut registry = ToolRegistry::new();
        register_question_tools(AgentProfileKind::OpenAi, &mut registry);
        let question_args = serde_json::json!({
            "questions": [{
                "id": "q1",
                "header": "Decision",
                "question": "Ship it?",
                "options": [{ "label": "Ship" }]
            }]
        });
        let tool_calls = vec![
            make_tool_call("request_user_input", "call_first", question_args.clone()),
            make_tool_call("request_user_input", "call_second", question_args),
        ];
        let runtime = AgentToolRuntime::with_question_runtime(Arc::new(StubQuestionRuntime));

        let results = execute_tool_calls(
            &tool_calls,
            true,
            &registry,
            Arc::new(LocalSandbox::new(std::env::current_dir().unwrap())),
            None,
            &CancellationToken::new(),
            &SessionOptions::default(),
            &Emitter::new(),
            "root",
            "root",
            None,
            &runtime,
        )
        .await;

        assert!(!results[0].is_error);
        assert!(results[1].is_error);
        assert!(
            results[1]
                .content
                .as_str()
                .unwrap()
                .contains("Combine all questions into a single questions[] batch")
        );
    }

    struct MockHookCallback {
        pre_decision:       ToolHookDecision,
        post_calls:         Arc<Mutex<Vec<(String, String, String)>>>,
        post_failure_calls: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    impl MockHookCallback {
        fn new(decision: ToolHookDecision) -> Self {
            Self {
                pre_decision:       decision,
                post_calls:         Arc::new(Mutex::new(Vec::new())),
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
            "test-session",
            None,
        )
        .await;

        assert!(!result.is_error);
        let content = result.content.to_string();
        assert!(content.contains("echo: hello"));
    }

    #[tokio::test]
    async fn denied_policy_tool_is_blocked_before_executor_lookup() {
        let executions = Arc::new(Mutex::new(0usize));
        let mut registry = ToolRegistry::new();
        let executions_for_tool = Arc::clone(&executions);
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name:        "write_file".to_string(),
                description: "Writes a file".to_string(),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(move |_args: serde_json::Value, _ctx: ToolContext| {
                let executions = Arc::clone(&executions_for_tool);
                Box::pin(async move {
                    *executions.lock().unwrap() += 1;
                    Ok("wrote".to_string())
                })
            }),
            source:     ToolSource::Native,
        });
        let config = SessionOptions {
            tool_access_policy: Some(Arc::new(NamedPolicy::new([(
                "write_file",
                ToolAccess::Denied,
            )]))),
            tool_exposure_mode: ToolExposureMode::IncludeRequiresApproval,
            ..SessionOptions::default()
        };

        let tc = make_tool_call("write_file", "call_1", serde_json::json!({}));
        let result = execute_and_emit_one_tool(
            &tc,
            &registry,
            make_sandbox(),
            None,
            CancellationToken::new(),
            &config,
            &Emitter::new(),
            "test-session",
            "test-session",
            None,
        )
        .await;

        assert!(result.is_error);
        assert!(
            result
                .content
                .as_str()
                .unwrap_or_default()
                .contains("denied by tool access policy")
        );
        assert_eq!(*executions.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn approval_required_tool_hidden_by_exposure_mode_is_blocked() {
        let executions = Arc::new(Mutex::new(0usize));
        let mut registry = ToolRegistry::new();
        let executions_for_tool = Arc::clone(&executions);
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name:        "shell".to_string(),
                description: "Runs a command".to_string(),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(move |_args: serde_json::Value, _ctx: ToolContext| {
                let executions = Arc::clone(&executions_for_tool);
                Box::pin(async move {
                    *executions.lock().unwrap() += 1;
                    Ok("ran".to_string())
                })
            }),
            source:     ToolSource::Native,
        });
        let config = SessionOptions {
            tool_access_policy: Some(Arc::new(NamedPolicy::new([(
                "shell",
                ToolAccess::RequiresApproval,
            )]))),
            tool_exposure_mode: ToolExposureMode::AutoApprovedOnly,
            ..SessionOptions::default()
        };

        let tc = make_tool_call("shell", "call_1", serde_json::json!({}));
        let result = execute_and_emit_one_tool(
            &tc,
            &registry,
            make_sandbox(),
            None,
            CancellationToken::new(),
            &config,
            &Emitter::new(),
            "test-session",
            "test-session",
            None,
        )
        .await;

        assert!(result.is_error);
        assert!(
            result
                .content
                .as_str()
                .unwrap_or_default()
                .contains("requires approval")
        );
        assert_eq!(*executions.lock().unwrap(), 0);
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
            "test-session",
            None,
        )
        .await;

        assert!(!result.is_error);
    }
}
