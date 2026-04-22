use serde::{Deserialize, Serialize};

use crate::RunEvent;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq:   u32,
    #[serde(flatten)]
    pub event: RunEvent,
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::EventEnvelope;
    use crate::run_event::RunCompletedProps;
    use crate::{
        ActorRef, EventBody, ParallelBranchId, RunEvent, StageId, SuccessReason, fixtures,
    };

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
                reason:               SuccessReason::Completed,
                total_usd_micros:     None,
                final_git_commit_sha: None,
                final_patch:          None,
                billing:              None,
            }),
        };
        let envelope = EventEnvelope { seq: 7, event };

        let wire = serde_json::to_value(&envelope).unwrap();
        assert_eq!(wire["seq"], 7);
        assert_eq!(wire["id"], "evt_1");
        assert_eq!(wire["event"], "run.completed");

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
                reason:               SuccessReason::Completed,
                total_usd_micros:     None,
                final_git_commit_sha: None,
                final_patch:          None,
                billing:              None,
            }),
        };
        let envelope = EventEnvelope { seq: 99, event };

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

        let parsed: EventEnvelope = serde_json::from_value(wire).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn preserves_unknown_event_names_and_properties() {
        let wire = serde_json::json!({
            "seq": 7,
            "id": "evt_unknown",
            "ts": "2026-04-20T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "vendor.custom.event",
            "properties": {
                "answer": 42,
                "nested": { "ok": true }
            }
        });

        let parsed: EventEnvelope = serde_json::from_value(wire.clone()).unwrap();
        let serialized = serde_json::to_value(&parsed).unwrap();

        assert_eq!(serialized["seq"], wire["seq"]);
        assert_eq!(serialized["id"], wire["id"]);
        assert_eq!(serialized["run_id"], wire["run_id"]);
        assert_eq!(serialized["event"], wire["event"]);
        assert_eq!(serialized["properties"], wire["properties"]);
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(serialized["ts"].as_str().unwrap()).unwrap(),
            chrono::DateTime::parse_from_rfc3339(wire["ts"].as_str().unwrap()).unwrap(),
        );
    }
}
