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
use strum::{Display, EnumString, IntoStaticStr};

/// Lifecycle status for a todo / task.
///
/// `Deleted` is reachable for Anthropic-style tasks (the model can request
/// `status: "deleted"` in `TaskUpdate`). The projection treats it as a hard
/// delete: any `todo.updated` carrying `status: Deleted` is followed by a
/// `todo.deleted` event and the todo disappears from the projected list.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Deleted,
}

/// Scoping convention for a [`TodoListProjection`].
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
pub enum TodoListKind {
    /// `update_plan` (OpenAI Codex-compatible). Scoped to the emitting
    /// session.
    #[default]
    #[serde(rename = "openai_plan")]
    #[strum(to_string = "openai_plan")]
    OpenAiPlan,
    /// `TaskCreate` / `TaskUpdate` / `TaskList` (Anthropic). Scoped to the
    /// root agent session and shared by subagent sessions.
    #[serde(rename = "anthropic_tasks")]
    #[strum(to_string = "anthropic_tasks")]
    AnthropicTasks,
}

impl TodoListKind {
    /// Build the list identifier (`"<prefix>:<session>"`) used as the
    /// projection key.
    #[must_use]
    pub fn list_id(self, session: &str) -> String {
        format!("{}:{}", <&'static str>::from(self), session)
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

    /// Apply a [`TodoPatch`] in place. `add_blocks` / `add_blocked_by`
    /// dedupe against existing entries. `metadata_patch` keys with a `null`
    /// JSON value delete that key; non-null values overwrite. Returns
    /// whether the `order` field changed (used by [`TodoListProjection`]
    /// to decide whether to re-sort).
    pub fn apply_patch(&mut self, patch: &TodoPatch<'_>) -> bool {
        let order_changed = patch.order.is_some_and(|o| o != self.order);
        if let Some(status) = patch.status {
            self.status = status;
        }
        if let Some(order) = patch.order {
            self.order = order;
        }
        if let Some(subject) = patch.subject {
            self.subject.clear();
            self.subject.push_str(subject);
        }
        if let Some(description) = patch.description {
            self.description.clear();
            self.description.push_str(description);
        }
        if let Some(active_form) = patch.active_form.as_ref() {
            self.active_form.clone_from(active_form);
        }
        if let Some(owner) = patch.owner.as_ref() {
            self.owner.clone_from(owner);
        }
        if let Some(extra) = patch.add_blocks {
            for id in extra {
                if !self.blocks.iter().any(|x| x == id) {
                    self.blocks.push(id.clone());
                }
            }
        }
        if let Some(extra) = patch.add_blocked_by {
            for id in extra {
                if !self.blocked_by.iter().any(|x| x == id) {
                    self.blocked_by.push(id.clone());
                }
            }
        }
        for (key, value) in patch.metadata_patch {
            if value.is_null() {
                self.metadata.remove(key);
            } else {
                self.metadata.insert(key.clone(), value.clone());
            }
        }
        order_changed
    }
}

/// Borrowed view of a `todo.updated` patch shared by the in-memory runtime
/// (`fabro-agent`) and the persisted-event reducer (`fabro-store`). Each
/// field follows the same "absent = no change" convention as
/// `TodoUpdatedProps`. `active_form` / `owner` are double-`Option` to
/// distinguish "unchanged" from "cleared".
#[derive(Debug, Clone, Copy)]
pub struct TodoPatch<'a> {
    pub status:         Option<TodoStatus>,
    pub order:          Option<u32>,
    pub subject:        Option<&'a str>,
    pub description:    Option<&'a str>,
    pub active_form:    Option<&'a Option<String>>,
    pub owner:          Option<&'a Option<String>>,
    pub add_blocks:     Option<&'a [String]>,
    pub add_blocked_by: Option<&'a [String]>,
    pub metadata_patch: &'a BTreeMap<String, serde_json::Value>,
}

impl<'a> TodoPatch<'a> {
    /// Borrow a [`TodoUpdatedProps`](super::run_event::TodoUpdatedProps) as
    /// a patch view. Used by the `fabro-store` reducer when replaying
    /// persisted events.
    #[must_use]
    pub fn from_props(props: &'a super::run_event::TodoUpdatedProps) -> Self {
        Self {
            status:         props.status,
            order:          props.order,
            subject:        props.subject.as_deref(),
            description:    props.description.as_deref(),
            active_form:    props.active_form.as_ref(),
            owner:          props.owner.as_ref(),
            add_blocks:     props.add_blocks.as_deref(),
            add_blocked_by: props.add_blocked_by.as_deref(),
            metadata_patch: &props.metadata_patch,
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
        self.sort();
    }

    /// Apply `patch` to the todo with id `todo_id`, returning `true` when
    /// the todo was found. Only re-sorts when `order` actually changed.
    pub fn apply_patch(&mut self, todo_id: &str, patch: &TodoPatch<'_>) -> bool {
        let Some(index) = self.items.iter().position(|t| t.id == todo_id) else {
            return false;
        };
        let order_changed = self.items[index].apply_patch(patch);
        if order_changed {
            self.sort();
        }
        true
    }

    /// Remove a todo by id. Returns whether anything was removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|todo| todo.id != id);
        before != self.items.len()
    }

    fn sort(&mut self) {
        self.items.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.id.cmp(&right.id))
        });
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
