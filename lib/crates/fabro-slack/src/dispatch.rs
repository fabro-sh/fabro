use crate::interaction;
use crate::payload::SlackAnswerSubmission;
use crate::socket::{SocketEnvelope, SocketEventKind, classify_envelope};
use crate::threads::{self, ThreadRegistry};

#[derive(Debug)]
pub enum DispatchAction {
    Connected,
    SubmitAnswer(Box<SlackAnswerSubmission>),
    Reconnect,
    Ignored,
}

pub fn dispatch(envelope: &SocketEnvelope, thread_registry: &ThreadRegistry) -> DispatchAction {
    match classify_envelope(envelope) {
        SocketEventKind::Hello => DispatchAction::Connected,
        SocketEventKind::Interactive => {
            let Some(ref payload) = envelope.payload else {
                return DispatchAction::Ignored;
            };
            match interaction::parse_interaction(payload) {
                Some(submission) => DispatchAction::SubmitAnswer(Box::new(submission)),
                None => DispatchAction::Ignored,
            }
        }
        SocketEventKind::EventsApi => {
            let Some(ref payload) = envelope.payload else {
                return DispatchAction::Ignored;
            };
            let Some((thread_ts, text)) = threads::parse_thread_reply(payload) else {
                return DispatchAction::Ignored;
            };
            let Some(question_ref) = thread_registry.resolve(&thread_ts) else {
                return DispatchAction::Ignored;
            };
            let Some(actor) = event_actor(payload) else {
                return DispatchAction::Ignored;
            };
            DispatchAction::SubmitAnswer(Box::new(SlackAnswerSubmission {
                run_id: question_ref.run_id,
                qid: question_ref.qid,
                answer: fabro_interview::Answer::text(text),
                actor,
            }))
        }
        SocketEventKind::Disconnect => DispatchAction::Reconnect,
        SocketEventKind::Unknown => DispatchAction::Ignored,
    }
}

fn event_actor(payload: &serde_json::Value) -> Option<fabro_types::Principal> {
    let event = &payload["event"];
    let team_id = payload["team_id"]
        .as_str()
        .or_else(|| event["team"].as_str())?
        .to_string();
    let user_id = event["user"].as_str()?.to_string();
    let user_name = event["user_name"].as_str().map(str::to_string);
    Some(fabro_types::Principal::Slack {
        team_id,
        user_id,
        user_name,
    })
}

#[cfg(test)]
mod tests {
    use fabro_interview::AnswerValue;

    use super::*;

    #[test]
    fn hello_produces_connected() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "hello".to_string(),
            envelope_id:   None,
            payload:       None,
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Connected));
    }

    #[test]
    fn interactive_button_produces_submit_answer() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "interactive".to_string(),
            envelope_id:   Some("env-1".to_string()),
            payload:       Some(serde_json::json!({
                "type": "block_actions",
                "team": { "id": "T123" },
                "user": { "id": "U123", "name": "ada" },
                "actions": [{
                    "action_id": "interview.answer",
                    "type": "button",
                    "value": "{\"kind\":\"yes\",\"run_id\":\"run-1\",\"qid\":\"q-1\"}"
                }]
            })),
        };
        let action = dispatch(&envelope, &registry);
        match action {
            DispatchAction::SubmitAnswer(submission) => {
                let submission = *submission;
                assert_eq!(submission.run_id, "run-1");
                assert_eq!(submission.qid, "q-1");
                assert_eq!(submission.answer.value, AnswerValue::Yes);
            }
            other => panic!("expected SubmitAnswer, got {other:?}"),
        }
    }

    #[test]
    fn interactive_with_unparseable_payload_produces_ignored() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "interactive".to_string(),
            envelope_id:   Some("env-2".to_string()),
            payload:       Some(serde_json::json!({
                "type": "view_submission"
            })),
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn interactive_with_no_payload_produces_ignored() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "interactive".to_string(),
            envelope_id:   Some("env-3".to_string()),
            payload:       None,
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn disconnect_produces_reconnect() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "disconnect".to_string(),
            envelope_id:   None,
            payload:       None,
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Reconnect));
    }

    #[test]
    fn events_api_non_thread_produces_ignored() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "events_api".to_string(),
            envelope_id:   Some("env-4".to_string()),
            payload:       Some(serde_json::json!({
                "event": { "type": "app_mention", "text": "hello" }
            })),
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn events_api_thread_reply_to_registered_question() {
        let registry = ThreadRegistry::new();
        registry.register("1234.5678", "run-10", "q-10");
        let envelope = SocketEnvelope {
            envelope_type: "events_api".to_string(),
            envelope_id:   Some("env-5".to_string()),
            payload:       Some(serde_json::json!({
                "team_id": "T123",
                "event": {
                    "type": "message",
                    "text": "https://github.com/org/repo",
                    "thread_ts": "1234.5678",
                    "user": "U123",
                    "user_name": "ada"
                }
            })),
        };
        let action = dispatch(&envelope, &registry);
        match action {
            DispatchAction::SubmitAnswer(submission) => {
                let submission = *submission;
                assert_eq!(submission.run_id, "run-10");
                assert_eq!(submission.qid, "q-10");
                assert_eq!(
                    submission.answer.value,
                    AnswerValue::Text("https://github.com/org/repo".to_string())
                );
            }
            other => panic!("expected SubmitAnswer, got {other:?}"),
        }
    }

    #[test]
    fn events_api_thread_reply_to_unknown_thread_ignored() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "events_api".to_string(),
            envelope_id:   Some("env-6".to_string()),
            payload:       Some(serde_json::json!({
                "event": {
                    "type": "message",
                    "text": "some reply",
                    "thread_ts": "9999.0000",
                    "user": "U123"
                }
            })),
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn unknown_type_produces_ignored() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "weird_type".to_string(),
            envelope_id:   None,
            payload:       None,
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Ignored));
    }
}
