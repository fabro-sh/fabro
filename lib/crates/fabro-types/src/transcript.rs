//! Canonical provider-neutral transcript primitives.
//!
//! These types are the durable replay shapes for agent sessions. They were
//! promoted from `fabro-llm` so the Fabro event stream, API responses, and
//! runtime history can share one canonical Rust model rather than ferrying
//! parallel DTOs between layers. `fabro-llm::types` re-exports these so
//! existing imports keep working.

use chrono::{DateTime, Utc};
use fabro_model::{ModelRef, TokenCounts};
use serde::{Deserialize, Serialize, de};
use strum::{Display, EnumString, IntoStaticStr};

use crate::id::ulid_id;
use crate::pair::{PairId, PairMessageId};
use crate::principal::Principal;
use crate::session::TurnId;

ulid_id!(MessageId);

// --- Content data structures -------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageData {
    pub url:        Option<String>,
    pub data:       Option<Vec<u8>>,
    pub media_type: Option<String>,
    pub detail:     Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioData {
    pub url:        Option<String>,
    pub data:       Option<Vec<u8>>,
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentData {
    pub url:        Option<String>,
    pub data:       Option<Vec<u8>>,
    pub media_type: Option<String>,
    pub file_name:  Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingData {
    pub text:      String,
    pub signature: Option<String>,
    pub redacted:  bool,
}

// --- Tool call / tool result -------------------------------------------------

fn default_tool_type() -> String {
    "function".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id:                String,
    pub name:              String,
    #[serde(rename = "type", default = "default_tool_type")]
    pub tool_type:         String,
    pub arguments:         serde_json::Value,
    pub raw_arguments:     Option<String>,
    /// Opaque provider-specific metadata (e.g. Gemini `thought_signature`).
    /// Preserved across round-trips so the provider can include it when
    /// sending conversation history back to the API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<serde_json::Value>,
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            tool_type: "function".to_string(),
            arguments,
            raw_arguments: None,
            provider_metadata: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id:     String,
    pub content:          serde_json::Value,
    pub is_error:         bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_data:       Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_media_type: Option<String>,
}

impl ToolResult {
    pub fn success(id: impl Into<String>, content: serde_json::Value) -> Self {
        Self {
            tool_call_id: id.into(),
            content,
            is_error: false,
            image_data: None,
            image_media_type: None,
        }
    }

    pub fn error(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            tool_call_id:     id.into(),
            content:          serde_json::Value::String(message.into()),
            is_error:         true,
            image_data:       None,
            image_media_type: None,
        }
    }
}

// --- ContentPart -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentPart {
    Text(String),
    Image(ImageData),
    Audio(AudioData),
    Document(DocumentData),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Thinking(ThinkingData),
    Other {
        kind: String,
        data: serde_json::Value,
    },
}

impl Serialize for ContentPart {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2))?;
        match self {
            Self::Text(v) => {
                map.serialize_entry("kind", "text")?;
                map.serialize_entry("data", v)?;
            }
            Self::Image(v) => {
                map.serialize_entry("kind", "image")?;
                map.serialize_entry("data", v)?;
            }
            Self::Audio(v) => {
                map.serialize_entry("kind", "audio")?;
                map.serialize_entry("data", v)?;
            }
            Self::Document(v) => {
                map.serialize_entry("kind", "document")?;
                map.serialize_entry("data", v)?;
            }
            Self::ToolCall(v) => {
                map.serialize_entry("kind", "tool_call")?;
                map.serialize_entry("data", v)?;
            }
            Self::ToolResult(v) => {
                map.serialize_entry("kind", "tool_result")?;
                map.serialize_entry("data", v)?;
            }
            Self::Thinking(v) => {
                let kind = if v.redacted {
                    "redacted_thinking"
                } else {
                    "thinking"
                };
                map.serialize_entry("kind", kind)?;
                map.serialize_entry("data", v)?;
            }
            Self::Other { kind, data } => {
                map.serialize_entry("kind", kind)?;
                map.serialize_entry("data", data)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ContentPart {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let kind = value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| de::Error::missing_field("kind"))?;
        let data = value
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match kind {
            "text" => serde_json::from_value(data)
                .map(Self::Text)
                .map_err(de::Error::custom),
            "image" => serde_json::from_value(data)
                .map(Self::Image)
                .map_err(de::Error::custom),
            "audio" => serde_json::from_value(data)
                .map(Self::Audio)
                .map_err(de::Error::custom),
            "document" => serde_json::from_value(data)
                .map(Self::Document)
                .map_err(de::Error::custom),
            "tool_call" => serde_json::from_value(data)
                .map(Self::ToolCall)
                .map_err(de::Error::custom),
            "tool_result" => serde_json::from_value(data)
                .map(Self::ToolResult)
                .map_err(de::Error::custom),
            "thinking" => serde_json::from_value(data)
                .map(Self::Thinking)
                .map_err(de::Error::custom),
            "redacted_thinking" => serde_json::from_value::<ThinkingData>(data)
                .map(|mut td| {
                    td.redacted = true;
                    Self::Thinking(td)
                })
                .map_err(de::Error::custom),
            other => Ok(Self::Other {
                kind: other.to_string(),
                data,
            }),
        }
    }
}

impl ContentPart {
    /// Kind string for opaque OpenAI reasoning output items.
    pub const OPENAI_REASONING: &str = "openai_reasoning";
    /// Kind string for opaque OpenAI message output items.
    pub const OPENAI_MESSAGE: &str = "openai_message";

    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// Returns `true` if this is an opaque OpenAI item (reasoning or message)
    /// that should be round-tripped verbatim through the API.
    pub fn is_opaque_openai(&self) -> bool {
        matches!(
            self,
            Self::Other { kind, .. }
                if kind == Self::OPENAI_REASONING || kind == Self::OPENAI_MESSAGE
        )
    }
}

// --- Role / Message
// -----------------------------------------------------------

/// Author role of a chat [`Message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
    Developer,
}

/// Provider-neutral chat message exchanged with an LLM.
///
/// This is the request/response message shape shared by `fabro-llm`
/// requests and the completions API wire contract. The durable
/// session-transcript record is [`TranscriptMessage`], which carries
/// identity, provenance, and usage on top of the same [`ContentPart`]
/// vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role:         Role,
    pub content:      Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role:         Role::System,
            content:      vec![ContentPart::text(text)],
            name:         None,
            tool_call_id: None,
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role:         Role::User,
            content:      vec![ContentPart::text(text)],
            name:         None,
            tool_call_id: None,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role:         Role::Assistant,
            content:      vec![ContentPart::text(text)],
            name:         None,
            tool_call_id: None,
        }
    }

    pub fn tool_result(
        tool_call_id: impl Into<String>,
        content: serde_json::Value,
        is_error: bool,
    ) -> Self {
        let id = tool_call_id.into();
        Self {
            role:         Role::Tool,
            content:      vec![ContentPart::ToolResult(ToolResult {
                tool_call_id: id.clone(),
                content,
                is_error,
                image_data: None,
                image_media_type: None,
            })],
            name:         None,
            tool_call_id: Some(id),
        }
    }

    /// Concatenates text from all text content parts.
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
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
/// with `fabro_agent::Message` and `fabro_llm::types::Message`.
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
    fn content_part_text_roundtrips() {
        let part = ContentPart::text("hello");
        let v = serde_json::to_value(&part).unwrap();
        assert_eq!(v, json!({"kind": "text", "data": "hello"}));
        let back: ContentPart = serde_json::from_value(v).unwrap();
        assert_eq!(back, part);
    }

    #[test]
    fn content_part_thinking_preserves_signature_and_redaction() {
        let part = ContentPart::Thinking(ThinkingData {
            text:      "private thought".to_string(),
            signature: Some("sig_abc".to_string()),
            redacted:  true,
        });
        let v = serde_json::to_value(&part).unwrap();
        assert_eq!(v["kind"], "redacted_thinking");
        assert_eq!(v["data"]["signature"], "sig_abc");
        let back: ContentPart = serde_json::from_value(v).unwrap();
        assert_eq!(back, part);
    }

    #[test]
    fn content_part_other_preserves_provider_kind() {
        let part = ContentPart::Other {
            kind: ContentPart::OPENAI_REASONING.to_string(),
            data: json!({"item_id": "rs_1", "encrypted": "x"}),
        };
        assert!(part.is_opaque_openai());
        let v = serde_json::to_value(&part).unwrap();
        let back: ContentPart = serde_json::from_value(v).unwrap();
        assert_eq!(back, part);
    }

    #[test]
    fn tool_call_preserves_provider_metadata() {
        let mut tc = ToolCall::new("call_1", "Bash", json!({"cmd": "ls"}));
        tc.provider_metadata = Some(json!({"thought_signature": "sig"}));
        tc.raw_arguments = Some("{\"cmd\":\"ls\"}".to_string());
        let v = serde_json::to_value(&tc).unwrap();
        assert_eq!(v["provider_metadata"]["thought_signature"], "sig");
        let back: ToolCall = serde_json::from_value(v).unwrap();
        assert_eq!(back, tc);
    }

    #[test]
    fn tool_result_round_trips_with_default_image_fields() {
        let tr = ToolResult::success("call_1", json!({"ok": true}));
        let v = serde_json::to_value(&tr).unwrap();
        // Optional image fields are omitted on serialize.
        assert!(v.get("image_data").is_none());
        let back: ToolResult = serde_json::from_value(v).unwrap();
        assert_eq!(back, tr);
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
            content:     vec![ContentPart::text("please continue")],
            model:       None,
            response_id: None,
            usage:       None,
            created_at:  None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["kind"], "user");
        assert_eq!(v["source"], "steer");
        let back: TranscriptMessage = serde_json::from_value(v).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn transcript_message_drops_optional_fields_on_serialize() {
        let msg = TranscriptMessage::new(MessageKind::Agent, MessageSource::ProviderAnswer, vec![
            ContentPart::text("done"),
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
