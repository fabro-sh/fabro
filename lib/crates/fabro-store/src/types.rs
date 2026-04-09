use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Result, StoreError};
use fabro_types::{RunControlAction, RunEvent, RunId, RunStatus, StatusReason};

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
    pub pending_control: Option<RunControlAction>,
    pub duration_ms: Option<u64>,
    pub total_usd_micros: Option<i64>,
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
        Self::from_ref(value.as_value())
            .map_err(|err| StoreError::InvalidEvent(format!("invalid stored event: {err}")))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: u32,
    pub payload: EventPayload,
}

impl EventEnvelope {
    pub fn from_wire_value(value: serde_json::Value) -> Result<Self> {
        let serde_json::Value::Object(mut obj) = value else {
            return Err(StoreError::InvalidEvent(
                "wire EventEnvelope must be a JSON object".into(),
            ));
        };
        let seq = obj
            .remove("seq")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                StoreError::InvalidEvent("wire EventEnvelope missing valid seq".into())
            })?;
        let run_id = obj
            .get("run_id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| StoreError::InvalidEvent("wire EventEnvelope missing run_id".into()))?
            .parse()
            .map_err(|err| StoreError::InvalidEvent(format!("invalid wire run_id: {err}")))?;
        let payload = EventPayload::new(serde_json::Value::Object(obj), &run_id)?;
        Ok(Self { seq, payload })
    }

    pub fn to_wire_value(&self) -> Result<serde_json::Value> {
        let mut value = self.payload.as_value().clone();
        let map = value.as_object_mut().ok_or_else(|| {
            StoreError::InvalidEvent("stored event payload must be a JSON object".into())
        })?;
        map.insert(
            "seq".to_string(),
            serde_json::Value::Number(u64::from(self.seq).into()),
        );
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use fabro_types::{EventBody, RunEvent, fixtures, run_event::RunCompletedProps};

    use super::{EventEnvelope, EventPayload};

    #[test]
    fn wire_event_envelope_round_trips() {
        let event = RunEvent {
            id: "evt_1".to_string(),
            ts: Utc.with_ymd_and_hms(2026, 4, 9, 12, 0, 0).unwrap(),
            run_id: fixtures::RUN_1,
            node_id: Some("code".to_string()),
            node_label: Some("Code".to_string()),
            stage_id: Some("code@1".to_string()),
            parallel_group_id: None,
            parallel_branch_id: None,
            session_id: None,
            parent_session_id: None,
            tool_call_id: None,
            actor: None,
            body: EventBody::RunCompleted(RunCompletedProps {
                duration_ms: 42,
                artifact_count: 0,
                status: "success".to_string(),
                reason: None,
                total_usd_micros: None,
                final_git_commit_sha: None,
                final_patch: None,
                billing: None,
            }),
        };
        let payload = EventPayload::new(event.to_value().unwrap(), &fixtures::RUN_1).unwrap();
        let envelope = EventEnvelope { seq: 7, payload };

        let wire = envelope.to_wire_value().unwrap();
        let parsed = EventEnvelope::from_wire_value(wire).unwrap();

        assert_eq!(parsed, envelope);
    }
}
