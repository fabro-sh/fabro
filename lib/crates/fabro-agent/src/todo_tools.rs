//! Model-facing todo / task tools.
//!
//! Two surfaces share one engine ([`TodoRuntime`]):
//!
//! - [`make_update_plan_tool`] — Codex-compatible OpenAI `update_plan`.
//! - [`make_task_create_tool`] / [`make_task_update_tool`] /
//!   [`make_task_list_tool`] — Claude task tools.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fabro_llm::types::ToolDefinition;
use fabro_types::{TodoListKind, TodoProjection, TodoStatus};
use serde_json::Value;

use crate::todo_runtime::{TodoRuntime, TodoUpdate};
use crate::tool_registry::{RegisteredTool, ToolContext};

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

fn parse_openai_status(value: &str) -> Result<TodoStatus, String> {
    match value {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" => Ok(TodoStatus::InProgress),
        "completed" => Ok(TodoStatus::Completed),
        other => Err(format!(
            "Invalid status `{other}` (expected pending|in_progress|completed)"
        )),
    }
}

fn parse_anthropic_status(value: &str) -> Result<TodoStatus, String> {
    match value {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" => Ok(TodoStatus::InProgress),
        "completed" => Ok(TodoStatus::Completed),
        "deleted" => Ok(TodoStatus::Deleted),
        other => Err(format!(
            "Invalid status `{other}` (expected pending|in_progress|completed|deleted)"
        )),
    }
}

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

                // Parse incoming steps and enforce step-text uniqueness.
                let mut incoming: Vec<(String, TodoStatus)> = Vec::with_capacity(plan.len());
                let mut seen: BTreeSet<String> = BTreeSet::new();
                for (index, entry) in plan.iter().enumerate() {
                    let step = entry
                        .get("step")
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("plan[{index}] is missing `step`"))?
                        .to_string();
                    let status = entry
                        .get("status")
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("plan[{index}] is missing `status`"))?;
                    let status = parse_openai_status(status)?;
                    if !seen.insert(step.clone()) {
                        return Err(format!(
                            "Duplicate plan step `{step}` — step text must be unique"
                        ));
                    }
                    incoming.push((step, status));
                }

                let previous = runtime
                    .snapshot(&list_id)
                    .map(|list| list.items)
                    .unwrap_or_default();
                let previous_ids: BTreeSet<String> =
                    previous.iter().map(|todo| todo.id.clone()).collect();

                let incoming_ids: BTreeSet<String> = incoming
                    .iter()
                    .map(|(step, _)| openai_step_id(&list_id, step))
                    .collect();

                // Deletes: anything in previous but not in incoming.
                for todo in &previous {
                    if !incoming_ids.contains(&todo.id) {
                        runtime.delete(
                            &ctx,
                            TodoListKind::OpenAiPlan,
                            list_id.clone(),
                            todo.id.clone(),
                        );
                    }
                }

                // Upserts: each incoming step becomes a create (new) or
                // update (existing id with changed status/order).
                for (index, (step, status)) in incoming.iter().enumerate() {
                    let todo_id = openai_step_id(&list_id, step);
                    let order = u32::try_from(index).unwrap_or(u32::MAX);
                    if previous_ids.contains(&todo_id) {
                        let prev = previous
                            .iter()
                            .find(|todo| todo.id == todo_id)
                            .expect("previous_ids consistency");
                        if prev.status == *status && prev.order == order && prev.subject == *step {
                            continue;
                        }
                        runtime.update(&ctx, TodoUpdate {
                            list_id: &list_id,
                            kind: TodoListKind::OpenAiPlan,
                            todo_id: &todo_id,
                            status: Some(*status),
                            order: Some(order),
                            subject: Some(step.as_str()),
                            ..TodoUpdate::default()
                        });
                    } else {
                        let mut projection = TodoProjection::new(todo_id, order, step.clone());
                        projection.status = *status;
                        runtime.create(&ctx, TodoListKind::OpenAiPlan, list_id.clone(), projection);
                    }
                }

                Ok("Plan updated".to_string())
            })
        }),
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

#[must_use]
pub fn make_task_create_tool(runtime: Arc<TodoRuntime>) -> RegisteredTool {
    let counters = Arc::new(AnthropicTaskCounters::default());
    RegisteredTool {
        definition: ToolDefinition {
            name:        "TaskCreate".into(),
            description: "Create a new task in the shared task list".into(),
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
                let active_form = args
                    .get("activeForm")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let metadata = args
                    .get("metadata")
                    .and_then(Value::as_object)
                    .map(|map| {
                        map.iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect::<BTreeMap<_, _>>()
                    })
                    .unwrap_or_default();
                let task_id = counters.next(&list_id);
                let id_string = task_id.to_string();
                let order = u32::try_from(task_id.saturating_sub(1)).unwrap_or(u32::MAX);

                let mut projection = TodoProjection::new(id_string.clone(), order, subject.clone());
                projection.description = description;
                projection.active_form = active_form;
                projection.metadata = metadata;

                runtime.create(&ctx, TodoListKind::AnthropicTasks, list_id, projection);

                Ok(format!("Task #{task_id} created successfully: {subject}"))
            })
        }),
    }
}

#[must_use]
pub fn make_task_update_tool(runtime: Arc<TodoRuntime>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "TaskUpdate".into(),
            description: "Update an existing task. status: \"deleted\" deletes it.".into(),
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
                    .map(parse_anthropic_status)
                    .transpose()?;
                let subject = args
                    .get("subject")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let description = args
                    .get("description")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let active_form = args
                    .get("activeForm")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let owner = args
                    .get("owner")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let add_blocks = args
                    .get("addBlocks")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|v| v.as_str().map(ToString::to_string))
                            .collect::<Vec<_>>()
                    });
                let add_blocked_by =
                    args.get("addBlockedBy")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|v| v.as_str().map(ToString::to_string))
                                .collect::<Vec<_>>()
                        });
                let metadata_patch = args
                    .get("metadata")
                    .and_then(Value::as_object)
                    .map(|map| {
                        map.iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect::<BTreeMap<_, _>>()
                    })
                    .unwrap_or_default();

                let found = runtime.update(&ctx, TodoUpdate {
                    list_id: &list_id,
                    kind: TodoListKind::AnthropicTasks,
                    todo_id: &task_id,
                    status,
                    order: None,
                    subject: subject.as_deref(),
                    description: description.as_deref(),
                    active_form: Some(active_form),
                    owner: Some(owner),
                    add_blocks,
                    add_blocked_by,
                    metadata_patch,
                });
                if !found {
                    // Anthropic spec: missing task returns a non-error result.
                    return Ok("Task not found".to_string());
                }
                Ok(format!("Task #{task_id} updated"))
            })
        }),
    }
}

#[must_use]
pub fn make_task_list_tool(runtime: Arc<TodoRuntime>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "TaskList".into(),
            description: "List all tasks in the shared task list".into(),
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
                let mut out = String::new();
                for todo in items {
                    let _ = write!(
                        out,
                        "#{} [{}] {}",
                        todo.id,
                        todo.status.as_str(),
                        todo.subject
                    );
                    if let Some(owner) = todo.owner.as_ref() {
                        let _ = write!(out, " (owner: {owner})");
                    }
                    // Uncompleted blockers only — Claude's convention.
                    let active_blockers: Vec<&String> = todo
                        .blocked_by
                        .iter()
                        .filter(|id| {
                            items
                                .iter()
                                .find(|other| &&other.id == id)
                                .is_none_or(|blocker| blocker.status != TodoStatus::Completed)
                        })
                        .collect();
                    if !active_blockers.is_empty() {
                        let _ = write!(
                            out,
                            " (blocked by: {})",
                            active_blockers
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    out.push('\n');
                }
                Ok(out.trim_end().to_string())
            })
        }),
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
            tool_call_id: Some("call_1".to_string()),
            agent_event_emitter: Some(Arc::new(SilentEmitter)),
        }
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
        let list = runtime
            .snapshot(&TodoListKind::OpenAiPlan.list_id("ses_a"))
            .unwrap();
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.items[0].subject, "a");
        assert_eq!(list.items[1].subject, "b");
        assert_eq!(list.items[1].status, TodoStatus::InProgress);
    }

    #[tokio::test]
    async fn update_plan_updates_status_and_order() {
        let runtime = Arc::new(TodoRuntime::new());
        let tool = make_update_plan_tool(runtime.clone());
        let ctx = ctx_for("ses_a", "ses_a");
        (tool.executor)(
            serde_json::json!({
                "plan": [
                    {"step": "a", "status": "pending"},
                    {"step": "b", "status": "pending"},
                ]
            }),
            ctx,
        )
        .await
        .unwrap();
        let ctx = ctx_for("ses_a", "ses_a");
        (tool.executor)(
            serde_json::json!({
                "plan": [
                    {"step": "b", "status": "in_progress"},
                    {"step": "a", "status": "completed"},
                ]
            }),
            ctx,
        )
        .await
        .unwrap();
        let list = runtime
            .snapshot(&TodoListKind::OpenAiPlan.list_id("ses_a"))
            .unwrap();
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
        let list = runtime
            .snapshot(&TodoListKind::OpenAiPlan.list_id("ses_a"))
            .unwrap();
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
        let parent = runtime
            .snapshot(&TodoListKind::OpenAiPlan.list_id("ses_parent"))
            .unwrap();
        let child = runtime
            .snapshot(&TodoListKind::OpenAiPlan.list_id("ses_child"))
            .unwrap();
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
        let list = runtime
            .snapshot(&TodoListKind::AnthropicTasks.list_id("ses_a"))
            .unwrap();
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].id, "1");
        assert_eq!(list.items[0].subject, "Do thing");
        assert_eq!(list.items[0].description, "details");
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
        let list = runtime
            .snapshot(&TodoListKind::AnthropicTasks.list_id("ses_a"))
            .unwrap();
        let meta = &list.items[0].metadata;
        assert!(!meta.contains_key("k1"));
        assert_eq!(meta.get("k2"), Some(&serde_json::json!("v2")));
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
        let list = runtime
            .snapshot(&TodoListKind::AnthropicTasks.list_id("ses_a"))
            .unwrap();
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
        assert!(
            runtime
                .snapshot(&TodoListKind::AnthropicTasks.list_id("ses_child"))
                .is_none()
        );
        let list = runtime
            .snapshot(&TodoListKind::AnthropicTasks.list_id("ses_parent"))
            .unwrap();
        assert_eq!(list.items.len(), 2);
    }
}
