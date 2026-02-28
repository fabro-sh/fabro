use crate::config::{SessionConfig, ToolApprovalFn};
use crate::event::EventEmitter;
use crate::execution_env::ExecutionEnvironment;
use crate::tool_registry::ToolRegistry;
use crate::truncation::truncate_tool_output;
use crate::types::AgentEvent;
use llm::types::ToolResult;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Execute tool calls, choosing parallel or sequential based on `parallel` flag.
#[allow(clippy::too_many_arguments)]
pub async fn execute_tool_calls(
    tool_calls: &[llm::types::ToolCall],
    parallel: bool,
    registry: &ToolRegistry,
    env: Arc<dyn ExecutionEnvironment>,
    tool_approval: Option<&ToolApprovalFn>,
    cancel_token: &CancellationToken,
    config: &SessionConfig,
    emitter: &EventEmitter,
    session_id: &str,
) -> Vec<ToolResult> {
    if parallel && tool_calls.len() > 1 {
        execute_tool_calls_parallel(
            tool_calls, registry, env, tool_approval, cancel_token, config, emitter, session_id,
        )
        .await
    } else {
        execute_tool_calls_sequential(
            tool_calls, registry, env, tool_approval, cancel_token, config, emitter, session_id,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_calls_sequential(
    tool_calls: &[llm::types::ToolCall],
    registry: &ToolRegistry,
    env: Arc<dyn ExecutionEnvironment>,
    tool_approval: Option<&ToolApprovalFn>,
    cancel_token: &CancellationToken,
    config: &SessionConfig,
    emitter: &EventEmitter,
    session_id: &str,
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
            tool_approval,
            cancel_token.child_token(),
            config,
            emitter,
            session_id,
        )
        .await;
        results.push(result);
    }
    results
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_calls_parallel(
    tool_calls: &[llm::types::ToolCall],
    registry: &ToolRegistry,
    env: Arc<dyn ExecutionEnvironment>,
    tool_approval: Option<&ToolApprovalFn>,
    cancel_token: &CancellationToken,
    config: &SessionConfig,
    emitter: &EventEmitter,
    session_id: &str,
) -> Vec<ToolResult> {
    let futures: Vec<_> = tool_calls
        .iter()
        .map(|tc| {
            let emitter = emitter.clone();
            let env = env.clone();
            let config = config.clone();
            let cancel_token = cancel_token.clone();
            let tc = tc.clone();
            let session_id = session_id.to_owned();
            let tool_approval = tool_approval.cloned();
            // Look up the tool before spawning since ToolRegistry is not Send.
            let registered_tool = registry.get(&tc.name).cloned();
            async move {
                execute_and_emit_one_tool_with_lookup(
                    &tc,
                    registered_tool.as_ref(),
                    env,
                    tool_approval.as_ref(),
                    cancel_token.child_token(),
                    &config,
                    &emitter,
                    &session_id,
                )
                .await
            }
        })
        .collect();

    futures::future::join_all(futures).await
}

/// Execute a single tool call with event emission and output truncation.
#[allow(clippy::too_many_arguments)]
pub async fn execute_and_emit_one_tool(
    tc: &llm::types::ToolCall,
    registry: &ToolRegistry,
    env: Arc<dyn ExecutionEnvironment>,
    tool_approval: Option<&ToolApprovalFn>,
    cancel_token: CancellationToken,
    config: &SessionConfig,
    emitter: &EventEmitter,
    session_id: &str,
) -> ToolResult {
    execute_and_emit_one_tool_with_lookup(
        tc,
        registry.get(&tc.name),
        env,
        tool_approval,
        cancel_token,
        config,
        emitter,
        session_id,
    )
    .await
}

/// Execute a single tool call with event emission, using a pre-looked-up tool reference.
#[allow(clippy::too_many_arguments)]
async fn execute_and_emit_one_tool_with_lookup(
    tc: &llm::types::ToolCall,
    registered_tool: Option<&crate::tool_registry::RegisteredTool>,
    env: Arc<dyn ExecutionEnvironment>,
    tool_approval: Option<&ToolApprovalFn>,
    cancel_token: CancellationToken,
    config: &SessionConfig,
    emitter: &EventEmitter,
    session_id: &str,
) -> ToolResult {
    emitter.emit(
        session_id.to_owned(),
        AgentEvent::ToolCallStarted {
            tool_name: tc.name.clone(),
            tool_call_id: tc.id.clone(),
            arguments: tc.arguments.clone(),
        },
    );

    let result = execute_one_tool(
        &tc.id,
        &tc.name,
        &tc.arguments,
        registered_tool,
        env,
        tool_approval,
        cancel_token,
    )
    .await;

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

    truncate_tool_result(&result, &tc.name, config)
}

/// Execute a single tool call: argument validation and execution.
async fn execute_one_tool(
    tool_call_id: &str,
    tool_name: &str,
    arguments: &serde_json::Value,
    registered_tool: Option<&crate::tool_registry::RegisteredTool>,
    env: Arc<dyn ExecutionEnvironment>,
    tool_approval: Option<&ToolApprovalFn>,
    cancel_token: CancellationToken,
) -> ToolResult {
    if let Some(approval_fn) = tool_approval {
        if let Err(denial_message) = approval_fn(tool_name, arguments) {
            return ToolResult::error(tool_call_id, denial_message);
        }
    }

    match registered_tool {
        Some(tool) => {
            if let Err(validation_error) =
                validate_tool_args(&tool.definition.parameters, arguments)
            {
                return ToolResult::error(tool_call_id, validation_error);
            }

            let ctx = crate::tool_registry::ToolContext {
                env,
                cancel: cancel_token,
            };
            match (tool.executor)(arguments.clone(), ctx).await {
                Ok(output) => ToolResult::success(tool_call_id, serde_json::json!(output)),
                Err(err) => ToolResult::error(tool_call_id, err),
            }
        }
        None => ToolResult::error(tool_call_id, format!("Unknown tool: {tool_name}")),
    }
}

/// Truncate tool output for history storage while preserving identity fields.
fn truncate_tool_result(
    result: &ToolResult,
    tool_name: &str,
    config: &SessionConfig,
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
