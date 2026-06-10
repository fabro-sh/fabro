//! Serde types mirroring the Anthropic Messages wire shapes.

#[derive(serde::Serialize)]
pub(super) struct ApiRequest {
    pub model:          String,
    pub messages:       Vec<ApiMessage>,
    pub max_tokens:     i64,
    /// System prompt: either a plain string or an array of content blocks
    /// (with optional `cache_control` annotations for prompt caching).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system:         Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature:    Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p:          Option<f64>,
    /// Always serialized, even when empty (pinned by wire tests).
    pub stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools:          Option<Vec<ApiToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice:    Option<serde_json::Value>,
    /// Extended thinking configuration (e.g. `{"type": "enabled",
    /// "budget_tokens": 10000}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking:       Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config:  Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed:          Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata:       Option<std::collections::HashMap<String, String>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream:         bool,
}

#[derive(serde::Serialize)]
pub(super) struct CountTokensRequest {
    pub model:       String,
    pub messages:    Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system:      Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools:       Option<Vec<ApiToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking:    Option<serde_json::Value>,
}

impl From<ApiRequest> for CountTokensRequest {
    fn from(request: ApiRequest) -> Self {
        Self {
            model:       request.model,
            messages:    request.messages,
            system:      request.system,
            tools:       request.tools,
            tool_choice: request.tool_choice,
            thinking:    request.thinking,
        }
    }
}

/// Anthropic messages use structured content blocks, not plain strings.
#[derive(serde::Serialize)]
pub(super) struct ApiMessage {
    pub role:    String,
    pub content: Vec<serde_json::Value>,
}

/// Anthropic tool definition format.
#[derive(serde::Serialize)]
pub(super) struct ApiToolDef {
    pub name:          String,
    pub description:   String,
    pub input_schema:  serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// Anthropic `cache_control` annotation.
#[derive(serde::Serialize, Clone)]
pub(super) struct CacheControl {
    #[serde(rename = "type")]
    pub kind: String,
}

impl CacheControl {
    pub(super) fn ephemeral() -> Self {
        Self {
            kind: "ephemeral".to_string(),
        }
    }
}

// --- Response types ---

#[derive(serde::Deserialize)]
pub(super) struct ApiResponse {
    pub id:           String,
    pub model:        String,
    pub content:      Vec<serde_json::Value>,
    pub stop_reason:  Option<String>,
    #[serde(default)]
    pub stop_details: Option<serde_json::Value>,
    pub usage:        ApiUsage,
}

#[derive(serde::Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the provider API payload."
)]
pub(super) struct ApiUsage {
    pub input_tokens:                i64,
    pub output_tokens:               i64,
    #[serde(default)]
    pub cache_read_input_tokens:     Option<i64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<i64>,
}

#[derive(serde::Deserialize)]
pub(super) struct CountTokensResponse {
    pub input_tokens: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_serialization_includes_cache_control() {
        let tool = ApiToolDef {
            name:          "test_tool".to_string(),
            description:   "A test tool".to_string(),
            input_schema:  serde_json::json!({"type": "object"}),
            cache_control: Some(CacheControl::ephemeral()),
        };
        let json = serde_json::to_value(&tool).expect("should serialize");
        assert_eq!(json["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn tool_serialization_omits_cache_control_when_none() {
        let tool = ApiToolDef {
            name:          "test_tool".to_string(),
            description:   "A test tool".to_string(),
            input_schema:  serde_json::json!({"type": "object"}),
            cache_control: None,
        };
        let json = serde_json::to_value(&tool).expect("should serialize");
        assert!(json.get("cache_control").is_none());
    }
}
