use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

use crate::RunId;
use crate::id::ulid_id;

ulid_id!(SessionId);
ulid_id!(TurnId);

/// Agent tool permission level applied to a session.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum PermissionLevel {
    ReadOnly,
    ReadWrite,
    Full,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    Failed,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id:         SessionId,
    pub run_id:     RunId,
    pub title:      Option<String>,
    pub status:     SessionStatus,
    pub model:      Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SessionRecord {
    pub fn new(id: SessionId, run_id: RunId, now: DateTime<Utc>) -> Self {
        Self {
            id,
            run_id,
            title: None,
            status: SessionStatus::Idle,
            model: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id:         SessionId,
    pub run_id:     RunId,
    pub title:      Option<String>,
    pub status:     SessionStatus,
    pub model:      Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&SessionRecord> for SessionSummary {
    fn from(record: &SessionRecord) -> Self {
        Self {
            id:         record.id,
            run_id:     record.run_id,
            title:      record.title.clone(),
            status:     record.status,
            model:      record.model.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionMessage {
    User {
        content:   String,
        timestamp: DateTime<Utc>,
    },
    Assistant {
        content:        String,
        #[serde(default)]
        tool_calls:     Vec<serde_json::Value>,
        #[serde(default)]
        provider_parts: Vec<serde_json::Value>,
        #[serde(default)]
        usage:          serde_json::Value,
        response_id:    String,
        timestamp:      DateTime<Utc>,
    },
    ToolResults {
        #[serde(default)]
        results:   Vec<serde_json::Value>,
        timestamp: DateTime<Utc>,
    },
    System {
        content:   String,
        timestamp: DateTime<Utc>,
    },
    Steering {
        content:   String,
        timestamp: DateTime<Utc>,
    },
}

impl SessionMessage {
    pub fn user(content: impl Into<String>, timestamp: DateTime<Utc>) -> Self {
        Self::User {
            content: content.into(),
            timestamp,
        }
    }

    pub fn system(content: impl Into<String>, timestamp: DateTime<Utc>) -> Self {
        Self::System {
            content: content.into(),
            timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::SessionStatus;

    #[test]
    fn session_status_rejects_removed_terminal_states() {
        assert!(serde_json::from_value::<SessionStatus>(json!("closed")).is_err());
        assert!(serde_json::from_value::<SessionStatus>(json!("deleted")).is_err());
    }
}
