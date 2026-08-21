use chrono::{DateTime, Utc};
use serde::de::Error as DeError;
use serde::ser::Error as SerError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use crate::{
    AutomationRef, BilledTokenCounts, FailureCategory, FailureReason, GitContext, Principal, RunId,
    SuccessReason,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainEventEnvelope {
    pub seq:   u32,
    pub event: MainEvent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MainEvent {
    pub id:   String,
    pub ts:   DateTime<Utc>,
    pub body: MainEventBody,
}

#[allow(
    clippy::large_enum_variant,
    reason = "Main event bodies stay inline to match the tagged wire format."
)]
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "event", content = "properties")]
pub enum MainEventBody {
    #[serde(rename = "fabro.run.created")]
    RunCreated(MainEventCreatedProps),
    #[serde(rename = "fabro.run.started")]
    RunStarted(MainEventStartedProps),
    #[serde(rename = "fabro.run.running")]
    RunRunning(MainEventLifecycleProps),
    #[serde(rename = "fabro.run.completed")]
    RunCompleted(MainEventCompletedProps),
    #[serde(rename = "fabro.run.failed")]
    RunFailed(MainEventFailedProps),
    #[serde(rename = "fabro.run.cancelled")]
    RunCancelled(MainEventCancelledProps),
    #[serde(rename = "fabro.run.paused")]
    RunPaused(MainEventLifecycleProps),
    #[serde(rename = "fabro.run.unpaused")]
    RunUnpaused(MainEventLifecycleProps),
    Unknown {
        name:       String,
        properties: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainEventSource {
    pub run_id:            RunId,
    pub source_event_id:   String,
    pub source_event_ts:   DateTime<Utc>,
    pub source_event_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor:             Option<Principal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainEventCreatedProps {
    #[serde(flatten)]
    pub source:             MainEventSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title:              Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation:         Option<AutomationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git:                Option<GitContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id:          Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_subject: Option<Principal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainEventLifecycleProps {
    #[serde(flatten)]
    pub source: MainEventSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainEventStartedProps {
    #[serde(flatten)]
    pub source:       MainEventSource,
    pub name:         String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_branch:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal:         Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainEventCompletedProps {
    #[serde(flatten)]
    pub source:               MainEventSource,
    pub status:               String,
    pub reason:               SuccessReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_usd_micros:     Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation:           Option<AutomationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing:              Option<BilledTokenCounts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainEventFailedProps {
    #[serde(flatten)]
    pub source:               MainEventSource,
    pub reason:               FailureReason,
    pub category:             FailureCategory,
    pub message:              String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation:           Option<AutomationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing:              Option<BilledTokenCounts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainEventCancelledProps {
    #[serde(flatten)]
    pub source:               MainEventSource,
    pub reason:               FailureReason,
    pub category:             FailureCategory,
    pub message:              String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation:           Option<AutomationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing:              Option<BilledTokenCounts>,
}

#[derive(Debug, Clone, Deserialize)]
struct MainEventRaw {
    id:         String,
    ts:         DateTime<Utc>,
    event:      String,
    #[serde(default = "default_properties")]
    properties: Value,
}

struct MainEventParts<'a> {
    id:         String,
    ts:         DateTime<Utc>,
    event:      &'a str,
    properties: &'a Value,
}

impl MainEventBody {
    pub fn event_name(&self) -> &str {
        match self {
            Self::RunCreated(_) => "fabro.run.created",
            Self::RunStarted(_) => "fabro.run.started",
            Self::RunRunning(_) => "fabro.run.running",
            Self::RunCompleted(_) => "fabro.run.completed",
            Self::RunFailed(_) => "fabro.run.failed",
            Self::RunCancelled(_) => "fabro.run.cancelled",
            Self::RunPaused(_) => "fabro.run.paused",
            Self::RunUnpaused(_) => "fabro.run.unpaused",
            Self::Unknown { name, .. } => name.as_str(),
        }
    }

    fn properties_value(&self) -> serde_json::Result<Value> {
        if let Self::Unknown { properties, .. } = self {
            return Ok(properties.clone());
        }

        match serde_json::to_value(self)? {
            Value::Object(mut map) => {
                Ok(map.remove("properties").unwrap_or_else(default_properties))
            }
            _ => Ok(default_properties()),
        }
    }
}

fn is_known_main_event_name(event: &str) -> bool {
    matches!(
        event,
        "fabro.run.created"
            | "fabro.run.started"
            | "fabro.run.running"
            | "fabro.run.completed"
            | "fabro.run.failed"
            | "fabro.run.cancelled"
            | "fabro.run.paused"
            | "fabro.run.unpaused"
    )
}

impl MainEvent {
    pub fn from_value(value: Value) -> serde_json::Result<Self> {
        let raw: MainEventRaw = serde_json::from_value(value)?;
        Self::from_parts(MainEventParts {
            id:         raw.id,
            ts:         raw.ts,
            event:      &raw.event,
            properties: &raw.properties,
        })
    }

    pub fn from_ref(value: &Value) -> serde_json::Result<Self> {
        let obj = value.as_object().ok_or_else(|| {
            <serde_json::Error as DeError>::custom("main event must be a JSON object")
        })?;
        let id = obj.get("id").and_then(Value::as_str).ok_or_else(|| {
            <serde_json::Error as DeError>::custom("missing or non-string field: id")
        })?;
        let ts = obj
            .get("ts")
            .ok_or_else(|| <serde_json::Error as DeError>::custom("missing field: ts"))
            .and_then(DateTime::<Utc>::deserialize)?;
        let event = obj.get("event").and_then(Value::as_str).ok_or_else(|| {
            <serde_json::Error as DeError>::custom("missing or non-string field: event")
        })?;
        let properties = obj
            .get("properties")
            .cloned()
            .unwrap_or_else(default_properties);
        Self::from_parts(MainEventParts {
            id: id.to_string(),
            ts,
            event,
            properties: &properties,
        })
    }

    fn from_parts(parts: MainEventParts<'_>) -> serde_json::Result<Self> {
        let body_payload = json!({
            "event": parts.event,
            "properties": parts.properties,
        });
        let body: MainEventBody = match serde_json::from_value(body_payload) {
            Ok(body) => body,
            Err(err) if is_known_main_event_name(parts.event) => return Err(err),
            Err(_) => MainEventBody::Unknown {
                name:       parts.event.to_string(),
                properties: parts.properties.clone(),
            },
        };
        Ok(Self {
            id: parts.id,
            ts: parts.ts,
            body,
        })
    }

    pub fn to_value(&self) -> serde_json::Result<Value> {
        let mut map = Map::new();
        map.insert("id".to_string(), Value::String(self.id.clone()));
        map.insert("ts".to_string(), serde_json::to_value(self.ts)?);
        map.insert(
            "event".to_string(),
            Value::String(self.body.event_name().to_string()),
        );
        map.insert("properties".to_string(), self.body.properties_value()?);
        Ok(Value::Object(map))
    }

    pub fn event_name(&self) -> &str {
        self.body.event_name()
    }

    pub fn properties(&self) -> serde_json::Result<Value> {
        self.body.properties_value()
    }
}

impl Serialize for MainEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_value()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MainEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(value).map_err(D::Error::custom)
    }
}

fn default_properties() -> Value {
    Value::Object(Map::new())
}
