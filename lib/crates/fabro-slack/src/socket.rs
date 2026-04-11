use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct SocketEnvelope {
    #[serde(rename = "type")]
    pub envelope_type: String,
    pub envelope_id:   Option<String>,
    pub payload:       Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SocketAck {
    pub envelope_id: String,
}

impl SocketAck {
    pub fn new(envelope_id: &str) -> Self {
        Self {
            envelope_id: envelope_id.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketEventKind {
    Hello,
    Interactive,
    EventsApi,
    Disconnect,
    Unknown,
}

pub fn classify_envelope(envelope: &SocketEnvelope) -> SocketEventKind {
    match envelope.envelope_type.as_str() {
        "hello" => SocketEventKind::Hello,
        "interactive" => SocketEventKind::Interactive,
        "events_api" => SocketEventKind::EventsApi,
        "disconnect" => SocketEventKind::Disconnect,
        _ => SocketEventKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hello_envelope() {
        let json = r#"{"type":"hello","num_connections":1}"#;
        let envelope: SocketEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.envelope_type, "hello");
        assert!(envelope.envelope_id.is_none());
    }

    #[test]
    fn parse_events_api_envelope() {
        let json = r#"{
            "type": "events_api",
            "envelope_id": "abc-123",
            "payload": {
                "event": {
                    "type": "app_mention",
                    "text": "<@U123> run workflow",
                    "channel": "C456"
                }
            }
        }"#;
        let envelope: SocketEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.envelope_type, "events_api");
        assert_eq!(envelope.envelope_id.as_deref(), Some("abc-123"));
        assert!(envelope.payload.is_some());
    }

    #[test]
    fn parse_interactive_envelope() {
        let json = r#"{
            "type": "interactive",
            "envelope_id": "def-456",
            "payload": {
                "type": "block_actions",
                "actions": [{
                    "action_id": "q-1:yes",
                    "type": "button",
                    "value": "yes"
                }]
            }
        }"#;
        let envelope: SocketEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.envelope_type, "interactive");
        assert_eq!(envelope.envelope_id.as_deref(), Some("def-456"));
        let payload = envelope.payload.unwrap();
        assert_eq!(payload["type"], "block_actions");
    }

    #[test]
    fn build_ack_message() {
        let ack = SocketAck::new("env-123");
        let json = serde_json::to_value(&ack).unwrap();
        assert_eq!(json["envelope_id"], "env-123");
    }

    #[test]
    fn classify_hello() {
        let envelope = SocketEnvelope {
            envelope_type: "hello".to_string(),
            envelope_id:   None,
            payload:       None,
        };
        assert_eq!(classify_envelope(&envelope), SocketEventKind::Hello);
    }

    #[test]
    fn classify_interactive() {
        let envelope = SocketEnvelope {
            envelope_type: "interactive".to_string(),
            envelope_id:   Some("e1".to_string()),
            payload:       Some(serde_json::json!({"type": "block_actions"})),
        };
        assert_eq!(classify_envelope(&envelope), SocketEventKind::Interactive);
    }

    #[test]
    fn classify_events_api() {
        let envelope = SocketEnvelope {
            envelope_type: "events_api".to_string(),
            envelope_id:   Some("e2".to_string()),
            payload:       Some(serde_json::json!({"event": {}})),
        };
        assert_eq!(classify_envelope(&envelope), SocketEventKind::EventsApi);
    }

    #[test]
    fn classify_disconnect() {
        let envelope = SocketEnvelope {
            envelope_type: "disconnect".to_string(),
            envelope_id:   None,
            payload:       None,
        };
        assert_eq!(classify_envelope(&envelope), SocketEventKind::Disconnect);
    }

    #[test]
    fn classify_unknown() {
        let envelope = SocketEnvelope {
            envelope_type: "something_else".to_string(),
            envelope_id:   None,
            payload:       None,
        };
        assert_eq!(classify_envelope(&envelope), SocketEventKind::Unknown);
    }
}
