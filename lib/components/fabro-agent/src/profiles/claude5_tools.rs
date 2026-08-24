//! Claude 5 harness adapters.
//!
//! Execution stays shared with Fabro wherever the behavior agrees. This module
//! narrows the model-facing schemas and supplies the few lifecycle semantics
//! that differ from Fabro's native tools.

use std::sync::Arc;
use std::time::Duration;

use fabro_llm::types::ToolDefinition;
use fabro_util::error as util_error;
use serde_json::Value;
use tokio::time;

use crate::config::NativeToolOptions;
use crate::error::{Error, InterruptReason};
use crate::native_tool::NativeTool;
use crate::session::Session;
use crate::subagent::{SessionFactory, SubAgentResult, SubAgentStatus, SubAgentSupervisor};
use crate::tool_registry::{RegisteredTool, ToolContext, ToolSource};
use crate::tools::{self, WebFetchSummarizer};
use crate::web_search::{self, SearchBackend};

fn definition(
    tool: NativeTool,
    description: impl Into<String>,
    parameters: Value,
) -> ToolDefinition {
    ToolDefinition {
        name: tool.canonical_name().to_string(),
        description: description.into(),
        parameters,
    }
}

/// Reject unknown top-level fields while retaining a shared executor.
#[must_use]
pub(crate) fn strict_object_tool(mut tool: RegisteredTool) -> RegisteredTool {
    let object = tool
        .definition
        .parameters
        .as_object_mut()
        .expect("native JSON-schema tools should use an object schema");
    object.insert("additionalProperties".to_string(), Value::Bool(false));
    tool
}

#[must_use]
pub(crate) fn make_read_tool() -> RegisteredTool {
    strict_object_tool(tools::make_read_file_tool())
}

#[must_use]
pub(crate) fn make_write_tool() -> RegisteredTool {
    strict_object_tool(tools::make_write_file_tool())
}

#[must_use]
pub(crate) fn make_edit_tool() -> RegisteredTool {
    strict_object_tool(tools::make_edit_file_tool())
}

#[must_use]
pub(crate) fn make_bash_tool(options: &NativeToolOptions) -> RegisteredTool {
    let default_timeout_ms = options.default_command_timeout_ms;
    let max_timeout_ms = options.max_command_timeout_ms;
    RegisteredTool {
        definition: definition(
            NativeTool::Shell,
            format!(
                "Execute a Bash command in a fresh foreground non-login shell. Use this for \
                 searches, git inspection, builds, tests, package managers, and terminal \
                 operations. Prefer `rg` for content search and `rg --files` for file discovery. \
                 Working-directory and environment changes do not persist between calls. \
                 `timeout` is in milliseconds, defaults to {default_timeout_ms}, and is capped at \
                 {max_timeout_ms}."
            ),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Bash source to evaluate."
                    },
                    "timeout": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": max_timeout_ms,
                        "description": format!(
                            "Maximum runtime in milliseconds (default {default_timeout_ms})."
                        )
                    },
                    "description": {
                        "type": "string",
                        "description": "Short description of what the command does."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        ),
        executor:   Arc::new(move |args, ctx| {
            Box::pin(async move {
                let command = tools::required_str(&args, "command")?;
                let timeout_ms = args
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .unwrap_or(default_timeout_ms)
                    .min(max_timeout_ms);
                tools::run_shell_command(&ctx, command, timeout_ms, None).await
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub(crate) fn make_web_search_tool(backend: SearchBackend) -> RegisteredTool {
    let mut tool = web_search::make_web_search_tool(backend);
    tool.definition = definition(
        NativeTool::WebSearch,
        "Search the web when current external information is needed. Returns result titles, URLs, \
         and descriptions; use WebFetch to inspect a specific URL.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The web search query."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    );
    tool
}

#[must_use]
pub(crate) fn make_web_fetch_tool(summarizer: Option<WebFetchSummarizer>) -> RegisteredTool {
    let mut tool = tools::make_web_fetch_tool(summarizer);
    tool.definition = definition(
        NativeTool::WebFetch,
        "Fetch an HTTP or HTTPS URL and answer the supplied prompt from its contents.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP or HTTPS URL to fetch."
                },
                "prompt": {
                    "type": "string",
                    "description": "The question or extraction instruction to apply to the page."
                }
            },
            "required": ["url", "prompt"],
            "additionalProperties": false
        }),
    );
    tool
}

fn child_session(session_factory: &SessionFactory, ctx: &ToolContext) -> Session {
    let mut session = session_factory();
    if let Some(root) = ctx.root_session_id.as_ref().or(ctx.session_id.as_ref()) {
        session.set_root_session_id(root.clone());
    }
    session
}

fn format_agent_result(result: &SubAgentResult) -> String {
    format!(
        "Agent completed (success: {}, turns: {})\n\n{}",
        result.success, result.turns_used, result.output
    )
}

fn format_error(error: &Error) -> String {
    util_error::collect_chain(error).join(": ")
}

#[must_use]
pub(crate) fn make_agent_tool(
    supervisor: SubAgentSupervisor,
    session_factory: SessionFactory,
    current_depth: usize,
) -> RegisteredTool {
    RegisteredTool {
        definition: definition(
            NativeTool::BackgroundAgent,
            "Launch a child agent for an independent task. Agents run in the background by \
             default and notify the parent when they finish. Set run_in_background to false to \
             wait for the result synchronously.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "A short 3-5 word description of the task."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task for the agent to perform."
                    },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "Whether to return immediately (default true)."
                    }
                },
                "required": ["description", "prompt"],
                "additionalProperties": false
            }),
        ),
        executor:   Arc::new(move |args, ctx| {
            let supervisor = supervisor.clone();
            let session_factory = session_factory.clone();
            Box::pin(async move {
                let description = tools::required_str(&args, "description")?;
                let prompt = tools::required_str(&args, "prompt")?;
                let run_in_background = args
                    .get("run_in_background")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let session = child_session(&session_factory, &ctx);

                if run_in_background {
                    let task_id = supervisor
                        .spawn_with_parent_notification(
                            session,
                            prompt.to_string(),
                            description.to_string(),
                            current_depth,
                        )
                        .map_err(|error| format_error(&error))?;
                    Ok(format!(
                        "Agent started in the background.\n\nTask ID: {task_id}"
                    ))
                } else {
                    let task_id = supervisor
                        .spawn(session, prompt.to_string(), current_depth)
                        .map_err(|error| format_error(&error))?;
                    match supervisor.wait_with_cancel(&task_id, &ctx.cancel).await {
                        Ok(result) => Ok(format_agent_result(&result)),
                        Err(Error::Interrupted(InterruptReason::Cancelled)) => {
                            Err("Cancelled".to_string())
                        }
                        Err(error) => Err(format_error(&error)),
                    }
                }
            })
        }),
        source:     ToolSource::Native,
    }
}

/// The schema keeps `block` and `timeout` required to match the Claude 5
/// contract, so these defaults only cover a model that omits them anyway.
const TASK_OUTPUT_DEFAULT_BLOCK: bool = true;
const TASK_OUTPUT_DEFAULT_TIMEOUT_MS: u64 = 30_000;
const TASK_OUTPUT_MAX_TIMEOUT_MS: u64 = 600_000;

fn optional_bool(args: &Value, key: &str, default: bool) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{key} must be a boolean")),
    }
}

fn optional_u64(args: &Value, key: &str, default: u64) -> Result<u64, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("{key} must be a non-negative integer")),
    }
}

fn finished_output(
    supervisor: &SubAgentSupervisor,
    task_id: &str,
    result: Result<SubAgentResult, Error>,
) -> Result<String, String> {
    supervisor.suppress_parent_notification(task_id);
    match result {
        Ok(result) => Ok(format_agent_result(&result)),
        Err(error) => Err(format_error(&error)),
    }
}

#[must_use]
pub(crate) fn make_task_output_tool(supervisor: SubAgentSupervisor) -> RegisteredTool {
    RegisteredTool {
        definition: definition(
            NativeTool::AgentOutput,
            "Get a background agent's current status or wait for its final output. Automatic \
             completion notifications make ordinary polling unnecessary.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The background agent task ID."
                    },
                    "block": {
                        "type": "boolean",
                        "default": TASK_OUTPUT_DEFAULT_BLOCK,
                        "description": "Whether to wait for completion."
                    },
                    "timeout": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": TASK_OUTPUT_MAX_TIMEOUT_MS,
                        "default": TASK_OUTPUT_DEFAULT_TIMEOUT_MS,
                        "description": "Maximum wait time in milliseconds."
                    }
                },
                "required": ["task_id", "block", "timeout"],
                "additionalProperties": false
            }),
        ),
        executor:   Arc::new(move |args, ctx| {
            let supervisor = supervisor.clone();
            Box::pin(async move {
                let task_id = tools::required_str(&args, "task_id")?;
                let block = optional_bool(&args, "block", TASK_OUTPUT_DEFAULT_BLOCK)?;
                let timeout_ms = optional_u64(&args, "timeout", TASK_OUTPUT_DEFAULT_TIMEOUT_MS)?;
                if timeout_ms > TASK_OUTPUT_MAX_TIMEOUT_MS {
                    return Err(format!(
                        "timeout must be between 0 and {TASK_OUTPUT_MAX_TIMEOUT_MS} milliseconds"
                    ));
                }

                match supervisor.status(task_id) {
                    Some(SubAgentStatus::Finished { result, .. }) => {
                        return finished_output(&supervisor, task_id, result);
                    }
                    Some(SubAgentStatus::Running) if !block => {
                        return Ok(format!("Agent {task_id} is still running."));
                    }
                    Some(SubAgentStatus::Closing | SubAgentStatus::Closed) => {
                        return Ok(format!("Agent {task_id} has been stopped."));
                    }
                    None => {
                        return Err(format!(
                            "No agent found with id: {task_id} (it was never spawned)"
                        ));
                    }
                    Some(SubAgentStatus::Running) => {}
                }

                match time::timeout(
                    Duration::from_millis(timeout_ms),
                    supervisor.wait_with_cancel(task_id, &ctx.cancel),
                )
                .await
                {
                    Ok(Ok(result)) => {
                        supervisor.suppress_parent_notification(task_id);
                        Ok(format_agent_result(&result))
                    }
                    Ok(Err(Error::Interrupted(InterruptReason::Cancelled))) => {
                        supervisor.suppress_parent_notification(task_id);
                        Err("Cancelled".to_string())
                    }
                    Ok(Err(error)) => {
                        supervisor.suppress_parent_notification(task_id);
                        Err(format_error(&error))
                    }
                    Err(_) => Ok(format!(
                        "Agent {task_id} is still running after waiting {timeout_ms} ms."
                    )),
                }
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub(crate) fn make_task_stop_tool(supervisor: SubAgentSupervisor) -> RegisteredTool {
    RegisteredTool {
        definition: definition(
            NativeTool::StopAgent,
            "Stop a running or completed background agent by task ID.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The background agent task ID to stop."
                    }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
        ),
        executor:   Arc::new(move |args, _ctx| {
            let supervisor = supervisor.clone();
            Box::pin(async move {
                let task_id = tools::required_str(&args, "task_id")?;
                supervisor
                    .close_agent(task_id)
                    .await
                    .map_err(|error| format_error(&error))?;
                Ok(format!("Agent {task_id} stopped."))
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub(crate) fn make_send_message_tool(supervisor: SubAgentSupervisor) -> RegisteredTool {
    RegisteredTool {
        definition: definition(
            NativeTool::MessageAgent,
            "Send additional instructions to a background agent by its task ID. A running agent receives them at a safe turn boundary. A completed agent starts another turn in the same session with its existing history.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "The background agent task ID."
                    },
                    "message": {
                        "type": "string",
                        "description": "The follow-up message."
                    },
                    "summary": {
                        "type": "string",
                        "maxLength": 200,
                        "description": "Optional short preview of the message."
                    }
                },
                "required": ["to", "message"],
                "additionalProperties": false
            }),
        ),
        executor:   Arc::new(move |args, _ctx| {
            let supervisor = supervisor.clone();
            Box::pin(async move {
                let recipient = tools::required_str(&args, "to")?;
                let message = tools::required_str(&args, "message")?;
                supervisor
                    .send_input(recipient, message)
                    .map_err(|error| format_error(&error))?;
                Ok(format!("Message sent to agent {recipient}."))
            })
        }),
        source:     ToolSource::Native,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::sandbox::Sandbox;
    use crate::test_support::{MockSandbox, make_session, text_response};
    use crate::todo_runtime::TodoRuntime;
    use crate::todo_tools::{
        make_task_create_tool, make_task_get_tool, make_task_list_tool, make_task_update_tool,
    };

    fn property_names(tool: &RegisteredTool) -> BTreeSet<&str> {
        tool.definition.parameters["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    fn required_names(tool: &RegisteredTool) -> BTreeSet<&str> {
        tool.definition.parameters["required"]
            .as_array()
            .map(|required| {
                required
                    .iter()
                    .map(|value| value.as_str().unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn assert_schema(tool: &RegisteredTool, properties: &[&str], required: &[&str]) {
        assert_eq!(tool.definition.parameters["type"], "object");
        assert_eq!(
            tool.definition.parameters["additionalProperties"],
            Value::Bool(false)
        );
        assert_eq!(property_names(tool), properties.iter().copied().collect());
        assert_eq!(required_names(tool), required.iter().copied().collect());
    }

    fn context() -> ToolContext {
        ToolContext {
            write_locks:         None,
            env:                 Arc::new(MockSandbox::default()) as Arc<dyn Sandbox>,
            cancel:              CancellationToken::new(),
            tool_env_provider:   None,
            session_id:          Some("root".to_string()),
            root_session_id:     Some("root".to_string()),
            tool_call_id:        Some("call".to_string()),
            agent_event_emitter: None,
        }
    }

    #[test]
    fn core_adapter_schemas_match_the_claude5_contract() {
        let options = NativeToolOptions::for_profile(fabro_model::AgentProfileKind::Claude5);
        assert_schema(&make_read_tool(), &["file_path", "limit", "offset"], &[
            "file_path",
        ]);
        assert_schema(&make_write_tool(), &["content", "file_path"], &[
            "content",
            "file_path",
        ]);
        assert_schema(
            &make_edit_tool(),
            &["file_path", "new_string", "old_string", "replace_all"],
            &["file_path", "new_string", "old_string"],
        );
        let bash = make_bash_tool(&options);
        assert_schema(&bash, &["command", "description", "timeout"], &["command"]);
        assert_eq!(
            bash.definition.parameters["properties"]["timeout"]["maximum"],
            600_000
        );
        assert_schema(&make_web_fetch_tool(None), &["prompt", "url"], &[
            "prompt", "url",
        ]);
        assert_schema(
            &make_web_search_tool(SearchBackend::brave("key".to_string())),
            &["query"],
            &["query"],
        );
        assert_schema(
            &make_web_search_tool(SearchBackend::venice("key".to_string())),
            &["query"],
            &["query"],
        );

        let todo_runtime = Arc::new(TodoRuntime::new());
        assert_schema(
            &strict_object_tool(make_task_create_tool(todo_runtime.clone())),
            &["activeForm", "description", "metadata", "subject"],
            &["description", "subject"],
        );
        assert_schema(
            &strict_object_tool(make_task_update_tool(todo_runtime.clone())),
            &[
                "activeForm",
                "addBlockedBy",
                "addBlocks",
                "description",
                "metadata",
                "owner",
                "status",
                "subject",
                "taskId",
            ],
            &["taskId"],
        );
        assert_schema(
            &strict_object_tool(make_task_get_tool(todo_runtime.clone())),
            &["taskId"],
            &["taskId"],
        );
        assert_schema(
            &strict_object_tool(make_task_list_tool(todo_runtime)),
            &[],
            &[],
        );
    }

    #[test]
    fn lifecycle_adapter_schemas_match_the_claude5_contract() {
        let supervisor = SubAgentSupervisor::new(3);
        let factory: SessionFactory = Arc::new(|| panic!("unused"));
        assert_schema(
            &make_agent_tool(supervisor.clone(), factory, 0),
            &["description", "prompt", "run_in_background"],
            &["description", "prompt"],
        );
        assert_schema(
            &make_task_output_tool(supervisor.clone()),
            &["block", "task_id", "timeout"],
            &["block", "task_id", "timeout"],
        );
        assert_schema(&make_task_stop_tool(supervisor.clone()), &["task_id"], &[
            "task_id",
        ]);
        let send_message = make_send_message_tool(supervisor);
        assert_schema(&send_message, &["message", "summary", "to"], &[
            "message", "to",
        ]);
        assert!(
            send_message
                .definition
                .description
                .contains("completed agent")
        );
        assert!(send_message.definition.description.contains("same session"));
    }

    #[tokio::test]
    async fn agent_defaults_to_background_and_produces_parent_notification() {
        let supervisor = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("child report")]).await;
        let session_slot = Arc::new(Mutex::new(Some(session)));
        let factory_slot = Arc::clone(&session_slot);
        let factory: SessionFactory = Arc::new(move || {
            factory_slot
                .lock()
                .unwrap()
                .take()
                .expect("factory should be called once")
        });
        let tool = make_agent_tool(supervisor.clone(), factory, 0);

        let output = (tool.executor)(
            json!({
                "description": "Inspect child",
                "prompt": "Inspect the child task"
            }),
            context(),
        )
        .await
        .unwrap();

        let task_id = output
            .strip_prefix("Agent started in the background.\n\nTask ID: ")
            .expect("Agent should return a background task ID");
        let notifications = supervisor
            .next_parent_notification_batch(&CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].agent_id, task_id);
        assert_eq!(notifications[0].description, "Inspect child");
        assert_eq!(
            notifications[0].result.as_ref().unwrap().output,
            "child report"
        );

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn task_output_suppresses_a_racing_automatic_notification() {
        let supervisor = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("explicit report")]).await;
        let task_id = supervisor
            .spawn_with_parent_notification(
                session,
                "Inspect".to_string(),
                "Inspect explicitly".to_string(),
                0,
            )
            .unwrap();
        supervisor
            .wait_with_cancel(&task_id, &CancellationToken::new())
            .await
            .unwrap();

        let tool = make_task_output_tool(supervisor.clone());
        let output = (tool.executor)(
            json!({
                "task_id": task_id,
                "block": false,
                "timeout": 0
            }),
            context(),
        )
        .await
        .unwrap();

        assert!(output.contains("explicit report"));
        assert!(
            supervisor
                .next_parent_notification_batch(&CancellationToken::new())
                .await
                .unwrap()
                .is_none()
        );

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn task_output_applies_the_schema_defaults_when_the_model_omits_them() {
        let supervisor = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("defaulted report")]).await;
        let task_id = supervisor.spawn(session, "Inspect".to_string(), 0).unwrap();
        supervisor
            .wait_with_cancel(&task_id, &CancellationToken::new())
            .await
            .unwrap();

        let tool = make_task_output_tool(supervisor.clone());
        let output = (tool.executor)(json!({ "task_id": task_id }), context())
            .await
            .unwrap();

        assert!(output.contains("defaulted report"));

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn task_output_rejects_a_wrongly_typed_optional_parameter() {
        let supervisor = SubAgentSupervisor::new(3);
        let tool = make_task_output_tool(supervisor);
        let error = (tool.executor)(
            json!({
                "task_id": "agent-1",
                "block": "yes"
            }),
            context(),
        )
        .await
        .unwrap_err();

        assert_eq!(error, "block must be a boolean");
    }
}
