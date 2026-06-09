//! Serde types mirroring the OpenAI Chat Completions wire shapes.

#[derive(serde::Serialize)]
pub(super) struct ApiRequest {
    pub model:           String,
    pub messages:        Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature:     Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens:      Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p:           Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop:            Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools:           Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice:     Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream:          Option<bool>,
}

#[derive(serde::Serialize)]
pub(super) struct ChatMessage {
    pub role:              String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content:           Option<String>,
    /// Reasoning/thinking content echoed back for providers that require it
    /// (Kimi).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id:      Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls:        Option<Vec<ChatToolCall>>,
}

#[derive(serde::Serialize)]
pub(super) struct ChatToolCall {
    pub id:       String,
    #[serde(rename = "type")]
    pub kind:     String,
    pub function: ChatFunction,
}

#[derive(serde::Serialize)]
pub(super) struct ChatFunction {
    pub name:      String,
    pub arguments: String,
}

// --- Response types (non-streaming) ---

#[derive(serde::Deserialize)]
pub(super) struct ApiResponse {
    pub id:      String,
    pub model:   String,
    pub choices: Vec<ApiChoice>,
    pub usage:   Option<ApiUsage>,
}

#[derive(serde::Deserialize)]
pub(super) struct ApiChoice {
    pub message:       ApiChoiceMessage,
    pub finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct ApiChoiceMessage {
    pub content:           Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls:        Option<Vec<ApiToolCall>>,
}

#[derive(serde::Deserialize)]
pub(super) struct ApiToolCall {
    pub id:       String,
    pub function: ApiFunction,
}

#[derive(serde::Deserialize)]
pub(super) struct ApiFunction {
    pub name:      String,
    pub arguments: String,
}

#[derive(serde::Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the provider API payload."
)]
pub(super) struct ApiUsage {
    pub prompt_tokens:     i64,
    pub completion_tokens: i64,
}

// --- Streaming response types ---

#[derive(serde::Deserialize)]
pub(super) struct StreamChunk {
    pub id:      Option<String>,
    pub model:   Option<String>,
    pub choices: Option<Vec<StreamChoice>>,
    pub usage:   Option<ApiUsage>,
}

#[derive(serde::Deserialize)]
pub(super) struct StreamChoice {
    pub delta:         Option<StreamDelta>,
    pub finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct StreamDelta {
    pub content:           Option<String>,
    /// Reasoning/thinking content (used by Kimi and other reasoning models).
    pub reasoning_content: Option<String>,
    pub tool_calls:        Option<Vec<StreamToolCall>>,
}

#[derive(serde::Deserialize)]
pub(super) struct StreamToolCall {
    pub index:    usize,
    pub id:       Option<String>,
    pub function: Option<StreamFunction>,
}

#[derive(serde::Deserialize)]
pub(super) struct StreamFunction {
    pub name:      Option<String>,
    pub arguments: Option<String>,
}

// --- Accumulated tool call state for streaming ---

pub(super) struct AccumulatedToolCall {
    pub id:        String,
    pub name:      String,
    pub arguments: String,
    pub started:   bool,
}
