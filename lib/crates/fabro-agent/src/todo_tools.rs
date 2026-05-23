//! Model-facing todo / task tools.
//!
//! Two surfaces share one engine ([`TodoRuntime`]):
//!
//! - [`make_update_plan_tool`] — Codex-compatible OpenAI `update_plan`.
//! - [`make_task_create_tool`] / [`make_task_update_tool`] /
//!   [`make_task_get_tool`] / [`make_task_list_tool`] — Claude task tools.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fabro_llm::types::ToolDefinition;
use fabro_types::{TodoListKind, TodoProjection, TodoStatus, TodoUpdatedProps};
use serde_json::Value;

use crate::todo_runtime::TodoRuntime;
use crate::tool_registry::{RegisteredTool, ToolContext, ToolSource};

/// Compute the OpenAI plan scope (`openai_plan:<session_id>`). Returns an
/// error string the model can see if no session ID is bound to the call.
fn openai_plan_scope(ctx: &ToolContext) -> Result<String, String> {
    ctx.session_id
        .as_ref()
        .map(|sid| TodoListKind::OpenAiPlan.list_id(sid))
        .ok_or_else(|| "update_plan requires an active session".to_string())
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

/// Deterministic todo id derived from `<list_id>::<step>`. Codex identifies
/// a plan step by the exact step text, so the projection ID is the
/// `sha256(list_id, step)` truncated for compactness.
fn openai_step_id(list_id: &str, step: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(list_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(step.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(out, "{byte:02x}");
    }
    out
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
                let list_id = openai_plan_scope(&ctx)?;
                let plan = args
                    .get("plan")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "Missing required parameter: plan".to_string())?;

                // Parse incoming steps, precompute ids, and enforce step-text uniqueness.
                let mut incoming: Vec<(String, String, TodoStatus)> =
                    Vec::with_capacity(plan.len());
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
                    let todo_id = openai_step_id(&list_id, step);
                    incoming.push((todo_id, step.to_string(), status));
                }

                // Snapshot previous state into a HashMap for O(1) lookup.
                let previous: HashMap<String, TodoProjection> = runtime
                    .snapshot(&list_id)
                    .map(|list| list.items.into_iter().map(|t| (t.id.clone(), t)).collect())
                    .unwrap_or_default();
                let incoming_ids: HashSet<&str> =
                    incoming.iter().map(|(id, _, _)| id.as_str()).collect();

                // Deletes: anything in previous but not in incoming.
                for id in previous.keys() {
                    if !incoming_ids.contains(id.as_str()) {
                        runtime.delete(&ctx, TodoListKind::OpenAiPlan, list_id.clone(), id.clone());
                    }
                }

                // Upserts: each incoming step becomes a create (new) or update.
                for (index, (todo_id, step, status)) in incoming.iter().enumerate() {
                    let order = u32::try_from(index).unwrap_or(u32::MAX);
                    match previous.get(todo_id) {
                        Some(prev)
                            if prev.status == *status
                                && prev.order == order
                                && prev.subject == *step =>
                        {
                            // No change.
                        }
                        Some(_) => {
                            runtime.update(&ctx, TodoUpdatedProps {
                                status: Some(*status),
                                order: Some(order),
                                subject: Some(step.clone()),
                                ..TodoUpdatedProps::new(&list_id, TodoListKind::OpenAiPlan, todo_id)
                            });
                        }
                        None => {
                            let mut projection =
                                TodoProjection::new(todo_id.clone(), order, step.clone());
                            projection.status = *status;
                            runtime.create(
                                &ctx,
                                TodoListKind::OpenAiPlan,
                                list_id.clone(),
                                projection,
                            );
                        }
                    }
                }

                Ok("Plan updated".to_string())
            })
        }),
        source:     ToolSource::Native,
    }
}

/// Per-list monotonically-increasing task counter for Anthropic
/// `TaskCreate`. Shared state lives inside the tool closure so two parallel
/// `TaskCreate` calls inside one session can never receive the same ID.
#[derive(Debug, Default)]
struct AnthropicTaskCounters {
    counters: Mutex<BTreeMap<String, Arc<AtomicU64>>>,
}

impl AnthropicTaskCounters {
    fn next(&self, list_id: &str) -> u64 {
        let counter = {
            let mut guard = self.counters.lock().expect("task counter lock poisoned");
            Arc::clone(
                guard
                    .entry(list_id.to_string())
                    .or_insert_with(|| Arc::new(AtomicU64::new(0))),
            )
        };
        counter.fetch_add(1, Ordering::Relaxed) + 1
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
    let counters = Arc::new(AnthropicTaskCounters::default());
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
            let counters = counters.clone();
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
                let task_id = counters.next(&list_id);
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
mod tests {
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;
    use crate::tool_registry::{AgentEventEmitter, ToolContext};
    use crate::types::AgentEvent;

    #[derive(Default)]
    struct SilentEmitter;
    impl AgentEventEmitter for SilentEmitter {
        fn emit(&self, _event: AgentEvent) {}
    }

    fn ctx_for(session: &str, root: &str) -> ToolContext {
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        ToolContext {
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
