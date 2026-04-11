use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct Track {
    #[serde(flatten)]
    pub user:       User,
    pub event:      String,
    pub properties: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context:    Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp:  Option<String>,
    #[serde(rename = "messageId")]
    pub message_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum User {
    Both {
        #[serde(rename = "userId")]
        user_id:      String,
        #[serde(rename = "anonymousId")]
        anonymous_id: String,
    },
    UserId {
        #[serde(rename = "userId")]
        user_id: String,
    },
    AnonymousId {
        #[serde(rename = "anonymousId")]
        anonymous_id: String,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn track_serialization_anonymous() {
        let track = Track {
            user:       User::AnonymousId {
                anonymous_id: "abc-123".to_string(),
            },
            event:      "Command Invoked".to_string(),
            properties: json!({"command": "run"}),
            context:    Some(json!({"os": {"name": "macos"}})),
            timestamp:  Some("2025-01-01T00:00:00Z".to_string()),
            message_id: "msg-1".to_string(),
        };

        insta::assert_snapshot!(serde_json::to_string_pretty(&track).unwrap(), @r#"
        {
          "anonymousId": "abc-123",
          "event": "Command Invoked",
          "properties": {
            "command": "run"
          },
          "context": {
            "os": {
              "name": "macos"
            }
          },
          "timestamp": "2025-01-01T00:00:00Z",
          "messageId": "msg-1"
        }
        "#);
    }

    #[test]
    fn track_serialization_user_id() {
        let track = Track {
            user:       User::UserId {
                user_id: "user-456".to_string(),
            },
            event:      "test".to_string(),
            properties: json!({}),
            context:    None,
            timestamp:  None,
            message_id: "msg-2".to_string(),
        };

        insta::assert_snapshot!(serde_json::to_string_pretty(&track).unwrap(), @r#"
        {
          "userId": "user-456",
          "event": "test",
          "properties": {},
          "messageId": "msg-2"
        }
        "#);
    }

    #[test]
    fn track_serialization_both() {
        let track = Track {
            user:       User::Both {
                user_id:      "user-456".to_string(),
                anonymous_id: "abc-123".to_string(),
            },
            event:      "test".to_string(),
            properties: json!({}),
            context:    None,
            timestamp:  None,
            message_id: "msg-3".to_string(),
        };

        insta::assert_snapshot!(serde_json::to_string_pretty(&track).unwrap(), @r#"
        {
          "userId": "user-456",
          "anonymousId": "abc-123",
          "event": "test",
          "properties": {},
          "messageId": "msg-3"
        }
        "#);
    }

    #[test]
    fn track_round_trip() {
        let track = Track {
            user:       User::AnonymousId {
                anonymous_id: "abc".to_string(),
            },
            event:      "test".to_string(),
            properties: json!({"key": "value"}),
            context:    Some(json!({"app": {"name": "fabro"}})),
            timestamp:  Some("2025-06-01T12:00:00Z".to_string()),
            message_id: "msg-rt".to_string(),
        };

        let json_str = serde_json::to_string(&track).unwrap();
        let deserialized: Track = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.event, "test");
        assert_eq!(deserialized.message_id, "msg-rt");
    }
}
