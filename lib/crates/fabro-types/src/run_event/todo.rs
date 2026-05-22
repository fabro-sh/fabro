//! Event bodies for the shared todo/task engine. See `crate::todo` for the
//! domain types and `RunProjectionReducer` (in `fabro-store`) for replay
//! semantics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{TodoListKind, TodoStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoCreatedProps {
    pub list_id:     String,
    pub list_kind:   TodoListKind,
    pub todo_id:     String,
    pub status:      TodoStatus,
    pub order:       u32,
    pub subject:     String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner:       Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks:      Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by:  Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata:    BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoUpdatedProps {
    pub list_id:        String,
    pub list_kind:      TodoListKind,
    pub todo_id:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status:         Option<TodoStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order:          Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description:    Option<String>,
    /// `Some(Some(_))` sets, `Some(None)` clears, `None` leaves unchanged.
    /// Encoded as JSON `null` vs absent on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form:    Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner:          Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_blocks:     Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_blocked_by: Option<Vec<String>>,
    /// Metadata patch. Keys with `null` value delete that key; non-null keys
    /// overwrite.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata_patch: BTreeMap<String, serde_json::Value>,
}

impl TodoUpdatedProps {
    /// Build an empty patch targeting `todo_id` in `list_id`. All optional
    /// patch fields default to "leave alone". Use the returned value with
    /// struct-update syntax to fill in the fields the caller wants to
    /// change.
    #[must_use]
    pub fn new(
        list_id: impl Into<String>,
        list_kind: TodoListKind,
        todo_id: impl Into<String>,
    ) -> Self {
        Self {
            list_id: list_id.into(),
            list_kind,
            todo_id: todo_id.into(),
            status: None,
            order: None,
            subject: None,
            description: None,
            active_form: None,
            owner: None,
            add_blocks: None,
            add_blocked_by: None,
            metadata_patch: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoDeletedProps {
    pub list_id:   String,
    pub list_kind: TodoListKind,
    pub todo_id:   String,
}
