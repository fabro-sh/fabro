//! In-memory todo / task projection shared across the `update_plan`
//! (OpenAI) and Anthropic task tools.
//!
//! The runtime is the source of truth while a session is live: tools mutate
//! it and emit one `todo.created` / `todo.updated` / `todo.deleted`
//! [`AgentEvent`] per change so the workflow event pipeline projects the
//! same state into the persisted [`fabro_types::RunProjection`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use fabro_types::{
    TodoCreatedProps, TodoDeletedProps, TodoListKind, TodoListProjection, TodoPatch,
    TodoProjection, TodoStatus, TodoUpdatedProps,
};

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
        let props = TodoCreatedProps {
            list_id:     list_id.clone(),
            list_kind:   kind,
            todo_id:     todo.id.clone(),
            status:      todo.status,
            order:       todo.order,
            subject:     todo.subject.clone(),
            description: todo.description.clone(),
            active_form: todo.active_form.clone(),
            owner:       todo.owner.clone(),
            blocks:      todo.blocks.clone(),
            blocked_by:  todo.blocked_by.clone(),
            metadata:    todo.metadata.clone(),
        };
        {
            let mut guard = self.lists.lock().expect("todo runtime lock poisoned");
            guard
                .entry(list_id)
                .or_insert_with(|| TodoListProjection::new(kind, props.list_id.clone()))
                .upsert(todo);
        }
        ctx.emit_agent_event(AgentEvent::TodoCreated(props));
    }

    /// Apply a typed update patch and emit `todo.updated` (or `todo.deleted`
    /// if `status == Deleted`). Returns whether a todo was found.
    pub fn update(&self, ctx: &ToolContext, props: TodoUpdatedProps) -> bool {
        // If the patch is a deletion, delegate to `delete` (atomic update).
        if matches!(props.status, Some(TodoStatus::Deleted)) {
            return self.delete(ctx, props.list_kind, props.list_id, props.todo_id);
        }

        let applied = {
            let mut guard = self.lists.lock().expect("todo runtime lock poisoned");
            let Some(list) = guard.get_mut(&props.list_id) else {
                return false;
            };
            list.apply_patch(&props.todo_id, &TodoPatch::from_props(&props))
        };
        if applied {
            ctx.emit_agent_event(AgentEvent::TodoUpdated(props));
        }
        applied
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
            ctx.emit_agent_event(AgentEvent::TodoDeleted(TodoDeletedProps {
                list_id,
                list_kind: kind,
                todo_id,
            }));
        }
        removed
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
            tool_call_id: None,
            agent_event_emitter: Some(emitter),
        }
    }

    #[test]
    fn create_then_update_then_delete_emits_three_events() {
        let runtime = TodoRuntime::new();
        let collector = Arc::new(CollectingEmitter::default());
        let ctx = ctx_with(collector.clone());
        let list_id = TodoListKind::OpenAiPlan.list_id("ses_a");

        runtime.create(
            &ctx,
            TodoListKind::OpenAiPlan,
            list_id.clone(),
            TodoProjection::new("a", 0, "first"),
        );
        runtime.update(&ctx, TodoUpdatedProps {
            status: Some(TodoStatus::InProgress),
            ..TodoUpdatedProps::new(&list_id, TodoListKind::OpenAiPlan, "a")
        });
        runtime.delete(&ctx, TodoListKind::OpenAiPlan, list_id, "a".to_string());

        let events = collector.events.lock().unwrap().clone();
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], AgentEvent::TodoCreated(_)));
        assert!(matches!(events[1], AgentEvent::TodoUpdated(_)));
        assert!(matches!(events[2], AgentEvent::TodoDeleted(_)));
    }

    #[test]
    fn update_with_deleted_status_emits_todo_deleted_only() {
        let runtime = TodoRuntime::new();
        let collector = Arc::new(CollectingEmitter::default());
        let ctx = ctx_with(collector.clone());
        let list_id = TodoListKind::AnthropicTasks.list_id("r");

        runtime.create(
            &ctx,
            TodoListKind::AnthropicTasks,
            list_id.clone(),
            TodoProjection::new("1", 0, "task"),
        );
        runtime.update(&ctx, TodoUpdatedProps {
            status: Some(TodoStatus::Deleted),
            ..TodoUpdatedProps::new(&list_id, TodoListKind::AnthropicTasks, "1")
        });

        let events = collector.events.lock().unwrap().clone();
        assert!(matches!(events[1], AgentEvent::TodoDeleted(_)));
        assert!(runtime.snapshot(&list_id).unwrap().items.is_empty());
    }

    #[test]
    fn update_returns_false_for_missing_todo() {
        let runtime = TodoRuntime::new();
        let collector = Arc::new(CollectingEmitter::default());
        let ctx = ctx_with(collector);
        let list_id = TodoListKind::AnthropicTasks.list_id("r");
        let found = runtime.update(
            &ctx,
            TodoUpdatedProps::new(&list_id, TodoListKind::AnthropicTasks, "missing"),
        );
        assert!(!found);
    }
}
