use std::collections::HashMap;

use chrono::{DateTime, Utc};
use fabro_types::{BlockedReason, RunControlAction, RunEvent, RunId, RunStatus, StatusReason};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id:           RunId,
    pub workflow_name:    Option<String>,
    pub workflow_slug:    Option<String>,
    pub goal:             Option<String>,
    pub labels:           HashMap<String, String>,
    pub host_repo_path:   Option<String>,
    pub start_time:       Option<DateTime<Utc>>,
    pub status:           RunStatus,
    pub status_reason:    Option<StatusReason>,
    pub blocked_reason:   Option<BlockedReason>,
    pub pending_control:  Option<RunControlAction>,
    pub duration_ms:      Option<u64>,
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
        let obj = self
            .0
            .as_object()
            .ok_or_else(|| Error::InvalidEvent("event payload must be a JSON object".into()))?;

        for field in ["id", "ts", "run_id", "event"] {
            match obj.get(field) {
                Some(serde_json::Value::String(_)) => {}
                _ => {
                    return Err(Error::InvalidEvent(format!(
                        "missing or non-string required field: {field}"
                    )));
                }
            }
        }

        match obj.get("run_id") {
            Some(serde_json::Value::String(run_id)) if run_id == &expected_run_id.to_string() => {
                Ok(())
            }
            Some(serde_json::Value::String(run_id)) => Err(Error::InvalidEvent(format!(
                "payload run_id {run_id:?} does not match store run_id {expected_run_id:?}"
            ))),
            _ => Err(Error::InvalidEvent(
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
    type Error = Error;

    fn try_from(value: &EventPayload) -> Result<Self> {
        Self::from_ref(value.as_value())
            .map_err(|err| Error::InvalidEvent(format!("invalid stored event: {err}")))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq:     u32,
    #[serde(flatten)]
    pub payload: EventPayload,
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use fabro_types::run_event::RunCompletedProps;
    use fabro_types::{ActorRef, EventBody, ParallelBranchId, RunEvent, StageId, fixtures};

    use super::{EventEnvelope, EventPayload};

    #[test]
    fn wire_event_envelope_round_trips() {
        let event = RunEvent {
            id:                 "evt_1".to_string(),
            ts:                 Utc.with_ymd_and_hms(2026, 4, 9, 12, 0, 0).unwrap(),
            run_id:             fixtures::RUN_1,
            node_id:            Some("code".to_string()),
            node_label:         Some("Code".to_string()),
            stage_id:           Some(StageId::new("code", 1)),
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::RunCompleted(RunCompletedProps {
                duration_ms:          42,
                artifact_count:       0,
                status:               "success".to_string(),
                reason:               None,
                total_usd_micros:     None,
                final_git_commit_sha: None,
                final_patch:          None,
                billing:              None,
            }),
        };
        let payload = EventPayload::new(event.to_value().unwrap(), &fixtures::RUN_1).unwrap();
        let envelope = EventEnvelope { seq: 7, payload };

        let wire = serde_json::to_value(&envelope).unwrap();
        assert_eq!(wire["seq"], 7);
        assert_eq!(wire["id"], "evt_1");
        assert_eq!(wire["event"], "run.completed");
        assert!(wire.get("payload").is_none(), "wire shape must be flat");

        let parsed: EventEnvelope = serde_json::from_value(wire).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn wire_event_envelope_round_trips_with_all_envelope_fields() {
        let group = StageId::new("review", 2);
        let branch = ParallelBranchId::new(group.clone(), 3);
        let event = RunEvent {
            id:                 "evt_2".to_string(),
            ts:                 Utc.with_ymd_and_hms(2026, 4, 9, 13, 0, 0).unwrap(),
            run_id:             fixtures::RUN_1,
            node_id:            Some("review".to_string()),
            node_label:         Some("Review".to_string()),
            stage_id:           Some(StageId::new("review", 2)),
            parallel_group_id:  Some(group),
            parallel_branch_id: Some(branch),
            session_id:         Some("ses_42".to_string()),
            parent_session_id:  Some("ses_root".to_string()),
            tool_call_id:       Some("tool_call_xyz".to_string()),
            actor:              Some(ActorRef::agent(
                Some("ses_42".to_string()),
                Some("claude-sonnet".to_string()),
            )),
            body:               EventBody::RunCompleted(RunCompletedProps {
                duration_ms:          100,
                artifact_count:       1,
                status:               "success".to_string(),
                reason:               None,
                total_usd_micros:     None,
                final_git_commit_sha: None,
                final_patch:          None,
                billing:              None,
            }),
        };
        let payload = EventPayload::new(event.to_value().unwrap(), &fixtures::RUN_1).unwrap();
        let envelope = EventEnvelope { seq: 99, payload };

        let wire = serde_json::to_value(&envelope).unwrap();
        assert_eq!(wire["seq"], 99);
        assert_eq!(wire["id"], "evt_2");
        assert_eq!(wire["stage_id"], "review@2");
        assert_eq!(wire["parallel_group_id"], "review@2");
        assert_eq!(wire["parallel_branch_id"], "review@2:3");
        assert_eq!(wire["session_id"], "ses_42");
        assert_eq!(wire["parent_session_id"], "ses_root");
        assert_eq!(wire["tool_call_id"], "tool_call_xyz");
        assert_eq!(wire["actor"]["kind"], "agent");
        assert_eq!(wire["actor"]["id"], "ses_42");
        assert_eq!(wire["actor"]["display"], "claude-sonnet");
        assert_eq!(wire["event"], "run.completed");
        assert!(wire.get("payload").is_none(), "wire shape must be flat");

        let parsed: EventEnvelope = serde_json::from_value(wire).unwrap();
        assert_eq!(parsed, envelope);
    }
}
