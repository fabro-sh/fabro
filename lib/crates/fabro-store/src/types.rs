use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{Result, StoreError};
use fabro_types::{RunId, RunStatus, StatusReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeVisitRef<'a> {
    pub node_id: &'a str,
    pub visit: u32,
}

impl<'a> NodeVisitRef<'a> {
    #[must_use]
    pub fn into_owned(self) -> NodeVisit {
        NodeVisit {
            node_id: self.node_id.to_string(),
            visit: self.visit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeVisit {
    pub node_id: String,
    pub visit: u32,
}

impl NodeVisit {
    #[must_use]
    pub fn as_ref(&self) -> NodeVisitRef<'_> {
        NodeVisitRef {
            node_id: &self.node_id,
            visit: self.visit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRecord {
    pub run_id: RunId,
    pub created_at: DateTime<Utc>,
    pub db_prefix: String,
    pub run_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    #[serde(flatten)]
    pub catalog: CatalogRecord,
    pub workflow_name: Option<String>,
    pub workflow_slug: Option<String>,
    pub goal: Option<String>,
    pub labels: HashMap<String, String>,
    pub host_repo_path: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub status: Option<RunStatus>,
    pub status_reason: Option<StatusReason>,
    pub duration_ms: Option<u64>,
    pub total_cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EventPayload(serde_json::Value);

impl EventPayload {
    pub fn new(value: serde_json::Value, expected_run_id: &RunId) -> Result<Self> {
        let payload = Self(value);
        payload.validate(expected_run_id)?;
        Ok(payload)
    }

    pub(crate) fn validate(&self, expected_run_id: &RunId) -> Result<()> {
        self.validate_shape()?;
        if self.run_id() == expected_run_id.to_string() {
            return Ok(());
        }
        Err(StoreError::InvalidEvent(format!(
            "payload run_id {:?} does not match store run_id {expected_run_id:?}",
            self.run_id()
        )))
    }

    fn validate_shape(&self) -> Result<()> {
        let obj = self.0.as_object().ok_or_else(|| {
            StoreError::InvalidEvent("event payload must be a JSON object".into())
        })?;

        for field in ["id", "ts", "run_id", "event"] {
            match obj.get(field) {
                Some(serde_json::Value::String(_)) => {}
                _ => {
                    return Err(StoreError::InvalidEvent(format!(
                        "missing or non-string required field: {field}"
                    )));
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        self.0["run_id"]
            .as_str()
            .expect("EventPayload::run_id called before validation")
    }

    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EventPayload {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let payload = Self(serde_json::Value::deserialize(deserializer)?);
        payload.validate_shape().map_err(serde::de::Error::custom)?;
        Ok(payload)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: u32,
    pub payload: EventPayload,
}
