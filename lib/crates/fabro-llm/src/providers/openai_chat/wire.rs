//! Serde wire types for the OpenAI Chat Completions request/response
//! protocol, plus the streaming-side `AccumulatedToolCall` helper.
//!
//! These structs are intentionally tiny and faithful to the wire — they
//! are shared between [`crate::providers::openai_compatible`] and (in
//! follow-up work) a dedicated OpenRouter adapter.

// --- Request types (Chat Completions format) ---

#[derive(serde::Serialize)]
pub(crate) struct ApiRequest {
    pub(crate) model:           String,
    pub(crate) messages:        Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature:     Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_tokens:      Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p:           Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop:            Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tools:           Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice:     Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream:          Option<bool>,
}

#[derive(serde::Serialize)]
pub(crate) struct ChatMessage {
    pub(crate) role:              String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content:           Option<String>,
    /// Reasoning/thinking content echoed back for providers that require it
    /// (Kimi).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id:      Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_calls:        Option<Vec<ChatToolCall>>,
}

#[derive(serde::Serialize)]
pub(crate) struct ChatToolCall {
    pub(crate) id:       String,
    #[serde(rename = "type")]
    pub(crate) kind:     String,
    pub(crate) function: ChatFunction,
}

#[derive(serde::Serialize)]
pub(crate) struct ChatFunction {
    pub(crate) name:      String,
    pub(crate) arguments: String,
}

// --- Response types (non-streaming) ---

#[derive(serde::Deserialize)]
pub(crate) struct ApiResponse {
    pub(crate) id:      String,
    pub(crate) model:   String,
    pub(crate) choices: Vec<ApiChoice>,
    pub(crate) usage:   Option<ApiUsage>,
}

#[derive(serde::Deserialize)]
pub(crate) struct ApiChoice {
    pub(crate) message:       ApiChoiceMessage,
    pub(crate) finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct ApiChoiceMessage {
    pub(crate) content:           Option<String>,
    pub(crate) reasoning_content: Option<String>,
    pub(crate) tool_calls:        Option<Vec<ApiToolCall>>,
}

#[derive(serde::Deserialize)]
pub(crate) struct ApiToolCall {
    pub(crate) id:       String,
    pub(crate) function: ApiFunction,
}

#[derive(serde::Deserialize)]
pub(crate) struct ApiFunction {
    pub(crate) name:      String,
    pub(crate) arguments: String,
}

#[derive(serde::Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the provider API payload."
)]
pub(crate) struct ApiUsage {
    pub(crate) prompt_tokens:         i64,
    pub(crate) completion_tokens:     i64,
    #[serde(default)]
    pub(crate) prompt_tokens_details: Option<ApiPromptTokensDetails>,
    #[serde(default)]
    pub(crate) cache_write_tokens:    Option<i64>,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct ApiPromptTokensDetails {
    #[serde(default)]
    pub(crate) cached_tokens: Option<i64>,
}

// --- Streaming response types ---

#[derive(serde::Deserialize)]
pub(crate) struct StreamChunk {
    pub(crate) id:      Option<String>,
    pub(crate) model:   Option<String>,
    pub(crate) choices: Option<Vec<StreamChoice>>,
    pub(crate) usage:   Option<ApiUsage>,
}

#[derive(serde::Deserialize)]
pub(crate) struct StreamChoice {
    pub(crate) delta:         Option<StreamDelta>,
    pub(crate) finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct StreamDelta {
    pub(crate) content:           Option<String>,
    /// Reasoning/thinking content (used by Kimi and other reasoning models).
    pub(crate) reasoning_content: Option<String>,
    pub(crate) tool_calls:        Option<Vec<StreamToolCall>>,
}

#[derive(serde::Deserialize)]
pub(crate) struct StreamToolCall {
    pub(crate) index:    usize,
    pub(crate) id:       Option<String>,
    pub(crate) function: Option<StreamFunction>,
}

#[derive(serde::Deserialize)]
pub(crate) struct StreamFunction {
    pub(crate) name:      Option<String>,
    pub(crate) arguments: Option<String>,
}

// --- Accumulated tool call state for streaming ---

pub(crate) struct AccumulatedToolCall {
    pub(crate) id:        String,
    pub(crate) name:      String,
    pub(crate) arguments: String,
    pub(crate) started:   bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_response_usage_parses_cached_tokens() {
        // Non-streaming response uses ApiResponse, which is a distinct
        // code path from the streaming StreamChunk parse. Verify the
        // same cached-token fields land on TokenCounts via that path.
        let json = r#"{"id":"chatcmpl-1","model":"anthropic/claude-sonnet-4.6","choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":80},"cache_write_tokens":12}}"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        let u = resp.usage.unwrap();
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(
            u.prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            Some(80)
        );
        assert_eq!(u.cache_write_tokens, Some(12));
    }
}
