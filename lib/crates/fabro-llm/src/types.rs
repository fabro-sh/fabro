use crate::error::SdkError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// --- 3.2 Role ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
    Developer,
}

// --- 3.5 Content Data Structures ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageData {
    pub url: Option<String>,
    pub data: Option<Vec<u8>>,
    pub media_type: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioData {
    pub url: Option<String>,
    pub data: Option<Vec<u8>>,
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentData {
    pub url: Option<String>,
    pub data: Option<Vec<u8>>,
    pub media_type: Option<String>,
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingData {
    pub text: String,
    pub signature: Option<String>,
    pub redacted: bool,
}

// --- 5.4 ToolCall / ToolResult ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub raw_arguments: Option<String>,
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
            arguments,
            raw_arguments: None,
            provider_metadata: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: serde_json::Value,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_data: Option<Vec<u8>>,
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
            tool_call_id: id.into(),
            content: serde_json::Value::String(message.into()),
            is_error: true,
            image_data: None,
            image_media_type: None,
        }
    }
}

// --- 3.3 ContentPart ---

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
            .ok_or_else(|| serde::de::Error::missing_field("kind"))?;
        let data = value
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match kind {
            "text" => serde_json::from_value(data)
                .map(Self::Text)
                .map_err(serde::de::Error::custom),
            "image" => serde_json::from_value(data)
                .map(Self::Image)
                .map_err(serde::de::Error::custom),
            "audio" => serde_json::from_value(data)
                .map(Self::Audio)
                .map_err(serde::de::Error::custom),
            "document" => serde_json::from_value(data)
                .map(Self::Document)
                .map_err(serde::de::Error::custom),
            "tool_call" => serde_json::from_value(data)
                .map(Self::ToolCall)
                .map_err(serde::de::Error::custom),
            "tool_result" => serde_json::from_value(data)
                .map(Self::ToolResult)
                .map_err(serde::de::Error::custom),
            "thinking" => serde_json::from_value(data)
                .map(Self::Thinking)
                .map_err(serde::de::Error::custom),
            "redacted_thinking" => serde_json::from_value::<ThinkingData>(data)
                .map(|mut td| {
                    td.redacted = true;
                    Self::Thinking(td)
                })
                .map_err(serde::de::Error::custom),
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
        matches!(self, ContentPart::Other { kind, .. } if kind == Self::OPENAI_REASONING || kind == Self::OPENAI_MESSAGE)
    }
}

// --- 3.1 Message ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentPart::text(text)],
            name: None,
            tool_call_id: None,
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentPart::text(text)],
            name: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentPart::text(text)],
            name: None,
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
            role: Role::Tool,
            content: vec![ContentPart::ToolResult(ToolResult {
                tool_call_id: id.clone(),
                content,
                is_error,
                image_data: None,
                image_media_type: None,
            })],
            name: None,
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

// --- 3.8 FinishReason ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Error,
    Other(String),
}

impl FinishReason {
    #[must_use]
    pub const fn as_str(&self) -> &str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolCalls => "tool_calls",
            Self::ContentFilter => "content_filter",
            Self::Error => "error",
            Self::Other(s) => s.as_str(),
        }
    }
}

impl Serialize for FinishReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FinishReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "stop" => Self::Stop,
            "length" => Self::Length,
            "tool_calls" => Self::ToolCalls,
            "content_filter" => Self::ContentFilter,
            "error" => Self::Error,
            _ => Self::Other(s),
        })
    }
}

// --- 3.9 Usage ---

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl std::ops::Add for Usage {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        const fn add_optional(a: Option<i64>, b: Option<i64>) -> Option<i64> {
            match (a, b) {
                (None, None) => None,
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (Some(a), Some(b)) => Some(a + b),
            }
        }

        Self {
            input_tokens: self.input_tokens + rhs.input_tokens,
            output_tokens: self.output_tokens + rhs.output_tokens,
            total_tokens: self.total_tokens + rhs.total_tokens,
            reasoning_tokens: add_optional(self.reasoning_tokens, rhs.reasoning_tokens),
            cache_read_tokens: add_optional(self.cache_read_tokens, rhs.cache_read_tokens),
            cache_write_tokens: add_optional(self.cache_write_tokens, rhs.cache_write_tokens),
            speed: self.speed,
            raw: None,
        }
    }
}

// --- 3.10 ResponseFormat ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormatType {
    Text,
    #[serde(rename = "json")]
    JsonObject,
    JsonSchema,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub kind: ResponseFormatType,
    pub json_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub strict: bool,
}

// --- 3.11 Warning ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub message: String,
    pub code: Option<String>,
}

// --- 3.12 RateLimitInfo ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub requests_remaining: Option<i64>,
    pub requests_limit: Option<i64>,
    pub tokens_remaining: Option<i64>,
    pub tokens_limit: Option<i64>,
    pub reset_at: Option<String>,
}

// --- 3.8 ReasoningEffort ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(format!(
                "invalid reasoning_effort: {other:?} (expected low, medium, or high)"
            )),
        }
    }
}

// --- 3.6 Request ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub model: String,
    pub messages: Vec<Message>,
    pub provider: Option<String>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub tool_choice: Option<ToolChoice>,
    pub response_format: Option<ResponseFormat>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i64>,
    pub stop_sequences: Option<Vec<String>>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub speed: Option<String>,
    pub metadata: Option<HashMap<String, String>>,
    pub provider_options: Option<serde_json::Value>,
}

// --- 5.1 ToolDefinition ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// --- 5.3 ToolChoice ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Named { tool_name: String },
}

impl ToolChoice {
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named {
            tool_name: name.into(),
        }
    }

    /// Return the mode string used by `ProviderAdapter::supports_tool_choice`.
    #[must_use]
    pub const fn mode_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Required => "required",
            Self::Named { .. } => "named",
        }
    }
}

// --- 3.7 Response ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    pub model: String,
    pub provider: String,
    pub message: Message,
    pub finish_reason: FinishReason,
    pub usage: Usage,
    pub raw: Option<serde_json::Value>,
    pub warnings: Vec<Warning>,
    pub rate_limit: Option<RateLimitInfo>,
}

impl Response {
    #[must_use]
    pub fn text(&self) -> String {
        self.message.text()
    }

    #[must_use]
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.message
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::ToolCall(tc) => Some(tc.clone()),
                _ => None,
            })
            .collect()
    }

    #[must_use]
    pub fn reasoning(&self) -> Option<String> {
        let reasoning: String = self
            .message
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Thinking(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();

        if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        }
    }
}

// --- 3.13 StreamEvent ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    StreamStart,
    TextStart {
        text_id: Option<String>,
    },
    TextDelta {
        delta: String,
        text_id: Option<String>,
    },
    TextEnd {
        text_id: Option<String>,
    },
    ReasoningStart,
    ReasoningDelta {
        delta: String,
    },
    ReasoningEnd,
    ToolCallStart {
        tool_call: ToolCall,
    },
    ToolCallDelta {
        tool_call: ToolCall,
    },
    ToolCallEnd {
        tool_call: ToolCall,
    },
    StepFinish {
        finish_reason: FinishReason,
        usage: Usage,
        response: Box<Response>,
        tool_calls: Vec<ToolCall>,
        tool_results: Vec<ToolResult>,
    },
    Finish {
        finish_reason: FinishReason,
        usage: Usage,
        response: Box<Response>,
    },
    Error {
        error: SdkError,
        raw: Option<serde_json::Value>,
    },
}

impl StreamEvent {
    pub fn text_delta(delta: impl Into<String>, text_id: Option<String>) -> Self {
        Self::TextDelta {
            delta: delta.into(),
            text_id,
        }
    }

    #[must_use]
    pub fn step_finish(
        reason: FinishReason,
        usage: Usage,
        response: Response,
        tool_calls: Vec<ToolCall>,
        tool_results: Vec<ToolResult>,
    ) -> Self {
        Self::StepFinish {
            finish_reason: reason,
            usage,
            response: Box::new(response),
            tool_calls,
            tool_results,
        }
    }

    #[must_use]
    pub fn finish(reason: FinishReason, usage: Usage, response: Response) -> Self {
        Self::Finish {
            finish_reason: reason,
            usage,
            response: Box::new(response),
        }
    }

    #[must_use]
    pub const fn error(error: SdkError) -> Self {
        Self::Error { error, raw: None }
    }
}

// --- 2.9 Model (re-exported from fabro-model) ---

pub use fabro_model::{Model, ModelCosts, ModelFeatures, ModelLimits};

// --- 4.7 Timeouts ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeoutConfig {
    pub total: Option<f64>,
    pub per_step: Option<f64>,
}

impl From<f64> for TimeoutConfig {
    fn from(total: f64) -> Self {
        Self {
            total: Some(total),
            per_step: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AdapterTimeout {
    pub connect: f64,
    pub request: Option<f64>,
    pub stream_read: Option<f64>,
}

impl Default for AdapterTimeout {
    fn default() -> Self {
        Self {
            connect: 30.0,
            request: None,
            stream_read: Some(300.0),
        }
    }
}

// --- 6.6 RetryPolicy ---

/// Callback invoked before each retry attempt with (error, attempt, delay in seconds).
pub type OnRetryCallback = Arc<dyn Fn(&SdkError, u32, f64) + Send + Sync>;

#[derive(Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: f64,
    pub max_delay: f64,
    pub backoff_multiplier: f64,
    pub jitter: bool,
    /// Called before each retry with (error, attempt number, delay in seconds).
    pub on_retry: Option<OnRetryCallback>,
}

impl std::fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("max_retries", &self.max_retries)
            .field("base_delay", &self.base_delay)
            .field("max_delay", &self.max_delay)
            .field("backoff_multiplier", &self.backoff_multiplier)
            .field("jitter", &self.jitter)
            .field("on_retry", &self.on_retry.as_ref().map(|_| "..."))
            .finish()
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            base_delay: 1.0,
            max_delay: 60.0,
            backoff_multiplier: 2.0,
            jitter: true,
            on_retry: None,
        }
    }
}

impl RetryPolicy {
    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> f64 {
        let delay = self.base_delay * self.backoff_multiplier.powi(attempt as i32);
        let delay = delay.min(self.max_delay);

        if self.jitter {
            let jitter_factor = 0.5 + rand::random::<f64>(); // 0.5..1.5
            delay * jitter_factor
        } else {
            delay
        }
    }
}

// --- 4.6 ObjectStreamEvent ---

/// Events yielded by `stream_object()` for streaming structured output.
#[derive(Debug, Clone)]
pub enum ObjectStreamEvent {
    /// A new partial parse of the accumulated JSON text.
    Partial { object: serde_json::Value },
    /// A raw stream event from the underlying provider stream.
    Delta { event: StreamEvent },
    /// The stream completed with a fully parsed object and response.
    Complete {
        object: serde_json::Value,
        response: Box<Response>,
    },
}

// --- 4.3 GenerateResult / StepResult ---

#[derive(Debug, Clone)]
pub struct GenerateResult {
    pub response: Response,
    pub tool_results: Vec<ToolResult>,
    pub total_usage: Usage,
    pub steps: Vec<StepResult>,
    pub output: Option<serde_json::Value>,
}

impl std::ops::Deref for GenerateResult {
    type Target = Response;
    fn deref(&self) -> &Response {
        &self.response
    }
}

#[derive(Debug, Clone)]
pub struct StepResult {
    pub response: Response,
    pub tool_results: Vec<ToolResult>,
}

impl std::ops::Deref for StepResult {
    type Target = Response;
    fn deref(&self) -> &Response {
        &self.response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_system_constructor() {
        let msg = Message::system("You are helpful.");
        assert_eq!(msg.role, Role::System);
        assert_eq!(msg.text(), "You are helpful.");
    }

    #[test]
    fn message_user_constructor() {
        let msg = Message::user("Hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.text(), "Hello");
    }

    #[test]
    fn message_assistant_constructor() {
        let msg = Message::assistant("Hi there");
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.text(), "Hi there");
    }

    #[test]
    fn message_tool_result_constructor() {
        let msg = Message::tool_result(
            "call_123",
            serde_json::Value::String("72F and sunny".into()),
            false,
        );
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.tool_call_id, Some("call_123".to_string()));
        match &msg.content[0] {
            ContentPart::ToolResult(tr) => {
                assert_eq!(tr.tool_call_id, "call_123");
                assert!(!tr.is_error);
            }
            other => panic!("Expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn message_text_concatenates_text_parts() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentPart::text("Hello "),
                ContentPart::ToolCall(ToolCall::new("c1", "test", serde_json::json!({}))),
                ContentPart::text("world"),
            ],
            name: None,
            tool_call_id: None,
        };
        assert_eq!(msg.text(), "Hello world");
    }

    #[test]
    fn message_text_returns_empty_for_no_text_parts() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall(ToolCall::new(
                "c1",
                "test",
                serde_json::json!({}),
            ))],
            name: None,
            tool_call_id: None,
        };
        assert_eq!(msg.text(), "");
    }

    #[test]
    fn finish_reason_variants() {
        assert_eq!(FinishReason::Stop.as_str(), "stop");
        assert_eq!(FinishReason::Length.as_str(), "length");
        assert_eq!(FinishReason::ToolCalls.as_str(), "tool_calls");
        assert_eq!(FinishReason::ContentFilter.as_str(), "content_filter");
        assert_eq!(FinishReason::Error.as_str(), "error");
        assert_eq!(
            FinishReason::Other("custom_reason".into()).as_str(),
            "custom_reason"
        );
    }

    #[test]
    fn finish_reason_serde_roundtrip() {
        let reasons = vec![
            FinishReason::Stop,
            FinishReason::Length,
            FinishReason::ToolCalls,
            FinishReason::Other("custom".into()),
        ];
        for reason in &reasons {
            let json = serde_json::to_string(reason).unwrap();
            let deserialized: FinishReason = serde_json::from_str(&json).unwrap();
            assert_eq!(&deserialized, reason);
        }
    }

    #[test]
    fn usage_serialization_skips_none_optional_fields() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            reasoning_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            speed: None,
            raw: None,
        };
        insta::assert_snapshot!(serde_json::to_string_pretty(&usage).unwrap(), @r#"
        {
          "input_tokens": 100,
          "output_tokens": 50,
          "total_tokens": 150
        }
        "#);
    }

    #[test]
    fn usage_serialization_includes_present_optional_fields() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            reasoning_tokens: Some(20),
            cache_read_tokens: Some(80),
            cache_write_tokens: Some(10),
            speed: None,
            raw: None,
        };
        insta::assert_snapshot!(serde_json::to_string_pretty(&usage).unwrap(), @r#"
        {
          "input_tokens": 100,
          "output_tokens": 50,
          "total_tokens": 150,
          "reasoning_tokens": 20,
          "cache_read_tokens": 80,
          "cache_write_tokens": 10
        }
        "#);
    }

    #[test]
    fn usage_deserialization_without_optional_fields() {
        let json = r#"{"input_tokens":100,"output_tokens":50,"total_tokens":150}"#;
        let usage: Usage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.reasoning_tokens, None);
        assert_eq!(usage.cache_read_tokens, None);
    }

    #[test]
    fn usage_addition_both_filled() {
        let a = Usage {
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
            reasoning_tokens: Some(5),
            cache_read_tokens: Some(3),
            cache_write_tokens: Some(1),
            speed: None,
            raw: None,
        };
        let b = Usage {
            input_tokens: 15,
            output_tokens: 25,
            total_tokens: 40,
            reasoning_tokens: Some(10),
            cache_read_tokens: Some(7),
            cache_write_tokens: Some(2),
            speed: None,
            raw: None,
        };
        let sum = a + b;
        assert_eq!(sum.input_tokens, 25);
        assert_eq!(sum.output_tokens, 45);
        assert_eq!(sum.total_tokens, 70);
        assert_eq!(sum.reasoning_tokens, Some(15));
        assert_eq!(sum.cache_read_tokens, Some(10));
        assert_eq!(sum.cache_write_tokens, Some(3));
    }

    #[test]
    fn usage_addition_one_none() {
        let a = Usage {
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
            reasoning_tokens: Some(5),
            cache_read_tokens: None,
            cache_write_tokens: None,
            speed: None,
            raw: None,
        };
        let b = Usage {
            input_tokens: 15,
            output_tokens: 25,
            total_tokens: 40,
            reasoning_tokens: None,
            cache_read_tokens: Some(7),
            cache_write_tokens: None,
            speed: None,
            raw: None,
        };
        let sum = a + b;
        assert_eq!(sum.reasoning_tokens, Some(5));
        assert_eq!(sum.cache_read_tokens, Some(7));
        assert_eq!(sum.cache_write_tokens, None);
    }

    #[test]
    fn tool_choice_variants() {
        assert_eq!(ToolChoice::Auto, ToolChoice::Auto);
        assert_eq!(ToolChoice::None, ToolChoice::None);
        assert_eq!(ToolChoice::Required, ToolChoice::Required);
        let named = ToolChoice::named("get_weather");
        assert_eq!(
            named,
            ToolChoice::Named {
                tool_name: "get_weather".to_string()
            }
        );
    }

    #[test]
    fn response_text_accessor() {
        let response = Response {
            id: "resp_1".into(),
            model: "test-model".into(),
            provider: "test".into(),
            message: Message::assistant("Hello world"),
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
            raw: None,
            warnings: vec![],
            rate_limit: None,
        };
        assert_eq!(response.text(), "Hello world");
    }

    #[test]
    fn response_tool_calls_accessor() {
        let response = Response {
            id: "resp_1".into(),
            model: "test-model".into(),
            provider: "test".into(),
            message: Message {
                role: Role::Assistant,
                content: vec![
                    ContentPart::text("Let me check"),
                    ContentPart::ToolCall(ToolCall::new(
                        "call_1",
                        "get_weather",
                        serde_json::json!({"city": "SF"}),
                    )),
                ],
                name: None,
                tool_call_id: None,
            },
            finish_reason: FinishReason::ToolCalls,
            usage: Usage::default(),
            raw: None,
            warnings: vec![],
            rate_limit: None,
        };
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].id, "call_1");
    }

    #[test]
    fn response_reasoning_accessor() {
        let response = Response {
            id: "resp_1".into(),
            model: "test-model".into(),
            provider: "test".into(),
            message: Message {
                role: Role::Assistant,
                content: vec![
                    ContentPart::Thinking(ThinkingData {
                        text: "Let me think...".into(),
                        signature: Some("sig_123".into()),
                        redacted: false,
                    }),
                    ContentPart::text("The answer is 42."),
                ],
                name: None,
                tool_call_id: None,
            },
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
            raw: None,
            warnings: vec![],
            rate_limit: None,
        };
        assert_eq!(response.reasoning(), Some("Let me think...".to_string()));
        assert_eq!(response.text(), "The answer is 42.");
    }

    #[test]
    fn response_reasoning_returns_none_when_absent() {
        let response = Response {
            id: "resp_1".into(),
            model: "test-model".into(),
            provider: "test".into(),
            message: Message::assistant("Hello"),
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
            raw: None,
            warnings: vec![],
            rate_limit: None,
        };
        assert_eq!(response.reasoning(), None);
    }

    #[test]
    fn stream_event_text_delta() {
        let event = StreamEvent::text_delta("hello", Some("t1".into()));
        match &event {
            StreamEvent::TextDelta { delta, text_id } => {
                assert_eq!(delta, "hello");
                assert_eq!(text_id, &Some("t1".to_string()));
            }
            other => panic!("Expected TextDelta, got {other:?}"),
        }
    }

    #[test]
    fn stream_event_error() {
        let event = StreamEvent::error(SdkError::Stream {
            message: "something went wrong".into(),
        });
        match &event {
            StreamEvent::Error { error, .. } => {
                assert_eq!(error.to_string(), "Stream error: something went wrong");
            }
            other => panic!("Expected Error, got {other:?}"),
        }
    }

    #[test]
    fn retry_policy_delay_no_jitter() {
        let policy = RetryPolicy {
            max_retries: 3,
            base_delay: 1.0,
            max_delay: 60.0,
            backoff_multiplier: 2.0,
            jitter: false,
            ..Default::default()
        };
        assert!((policy.delay_for_attempt(0) - 1.0).abs() < f64::EPSILON);
        assert!((policy.delay_for_attempt(1) - 2.0).abs() < f64::EPSILON);
        assert!((policy.delay_for_attempt(2) - 4.0).abs() < f64::EPSILON);
        assert!((policy.delay_for_attempt(3) - 8.0).abs() < f64::EPSILON);
    }

    #[test]
    fn retry_policy_delay_respects_max() {
        let policy = RetryPolicy {
            max_retries: 10,
            base_delay: 1.0,
            max_delay: 5.0,
            backoff_multiplier: 2.0,
            jitter: false,
            ..Default::default()
        };
        assert!((policy.delay_for_attempt(5) - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn retry_policy_delay_with_jitter_in_range() {
        let policy = RetryPolicy {
            max_retries: 3,
            base_delay: 1.0,
            max_delay: 60.0,
            backoff_multiplier: 2.0,
            jitter: true,
            ..Default::default()
        };
        let delay = policy.delay_for_attempt(0);
        // base * 0.5 to base * 1.5 => 0.5 to 1.5
        assert!(delay >= 0.5);
        assert!(delay <= 1.5);
    }

    #[test]
    fn adapter_timeout_defaults() {
        let timeout = AdapterTimeout::default();
        assert!((timeout.connect - 30.0).abs() < f64::EPSILON);
        assert!(timeout.request.is_none());
        assert!((timeout.stream_read.unwrap() - 300.0).abs() < f64::EPSILON);
    }

    #[test]
    fn content_part_text_constructor() {
        let part = ContentPart::text("hello");
        assert_eq!(part, ContentPart::Text("hello".to_string()));
    }

    #[test]
    fn content_part_image_constructor() {
        let part = ContentPart::Image(ImageData {
            url: Some("https://example.com/img.png".into()),
            data: None,
            media_type: None,
            detail: None,
        });
        assert!(matches!(part, ContentPart::Image(_)));
    }

    #[test]
    fn tool_call_serde_roundtrip() {
        let tc = ToolCall::new("c1", "test", serde_json::json!({}));
        let json = serde_json::to_string(&tc).unwrap();
        let deserialized: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, tc);
    }

    #[test]
    fn tool_result_with_image_data() {
        let result = ToolResult {
            tool_call_id: "call_1".into(),
            content: serde_json::json!("screenshot taken"),
            is_error: false,
            image_data: Some(vec![0x89, 0x50, 0x4E, 0x47]),
            image_media_type: Some("image/png".into()),
        };
        assert!(result.image_data.is_some());
        assert_eq!(result.image_media_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn tool_call_new_constructor() {
        let tc = ToolCall::new("c1", "test", serde_json::json!({}));
        assert_eq!(tc.id, "c1");
        assert_eq!(tc.name, "test");
        assert_eq!(tc.raw_arguments, None);
    }

    #[test]
    fn stream_event_step_finish_constructor() {
        let response = Response {
            id: "resp_1".into(),
            model: "test-model".into(),
            provider: "test".into(),
            message: Message::assistant("tool response"),
            finish_reason: FinishReason::ToolCalls,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
                ..Default::default()
            },
            raw: None,
            warnings: vec![],
            rate_limit: None,
        };
        let tool_calls = vec![ToolCall::new(
            "call_1",
            "get_weather",
            serde_json::json!({"city": "SF"}),
        )];
        let tool_results = vec![ToolResult::success("call_1", serde_json::json!("72F"))];

        let event = StreamEvent::step_finish(
            FinishReason::ToolCalls,
            response.usage.clone(),
            response,
            tool_calls,
            tool_results,
        );

        match &event {
            StreamEvent::StepFinish {
                finish_reason,
                usage,
                tool_calls,
                tool_results,
                ..
            } => {
                assert_eq!(*finish_reason, FinishReason::ToolCalls);
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].name, "get_weather");
                assert_eq!(tool_results.len(), 1);
                assert_eq!(tool_results[0].tool_call_id, "call_1");
            }
            other => panic!("Expected StepFinish, got {other:?}"),
        }
    }

    #[test]
    fn tool_choice_mode_str_auto() {
        assert_eq!(ToolChoice::Auto.mode_str(), "auto");
    }

    #[test]
    fn tool_choice_mode_str_none() {
        assert_eq!(ToolChoice::None.mode_str(), "none");
    }

    #[test]
    fn tool_choice_mode_str_required() {
        assert_eq!(ToolChoice::Required.mode_str(), "required");
    }

    #[test]
    fn tool_choice_mode_str_named() {
        assert_eq!(ToolChoice::named("get_weather").mode_str(), "named");
    }
}
