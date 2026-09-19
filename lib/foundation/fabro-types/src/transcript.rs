//! Canonical transcript primitives.
//!
//! The message vocabulary (`Message`, `ContentPart`, `ToolCall`, `ToolResult`,
//! and friends) is lithos's, re-exported here so the event stream, API
//! responses, and runtime history share one Rust model. [`TranscriptMessage`]
//! is Fabro's durable replay record: identity, provenance, and usage wrapped
//! around lithos content parts.

use chrono::{DateTime, Utc};
use lithos_llm::types::{ContentPart, TokenCounts, ToolCall, ToolResult};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

use crate::id::ulid_id;
use crate::pair::{PairId, PairMessageId};
use crate::principal::Principal;
use crate::session::TurnId;
use crate::usage::ModelRef;

ulid_id!(MessageId);

/// Concatenates the text parts of a message or response.
#[must_use]
pub fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Builds a tool result whose content is one JSON value.
///
/// Plain strings become a text part so providers render them as text; every
/// other value is carried as structured JSON.
#[must_use]
pub fn tool_result_from_json(
    tool_call_id: impl Into<String>,
    content: serde_json::Value,
    is_error: bool,
) -> ToolResult {
    let part = match content {
        serde_json::Value::String(text) => ContentPart::Text { text },
        value => ContentPart::Json { value },
    };
    ToolResult {
        tool_call_id: tool_call_id.into(),
        name: None,
        content: vec![part],
        is_error,
    }
}

/// Projects a tool result back to one JSON value, the inverse of
/// [`tool_result_from_json`].
///
/// A lone text part becomes a string and a lone JSON part its value. Any
/// other shape is carried as the array of serialized parts.
#[must_use]
pub fn tool_result_to_json(result: &ToolResult) -> serde_json::Value {
    match result.content.as_slice() {
        [ContentPart::Text { text }] => serde_json::Value::String(text.clone()),
        [ContentPart::Json { value }] => value.clone(),
        parts => serde_json::Value::Array(
            parts
                .iter()
                .map(|part| serde_json::to_value(part).unwrap_or(serde_json::Value::Null))
                .collect(),
        ),
    }
}

/// The arguments of a tool call as one JSON value.
///
/// Function arguments are the parsed JSON object; malformed arguments and
/// custom free-form input are carried as their raw text.
#[must_use]
pub fn tool_call_arguments(call: &ToolCall) -> serde_json::Value {
    call.input
        .to_value()
        .unwrap_or_else(|_| serde_json::Value::String(call.input.raw().to_string()))
}

// --- TranscriptMessage ------------------------------------------------------

/// Provider/model-role semantics for a committed transcript message.
///
/// Captured separately from [`MessageSource`] so audit/UI provenance
/// (`steer`, `pair`, …) does not collapse the LLM role that the message
/// replays as.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MessageKind {
    System,
    User,
    Reasoning,
    Agent,
}

/// Audit/UI provenance for a committed transcript message.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MessageSource {
    SystemPrompt,
    TurnInput,
    Followup,
    Steer,
    Pair,
    InjectedSystem,
    InjectedUser,
    LoopDetection,
    /// Reasoning blocks emitted by the model.
    ProviderReasoning,
    /// Final agent answer emitted by the model.
    ProviderAnswer,
}

/// Reference to the originating pair chat message for messages that
/// entered LLM history via the pair channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairMessageRef {
    pub pair_id:           PairId,
    pub message_id:        PairMessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
}

/// Canonical durable transcript message.
///
/// Named `TranscriptMessage` rather than `Message` to avoid import ambiguity
/// with pebble's `Message` and the lithos request `Message`.
///
/// `kind` captures provider/model-role semantics for replay; `source`
/// captures audit/UI provenance. Both are required to faithfully reconstruct
/// an API-mode session from the event stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub id:          MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id:     Option<TurnId>,
    pub kind:        MessageKind,
    pub source:      MessageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor:       Option<Principal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair:        Option<PairMessageRef>,
    pub content:     Vec<ContentPart>,
    /// Provider + model identity for the response that produced this
    /// message, when applicable. Strongly typed via [`ModelRef`] so
    /// provider and model id can never drift apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:       Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage:       Option<TokenCounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at:  Option<DateTime<Utc>>,
}

impl TranscriptMessage {
    /// Constructs a new transcript message with the supplied kind, source, and
    /// content.
    pub fn new(kind: MessageKind, source: MessageSource, content: Vec<ContentPart>) -> Self {
        Self {
            id: MessageId::new(),
            turn_id: None,
            kind,
            source,
            actor: None,
            pair: None,
            content,
            model: None,
            response_id: None,
            usage: None,
            created_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn text_of_concatenates_text_parts_only() {
        let parts = vec![
            ContentPart::Text {
                text: "hello ".to_string(),
            },
            ContentPart::Json { value: json!(1) },
            ContentPart::Text {
                text: "world".to_string(),
            },
        ];
        assert_eq!(text_of(&parts), "hello world");
    }

    #[test]
    fn tool_result_from_json_keeps_strings_as_text() {
        let result = tool_result_from_json("call_1", json!("ok"), false);
        assert_eq!(result.content, vec![ContentPart::Text {
            text: "ok".to_string(),
        }]);
        let result = tool_result_from_json("call_1", json!({"ok": true}), true);
        assert!(result.is_error);
        assert_eq!(result.content, vec![ContentPart::Json {
            value: json!({"ok": true}),
        }]);
    }

    #[test]
    fn transcript_message_serde_round_trip() {
        let msg = TranscriptMessage {
            id:          MessageId::new(),
            turn_id:     None,
            kind:        MessageKind::User,
            source:      MessageSource::Steer,
            actor:       None,
            pair:        None,
            content:     vec![ContentPart::Text {
                text: "please continue".to_string(),
            }],
            model:       None,
            response_id: None,
            usage:       None,
            created_at:  None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["kind"], "user");
        assert_eq!(v["source"], "steer");
        assert_eq!(
            v["content"][0],
            json!({"type": "text", "text": "please continue"})
        );
        let back: TranscriptMessage = serde_json::from_value(v).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn transcript_message_drops_optional_fields_on_serialize() {
        let msg = TranscriptMessage::new(MessageKind::Agent, MessageSource::ProviderAnswer, vec![
            ContentPart::Text {
                text: "done".to_string(),
            },
        ]);
        let v = serde_json::to_value(&msg).unwrap();
        let obj = v.as_object().unwrap();
        // Optional fields should be omitted, not present as nulls.
        assert!(!obj.contains_key("turn_id"));
        assert!(!obj.contains_key("actor"));
        assert!(!obj.contains_key("pair"));
        assert!(!obj.contains_key("model"));
        assert!(!obj.contains_key("response_id"));
        assert!(!obj.contains_key("usage"));
        assert!(!obj.contains_key("created_at"));
    }

    #[test]
    fn transcript_message_usage_uses_lithos_buckets() {
        let mut msg =
            TranscriptMessage::new(MessageKind::Agent, MessageSource::ProviderAnswer, vec![]);
        msg.usage = Some(TokenCounts {
            input: 10,
            output: 2,
            ..TokenCounts::default()
        });
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(
            v["usage"],
            json!({"input": 10, "output": 2, "reasoning": 0, "cache_read": 0, "cache_write": 0})
        );
    }

    #[test]
    fn pair_message_ref_skips_empty_client_id() {
        let r = PairMessageRef {
            pair_id:           PairId::new(),
            message_id:        PairMessageId::new(),
            client_message_id: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.as_object().unwrap().get("client_message_id").is_none());
    }
}
