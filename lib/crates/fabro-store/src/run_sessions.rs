use std::collections::BTreeMap;

use fabro_types::run_event::{RunSessionToolCallCompletedProps, RunSessionToolCallStartedProps};
use fabro_types::{
    EventBody, EventEnvelope, RunId, SessionId, SessionMessage, SessionRecord, SessionStatus,
    SessionSummary, SessionTurn,
};
use serde_json::json;

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedRunSession {
    pub record:          SessionRecord,
    pub runtime_context: Vec<SessionMessage>,
    pub last_seq:        u32,
}

pub fn project_run_sessions(run_id: RunId, events: &[EventEnvelope]) -> Vec<SessionSummary> {
    let mut projection = RunSessionProjection::metadata_only();
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
    let mut projection = RunSessionProjection::with_context_for(session_id);
    projection.apply(run_id, events);
    projection.sessions.remove(&session_id)
}

struct RunSessionProjection {
    sessions: BTreeMap<SessionId, ProjectedRunSession>,
    context:  RuntimeContextProjection,
}

enum RuntimeContextProjection {
    None,
    Session(SessionId),
}

impl RunSessionProjection {
    fn metadata_only() -> Self {
        Self {
            sessions: BTreeMap::new(),
            context:  RuntimeContextProjection::None,
        }
    }

    fn with_context_for(session_id: SessionId) -> Self {
        Self {
            sessions: BTreeMap::new(),
            context:  RuntimeContextProjection::Session(session_id),
        }
    }

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
                        last_seq: envelope.seq,
                    };
                    self.sessions.insert(session_id, projected);
                }
                EventBody::RunSessionTurnStarted(props) => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.last_seq = envelope.seq;
                        session.record.status = SessionStatus::Running;
                        session.record.active_turn = Some(SessionTurn {
                            id:         props.turn_id,
                            started_at: envelope.event.ts,
                            input:      props.input.clone(),
                        });
                        session.record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionUserMessage(props) => {
                    let project_context = self.should_project_context(session_id);
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.last_seq = envelope.seq;
                        if project_context {
                            session
                                .runtime_context
                                .push(SessionMessage::user(props.text.clone(), envelope.event.ts));
                        }
                        session.record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionAssistantMessage(props) => {
                    let project_context = self.should_project_context(session_id);
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.last_seq = envelope.seq;
                        if project_context {
                            session.runtime_context.push(SessionMessage::Assistant {
                                content:        props.text.clone(),
                                tool_calls:     Vec::new(),
                                provider_parts: Vec::new(),
                                usage:          props.usage.clone(),
                                response_id:    String::new(),
                                timestamp:      envelope.event.ts,
                            });
                        }
                        session.record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionAssistantDelta(_) => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.last_seq = envelope.seq;
                        session.record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionToolCallStarted(props) => {
                    let project_context = self.should_project_context(session_id);
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.last_seq = envelope.seq;
                        if project_context {
                            append_tool_call(session, props);
                        }
                        session.record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionToolCallCompleted(props) => {
                    let project_context = self.should_project_context(session_id);
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.last_seq = envelope.seq;
                        if project_context {
                            append_tool_result(session, props, envelope.event.ts);
                        }
                        session.record.updated_at = envelope.event.ts;
                    }
                }
                EventBody::RunSessionTurnFailed(_) => {
                    self.finish_turn(session_id, true, envelope.event.ts, envelope.seq);
                }
                EventBody::RunSessionTurnSucceeded(_) | EventBody::RunSessionTurnInterrupted(_) => {
                    self.finish_turn(session_id, false, envelope.event.ts, envelope.seq);
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

    fn should_project_context(&self, session_id: SessionId) -> bool {
        match self.context {
            RuntimeContextProjection::None => false,
            RuntimeContextProjection::Session(target) => target == session_id,
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

fn append_tool_call(session: &mut ProjectedRunSession, props: &RunSessionToolCallStartedProps) {
    if let Some(SessionMessage::Assistant { tool_calls, .. }) = session
        .runtime_context
        .iter_mut()
        .rev()
        .find(|message| matches!(message, SessionMessage::Assistant { .. }))
    {
        tool_calls.push(json!({
            "id": props.tool_call_id.clone(),
            "name": props.tool_name.clone(),
            "arguments": props.arguments.clone(),
        }));
    }
}

fn append_tool_result(
    session: &mut ProjectedRunSession,
    props: &RunSessionToolCallCompletedProps,
    timestamp: chrono::DateTime<chrono::Utc>,
) {
    let result = json!({
        "tool_call_id": props.tool_call_id.clone(),
        "content": props.output.clone(),
        "is_error": props.is_error,
    });
    if let Some(SessionMessage::ToolResults { results, .. }) = session.runtime_context.last_mut() {
        results.push(result);
    } else {
        session.runtime_context.push(SessionMessage::ToolResults {
            results: vec![result],
            timestamp,
        });
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use fabro_types::run_event::{
        RunSessionAssistantMessageProps, RunSessionCreatedProps, RunSessionToolCallCompletedProps,
        RunSessionToolCallStartedProps, RunSessionTurnFailedCode, RunSessionTurnFailedProps,
        RunSessionTurnStartedProps, RunSessionTurnSucceededProps, RunSessionUserMessageProps,
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
    fn projection_rebuilds_tool_calls_and_results() {
        let session_id = fabro_types::SessionId::new();
        let turn_id = TurnId::new();
        let events = vec![
            event(
                1,
                session_id,
                EventBody::RunSessionCreated(RunSessionCreatedProps {
                    title: None,
                    model: None,
                }),
            ),
            event(
                2,
                session_id,
                EventBody::RunSessionAssistantMessage(RunSessionAssistantMessageProps {
                    turn_id,
                    text: String::new(),
                    model: Some("test-model".to_string()),
                    usage: json!({}),
                }),
            ),
            event(
                3,
                session_id,
                EventBody::RunSessionToolCallStarted(RunSessionToolCallStartedProps {
                    turn_id,
                    tool_name: "read_file".to_string(),
                    tool_call_id: "call_1".to_string(),
                    arguments: json!({ "path": "README.md" }),
                }),
            ),
            event(
                4,
                session_id,
                EventBody::RunSessionToolCallCompleted(RunSessionToolCallCompletedProps {
                    turn_id,
                    tool_name: "read_file".to_string(),
                    tool_call_id: "call_1".to_string(),
                    output: json!("contents"),
                    is_error: false,
                }),
            ),
        ];

        let session = project_run_session_with_context(fixtures::RUN_1, session_id, &events)
            .expect("session should project from run events");

        assert!(matches!(
            &session.runtime_context[0],
            SessionMessage::Assistant { tool_calls, .. }
                if tool_calls == &vec![json!({
                    "id": "call_1",
                    "name": "read_file",
                    "arguments": { "path": "README.md" },
                })]
        ));
        assert!(matches!(
            &session.runtime_context[1],
            SessionMessage::ToolResults { results, .. }
                if results == &vec![json!({
                    "tool_call_id": "call_1",
                    "content": "contents",
                    "is_error": false,
                })]
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
        assert_eq!(session.updated_at, events[1].event.ts);
        let value = serde_json::to_value(session).expect("session should serialize");

        assert!(value.get("runtime_context").is_none());
        assert!(value.get("working_dir").is_none());
        assert!(value.get("provider").is_none());
        assert!(value.get("permissions").is_none());
        assert!(value.get("deleted_at").is_none());
    }

    #[test]
    fn projection_tracks_active_turn_and_last_matching_sequence() {
        let session_id = fabro_types::SessionId::new();
        let other_session_id = fabro_types::SessionId::new();
        let turn_id = TurnId::new();
        let events = vec![
            event(
                1,
                session_id,
                EventBody::RunSessionCreated(RunSessionCreatedProps {
                    title: None,
                    model: None,
                }),
            ),
            event(
                2,
                session_id,
                EventBody::RunSessionTurnStarted(RunSessionTurnStartedProps {
                    turn_id,
                    input: "Summarize".to_string(),
                }),
            ),
            event(
                3,
                other_session_id,
                EventBody::RunSessionCreated(RunSessionCreatedProps {
                    title: Some("Other".to_string()),
                    model: None,
                }),
            ),
        ];

        let session = project_run_session_with_context(fixtures::RUN_1, session_id, &events)
            .expect("session should project from run events");

        assert_eq!(session.last_seq, 2);
        let active = session.record.active_turn.expect("turn should be active");
        assert_eq!(active.id, turn_id);
        assert_eq!(active.started_at, events[1].event.ts);
        assert_eq!(active.input, "Summarize");
    }

    #[test]
    fn projection_clears_active_turn_when_turn_finishes() {
        let session_id = fabro_types::SessionId::new();
        let turn_id = TurnId::new();

        for body in [
            EventBody::RunSessionTurnSucceeded(RunSessionTurnSucceededProps {
                turn_id,
                output: None,
            }),
            EventBody::RunSessionTurnFailed(RunSessionTurnFailedProps {
                turn_id,
                error: "no sandbox".to_string(),
                output: None,
                code: RunSessionTurnFailedCode::default(),
                retryable: false,
            }),
        ] {
            let events = vec![
                event(
                    1,
                    session_id,
                    EventBody::RunSessionCreated(RunSessionCreatedProps {
                        title: None,
                        model: None,
                    }),
                ),
                event(
                    2,
                    session_id,
                    EventBody::RunSessionTurnStarted(RunSessionTurnStartedProps {
                        turn_id,
                        input: "Summarize".to_string(),
                    }),
                ),
                event(3, session_id, body),
            ];

            let session = project_run_session_with_context(fixtures::RUN_1, session_id, &events)
                .expect("session should project from run events");

            assert_eq!(session.last_seq, 3);
            assert_eq!(session.record.active_turn, None);
        }
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
