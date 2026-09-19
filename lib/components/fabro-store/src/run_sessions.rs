use std::collections::BTreeMap;

use fabro_types::{
    RunSessionMetadata, SessionEvent, SessionEventBody, SessionId, SessionStatus, SessionSummary,
    SessionTurn,
};

/// Ask Fabro session metadata at the session's event position it was read
/// at.
///
/// The transcript is not projected from the events: pebble's session record
/// holds the durable history, and the events stream it live.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedRunSession {
    pub record:   RunSessionMetadata,
    pub last_seq: u32,
}

/// Every session the events describe, in session id order.
pub fn project_run_sessions(events: &[SessionEvent]) -> Vec<SessionSummary> {
    let mut projection = RunSessionProjection::default();
    projection.apply(events);
    projection
        .sessions
        .values()
        .map(|session| SessionSummary::from(&session.record))
        .collect()
}

/// The session `session_id` as its events describe it, if the events hold
/// its creation.
pub fn project_run_session(
    session_id: SessionId,
    events: &[SessionEvent],
) -> Option<ProjectedRunSession> {
    let mut projection = RunSessionProjection::default();
    projection.apply(events.iter().filter(|event| event.session_id == session_id));
    projection.sessions.remove(&session_id)
}

#[derive(Default)]
struct RunSessionProjection {
    sessions: BTreeMap<SessionId, ProjectedRunSession>,
}

impl RunSessionProjection {
    fn apply<'a>(&mut self, events: impl IntoIterator<Item = &'a SessionEvent>) {
        for event in events {
            let session_id = event.session_id;
            match &event.body {
                SessionEventBody::Created(props) => {
                    let mut record = RunSessionMetadata::new(session_id, event.run_id, event.ts);
                    record.title.clone_from(&props.title);
                    record.model.clone_from(&props.model);
                    record.provider.clone_from(&props.provider);
                    self.sessions.insert(session_id, ProjectedRunSession {
                        record,
                        last_seq: event.seq,
                    });
                }
                SessionEventBody::TurnStarted(props) => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.last_seq = event.seq;
                        session.record.status = SessionStatus::Running;
                        session.record.active_turn = Some(SessionTurn {
                            id:         props.turn_id,
                            started_at: event.ts,
                            input:      props.input.clone(),
                        });
                        session.record.updated_at = event.ts;
                    }
                }
                SessionEventBody::UserMessage(_)
                | SessionEventBody::AssistantMessage(_)
                | SessionEventBody::AssistantDelta(_)
                | SessionEventBody::ToolCallStarted(_)
                | SessionEventBody::ToolCallCompleted(_) => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.last_seq = event.seq;
                        session.record.updated_at = event.ts;
                    }
                }
                SessionEventBody::TurnFailed(_) => {
                    self.finish_turn(session_id, true, event.ts, event.seq);
                }
                SessionEventBody::TurnSucceeded(_) | SessionEventBody::TurnInterrupted(_) => {
                    self.finish_turn(session_id, false, event.ts, event.seq);
                }
            }
        }
    }

    fn finish_turn(
        &mut self,
        session_id: SessionId,
        failed: bool,
        timestamp: chrono::DateTime<chrono::Utc>,
        seq: u32,
    ) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.last_seq = seq;
            session.record.status = if failed {
                SessionStatus::Failed
            } else {
                SessionStatus::Idle
            };
            session.record.active_turn = None;
            session.record.updated_at = timestamp;
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use fabro_types::session_event::{
        SessionAssistantMessageProps, SessionCreatedProps, SessionTurnFailedCode,
        SessionTurnFailedProps, SessionTurnStartedProps, SessionTurnSucceededProps,
        SessionUserMessageProps,
    };
    use fabro_types::{SessionEvent, SessionEventBody, TurnId, fixtures};
    use serde_json::json;

    use super::{project_run_session, project_run_sessions};

    #[test]
    fn projection_tracks_turn_lifecycle_and_last_seq() {
        let session_id = fabro_types::SessionId::new();
        let turn_id = TurnId::new();
        let events = vec![
            event(
                1,
                session_id,
                SessionEventBody::Created(SessionCreatedProps {
                    title:    Some("Ask".to_string()),
                    model:    Some("test-model".to_string()),
                    provider: None,
                }),
            ),
            event(
                2,
                session_id,
                SessionEventBody::TurnStarted(SessionTurnStartedProps {
                    turn_id,
                    input: "What happened?".to_string(),
                }),
            ),
            event(
                3,
                session_id,
                SessionEventBody::UserMessage(SessionUserMessageProps {
                    turn_id,
                    text: "What happened?".to_string(),
                }),
            ),
        ];

        let running = project_run_session(session_id, &events)
            .expect("session should project from its events");
        assert_eq!(running.record.status, fabro_types::SessionStatus::Running);
        assert_eq!(
            running.record.active_turn.as_ref().map(|turn| turn.id),
            Some(turn_id)
        );
        assert_eq!(running.last_seq, 3);

        let mut events = events;
        events.push(event(
            4,
            session_id,
            SessionEventBody::AssistantMessage(SessionAssistantMessageProps {
                turn_id,
                text: "The run finished.".to_string(),
                model: Some("test-model".to_string()),
                usage: json!({ "output_tokens": 4 }),
            }),
        ));
        events.push(event(
            5,
            session_id,
            SessionEventBody::TurnSucceeded(SessionTurnSucceededProps {
                turn_id,
                output: Some("The run finished.".to_string()),
            }),
        ));

        let idle = project_run_session(session_id, &events).unwrap();
        assert_eq!(idle.record.status, fabro_types::SessionStatus::Idle);
        assert!(idle.record.active_turn.is_none());
        assert_eq!(idle.record.model.as_deref(), Some("test-model"));
        assert_eq!(idle.last_seq, 5);
    }

    #[test]
    fn a_failed_turn_marks_the_session_failed() {
        let session_id = fabro_types::SessionId::new();
        let turn_id = TurnId::new();
        let events = vec![
            event(
                1,
                session_id,
                SessionEventBody::Created(SessionCreatedProps {
                    title:    None,
                    model:    None,
                    provider: None,
                }),
            ),
            event(
                2,
                session_id,
                SessionEventBody::TurnStarted(SessionTurnStartedProps {
                    turn_id,
                    input: "hi".to_string(),
                }),
            ),
            event(
                3,
                session_id,
                SessionEventBody::TurnFailed(SessionTurnFailedProps {
                    turn_id,
                    error: "boom".to_string(),
                    output: None,
                    code: SessionTurnFailedCode::AgentError,
                    retryable: false,
                }),
            ),
        ];

        let summaries = project_run_sessions(&events);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].status, fabro_types::SessionStatus::Failed);
        assert!(summaries[0].active_turn.is_none());
    }

    #[test]
    fn a_session_projects_only_from_its_own_events() {
        let first = fabro_types::SessionId::new();
        let second = fabro_types::SessionId::new();
        let events = vec![
            event(
                1,
                first,
                SessionEventBody::Created(SessionCreatedProps {
                    title:    Some("First".to_string()),
                    model:    None,
                    provider: None,
                }),
            ),
            event(
                1,
                second,
                SessionEventBody::Created(SessionCreatedProps {
                    title:    Some("Second".to_string()),
                    model:    None,
                    provider: None,
                }),
            ),
            event(
                2,
                second,
                SessionEventBody::TurnStarted(SessionTurnStartedProps {
                    turn_id: TurnId::new(),
                    input:   "hi".to_string(),
                }),
            ),
        ];

        let projected = project_run_session(first, &events).unwrap();
        assert_eq!(projected.record.title.as_deref(), Some("First"));
        assert_eq!(projected.record.status, fabro_types::SessionStatus::Idle);
        assert_eq!(projected.last_seq, 1);
        assert_eq!(project_run_sessions(&events).len(), 2);
        assert!(project_run_session(fabro_types::SessionId::new(), &events).is_none());
    }

    fn event(seq: u32, session_id: fabro_types::SessionId, body: SessionEventBody) -> SessionEvent {
        SessionEvent {
            seq,
            session_id,
            run_id: fixtures::RUN_1,
            ts: Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, seq).unwrap(),
            body,
        }
    }
}
