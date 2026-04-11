use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

use crate::payload::SlackQuestionRef;

#[derive(Default)]
pub struct ThreadRegistry {
    ts_to_question: Mutex<HashMap<String, SlackQuestionRef>>,
}

impl ThreadRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, message_ts: &str, run_id: &str, question_id: &str) {
        self.ts_to_question
            .lock()
            .expect("thread registry lock poisoned")
            .insert(message_ts.to_string(), SlackQuestionRef {
                run_id: run_id.to_string(),
                qid:    question_id.to_string(),
            });
    }

    pub fn resolve(&self, thread_ts: &str) -> Option<SlackQuestionRef> {
        self.ts_to_question
            .lock()
            .expect("thread registry lock poisoned")
            .get(thread_ts)
            .cloned()
    }

    pub fn remove(&self, message_ts: &str) {
        self.ts_to_question
            .lock()
            .expect("thread registry lock poisoned")
            .remove(message_ts);
    }
}

/// Parse a thread reply from an events_api payload.
/// Returns (thread_ts, reply_text) if this is a thread reply from a human user.
/// Accepts both `message` and `app_mention` event types (some workspaces only
/// deliver `app_mention` to bots).
pub fn parse_thread_reply(payload: &Value) -> Option<(String, String)> {
    let event = &payload["event"];
    let event_type = event["type"].as_str()?;
    if event_type != "message" && event_type != "app_mention" {
        return None;
    }
    // Ignore bot messages (our own replies)
    if event["bot_id"].is_string() || event["subtype"].is_string() {
        return None;
    }
    let thread_ts = event["thread_ts"].as_str()?;
    let mut text = event["text"].as_str()?.to_string();
    // Strip the @mention prefix from app_mention events (e.g. "<@U123> my answer" →
    // "my answer")
    if event_type == "app_mention" {
        if let Some(rest) = text.strip_prefix('<') {
            if let Some(after_mention) = rest.split_once('>') {
                text = after_mention.1.trim().to_string();
            }
        }
    }
    if text.is_empty() {
        return None;
    }
    Some((thread_ts.to_string(), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_resolve() {
        let registry = ThreadRegistry::new();
        registry.register("1234.5678", "run-1", "q-1");
        assert_eq!(
            registry.resolve("1234.5678"),
            Some(SlackQuestionRef {
                run_id: "run-1".to_string(),
                qid:    "q-1".to_string(),
            })
        );
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let registry = ThreadRegistry::new();
        assert_eq!(registry.resolve("unknown"), None);
    }

    #[test]
    fn remove_clears_mapping() {
        let registry = ThreadRegistry::new();
        registry.register("1234.5678", "run-1", "q-1");
        registry.remove("1234.5678");
        assert_eq!(registry.resolve("1234.5678"), None);
    }

    #[test]
    fn parse_thread_reply_valid() {
        let payload = serde_json::json!({
            "event": {
                "type": "message",
                "text": "https://github.com/org/repo",
                "thread_ts": "1234.5678",
                "user": "U123"
            }
        });
        let result = parse_thread_reply(&payload).unwrap();
        assert_eq!(result.0, "1234.5678");
        assert_eq!(result.1, "https://github.com/org/repo");
    }

    #[test]
    fn parse_thread_reply_ignores_bot_messages() {
        let payload = serde_json::json!({
            "event": {
                "type": "message",
                "text": "bot reply",
                "thread_ts": "1234.5678",
                "bot_id": "B123"
            }
        });
        assert!(parse_thread_reply(&payload).is_none());
    }

    #[test]
    fn parse_thread_reply_ignores_subtypes() {
        let payload = serde_json::json!({
            "event": {
                "type": "message",
                "subtype": "message_changed",
                "text": "edited",
                "thread_ts": "1234.5678"
            }
        });
        assert!(parse_thread_reply(&payload).is_none());
    }

    #[test]
    fn parse_thread_reply_ignores_non_thread_messages() {
        let payload = serde_json::json!({
            "event": {
                "type": "message",
                "text": "hello",
                "user": "U123"
            }
        });
        assert!(parse_thread_reply(&payload).is_none());
    }

    #[test]
    fn parse_thread_reply_ignores_empty_text() {
        let payload = serde_json::json!({
            "event": {
                "type": "message",
                "text": "",
                "thread_ts": "1234.5678",
                "user": "U123"
            }
        });
        assert!(parse_thread_reply(&payload).is_none());
    }

    #[test]
    fn parse_thread_reply_ignores_non_message_events() {
        let payload = serde_json::json!({
            "event": {
                "type": "reaction_added",
                "reaction": "thumbsup"
            }
        });
        assert!(parse_thread_reply(&payload).is_none());
    }

    #[test]
    fn parse_thread_reply_app_mention_in_thread() {
        let payload = serde_json::json!({
            "event": {
                "type": "app_mention",
                "text": "<@U0BOTID> https://github.com/org/repo",
                "thread_ts": "1234.5678",
                "user": "U123"
            }
        });
        let result = parse_thread_reply(&payload).unwrap();
        assert_eq!(result.0, "1234.5678");
        assert_eq!(result.1, "https://github.com/org/repo");
    }

    #[test]
    fn parse_thread_reply_app_mention_strips_mention_prefix() {
        let payload = serde_json::json!({
            "event": {
                "type": "app_mention",
                "text": "<@U0BOTID> my answer here",
                "thread_ts": "1234.5678",
                "user": "U123"
            }
        });
        let result = parse_thread_reply(&payload).unwrap();
        assert_eq!(result.1, "my answer here");
    }

    #[test]
    fn parse_thread_reply_app_mention_ignores_non_thread() {
        let payload = serde_json::json!({
            "event": {
                "type": "app_mention",
                "text": "<@U0BOTID> hello",
                "user": "U123"
            }
        });
        assert!(parse_thread_reply(&payload).is_none());
    }

    #[test]
    fn parse_thread_reply_app_mention_only_mention_is_empty() {
        let payload = serde_json::json!({
            "event": {
                "type": "app_mention",
                "text": "<@U0BOTID>",
                "thread_ts": "1234.5678",
                "user": "U123"
            }
        });
        assert!(parse_thread_reply(&payload).is_none());
    }
}
