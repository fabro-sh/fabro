//! Serde types mirroring the Gemini `generateContent` wire shapes.

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ApiRequest {
    pub contents:           Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<SystemInstruction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config:  Option<GenerationOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools:              Option<Vec<GeminiToolGroup>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config:        Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
pub(super) struct Content {
    pub role:  String,
    pub parts: Vec<serde_json::Value>,
}

#[derive(serde::Serialize)]
pub(super) struct SystemInstruction {
    pub parts: Vec<serde_json::Value>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GenerationOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature:        Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens:  Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p:              Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences:     Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_schema:    Option<serde_json::Value>,
}

/// Gemini groups function declarations under a `tools` array.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GeminiToolGroup {
    pub function_declarations: Vec<GeminiFunctionDecl>,
}

#[derive(serde::Serialize)]
pub(super) struct GeminiFunctionDecl {
    pub name:        String,
    pub description: String,
    pub parameters:  serde_json::Value,
}

// --- Response types ---

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ApiResponse {
    pub candidates:     Option<Vec<Candidate>>,
    pub usage_metadata: Option<UsageMetadata>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Candidate {
    pub content:       Option<CandidateContent>,
    pub finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct CandidateContent {
    pub parts: Option<Vec<serde_json::Value>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the provider API payload."
)]
pub(super) struct UsageMetadata {
    pub prompt_token_count:          Option<i64>,
    pub candidates_token_count:      Option<i64>,
    pub thoughts_token_count:        Option<i64>,
    pub cached_content_token_count:  Option<i64>,
    pub tool_use_prompt_token_count: Option<i64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CountTokensResponse {
    pub total_tokens: i64,
}
