//! In-memory todo / task projection shared across the `update_plan`
//! (OpenAI) and Anthropic task tools.
//!
//! The runtime is the source of truth while a session is live: tools mutate
//! it and emit one `todo.created` / `todo.updated` / `todo.deleted`
//! [`AgentEvent`] per change so the workflow event pipeline projects the
//! same state into the persisted [`fabro_types::RunProjection`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use fabro_types::{TodoListKind, TodoListProjection, TodoProjection, TodoStatus};
use serde_json::Value;

use crate::tool_registry::ToolContext;
use crate::types::AgentEvent;

/// Shared, thread-safe todo projection. Wrap it in `Arc` and clone the
/// `Arc` into each tool closure that needs it.
#[derive(Debug, Default)]
pub struct TodoRuntime {
    lists: Mutex<BTreeMap<String, TodoListProjection>>,
}

impl TodoRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self {
            lists: Mutex::new(BTreeMap::new()),
        }
    }

    /// Snapshot the projection for `list_id`. Used by tests and by the
    /// list-style tools that need a stable view.
    #[must_use]
    pub fn snapshot(&self, list_id: &str) -> Option<TodoListProjection> {
        let guard = self.lists.lock().expect("todo runtime lock poisoned");
        guard.get(list_id).cloned()
    }

    /// Insert (or replace) a todo and emit `todo.created`.
    pub fn create(
        &self,
        ctx: &ToolContext,
        kind: TodoListKind,
        list_id: String,
        todo: TodoProjection,
    ) {
        {
            let mut guard = self.lists.lock().expect("todo runtime lock poisoned");
            let list = guard
                .entry(list_id.clone())
                .or_insert_with(|| TodoListProjection::new(kind, list_id.clone()));
            list.upsert(todo.clone());
        }
        ctx.emit_agent_event(AgentEvent::TodoCreated {
            list_id,
            list_kind: kind,
            todo_id: todo.id,
            status: todo.status,
            order: todo.order,
            subject: todo.subject,
            description: todo.description,
            active_form: todo.active_form,
            owner: todo.owner,
            blocks: todo.blocks,
            blocked_by: todo.blocked_by,
            metadata: todo.metadata,
        });
    }

    /// Apply a typed update patch and emit `todo.updated` (or `todo.deleted`
    /// if `status == Deleted`). Returns whether a todo was found.
    pub fn update(&self, ctx: &ToolContext, patch: TodoUpdate<'_>) -> bool {
        let TodoUpdate {
            list_id,
            kind,
            todo_id,
            status,
            order,
            subject,
            description,
            active_form,
            owner,
            add_blocks,
            add_blocked_by,
            metadata_patch,
        } = patch;

        // If the patch is a deletion, delegate to `delete` (atomic update).
        if matches!(status, Some(TodoStatus::Deleted)) {
            return self.delete(ctx, kind, list_id.to_string(), todo_id.to_string());
        }

        let updated_event_payload;
        {
            let mut guard = self.lists.lock().expect("todo runtime lock poisoned");
            let Some(list) = guard.get_mut(list_id) else {
                return false;
            };
            let Some(index) = list.items.iter().position(|item| item.id == todo_id) else {
                return false;
            };
            let mut todo = list.items[index].clone();
            if let Some(status) = status {
                todo.status = status;
            }
            if let Some(order) = order {
                todo.order = order;
            }
            if let Some(subject) = subject {
                todo.subject = subject.to_string();
            }
            if let Some(description) = description {
                todo.description = description.to_string();
            }
            if let Some(active_form) = active_form.clone() {
                todo.active_form = active_form;
            }
            if let Some(owner) = owner.clone() {
                todo.owner = owner;
            }
            if let Some(extra) = add_blocks.as_ref() {
                for id in extra {
                    if !todo.blocks.contains(id) {
                        todo.blocks.push(id.clone());
                    }
                }
            }
            if let Some(extra) = add_blocked_by.as_ref() {
                for id in extra {
                    if !todo.blocked_by.contains(id) {
                        todo.blocked_by.push(id.clone());
                    }
                }
            }
            for (key, value) in &metadata_patch {
                if value.is_null() {
                    todo.metadata.remove(key);
                } else {
                    todo.metadata.insert(key.clone(), value.clone());
                }
            }
            list.upsert(todo);

            updated_event_payload = AgentEvent::TodoUpdated {
                list_id: list_id.to_string(),
                list_kind: kind,
                todo_id: todo_id.to_string(),
                status,
                order,
                subject: subject.map(ToString::to_string),
                description: description.map(ToString::to_string),
                active_form,
                owner,
                add_blocks,
                add_blocked_by,
                metadata_patch,
            };
        }
        ctx.emit_agent_event(updated_event_payload);
        true
    }

    /// Remove `todo_id` from `list_id` and emit `todo.deleted`. Returns
    /// whether anything was removed.
    pub fn delete(
        &self,
        ctx: &ToolContext,
        kind: TodoListKind,
        list_id: String,
        todo_id: String,
    ) -> bool {
        let removed = {
            let mut guard = self.lists.lock().expect("todo runtime lock poisoned");
            let Some(list) = guard.get_mut(&list_id) else {
                return false;
            };
            list.remove(&todo_id)
        };
        if removed {
            ctx.emit_agent_event(AgentEvent::TodoDeleted {
                list_id,
                list_kind: kind,
                todo_id,
            });
        }
        removed
    }
}

/// Strongly-typed input to [`TodoRuntime::update`]. `None` fields leave
/// the corresponding column unchanged. `metadata_patch` keys with `null`
/// values are deleted; non-null values overwrite.
#[derive(Debug, Default)]
pub struct TodoUpdate<'a> {
    pub list_id:        &'a str,
    pub kind:           TodoListKind,
    pub todo_id:        &'a str,
    pub status:         Option<TodoStatus>,
    pub order:          Option<u32>,
    pub subject:        Option<&'a str>,
    pub description:    Option<&'a str>,
    pub active_form:    Option<Option<String>>,
    pub owner:          Option<Option<String>>,
    pub add_blocks:     Option<Vec<String>>,
    pub add_blocked_by: Option<Vec<String>>,
    pub metadata_patch: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;
    use crate::tool_registry::{AgentEventEmitter, ToolContext};

    #[derive(Default)]
    struct CollectingEmitter {
        events: Mutex<Vec<AgentEvent>>,
    }

    impl AgentEventEmitter for CollectingEmitter {
        fn emit(&self, event: AgentEvent) {
            self.events
                .lock()
                .expect("collector lock poisoned")
                .push(event);
        }
    }

    fn ctx_with(emitter: Arc<CollectingEmitter>) -> ToolContext {
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        ToolContext {
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: Some("ses_a".to_string()),
            root_session_id: Some("ses_a".to_string()),
            tool_call_id: Some("call_1".to_string()),
            agent_event_emitter: Some(emitter),
        }
    }

    #[test]
    fn create_then_update_then_delete_emits_three_events() {
        let runtime = TodoRuntime::new();
        let collector = Arc::new(CollectingEmitter::default());
        let ctx = ctx_with(collector.clone());

        runtime.create(
            &ctx,
            TodoListKind::OpenAiPlan,
            "openai_plan:ses_a".to_string(),
            TodoProjection::new("a", 0, "first"),
        );
        runtime.update(&ctx, TodoUpdate {
            list_id: "openai_plan:ses_a",
            kind: TodoListKind::OpenAiPlan,
            todo_id: "a",
            status: Some(TodoStatus::InProgress),
            ..TodoUpdate::default()
        });
        runtime.delete(
            &ctx,
            TodoListKind::OpenAiPlan,
            "openai_plan:ses_a".to_string(),
            "a".to_string(),
        );

        let events = collector.events.lock().unwrap().clone();
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], AgentEvent::TodoCreated { .. }));
        assert!(matches!(events[1], AgentEvent::TodoUpdated { .. }));
        assert!(matches!(events[2], AgentEvent::TodoDeleted { .. }));
    }

    #[test]
    fn update_with_deleted_status_emits_todo_deleted_only() {
        let runtime = TodoRuntime::new();
        let collector = Arc::new(CollectingEmitter::default());
        let ctx = ctx_with(collector.clone());

        runtime.create(
            &ctx,
            TodoListKind::AnthropicTasks,
            "anthropic_tasks:r".to_string(),
            TodoProjection::new("1", 0, "task"),
        );
        runtime.update(&ctx, TodoUpdate {
            list_id: "anthropic_tasks:r",
            kind: TodoListKind::AnthropicTasks,
            todo_id: "1",
            status: Some(TodoStatus::Deleted),
            ..TodoUpdate::default()
        });

        let events = collector.events.lock().unwrap().clone();
        assert!(matches!(events[1], AgentEvent::TodoDeleted { .. }));
        assert!(
            runtime
                .snapshot("anthropic_tasks:r")
                .unwrap()
                .items
                .is_empty()
        );
    }

    #[test]
    fn update_returns_false_for_missing_todo() {
        let runtime = TodoRuntime::new();
        let collector = Arc::new(CollectingEmitter::default());
        let ctx = ctx_with(collector);
        let found = runtime.update(&ctx, TodoUpdate {
            list_id: "anthropic_tasks:r",
            kind: TodoListKind::AnthropicTasks,
            todo_id: "missing",
            ..TodoUpdate::default()
        });
        assert!(!found);
    }
}
