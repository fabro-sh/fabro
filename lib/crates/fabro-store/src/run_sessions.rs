use std::collections::BTreeMap;

use fabro_types::{
    EventBody, EventEnvelope, RunId, SessionId, SessionMessage, SessionRecord, SessionSummary,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedRunSession {
    pub record:          SessionRecord,
    pub runtime_context: Vec<SessionMessage>,
}

pub fn project_run_sessions(run_id: RunId, events: &[EventEnvelope]) -> Vec<SessionSummary> {
    let mut projection = RunSessionProjection::default();
    projection.apply(run_id, events);
    projection
        .sessions
        .values()
        .map(|session| SessionSummary::from(&session.record))
        .collect()
}

pub fn project_run_session(
    run_id: RunId,
    session_id: SessionId,
    events: &[EventEnvelope],
) -> Option<SessionRecord> {
    project_run_session_with_context(run_id, session_id, events).map(|session| session.record)
}

pub fn project_run_session_with_context(
    run_id: RunId,
    session_id: SessionId,
    events: &[EventEnvelope],
) -> Option<ProjectedRunSession> {
    let mut projection = RunSessionProjection::default();
    projection.apply(run_id, events);
    projection.sessions.remove(&session_id)
}

#[derive(Default)]
struct RunSessionProjection {
    sessions: BTreeMap<SessionId, ProjectedRunSession>,
}

impl RunSessionProjection {
    fn apply(&mut self, run_id: RunId, events: &[EventEnvelope]) {
        for envelope in events {
            let Some(session_id) = event_session_id(envelope) else {
                continue;
            };
            match &envelope.event.body {
                EventBody::RunSessionCreated(props) => {
                    let mut record = SessionRecord::new(session_id, run_id, envelope.event.ts);
                    record.title.clone_from(&props.title);
                    record.model.clone_from(&props.model);
                    let projected = ProjectedRunSession {
                        record,
                        runtime_context: Vec::new(),
                    };
                    self.sessions.insert(session_id, projected);
                }
                EventBody::RunSessionTurnStarted(_) => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.record.status = fabro_types::SessionStatus::Running;
                        session.record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionUserMessage(props) => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session
                            .runtime_context
                            .push(SessionMessage::user(props.text.clone(), envelope.event.ts));
                        session.record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionAssistantMessage(props) => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.runtime_context.push(SessionMessage::Assistant {
                            content:        props.text.clone(),
                            tool_calls:     Vec::new(),
                            provider_parts: Vec::new(),
                            usage:          props.usage.clone(),
                            response_id:    String::new(),
                            timestamp:      envelope.event.ts,
                        });
                        session.record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionTurnFailed(_) => {
                    self.finish_turn(session_id, true, envelope.event.ts);
                }
                EventBody::RunSessionTurnSucceeded(_) | EventBody::RunSessionTurnInterrupted(_) => {
                    self.finish_turn(session_id, false, envelope.event.ts);
                }
                _ => {}
            }
        }
    }

    fn finish_turn(
        &mut self,
        session_id: SessionId,
        failed: bool,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.record.status = if failed {
                fabro_types::SessionStatus::Failed
            } else {
                fabro_types::SessionStatus::Idle
            };
            session.record.updated_at = timestamp;
        }
    }
}

fn event_session_id(envelope: &EventEnvelope) -> Option<SessionId> {
    envelope
        .event
        .session_id
        .as_deref()
        .and_then(|id| id.parse().ok())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use fabro_types::run_event::{
        RunSessionAssistantMessageProps, RunSessionCreatedProps, RunSessionTurnStartedProps,
        RunSessionTurnSucceededProps, RunSessionUserMessageProps,
    };
    use fabro_types::{EventBody, EventEnvelope, RunEvent, SessionMessage, TurnId, fixtures};
    use serde_json::json;

    use super::{project_run_session, project_run_session_with_context};

    #[test]
    fn projection_rebuilds_runtime_context_from_run_events() {
        let session_id = fabro_types::SessionId::new();
        let turn_id = TurnId::new();
        let events = vec![
            event(
                1,
                session_id,
                EventBody::RunSessionCreated(RunSessionCreatedProps {
                    title: Some("Ask".to_string()),
                    model: Some("test-model".to_string()),
                }),
            ),
            event(
                2,
                session_id,
                EventBody::RunSessionTurnStarted(RunSessionTurnStartedProps {
                    turn_id,
                    input: "What happened?".to_string(),
                }),
            ),
            event(
                3,
                session_id,
                EventBody::RunSessionUserMessage(RunSessionUserMessageProps {
                    turn_id,
                    text: "What happened?".to_string(),
                }),
            ),
            event(
                4,
                session_id,
                EventBody::RunSessionAssistantMessage(RunSessionAssistantMessageProps {
                    turn_id,
                    text: "The run finished.".to_string(),
                    model: Some("test-model".to_string()),
                    usage: json!({ "output_tokens": 4 }),
                }),
            ),
            event(
                5,
                session_id,
                EventBody::RunSessionTurnSucceeded(RunSessionTurnSucceededProps {
                    turn_id,
                    output: Some("The run finished.".to_string()),
                }),
            ),
        ];

        let session = project_run_session_with_context(fixtures::RUN_1, session_id, &events)
            .expect("session should project from run events");

        assert_eq!(session.runtime_context.len(), 2);
        assert!(matches!(
            &session.runtime_context[0],
            SessionMessage::User { content, .. } if content == "What happened?"
        ));
        assert!(matches!(
            &session.runtime_context[1],
            SessionMessage::Assistant { content, usage, .. }
                if content == "The run finished." && usage == &json!({ "output_tokens": 4 })
        ));
    }

    #[test]
    fn public_session_record_projection_omits_runtime_context() {
        let session_id = fabro_types::SessionId::new();
        let turn_id = TurnId::new();
        let events = vec![
            event(
                1,
                session_id,
                EventBody::RunSessionCreated(RunSessionCreatedProps {
                    title: Some("Ask".to_string()),
                    model: Some("test-model".to_string()),
                }),
            ),
            event(
                2,
                session_id,
                EventBody::RunSessionUserMessage(RunSessionUserMessageProps {
                    turn_id,
                    text: "What happened?".to_string(),
                }),
            ),
        ];

        let session = project_run_session(fixtures::RUN_1, session_id, &events)
            .expect("session should project from run events");
        let value = serde_json::to_value(session).expect("session should serialize");

        assert!(value.get("runtime_context").is_none());
        assert!(value.get("working_dir").is_none());
        assert!(value.get("provider").is_none());
        assert!(value.get("permissions").is_none());
        assert!(value.get("deleted_at").is_none());
    }

    fn event(seq: u32, session_id: fabro_types::SessionId, body: EventBody) -> EventEnvelope {
        let event = RunEvent {
            id: format!("evt-{seq}"),
            ts: Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, seq).unwrap(),
            run_id: fixtures::RUN_1,
            node_id: None,
            node_label: None,
            stage_id: None,
            parallel_group_id: None,
            parallel_branch_id: None,
            session_id: Some(session_id.to_string()),
            parent_session_id: None,
            tool_call_id: None,
            actor: None,
            body,
        };

        EventEnvelope { seq, event }
    }
}
