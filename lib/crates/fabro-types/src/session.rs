use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

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
    Closed,
    Deleted,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum TurnStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl TurnStatus {
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id:              SessionId,
    pub title:           Option<String>,
    pub status:          SessionStatus,
    pub working_dir:     Option<String>,
    pub provider:        Option<String>,
    pub model:           Option<String>,
    pub permissions:     PermissionLevel,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
    pub deleted_at:      Option<DateTime<Utc>>,
    #[serde(default)]
    pub runtime_context: Vec<SessionMessage>,
}

impl SessionRecord {
    pub fn new(id: SessionId, now: DateTime<Utc>) -> Self {
        Self {
            id,
            title: None,
            status: SessionStatus::Idle,
            working_dir: None,
            provider: None,
            model: None,
            permissions: PermissionLevel::ReadWrite,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            runtime_context: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id:          SessionId,
    pub title:       Option<String>,
    pub status:      SessionStatus,
    pub working_dir: Option<String>,
    pub provider:    Option<String>,
    pub model:       Option<String>,
    pub created_at:  DateTime<Utc>,
    pub updated_at:  DateTime<Utc>,
}

impl From<&SessionRecord> for SessionSummary {
    fn from(record: &SessionRecord) -> Self {
        Self {
            id:          record.id,
            title:       record.title.clone(),
            status:      record.status,
            working_dir: record.working_dir.clone(),
            provider:    record.provider.clone(),
            model:       record.model.clone(),
            created_at:  record.created_at,
            updated_at:  record.updated_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub id:           TurnId,
    pub session_id:   SessionId,
    pub input:        String,
    pub status:       TurnStatus,
    pub output:       Option<String>,
    pub error:        Option<String>,
    pub created_at:   DateTime<Utc>,
    pub updated_at:   DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEventEnvelope {
    pub seq:        u32,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id:    Option<TurnId>,
    pub event:      String,
    pub properties: serde_json::Value,
    pub ts:         DateTime<Utc>,
}

impl SessionEventEnvelope {
    pub fn new(
        session_id: SessionId,
        turn_id: Option<TurnId>,
        event: impl Into<String>,
        properties: serde_json::Value,
        ts: DateTime<Utc>,
    ) -> Self {
        Self {
            seq: 0,
            session_id,
            turn_id,
            event: event.into(),
            properties,
            ts,
        }
    }

    #[must_use]
    pub fn with_seq(mut self, seq: u32) -> Self {
        self.seq = seq;
        self
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
