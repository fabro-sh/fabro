use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Result, StoreError};
use fabro_types::{RunEvent, RunId, RunStatus, StatusReason};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: RunId,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventPayload(serde_json::Value);

impl EventPayload {
    pub fn new(value: serde_json::Value, expected_run_id: &RunId) -> Result<Self> {
        let payload = Self(value);
        payload.validate(expected_run_id)?;
        Ok(payload)
    }

    pub(crate) fn validate(&self, expected_run_id: &RunId) -> Result<()> {
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

        match obj.get("run_id") {
            Some(serde_json::Value::String(run_id)) if run_id == &expected_run_id.to_string() => {
                Ok(())
            }
            Some(serde_json::Value::String(run_id)) => Err(StoreError::InvalidEvent(format!(
                "payload run_id {run_id:?} does not match store run_id {expected_run_id:?}"
            ))),
            _ => Err(StoreError::InvalidEvent(
                "missing or non-string required field: run_id".into(),
            )),
        }
    }

    pub fn into_inner(self) -> serde_json::Value {
        self.0
    }

    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }
}

impl TryFrom<&EventPayload> for RunEvent {
    type Error = StoreError;

    fn try_from(value: &EventPayload) -> Result<Self> {
        RunEvent::from_value(value.as_value().clone())
            .map_err(|err| StoreError::InvalidEvent(format!("invalid stored event: {err}")))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: u32,
    pub payload: EventPayload,
}
