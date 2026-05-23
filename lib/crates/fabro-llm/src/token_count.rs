use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::types::{
    AudioData, ContentPart, DocumentData, ImageData, Message, Request, Role, ToolDefinition,
    ToolResult, Warning,
};

const LOCAL_ESTIMATE_WARNING: &str = "local_token_estimate";
const MEDIA_ESTIMATE_WARNING: &str = "media_token_estimate";
const OPAQUE_CONTEXT_ESTIMATE_WARNING: &str = "opaque_context_estimate";
const PROVIDER_OPTIONS_ESTIMATE_WARNING: &str = "provider_options_estimate";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputTokenCountPreference {
    PreferProvider,
    RequireProvider,
    EstimateOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputTokenCountMethod {
    ProviderApi,
    LocalEstimate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputTokenCount {
    pub input_tokens: i64,
    pub method:       InputTokenCountMethod,
    pub provider:     String,
    pub model:        String,
    #[serde(default)]
    pub warnings:     Vec<Warning>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalTokenEstimate {
    pub tokens:   usize,
    pub warnings: Vec<Warning>,
}

#[must_use]
pub fn estimate_input_tokens(request: &Request, provider: impl Into<String>) -> InputTokenCount {
    let mut estimator = Estimator::default();
    let mut tokens = 0usize;

    for message in &request.messages {
        tokens += estimator.estimate_message(message);
    }

    if let Some(tools) = &request.tools {
        tokens += tools.iter().map(estimate_tool).sum::<usize>();
    }

    tokens += estimator.estimate_request_controls(request);

    estimator.warn(
        LOCAL_ESTIMATE_WARNING,
        "input token count is a local estimate",
    );

    InputTokenCount {
        input_tokens: i64::try_from(tokens).unwrap_or(i64::MAX),
        method:       InputTokenCountMethod::LocalEstimate,
        provider:     provider.into(),
        model:        request.model.clone(),
        warnings:     estimator.warnings,
    }
}

#[must_use]
pub fn estimate_text_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[must_use]
pub fn estimate_json_tokens(value: &serde_json::Value) -> usize {
    serde_json::to_string(value).map_or(0, |json| json.len().div_ceil(4))
}

#[must_use]
pub fn estimate_message_tokens(message: &Message) -> LocalTokenEstimate {
    let mut estimator = Estimator::default();
    let tokens = estimator.estimate_message(message);
    LocalTokenEstimate {
        tokens,
        warnings: estimator.warnings,
    }
}

#[must_use]
pub fn estimate_content_part_tokens(part: &ContentPart) -> LocalTokenEstimate {
    let mut estimator = Estimator::default();
    let tokens = estimator.estimate_content_part(part);
    LocalTokenEstimate {
        tokens,
        warnings: estimator.warnings,
    }
}

#[must_use]
pub fn estimate_tool_definition_tokens(tool: &ToolDefinition) -> usize {
    estimate_tool(tool)
}

#[must_use]
pub fn estimate_request_control_tokens(request: &Request) -> LocalTokenEstimate {
    let mut estimator = Estimator::default();
    let tokens = estimator.estimate_request_controls(request);
    LocalTokenEstimate {
        tokens,
        warnings: estimator.warnings,
    }
}

#[derive(Default)]
struct Estimator {
    warnings:   Vec<Warning>,
    seen_codes: HashSet<&'static str>,
}

impl Estimator {
    fn estimate_message(&mut self, message: &Message) -> usize {
        let mut tokens = 4 + estimate_text_tokens(message.role_name());
        if let Some(name) = &message.name {
            tokens += estimate_text_tokens(name);
        }
        if let Some(tool_call_id) = &message.tool_call_id {
            tokens += estimate_text_tokens(tool_call_id);
        }
        for part in &message.content {
            tokens += 1 + self.estimate_content_part(part);
        }
        tokens
    }

    fn estimate_request_controls(&mut self, request: &Request) -> usize {
        let mut tokens = 0;
        if let Some(tool_choice) = &request.tool_choice {
            if let Ok(value) = serde_json::to_value(tool_choice) {
                tokens += estimate_json_tokens(&value);
            }
        }

        if let Some(response_format) = &request.response_format {
            if let Ok(value) = serde_json::to_value(response_format) {
                tokens += estimate_json_tokens(&value);
            }
        }

        if let Some(reasoning_effort) = request.reasoning_effort {
            tokens += estimate_text_tokens(reasoning_effort.to_string().as_str());
        }

        if let Some(provider_options) = &request.provider_options {
            tokens += estimate_json_tokens(provider_options);
            self.warn(
                PROVIDER_OPTIONS_ESTIMATE_WARNING,
                "provider options estimated from JSON",
            );
        }
        tokens
    }

    fn estimate_content_part(&mut self, part: &ContentPart) -> usize {
        match part {
            ContentPart::Text(text) => estimate_text_tokens(text),
            ContentPart::Image(image) => self.estimate_image(image),
            ContentPart::Audio(audio) => self.estimate_audio(audio),
            ContentPart::Document(document) => self.estimate_document(document),
            ContentPart::ToolCall(tool_call) => estimate_json_tokens(&serde_json::json!(tool_call)),
            ContentPart::ToolResult(result) => self.estimate_tool_result(result),
            ContentPart::Thinking(thinking) => {
                estimate_text_tokens(&thinking.text)
                    + thinking
                        .signature
                        .as_deref()
                        .map_or(0, estimate_text_tokens)
                    + usize::from(thinking.redacted)
            }
            ContentPart::Other { kind, data } => {
                self.warn(
                    OPAQUE_CONTEXT_ESTIMATE_WARNING,
                    "opaque provider context estimated from JSON",
                );
                estimate_text_tokens(kind) + estimate_json_tokens(data)
            }
        }
    }

    fn estimate_tool_result(&mut self, result: &ToolResult) -> usize {
        let mut tokens =
            estimate_text_tokens(&result.tool_call_id) + estimate_json_tokens(&result.content);
        if let Some(image_data) = &result.image_data {
            tokens += estimate_embedded_bytes(image_data.len());
            self.warn(
                MEDIA_ESTIMATE_WARNING,
                "media content estimated heuristically",
            );
        }
        if let Some(media_type) = &result.image_media_type {
            tokens += estimate_text_tokens(media_type);
        }
        tokens + usize::from(result.is_error)
    }

    fn estimate_image(&mut self, image: &ImageData) -> usize {
        let mut tokens =
            self.estimate_media_common(image.url.as_deref(), image.media_type.as_deref());
        if let Some(detail) = &image.detail {
            tokens += estimate_text_tokens(detail);
        }
        tokens
            + image
                .data
                .as_ref()
                .map_or(2000, |data| estimate_embedded_bytes(data.len()).max(2000))
    }

    fn estimate_audio(&mut self, audio: &AudioData) -> usize {
        let tokens = self.estimate_media_common(audio.url.as_deref(), audio.media_type.as_deref());
        tokens
            + audio
                .data
                .as_ref()
                .map_or(2000, |data| estimate_embedded_bytes(data.len()))
    }

    fn estimate_document(&mut self, document: &DocumentData) -> usize {
        let mut tokens =
            self.estimate_media_common(document.url.as_deref(), document.media_type.as_deref());
        if let Some(file_name) = &document.file_name {
            tokens += estimate_text_tokens(file_name);
        }
        tokens
            + document
                .data
                .as_ref()
                .map_or(2000, |data| estimate_embedded_bytes(data.len()))
    }

    fn estimate_media_common(&mut self, url: Option<&str>, media_type: Option<&str>) -> usize {
        self.warn(
            MEDIA_ESTIMATE_WARNING,
            "media content estimated heuristically",
        );
        url.map_or(0, estimate_text_tokens) + media_type.map_or(0, estimate_text_tokens)
    }

    fn warn(&mut self, code: &'static str, message: &'static str) {
        if self.seen_codes.insert(code) {
            self.warnings.push(Warning {
                message: message.to_string(),
                code:    Some(code.to_string()),
            });
        }
    }
}

fn estimate_tool(tool: &ToolDefinition) -> usize {
    8 + estimate_text_tokens(&tool.name)
        + estimate_text_tokens(&tool.description)
        + estimate_json_tokens(&tool.parameters)
}

fn estimate_embedded_bytes(byte_len: usize) -> usize {
    byte_len.div_ceil(4)
}

trait RoleName {
    fn role_name(&self) -> &'static str;
}

impl RoleName for Message {
    fn role_name(&self) -> &'static str {
        match self.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            Role::Developer => "developer",
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::types::{
        DocumentData, ImageData, Request, ResponseFormat, ResponseFormatType, ToolDefinition,
    };

    fn request(messages: Vec<Message>) -> Request {
        Request {
            model: "model-a".to_string(),
            messages,
            provider: Some("test".to_string()),
            tools: None,
            tool_choice: None,
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            reasoning_effort: None,
            speed: None,
            metadata: None,
            provider_options: None,
        }
    }

    fn warning_codes(count: &InputTokenCount) -> Vec<&str> {
        count
            .warnings
            .iter()
            .filter_map(|warning| warning.code.as_deref())
            .collect()
    }

    #[test]
    fn text_only_request_returns_positive_local_estimate() {
        let count = estimate_input_tokens(&request(vec![Message::user("hello world")]), "test");

        assert!(count.input_tokens > 0);
        assert_eq!(count.method, InputTokenCountMethod::LocalEstimate);
        assert!(warning_codes(&count).contains(&LOCAL_ESTIMATE_WARNING));
    }

    #[test]
    fn adding_tool_increases_estimate() {
        let mut with_tool = request(vec![Message::user("hello")]);
        let without_tool = estimate_input_tokens(&with_tool, "test");

        with_tool.tools = Some(vec![ToolDefinition::function(
            "search",
            "Search files",
            json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        )]);
        let with_tool = estimate_input_tokens(&with_tool, "test");

        assert!(with_tool.input_tokens > without_tool.input_tokens);
    }

    #[test]
    fn adding_response_format_increases_estimate() {
        let mut with_schema = request(vec![Message::user("hello")]);
        let without_schema = estimate_input_tokens(&with_schema, "test");

        with_schema.response_format = Some(ResponseFormat {
            kind:        ResponseFormatType::JsonSchema,
            json_schema: Some(
                json!({"type": "object", "properties": {"answer": {"type": "string"}}}),
            ),
            strict:      true,
        });
        let with_schema = estimate_input_tokens(&with_schema, "test");

        assert!(with_schema.input_tokens > without_schema.input_tokens);
    }

    #[test]
    fn media_content_gets_media_warning_and_sized_estimate() {
        let count = estimate_input_tokens(
            &request(vec![Message {
                role:         Role::User,
                content:      vec![
                    ContentPart::Image(ImageData {
                        url:        Some("https://example.test/image.png".to_string()),
                        data:       None,
                        media_type: Some("image/png".to_string()),
                        detail:     Some("high".to_string()),
                    }),
                    ContentPart::Document(DocumentData {
                        url:        None,
                        data:       Some(vec![0; 4096]),
                        media_type: Some("application/pdf".to_string()),
                        file_name:  Some("doc.pdf".to_string()),
                    }),
                ],
                name:         None,
                tool_call_id: None,
            }]),
            "test",
        );

        assert!(count.input_tokens >= 3000);
        assert!(warning_codes(&count).contains(&MEDIA_ESTIMATE_WARNING));
    }

    #[test]
    fn provider_options_produce_provider_options_warning() {
        let mut req = request(vec![Message::user("hello")]);
        req.provider_options = Some(json!({"gemini": {"cached_content": "cachedContents/1"}}));

        let count = estimate_input_tokens(&req, "test");

        assert!(warning_codes(&count).contains(&PROVIDER_OPTIONS_ESTIMATE_WARNING));
    }

    #[test]
    fn opaque_content_produces_opaque_warning() {
        let count = estimate_input_tokens(
            &request(vec![Message {
                role:         Role::Assistant,
                content:      vec![ContentPart::Other {
                    kind: "openai_reasoning".to_string(),
                    data: json!({"id": "rs_123", "summary": []}),
                }],
                name:         None,
                tool_call_id: None,
            }]),
            "test",
        );

        assert!(warning_codes(&count).contains(&OPAQUE_CONTEXT_ESTIMATE_WARNING));
    }

    #[test]
    fn estimate_is_deterministic() {
        let req = request(vec![Message::user("repeatable")]);

        assert_eq!(
            estimate_input_tokens(&req, "test"),
            estimate_input_tokens(&req, "test")
        );
    }
}
