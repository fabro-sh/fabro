//! Serde types mirroring the OpenAI Responses API wire shapes.

#[derive(serde::Serialize)]
pub(super) struct ApiRequest {
    pub model:             String,
    pub input:             Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions:      Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature:       Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p:             Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools:             Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice:       Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning:         Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text:              Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop:              Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata:          Option<std::collections::HashMap<String, String>>,
    pub store:             bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include:           Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream:            bool,
}

// --- Response types ---

#[derive(serde::Deserialize)]
pub(super) struct ApiResponse {
    pub id:     String,
    pub model:  Option<String>,
    pub output: Vec<serde_json::Value>,
    pub status: Option<String>,
    pub usage:  Option<ApiUsage>,
}

#[derive(serde::Deserialize)]
pub(super) struct InputTokensResponse {
    pub input_tokens: i64,
    pub object:       String,
}

#[derive(serde::Deserialize)]
pub(super) struct ApiUsage {
    pub input_tokens:          i64,
    pub output_tokens:         i64,
    pub output_tokens_details: Option<OutputTokenDetails>,
    pub input_tokens_details:  Option<InputTokenDetails>,
}

#[derive(serde::Deserialize)]
pub(super) struct OutputTokenDetails {
    pub reasoning_tokens: Option<i64>,
}

#[derive(serde::Deserialize)]
pub(super) struct InputTokenDetails {
    pub cached_tokens: Option<i64>,
}
