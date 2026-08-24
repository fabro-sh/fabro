//! Model-facing todo / task tools.
//!
//! Three surfaces share one engine ([`TodoRuntime`]):
//!
//! - [`make_update_plan_tool`] — Codex-compatible OpenAI `update_plan`.
//! - [`make_todo_list_tool`] — Kimi Code-compatible whole-list `TodoList`.
//! - [`make_task_create_tool`] / [`make_task_update_tool`] /
//!   [`make_task_get_tool`] / [`make_task_list_tool`] — Claude task tools.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::str::FromStr;
use std::sync::Arc;

use fabro_llm::types::ToolDefinition;
use fabro_types::{TodoListKind, TodoProjection, TodoStatus, TodoUpdatedProps};
use serde_json::Value;
use strum::{EnumString, IntoStaticStr};

use crate::todo_runtime::TodoRuntime;
use crate::tool_registry::{RegisteredTool, ToolContext, ToolSource};

/// Compute a session-scoped todo-list ID. Returns an error the model can see
/// when a tool is invoked without an active session.
fn session_todo_scope(
    ctx: &ToolContext,
    kind: TodoListKind,
    tool_name: &str,
) -> Result<String, String> {
    ctx.session_id
        .as_ref()
        .map(|session_id| kind.list_id(session_id))
        .ok_or_else(|| format!("{tool_name} requires an active session"))
}

/// Compute the Anthropic task scope
/// (`anthropic_tasks:<root_session_id>`). Falls back to `session_id` when
/// the root is not bound; errors if neither is set.
fn anthropic_task_scope(ctx: &ToolContext) -> Result<String, String> {
    ctx.root_session_id
        .as_ref()
        .or(ctx.session_id.as_ref())
        .map(|sid| TodoListKind::AnthropicTasks.list_id(sid))
        .ok_or_else(|| "task tools require an active session".to_string())
}

/// Parse a wire status string into a [`TodoStatus`], optionally rejecting
/// `"deleted"` (OpenAI's `update_plan` does not accept deletions).
fn parse_status(value: &str, allow_deleted: bool) -> Result<TodoStatus, String> {
    let status = TodoStatus::from_str(value).map_err(|_| {
        if allow_deleted {
            format!("Invalid status `{value}` (expected pending|in_progress|completed|deleted)")
        } else {
            format!("Invalid status `{value}` (expected pending|in_progress|completed)")
        }
    })?;
    if !allow_deleted && status == TodoStatus::Deleted {
        return Err(format!(
            "Invalid status `{value}` (expected pending|in_progress|completed)"
        ));
    }
    Ok(status)
}

const TASK_CREATE_DESCRIPTION: &str = "Create pending tasks in the current session. \
Use concise subjects, descriptions, optional activeForm text, and metadata. Check \
TaskList first to avoid duplicate tasks.";

const TASK_UPDATE_DESCRIPTION: &str = "Update an existing task's status, text, owner, \
metadata, or dependencies. Valid statuses are pending, in_progress, completed, and \
deleted. After completing a task, call TaskList to find newly unblocked work.";

const TASK_LIST_DESCRIPTION: &str = "List tasks for the current session, including \
status, owner, and blocking dependencies. Use TaskGet with a taskId for full \
description and dependency details.";

const TASK_GET_DESCRIPTION: &str = "Get one task by taskId, including subject, status, \
description, owner, blockedBy, and blocks.";

/// Deterministic todo id derived from `<list_id>::<text>`. Whole-list tools
/// identify an item by its exact text, so unchanged entries preserve identity.
fn todo_text_id(list_id: &str, text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(list_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

struct ReplacementTodo {
    id:      String,
    subject: String,
    status:  TodoStatus,
}

fn reconcile_replacement_list(
    runtime: &TodoRuntime,
    ctx: &ToolContext,
    kind: TodoListKind,
    list_id: &str,
    incoming: &[ReplacementTodo],
) {
    let previous = runtime
        .snapshot(list_id)
        .map(|list| list.items)
        .unwrap_or_default();
    let previous_by_id: HashMap<&str, &TodoProjection> = previous
        .iter()
        .map(|todo| (todo.id.as_str(), todo))
        .collect();
    let incoming_ids: HashSet<&str> = incoming.iter().map(|todo| todo.id.as_str()).collect();

    for todo in &previous {
        if !incoming_ids.contains(todo.id.as_str()) {
            runtime.delete(ctx, kind, list_id.to_string(), todo.id.clone());
        }
    }

    for (index, todo) in incoming.iter().enumerate() {
        let order = u32::try_from(index).unwrap_or(u32::MAX);
        match previous_by_id.get(todo.id.as_str()) {
            Some(previous)
                if previous.status == todo.status
                    && previous.order == order
                    && previous.subject == todo.subject => {}
            Some(_) => {
                runtime.update(ctx, TodoUpdatedProps {
                    status: Some(todo.status),
                    order: Some(order),
                    subject: Some(todo.subject.clone()),
                    ..TodoUpdatedProps::new(list_id, kind, &todo.id)
                });
            }
            None => {
                let mut projection =
                    TodoProjection::new(todo.id.clone(), order, todo.subject.clone());
                projection.status = todo.status;
                runtime.create(ctx, kind, list_id.to_string(), projection);
            }
        }
    }
}

/// OpenAI `update_plan` tool. See plan summary for semantics.
#[must_use]
pub fn make_update_plan_tool(runtime: Arc<TodoRuntime>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "update_plan".into(),
            description: "Update the multi-step plan for the current task. Submit the entire \
                          plan; existing steps are reconciled by exact step text."
                .into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "explanation": {
                        "type": "string",
                        "description": "Optional natural-language note about why the plan changed"
                    },
                    "plan": {
                        "type": "array",
                        "description": "Ordered list of plan steps, each with a status",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": {"type": "string"},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["step", "status"]
                        }
                    }
                },
                "required": ["plan"]
            }),
        },
        executor:   Arc::new(move |args, ctx| {
            let runtime = runtime.clone();
            Box::pin(async move {
                let list_id = session_todo_scope(&ctx, TodoListKind::OpenAiPlan, "update_plan")?;
                let plan = args
                    .get("plan")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "Missing required parameter: plan".to_string())?;

                // Parse incoming steps, precompute ids, and enforce step-text uniqueness.
                let mut incoming = Vec::with_capacity(plan.len());
                let mut seen_steps: HashSet<&str> = HashSet::with_capacity(plan.len());
                for (index, entry) in plan.iter().enumerate() {
                    let step = entry
                        .get("step")
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("plan[{index}] is missing `step`"))?;
                    let status = entry
                        .get("status")
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("plan[{index}] is missing `status`"))?;
                    let status = parse_status(status, false)?;
                    if !seen_steps.insert(step) {
                        return Err(format!(
                            "Duplicate plan step `{step}` — step text must be unique"
                        ));
                    }
                    incoming.push(ReplacementTodo {
                        id: todo_text_id(&list_id, step),
                        subject: step.to_string(),
                        status,
                    });
                }

                reconcile_replacement_list(
                    &runtime,
                    &ctx,
                    TodoListKind::OpenAiPlan,
                    &list_id,
                    &incoming,
                );

                Ok("Plan updated".to_string())
            })
        }),
        source:     ToolSource::Native,
    }
}

#[derive(Clone, Copy, EnumString, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum KimiTodoStatus {
    Pending,
    InProgress,
    #[strum(to_string = "done")]
    Done,
}

impl From<KimiTodoStatus> for TodoStatus {
    fn from(status: KimiTodoStatus) -> Self {
        match status {
            KimiTodoStatus::Pending => Self::Pending,
            KimiTodoStatus::InProgress => Self::InProgress,
            KimiTodoStatus::Done => Self::Completed,
        }
    }
}

impl From<TodoStatus> for KimiTodoStatus {
    fn from(status: TodoStatus) -> Self {
        match status {
            TodoStatus::Pending => Self::Pending,
            TodoStatus::InProgress => Self::InProgress,
            TodoStatus::Completed | TodoStatus::Deleted => Self::Done,
        }
    }
}

/// Kimi Code spells the terminal status `done`; internally it is
/// [`TodoStatus::Completed`].
fn parse_kimi_status(value: &str) -> Result<TodoStatus, String> {
    value
        .parse::<KimiTodoStatus>()
        .map(TodoStatus::from)
        .map_err(|_| format!("Invalid status `{value}` (expected pending|in_progress|done)"))
}

fn kimi_status_name(status: TodoStatus) -> &'static str {
    KimiTodoStatus::from(status).into()
}

fn render_kimi_todos<'a>(items: impl IntoIterator<Item = (TodoStatus, &'a str)>) -> String {
    let mut items = items.into_iter().peekable();
    if items.peek().is_none() {
        return "The todo list is empty.".to_string();
    }
    let mut out = String::new();
    for (status, subject) in items {
        let _ = writeln!(out, "[{}] {subject}", kimi_status_name(status));
    }
    out.truncate(out.trim_end().len());
    out
}

/// Kimi Code-compatible `TodoList`.
///
/// A single tool serves reads and writes, matching the surface Kimi models are
/// trained against: omit `todos` to read, pass `[]` to clear, pass a list to
/// replace the whole thing. Items carry only `title` and `status`, and the
/// terminal status is spelled `done`.
///
/// Reconciliation mirrors `update_plan` — items are identified by their text,
/// so a re-submitted list preserves identity for unchanged entries — and the
/// same [`TodoRuntime`] backs it, so projections and events are unchanged.
pub fn make_todo_list_tool(runtime: Arc<TodoRuntime>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "TodoList".into(),
            description: "Maintain a structured TODO list for the current task. Use it \
                          proactively for multi-step work. Pass `todos` to replace the entire \
                          list, omit `todos` to read the current list without changing it, and \
                          pass an empty array to clear it. Keep exactly one item `in_progress` \
                          while work is underway, and mark an item `done` as soon as it is \
                          finished rather than batching completions at the end."
                .into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The updated todo list. Omit to read the current list \
            without making changes. Pass an empty array to clear the list.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {
                                    "type": "string",
                                    "description": "Short, actionable title for the todo."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "done"],
                                    "description": "Current status of the todo."
                                }
                            },
                            "required": ["title", "status"]
                        }
                    }
                }
            }),
        },
        executor:   Arc::new(move |args, ctx| {
            let runtime = runtime.clone();
            Box::pin(async move {
                let list_id = session_todo_scope(&ctx, TodoListKind::KimiTodos, "TodoList")?;

                // Read mode: `todos` omitted entirely.
                let Some(todos) = args.get("todos") else {
                    let items = runtime
                        .snapshot(&list_id)
                        .map(|l| l.items)
                        .unwrap_or_default();
                    return Ok(render_kimi_todos(
                        items
                            .iter()
                            .map(|todo| (todo.status, todo.subject.as_str())),
                    ));
                };
                let todos = todos
                    .as_array()
                    .ok_or_else(|| "`todos` must be an array".to_string())?;

                let mut incoming = Vec::with_capacity(todos.len());
                let mut seen: HashSet<&str> = HashSet::with_capacity(todos.len());
                for (index, entry) in todos.iter().enumerate() {
                    let title = entry
                        .get("title")
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("todos[{index}] is missing `title`"))?;
                    let status = entry
                        .get("status")
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("todos[{index}] is missing `status`"))?;
                    let status = parse_kimi_status(status)?;
                    if !seen.insert(title) {
                        return Err(format!("Duplicate todo `{title}` — titles must be unique"));
                    }
                    incoming.push(ReplacementTodo {
                        id: todo_text_id(&list_id, title),
                        subject: title.to_string(),
                        status,
                    });
                }

                reconcile_replacement_list(
                    &runtime,
                    &ctx,
                    TodoListKind::KimiTodos,
                    &list_id,
                    &incoming,
                );
                Ok(render_kimi_todos(
                    incoming
                        .iter()
                        .map(|todo| (todo.status, todo.subject.as_str())),
                ))
            })
        }),
        source:     ToolSource::Native,
    }
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn optional_string_vec(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key).and_then(Value::as_array).map(|values| {
        values
            .iter()
            .filter_map(|v| v.as_str().map(ToString::to_string))
            .collect()
    })
}

fn metadata_map(args: &Value) -> BTreeMap<String, Value> {
    args.get("metadata")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default()
}

fn append_task_refs(out: &mut String, label: &str, task_ids: &[String]) {
    if task_ids.is_empty() {
        return;
    }
    let _ = write!(out, "\n{label}: ");
    for (index, task_id) in task_ids.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "#{task_id}");
    }
}

fn format_task_details(todo: &TodoProjection) -> String {
    let mut out = format!(
        "Task #{}: {}\nStatus: {}\nDescription: {}",
        todo.id, todo.subject, todo.status, todo.description
    );
    if let Some(owner) = todo.owner.as_ref() {
        let _ = write!(out, "\nOwner: {owner}");
    }
    append_task_refs(&mut out, "Blocked by", &todo.blocked_by);
    append_task_refs(&mut out, "Blocks", &todo.blocks);
    out
}

#[must_use]
pub fn make_task_create_tool(runtime: Arc<TodoRuntime>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "TaskCreate".into(),
            description: TASK_CREATE_DESCRIPTION.into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "subject":     {"type": "string"},
                    "description": {"type": "string"},
                    "activeForm":  {"type": "string"},
                    "metadata":    {"type": "object", "additionalProperties": true}
                },
                "required": ["subject", "description"]
            }),
        },
        executor:   Arc::new(move |args, ctx| {
            let runtime = runtime.clone();
            Box::pin(async move {
                let list_id = anthropic_task_scope(&ctx)?;
                let subject = args
                    .get("subject")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Missing required parameter: subject".to_string())?
                    .to_string();
                let description = args
                    .get("description")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Missing required parameter: description".to_string())?
                    .to_string();
                let task_id = runtime.next_task_id(&list_id);
                let id_string = task_id.to_string();
                let order = u32::try_from(task_id.saturating_sub(1)).unwrap_or(u32::MAX);

                let mut projection = TodoProjection::new(id_string, order, subject.clone());
                projection.description = description;
                projection.active_form = optional_string(&args, "activeForm");
                projection.metadata = metadata_map(&args);

                runtime.create(&ctx, TodoListKind::AnthropicTasks, list_id, projection);

                Ok(format!("Task #{task_id} created successfully: {subject}"))
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub fn make_task_update_tool(runtime: Arc<TodoRuntime>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "TaskUpdate".into(),
            description: TASK_UPDATE_DESCRIPTION.into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "taskId":       {"type": "string"},
                    "subject":      {"type": "string"},
                    "description":  {"type": "string"},
                    "activeForm":   {"type": "string"},
                    "status":       {
                        "type": "string",
                        "enum": ["pending", "in_progress", "completed", "deleted"]
                    },
                    "owner":        {"type": "string"},
                    "addBlocks":    {"type": "array", "items": {"type": "string"}},
                    "addBlockedBy": {"type": "array", "items": {"type": "string"}},
                    "metadata":     {"type": "object", "additionalProperties": true}
                },
                "required": ["taskId"]
            }),
        },
        executor:   Arc::new(move |args, ctx| {
            let runtime = runtime.clone();
            Box::pin(async move {
                let list_id = anthropic_task_scope(&ctx)?;
                let task_id = args
                    .get("taskId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Missing required parameter: taskId".to_string())?
                    .to_string();

                let status = args
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|s| parse_status(s, true))
                    .transpose()?;

                let props = TodoUpdatedProps {
                    status,
                    subject: optional_string(&args, "subject"),
                    description: optional_string(&args, "description"),
                    active_form: args
                        .get("activeForm")
                        .map(|value| value.as_str().map(ToString::to_string)),
                    owner: args
                        .get("owner")
                        .map(|value| value.as_str().map(ToString::to_string)),
                    add_blocks: optional_string_vec(&args, "addBlocks"),
                    add_blocked_by: optional_string_vec(&args, "addBlockedBy"),
                    metadata_patch: metadata_map(&args),
                    ..TodoUpdatedProps::new(&list_id, TodoListKind::AnthropicTasks, &task_id)
                };

                if runtime.update(&ctx, props) {
                    Ok(format!("Task #{task_id} updated"))
                } else {
                    // Anthropic spec: missing task returns a non-error result.
                    Ok("Task not found".to_string())
                }
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub fn make_task_get_tool(runtime: Arc<TodoRuntime>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "TaskGet".into(),
            description: TASK_GET_DESCRIPTION.into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "taskId": {"type": "string"}
                },
                "required": ["taskId"]
            }),
        },
        executor:   Arc::new(move |args, ctx| {
            let runtime = runtime.clone();
            Box::pin(async move {
                let list_id = anthropic_task_scope(&ctx)?;
                let task_id = args
                    .get("taskId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Missing required parameter: taskId".to_string())?;

                let Some(snapshot) = runtime.snapshot(&list_id) else {
                    return Ok("Task not found".to_string());
                };
                let Some(todo) = snapshot.get(task_id) else {
                    return Ok("Task not found".to_string());
                };

                Ok(format_task_details(todo))
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub fn make_task_list_tool(runtime: Arc<TodoRuntime>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "TaskList".into(),
            description: TASK_LIST_DESCRIPTION.into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        executor:   Arc::new(move |_args, ctx| {
            let runtime = runtime.clone();
            Box::pin(async move {
                let list_id = anthropic_task_scope(&ctx)?;
                let snapshot = runtime.snapshot(&list_id);
                let items: &[TodoProjection] = snapshot.as_ref().map_or(&[], |list| &list.items);
                if items.is_empty() {
                    return Ok("No tasks found".to_string());
                }
                // Pre-build a status lookup so the per-row blocker filter is
                // O(B) rather than O(B * N).
                let status_by_id: HashMap<&str, TodoStatus> =
                    items.iter().map(|t| (t.id.as_str(), t.status)).collect();

                let mut out = String::new();
                for todo in items {
                    let _ = write!(out, "#{} [{}] {}", todo.id, todo.status, todo.subject);
                    if let Some(owner) = todo.owner.as_ref() {
                        let _ = write!(out, " (owner: {owner})");
                    }
                    // Uncompleted blockers only — Claude's convention.
                    let mut blockers = todo.blocked_by.iter().filter(|id| {
                        status_by_id
                            .get(id.as_str())
                            .copied()
                            .is_none_or(|s| s != TodoStatus::Completed)
                    });
                    if let Some(first) = blockers.next() {
                        let _ = write!(out, " (blocked by: {first}");
                        for blocker in blockers {
                            let _ = write!(out, ", {blocker}");
                        }
                        out.push(')');
                    }
                    out.push('\n');
                }
                Ok(out.trim_end().to_string())
            })
        }),
        source:     ToolSource::Native,
    }
}

#[cfg(test)]
mod kimi_todo_tests {
    use std::sync::Arc;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::tests::SilentEmitter;
    use super::*;
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;

    fn ctx() -> ToolContext {
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: Some("ses_kimi".to_string()),
            root_session_id: Some("ses_kimi".to_string()),
            tool_call_id: None,
            agent_event_emitter: Some(Arc::new(SilentEmitter)),
        }
    }

    async fn call(tool: &RegisteredTool, args: serde_json::Value) -> Result<String, String> {
        (tool.executor)(args, ctx()).await
    }

    #[tokio::test]
    async fn replaces_the_whole_list_and_reads_it_back() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_todo_list_tool(runtime);

        call(
            &tool,
            json!({"todos": [
                {"title": "read the config", "status": "done"},
                {"title": "patch the parser", "status": "in_progress"},
                {"title": "add a test", "status": "pending"}
            ]}),
        )
        .await
        .unwrap();

        // Read mode: `todos` omitted entirely.
        let listed = call(&tool, json!({})).await.unwrap();
        assert!(listed.contains("[done] read the config"), "{listed}");
        assert!(
            listed.contains("[in_progress] patch the parser"),
            "{listed}"
        );

        // Re-submitting a shorter list drops the missing entries.
        call(
            &tool,
            json!({"todos": [{"title": "add a test", "status": "done"}]}),
        )
        .await
        .unwrap();
        let listed = call(&tool, json!({})).await.unwrap();
        assert!(listed.contains("[done] add a test"), "{listed}");
        assert!(!listed.contains("patch the parser"), "{listed}");
    }

    #[tokio::test]
    async fn empty_array_clears_the_list() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_todo_list_tool(runtime);
        call(
            &tool,
            json!({"todos": [{"title": "x", "status": "pending"}]}),
        )
        .await
        .unwrap();
        call(&tool, json!({"todos": []})).await.unwrap();
        assert_eq!(
            call(&tool, json!({})).await.unwrap(),
            "The todo list is empty."
        );
    }

    /// Kimi Code spells the terminal status `done`; `completed` is the
    /// Anthropic/Codex spelling and must not be silently accepted.
    #[tokio::test]
    async fn status_vocabulary_is_kimi_codes() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_todo_list_tool(runtime);
        let err = call(
            &tool,
            json!({"todos": [{"title": "x", "status": "completed"}]}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("expected pending|in_progress|done"), "{err}");
    }

    #[tokio::test]
    async fn duplicate_titles_are_rejected() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_todo_list_tool(runtime);
        let err = call(
            &tool,
            json!({"todos": [
                {"title": "same", "status": "pending"},
                {"title": "same", "status": "done"}
            ]}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("must be unique"), "{err}");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;
    use crate::tool_registry::{AgentEventEmitter, ToolContext};
    use crate::types::AgentEvent;

    #[derive(Default)]
    pub(super) struct SilentEmitter;
    impl AgentEventEmitter for SilentEmitter {
        fn emit(&self, _event: AgentEvent) {}
    }

    fn ctx_for(session: &str, root: &str) -> ToolContext {
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: Some(session.to_string()),
            root_session_id: Some(root.to_string()),
            tool_call_id: None,
            agent_event_emitter: Some(Arc::new(SilentEmitter)),
        }
    }

    fn openai_list(session: &str) -> String {
        TodoListKind::OpenAiPlan.list_id(session)
    }

    fn anthropic_list(session: &str) -> String {
        TodoListKind::AnthropicTasks.list_id(session)
    }

    #[tokio::test]
    async fn update_plan_creates_initial_steps() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_update_plan_tool(runtime.clone());
        let ctx = ctx_for("ses_a", "ses_a");
        let out = (tool.executor)(
            serde_json::json!({
                "plan": [
                    {"step": "a", "status": "pending"},
                    {"step": "b", "status": "in_progress"},
                ]
            }),
            ctx,
        )
        .await
        .unwrap();
        assert_eq!(out, "Plan updated");
        let list = runtime.snapshot(&openai_list("ses_a")).unwrap();
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.items[0].subject, "a");
        assert_eq!(list.items[1].subject, "b");
        assert_eq!(list.items[1].status, TodoStatus::InProgress);
    }

    #[tokio::test]
    async fn update_plan_updates_status_and_order() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_update_plan_tool(runtime.clone());
        (tool.executor)(
            serde_json::json!({
                "plan": [
                    {"step": "a", "status": "pending"},
                    {"step": "b", "status": "pending"},
                ]
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (tool.executor)(
            serde_json::json!({
                "plan": [
                    {"step": "b", "status": "in_progress"},
                    {"step": "a", "status": "completed"},
                ]
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        let list = runtime.snapshot(&openai_list("ses_a")).unwrap();
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.items[0].subject, "b");
        assert_eq!(list.items[0].status, TodoStatus::InProgress);
        assert_eq!(list.items[1].subject, "a");
        assert_eq!(list.items[1].status, TodoStatus::Completed);
    }

    #[tokio::test]
    async fn update_plan_deletes_omitted_steps() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_update_plan_tool(runtime.clone());
        (tool.executor)(
            serde_json::json!({
                "plan": [
                    {"step": "a", "status": "pending"},
                    {"step": "b", "status": "pending"},
                    {"step": "c", "status": "pending"},
                ]
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (tool.executor)(
            serde_json::json!({
                "plan": [{"step": "b", "status": "completed"}]
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        let list = runtime.snapshot(&openai_list("ses_a")).unwrap();
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].subject, "b");
    }

    #[tokio::test]
    async fn update_plan_rejects_duplicate_steps() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_update_plan_tool(runtime);
        let err = (tool.executor)(
            serde_json::json!({
                "plan": [
                    {"step": "same", "status": "pending"},
                    {"step": "same", "status": "completed"},
                ]
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("Duplicate plan step"), "got: {err}");
    }

    #[tokio::test]
    async fn update_plan_subagent_writes_different_list_than_parent() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_update_plan_tool(runtime.clone());
        (tool.executor)(
            serde_json::json!({"plan": [{"step": "parent_step", "status": "pending"}]}),
            ctx_for("ses_parent", "ses_parent"),
        )
        .await
        .unwrap();
        (tool.executor)(
            serde_json::json!({"plan": [{"step": "child_step", "status": "pending"}]}),
            // Subagent session: own session_id is distinct from root.
            ctx_for("ses_child", "ses_parent"),
        )
        .await
        .unwrap();
        let parent = runtime.snapshot(&openai_list("ses_parent")).unwrap();
        let child = runtime.snapshot(&openai_list("ses_child")).unwrap();
        assert_eq!(parent.items.len(), 1);
        assert_eq!(parent.items[0].subject, "parent_step");
        assert_eq!(child.items.len(), 1);
        assert_eq!(child.items[0].subject, "child_step");
    }

    #[tokio::test]
    async fn task_create_returns_numeric_id_and_message() {
        let runtime = Arc::new(TodoRuntime::new());
        let create = make_task_create_tool(runtime.clone());
        let out = (create.executor)(
            serde_json::json!({"subject": "Do thing", "description": "details"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        assert_eq!(out, "Task #1 created successfully: Do thing");
        let list = runtime.snapshot(&anthropic_list("ses_a")).unwrap();
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].id, "1");
        assert_eq!(list.items[0].subject, "Do thing");
        assert_eq!(list.items[0].description, "details");
    }

    #[test]
    fn anthropic_task_tool_descriptions_are_concise() {
        let runtime = Arc::new(TodoRuntime::new());
        let create = make_task_create_tool(runtime.clone());
        let update = make_task_update_tool(runtime.clone());
        let list = make_task_list_tool(runtime);

        assert!(
            create
                .definition
                .description
                .contains("Create pending tasks")
        );
        assert!(create.definition.description.contains("activeForm"));
        assert!(update.definition.description.contains("pending"));
        assert!(update.definition.description.contains("deleted"));
        assert!(
            list.definition
                .description
                .contains("blocking dependencies")
        );

        let total_description_bytes = create.definition.description.len()
            + update.definition.description.len()
            + list.definition.description.len();
        assert!(total_description_bytes < 600);
        assert!(!create.definition.description.contains("##"));
        assert!(!update.definition.description.contains("```"));
    }

    #[tokio::test]
    async fn task_create_list_update_delete_cycle() {
        let runtime = Arc::new(TodoRuntime::new());
        let create = make_task_create_tool(runtime.clone());
        let update = make_task_update_tool(runtime.clone());
        let list_tool = make_task_list_tool(runtime.clone());

        (create.executor)(
            serde_json::json!({"subject": "First", "description": "desc"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (create.executor)(
            serde_json::json!({"subject": "Second", "description": "desc"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        let listing = (list_tool.executor)(serde_json::json!({}), ctx_for("ses_a", "ses_a"))
            .await
            .unwrap();
        assert!(listing.contains("#1 [pending] First"));
        assert!(listing.contains("#2 [pending] Second"));

        (update.executor)(
            serde_json::json!({"taskId": "1", "status": "completed"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (update.executor)(
            serde_json::json!({"taskId": "2", "status": "deleted"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        let listing = (list_tool.executor)(serde_json::json!({}), ctx_for("ses_a", "ses_a"))
            .await
            .unwrap();
        assert!(listing.contains("#1 [completed] First"));
        assert!(!listing.contains("#2"));
    }

    #[tokio::test]
    async fn task_update_metadata_merges_and_null_deletes() {
        let runtime = Arc::new(TodoRuntime::new());
        let create = make_task_create_tool(runtime.clone());
        let update = make_task_update_tool(runtime.clone());
        (create.executor)(
            serde_json::json!({"subject": "t", "description": "d", "metadata": {"k1": "v1"}}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (update.executor)(
            serde_json::json!({"taskId": "1", "metadata": {"k2": "v2"}}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (update.executor)(
            serde_json::json!({"taskId": "1", "metadata": {"k1": null}}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        let list = runtime.snapshot(&anthropic_list("ses_a")).unwrap();
        let meta = &list.items[0].metadata;
        assert!(!meta.contains_key("k1"));
        assert_eq!(meta.get("k2"), Some(&serde_json::json!("v2")));
    }

    #[tokio::test]
    async fn task_update_omitted_optional_strings_do_not_clear_existing_values() {
        let runtime = Arc::new(TodoRuntime::new());
        let create = make_task_create_tool(runtime.clone());
        let update = make_task_update_tool(runtime.clone());

        (create.executor)(
            serde_json::json!({
                "subject": "t",
                "description": "d",
                "activeForm": "doing t"
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (update.executor)(
            serde_json::json!({"taskId": "1", "owner": "alice"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (update.executor)(
            serde_json::json!({"taskId": "1", "metadata": {"k": "v"}}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();

        let list = runtime.snapshot(&anthropic_list("ses_a")).unwrap();
        assert_eq!(list.items[0].active_form.as_deref(), Some("doing t"));
        assert_eq!(list.items[0].owner.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn task_update_add_blocks_and_add_blocked_by_dedupe() {
        let runtime = Arc::new(TodoRuntime::new());
        let create = make_task_create_tool(runtime.clone());
        let update = make_task_update_tool(runtime.clone());
        (create.executor)(
            serde_json::json!({"subject": "t", "description": "d"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (update.executor)(
            serde_json::json!({
                "taskId": "1",
                "addBlocks": ["b1", "b2"],
                "addBlockedBy": ["c1"]
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (update.executor)(
            serde_json::json!({
                "taskId": "1",
                "addBlocks": ["b1", "b3"]
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        let list = runtime.snapshot(&anthropic_list("ses_a")).unwrap();
        assert_eq!(list.items[0].blocks, vec!["b1", "b2", "b3"]);
        assert_eq!(list.items[0].blocked_by, vec!["c1"]);
    }

    #[tokio::test]
    async fn task_list_empty_returns_no_tasks_found() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_task_list_tool(runtime);
        let out = (tool.executor)(serde_json::json!({}), ctx_for("ses_a", "ses_a"))
            .await
            .unwrap();
        assert_eq!(out, "No tasks found");
    }

    #[tokio::test]
    async fn task_update_missing_task_returns_not_found() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_task_update_tool(runtime);
        let out = (tool.executor)(
            serde_json::json!({"taskId": "999", "status": "completed"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        assert_eq!(out, "Task not found");
    }

    #[tokio::test]
    async fn task_get_returns_full_task_details() {
        let runtime = Arc::new(TodoRuntime::new());
        let create = make_task_create_tool(runtime.clone());
        let update = make_task_update_tool(runtime.clone());
        let get = make_task_get_tool(runtime);

        (create.executor)(
            serde_json::json!({
                "subject": "Investigate failing tests",
                "description": "Find the failing assertions and identify the smallest fix."
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        (update.executor)(
            serde_json::json!({
                "taskId": "1",
                "status": "in_progress",
                "owner": "agent-1",
                "addBlockedBy": ["2", "3"],
                "addBlocks": ["4"]
            }),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();

        let out = (get.executor)(
            serde_json::json!({"taskId": "1"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();

        assert_eq!(
            out,
            "\
Task #1: Investigate failing tests
Status: in_progress
Description: Find the failing assertions and identify the smallest fix.
Owner: agent-1
Blocked by: #2, #3
Blocks: #4"
        );
    }

    #[tokio::test]
    async fn task_get_missing_task_returns_not_found() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_task_get_tool(runtime);
        let out = (tool.executor)(
            serde_json::json!({"taskId": "999"}),
            ctx_for("ses_a", "ses_a"),
        )
        .await
        .unwrap();
        assert_eq!(out, "Task not found");
    }

    #[tokio::test]
    async fn parent_and_subagent_share_anthropic_task_list() {
        let runtime = Arc::new(TodoRuntime::new());
        let create = make_task_create_tool(runtime.clone());
        // Parent: session_id == root_session_id.
        (create.executor)(
            serde_json::json!({"subject": "p", "description": "d"}),
            ctx_for("ses_parent", "ses_parent"),
        )
        .await
        .unwrap();
        // Subagent: own session id but inherits parent's root.
        (create.executor)(
            serde_json::json!({"subject": "c", "description": "d"}),
            ctx_for("ses_child", "ses_parent"),
        )
        .await
        .unwrap();

        // Only one list keyed by the parent root.
        assert!(runtime.snapshot(&anthropic_list("ses_child")).is_none());
        let list = runtime.snapshot(&anthropic_list("ses_parent")).unwrap();
        assert_eq!(list.items.len(), 2);
    }
}
