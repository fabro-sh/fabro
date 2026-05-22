//! Shared todo / task domain types used by `update_plan` (OpenAI) and the
//! Claude task tools (`TaskCreate`, `TaskUpdate`, `TaskList`).
//!
//! Both tool families share the same event-sourced projection. The only
//! difference is the scoping convention captured by [`TodoListKind`]:
//!
//! - `openai_plan:<session_id>` — one list per emitting session.
//! - `anthropic_tasks:<root_session_id>` — one list shared by a root session
//!   and all of its subagent sessions.
//!
//! All mutations are projected from individual `todo.created`, `todo.updated`,
//! and `todo.deleted` run events.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Lifecycle status for a todo / task.
///
/// `Deleted` is reachable for Anthropic-style tasks (the model can request
/// `status: "deleted"` in `TaskUpdate`). The projection treats it as a hard
/// delete: any `todo.updated` carrying `status: Deleted` is followed by a
/// `todo.deleted` event and the todo disappears from the projected list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Deleted,
}

impl TodoStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Deleted => "deleted",
        }
    }
}

/// Scoping convention for a [`TodoListProjection`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoListKind {
    /// `update_plan` (OpenAI Codex-compatible). Scoped to the emitting
    /// session.
    #[default]
    OpenAiPlan,
    /// `TaskCreate` / `TaskUpdate` / `TaskList` (Anthropic). Scoped to the
    /// root agent session and shared by subagent sessions.
    AnthropicTasks,
}

impl TodoListKind {
    /// Wire prefix used in list identifiers.
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::OpenAiPlan => "openai_plan",
            Self::AnthropicTasks => "anthropic_tasks",
        }
    }

    /// Build the list identifier (`"<prefix>:<session>"`) used as the
    /// projection key.
    #[must_use]
    pub fn list_id(self, session: &str) -> String {
        format!("{}:{}", self.prefix(), session)
    }
}

/// One projected todo / task item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoProjection {
    /// Identity within `list_id`.
    pub id:          String,
    /// Lifecycle status. `Deleted` does not appear in the current projection
    /// because such todos are removed entirely.
    pub status:      TodoStatus,
    /// Ordering within the list. Lower comes first.
    pub order:       u32,
    /// Free-form summary (Claude `subject`, Codex `step`).
    pub subject:     String,
    /// Longer description (Claude `description`); empty when not provided.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Claude `activeForm` — phrasing used while the task is in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner:       Option<String>,
    /// IDs of other tasks this one blocks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks:      Vec<String>,
    /// IDs of tasks this one is blocked by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by:  Vec<String>,
    /// Per-todo metadata bag. Keys with `null` values are removed by
    /// `TaskUpdate`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata:    BTreeMap<String, serde_json::Value>,
}

impl TodoProjection {
    /// Build a minimal projection for a freshly-created todo.
    #[must_use]
    pub fn new(id: impl Into<String>, order: u32, subject: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: TodoStatus::Pending,
            order,
            subject: subject.into(),
            description: String::new(),
            active_form: None,
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
}

/// All currently-projected todos for one `list_id`.
///
/// Items are kept sorted by `(order, id)` and exposed via [`Self::items`] so
/// callers do not have to re-sort the projection on every read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoListProjection {
    pub kind:    TodoListKind,
    pub list_id: String,
    /// Items currently in the list, in display order.
    #[serde(default)]
    pub items:   Vec<TodoProjection>,
}

impl TodoListProjection {
    #[must_use]
    pub fn new(kind: TodoListKind, list_id: impl Into<String>) -> Self {
        Self {
            kind,
            list_id: list_id.into(),
            items: Vec::new(),
        }
    }

    /// Look up a todo by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&TodoProjection> {
        self.items.iter().find(|todo| todo.id == id)
    }

    /// Insert or replace a todo and re-sort by `(order, id)`.
    pub fn upsert(&mut self, todo: TodoProjection) {
        match self
            .items
            .iter()
            .position(|existing| existing.id == todo.id)
        {
            Some(index) => self.items[index] = todo,
            None => self.items.push(todo),
        }
        self.items.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.id.cmp(&right.id))
        });
    }

    /// Remove a todo by id. Returns whether anything was removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|todo| todo.id != id);
        before != self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_id_is_prefix_colon_session() {
        assert_eq!(
            TodoListKind::OpenAiPlan.list_id("ses_abc"),
            "openai_plan:ses_abc"
        );
        assert_eq!(
            TodoListKind::AnthropicTasks.list_id("ses_root"),
            "anthropic_tasks:ses_root"
        );
    }

    #[test]
    fn upsert_orders_by_order_then_id() {
        let mut list = TodoListProjection::new(TodoListKind::OpenAiPlan, "openai_plan:s");
        list.upsert(TodoProjection::new("a", 2, "second"));
        list.upsert(TodoProjection::new("b", 0, "first"));
        list.upsert(TodoProjection::new("c", 2, "second-tie"));

        let ids: Vec<&str> = list.items.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "a", "c"]);
    }

    #[test]
    fn upsert_replaces_existing_id() {
        let mut list = TodoListProjection::new(TodoListKind::OpenAiPlan, "openai_plan:s");
        list.upsert(TodoProjection::new("a", 0, "first"));
        let mut updated = TodoProjection::new("a", 0, "first");
        updated.status = TodoStatus::Completed;
        list.upsert(updated);

        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].status, TodoStatus::Completed);
    }

    #[test]
    fn remove_returns_true_when_present() {
        let mut list = TodoListProjection::new(TodoListKind::OpenAiPlan, "openai_plan:s");
        list.upsert(TodoProjection::new("a", 0, "first"));
        assert!(list.remove("a"));
        assert!(!list.remove("a"));
        assert!(list.items.is_empty());
    }
}
