use fabro_interview::Answer;

use crate::interaction;
use crate::socket::{SocketEnvelope, SocketEventKind, classify_envelope};
use crate::threads::{self, ThreadRegistry};

#[derive(Debug)]
pub enum DispatchAction {
    Connected,
    SubmitAnswer { question_id: String, answer: Answer },
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
                Some((question_id, answer)) => DispatchAction::SubmitAnswer {
                    question_id,
                    answer,
                },
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
            let Some(question_id) = thread_registry.resolve(&thread_ts) else {
                return DispatchAction::Ignored;
            };
            DispatchAction::SubmitAnswer {
                question_id,
                answer: Answer::text(text),
            }
        }
        SocketEventKind::Disconnect => DispatchAction::Reconnect,
        SocketEventKind::Unknown => DispatchAction::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_interview::AnswerValue;

    #[test]
    fn hello_produces_connected() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "hello".to_string(),
            envelope_id: None,
            payload: None,
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Connected));
    }

    #[test]
    fn interactive_button_produces_submit_answer() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "interactive".to_string(),
            envelope_id: Some("env-1".to_string()),
            payload: Some(serde_json::json!({
                "type": "block_actions",
                "actions": [{
                    "action_id": "q-1:yes",
                    "type": "button",
                    "value": "yes"
                }]
            })),
        };
        let action = dispatch(&envelope, &registry);
        match action {
            DispatchAction::SubmitAnswer {
                question_id,
                answer,
            } => {
                assert_eq!(question_id, "q-1");
                assert_eq!(answer.value, AnswerValue::Yes);
            }
            other => panic!("expected SubmitAnswer, got {other:?}"),
        }
    }

    #[test]
    fn interactive_with_unparseable_payload_produces_ignored() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "interactive".to_string(),
            envelope_id: Some("env-2".to_string()),
            payload: Some(serde_json::json!({
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
            envelope_id: Some("env-3".to_string()),
            payload: None,
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn disconnect_produces_reconnect() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "disconnect".to_string(),
            envelope_id: None,
            payload: None,
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Reconnect));
    }

    #[test]
    fn events_api_non_thread_produces_ignored() {
        let registry = ThreadRegistry::new();
        let envelope = SocketEnvelope {
            envelope_type: "events_api".to_string(),
            envelope_id: Some("env-4".to_string()),
            payload: Some(serde_json::json!({
                "event": { "type": "app_mention", "text": "hello" }
            })),
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn events_api_thread_reply_to_registered_question() {
        let registry = ThreadRegistry::new();
        registry.register("1234.5678", "q-10");
        let envelope = SocketEnvelope {
            envelope_type: "events_api".to_string(),
            envelope_id: Some("env-5".to_string()),
            payload: Some(serde_json::json!({
                "event": {
                    "type": "message",
                    "text": "https://github.com/org/repo",
                    "thread_ts": "1234.5678",
                    "user": "U123"
                }
            })),
        };
        let action = dispatch(&envelope, &registry);
        match action {
            DispatchAction::SubmitAnswer {
                question_id,
                answer,
            } => {
                assert_eq!(question_id, "q-10");
                assert_eq!(
                    answer.value,
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
            envelope_id: Some("env-6".to_string()),
            payload: Some(serde_json::json!({
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
            envelope_id: None,
            payload: None,
        };
        let action = dispatch(&envelope, &registry);
        assert!(matches!(action, DispatchAction::Ignored));
    }
}
