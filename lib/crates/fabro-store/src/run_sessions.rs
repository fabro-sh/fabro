use std::collections::BTreeMap;

use fabro_types::{
    EventBody, EventEnvelope, PermissionLevel, RunId, SessionId, SessionMessage, SessionRecord,
    SessionSummary, TurnId, TurnRecord, TurnStatus,
};

pub fn project_run_sessions(run_id: RunId, events: &[EventEnvelope]) -> Vec<SessionSummary> {
    let mut projection = RunSessionProjection::default();
    projection.apply(run_id, events);
    projection
        .sessions
        .values()
        .map(SessionSummary::from)
        .collect()
}

pub fn project_run_session(
    run_id: RunId,
    session_id: SessionId,
    events: &[EventEnvelope],
) -> Option<SessionRecord> {
    let mut projection = RunSessionProjection::default();
    projection.apply(run_id, events);
    projection.sessions.remove(&session_id)
}

pub fn project_run_session_turns(
    run_id: RunId,
    session_id: SessionId,
    events: &[EventEnvelope],
) -> Vec<TurnRecord> {
    let mut projection = RunSessionProjection::default();
    projection.apply(run_id, events);
    projection
        .turns
        .remove(&session_id)
        .map(|turns| turns.into_values().collect())
        .unwrap_or_default()
}

pub fn project_run_session_turn(
    run_id: RunId,
    session_id: SessionId,
    turn_id: TurnId,
    events: &[EventEnvelope],
) -> Option<TurnRecord> {
    let turns = project_run_session_turns(run_id, session_id, events);
    turns.into_iter().find(|turn| turn.id == turn_id)
}

#[derive(Default)]
struct RunSessionProjection {
    sessions: BTreeMap<SessionId, SessionRecord>,
    turns:    BTreeMap<SessionId, BTreeMap<TurnId, TurnRecord>>,
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
                    record.permissions = PermissionLevel::ReadOnly;
                    self.sessions.insert(session_id, record);
                }
                EventBody::RunSessionTitleUpdated(props) => {
                    if let Some(record) = self.sessions.get_mut(&session_id) {
                        record.title.clone_from(&props.title);
                        record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionTurnStarted(props) => {
                    if let Some(record) = self.sessions.get_mut(&session_id) {
                        record.status = fabro_types::SessionStatus::Running;
                        record.updated_at = envelope.event.ts;
                    }
                    self.turns
                        .entry(session_id)
                        .or_default()
                        .insert(props.turn_id, TurnRecord {
                            id: props.turn_id,
                            session_id,
                            run_id,
                            input: props.input.clone(),
                            status: TurnStatus::Running,
                            output: None,
                            error: None,
                            created_at: envelope.event.ts,
                            updated_at: envelope.event.ts,
                            completed_at: None,
                        });
                }
                EventBody::RunSessionUserMessage(props) => {
                    if let Some(record) = self.sessions.get_mut(&session_id) {
                        record
                            .runtime_context
                            .push(SessionMessage::user(props.text.clone(), envelope.event.ts));
                        record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionAssistantMessage(props) => {
                    if let Some(record) = self.sessions.get_mut(&session_id) {
                        record.runtime_context.push(SessionMessage::Assistant {
                            content:        props.text.clone(),
                            tool_calls:     Vec::new(),
                            provider_parts: Vec::new(),
                            usage:          props.usage.clone(),
                            response_id:    String::new(),
                            timestamp:      envelope.event.ts,
                        });
                        record.updated_at = envelope.event.ts;
                    }
                    if let Some(turn) = self
                        .turns
                        .get_mut(&session_id)
                        .and_then(|turns| turns.get_mut(&props.turn_id))
                    {
                        turn.output = Some(props.text.clone());
                        turn.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionTurnSucceeded(props) => {
                    self.finish_turn(
                        session_id,
                        props.turn_id,
                        TurnStatus::Succeeded,
                        props.output.clone(),
                        None,
                        envelope.event.ts,
                    );
                }
                EventBody::RunSessionTurnFailed(props) => {
                    self.finish_turn(
                        session_id,
                        props.turn_id,
                        TurnStatus::Failed,
                        props.output.clone(),
                        Some(props.error.clone()),
                        envelope.event.ts,
                    );
                }
                EventBody::RunSessionTurnInterrupted(props) => {
                    self.finish_turn(
                        session_id,
                        props.turn_id,
                        TurnStatus::Interrupted,
                        None,
                        props.error.clone(),
                        envelope.event.ts,
                    );
                }
                _ => {}
            }
        }
    }

    fn finish_turn(
        &mut self,
        session_id: SessionId,
        turn_id: TurnId,
        status: TurnStatus,
        output: Option<String>,
        error: Option<String>,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) {
        if let Some(record) = self.sessions.get_mut(&session_id) {
            record.status = if status == TurnStatus::Failed {
                fabro_types::SessionStatus::Failed
            } else {
                fabro_types::SessionStatus::Idle
            };
            record.updated_at = timestamp;
        }
        if let Some(turn) = self
            .turns
            .get_mut(&session_id)
            .and_then(|turns| turns.get_mut(&turn_id))
        {
            turn.status = status;
            if output.is_some() {
                turn.output = output;
            }
            turn.error = error;
            turn.updated_at = timestamp;
            turn.completed_at = Some(timestamp);
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
    use fabro_types::{
        EventBody, EventEnvelope, PermissionLevel, RunEvent, SessionMessage, TurnId, fixtures,
    };
    use serde_json::json;

    use super::project_run_session;

    #[test]
    fn projection_rebuilds_runtime_context_from_run_events() {
        let session_id = fabro_types::SessionId::new();
        let turn_id = TurnId::new();
        let events = vec![
            event(
                1,
                session_id,
                EventBody::RunSessionCreated(RunSessionCreatedProps {
                    title:       Some("Ask".to_string()),
                    model:       Some("test-model".to_string()),
                    permissions: PermissionLevel::ReadOnly,
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

        let session = project_run_session(fixtures::RUN_1, session_id, &events)
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
