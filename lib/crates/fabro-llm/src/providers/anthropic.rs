use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use futures::stream;

use crate::error::{Error, error_from_status_code};
use crate::provider::{ProviderAdapter, StreamEventStream, validate_tool_choice};
use crate::providers::common::{
    self as common, extract_system_prompt, parse_error_body, parse_rate_limit_headers,
    parse_retry_after, send_and_read_response,
};
use crate::types::{
    AdapterTimeout, ContentPart, FinishReason, Message, RateLimitInfo, Request, Response,
    ResponseFormatType, Role, StreamEvent, ThinkingData, TokenCounts, ToolCall, ToolChoice,
    ToolDefinition,
};

/// Provider adapter for the Anthropic Messages API.
pub struct Adapter {
    pub(crate) http: super::http_api::HttpApi,
    provider_name:   String,
}

impl Adapter {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http:          super::http_api::HttpApi::new(api_key, DEFAULT_BASE_URL),
            provider_name: "anthropic".to_string(),
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.http.base_url = base_url.into();
        self
    }

    #[must_use]
    pub fn with_default_headers(self, headers: std::collections::HashMap<String, String>) -> Self {
        Self {
            http: self.http.with_default_headers(headers),
            ..self
        }
    }

    #[must_use]
    pub fn with_timeout(self, timeout: AdapterTimeout) -> Self {
        Self {
            http: self.http.with_timeout(timeout),
            ..self
        }
    }

    fn messages_url(&self) -> String {
        format!("{}/messages", self.http.base_url)
    }

    /// Collect a streaming response into a single [`Response`].
    ///
    /// Used by non-Anthropic providers (e.g. Kimi) that require `stream=true`.
    async fn complete_via_stream(&self, request: &Request) -> Result<Response, Error> {
        use futures::StreamExt;

        let mut stream = self.stream(request).await?;
        let mut response: Option<Response> = None;

        while let Some(event) = stream.next().await {
            if let StreamEvent::Finish { response: r, .. } = event? {
                response = Some(*r);
            }
        }

        response.ok_or_else(|| Error::Stream {
            message: "complete_via_stream: stream ended without a Finish event".to_string(),
            source:  None,
        })
    }
}

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

// --- Request types ---

#[derive(serde::Serialize)]
struct ApiRequest {
    model:          String,
    messages:       Vec<ApiMessage>,
    max_tokens:     i64,
    /// System prompt: either a plain string or an array of content blocks
    /// (with optional `cache_control` annotations for prompt caching).
    #[serde(skip_serializing_if = "Option::is_none")]
    system:         Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature:    Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p:          Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools:          Option<Vec<ApiToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice:    Option<serde_json::Value>,
    /// Extended thinking configuration (e.g. `{"type": "enabled",
    /// "budget_tokens": 10000}`). Passed through from
    /// `provider_options.anthropic.thinking`.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking:       Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config:  Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed:          Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata:       Option<std::collections::HashMap<String, String>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream:         bool,
}

/// Anthropic messages use structured content blocks, not plain strings.
#[derive(serde::Serialize)]
struct ApiMessage {
    role:    String,
    content: Vec<serde_json::Value>,
}

/// Anthropic tool definition format.
#[derive(serde::Serialize)]
struct ApiToolDef {
    name:          String,
    description:   String,
    input_schema:  serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

/// Anthropic `cache_control` annotation.
#[derive(serde::Serialize, Clone)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: String,
}

impl CacheControl {
    fn ephemeral() -> Self {
        Self {
            kind: "ephemeral".to_string(),
        }
    }
}

// --- Response types ---

#[derive(serde::Deserialize)]
struct ApiResponse {
    id:          String,
    model:       String,
    content:     Vec<serde_json::Value>,
    stop_reason: Option<String>,
    usage:       ApiUsage,
}

#[derive(serde::Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the provider API payload."
)]
struct ApiUsage {
    input_tokens:                i64,
    output_tokens:               i64,
    #[serde(default)]
    cache_read_input_tokens:     Option<i64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<i64>,
}

/// Estimate reasoning tokens from thinking content blocks.
/// Anthropic does not provide a separate reasoning token count,
/// so we estimate by dividing the character count of thinking text by 4.
fn estimate_reasoning_tokens(content_parts: &[ContentPart]) -> Option<i64> {
    let total_chars: usize = content_parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Thinking(td) => Some(td.text.len()),
            _ => None,
        })
        .sum();
    if total_chars > 0 {
        Some(i64::try_from((total_chars / 4).max(1)).unwrap())
    } else {
        None
    }
}

fn map_finish_reason(stop_reason: Option<&str>) -> FinishReason {
    match stop_reason {
        Some("end_turn" | "stop_sequence") | None => FinishReason::Stop,
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCalls,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

fn parse_content_block(block: &serde_json::Value) -> Option<ContentPart> {
    match block.get("type")?.as_str()? {
        "text" => Some(ContentPart::text(block.get("text")?.as_str()?)),
        "tool_use" => Some(ContentPart::ToolCall(ToolCall::new(
            block.get("id")?.as_str()?,
            block.get("name")?.as_str()?,
            block.get("input")?.clone(),
        ))),
        "thinking" => Some(ContentPart::Thinking(ThinkingData {
            text:      block.get("thinking")?.as_str()?.to_string(),
            signature: block
                .get("signature")
                .and_then(serde_json::Value::as_str)
                .map(String::from),
            redacted:  false,
        })),
        "redacted_thinking" => Some(ContentPart::Thinking(ThinkingData {
            text:      block
                .get("data")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            signature: None,
            redacted:  true,
        })),
        _ => None,
    }
}

/// Translate a unified `ContentPart` to an Anthropic content block JSON value.
async fn content_part_to_api(part: &ContentPart) -> Option<serde_json::Value> {
    match part {
        ContentPart::Text(text) => Some(serde_json::json!({"type": "text", "text": text})),
        ContentPart::ToolCall(tc) => Some(serde_json::json!({
            "type": "tool_use",
            "id": tc.id,
            "name": tc.name,
            "input": tc.arguments,
        })),
        ContentPart::ToolResult(tr) => {
            let content = tr
                .content
                .as_str()
                .map_or_else(|| tr.content.to_string(), str::to_string);
            Some(serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tr.tool_call_id,
                "content": content,
                "is_error": tr.is_error,
            }))
        }
        ContentPart::Thinking(td) if td.redacted => Some(serde_json::json!({
            "type": "redacted_thinking",
            "data": td.text,
        })),
        ContentPart::Thinking(td) => {
            let mut block = serde_json::json!({
                "type": "thinking",
                "thinking": td.text,
            });
            if let Some(sig) = &td.signature {
                block["signature"] = serde_json::Value::String(sig.clone());
            }
            Some(block)
        }
        ContentPart::Image(img) => {
            if let Some(url) = &img.url {
                if common::is_file_path(url) {
                    return match common::load_file_as_base64(url).await {
                        Ok((b64, mime)) => Some(serde_json::json!({
                            "type": "image",
                            "source": {"type": "base64", "media_type": mime, "data": b64}
                        })),
                        Err(_) => None,
                    };
                }
                Some(serde_json::json!({"type": "image", "source": {"type": "url", "url": url}}))
            } else {
                img.data.as_ref().map(|data| {
                    let mime = img.media_type.as_deref().unwrap_or("image/png");
                    let b64 = BASE64_STANDARD.encode(data);
                    serde_json::json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": b64}})
                })
            }
        }
        ContentPart::Document(doc) => {
            if let Some(url) = &doc.url {
                if common::is_file_path(url) {
                    return match common::load_file_as_base64(url).await {
                        Ok((b64, mime)) => Some(serde_json::json!({
                            "type": "document",
                            "source": {"type": "base64", "media_type": mime, "data": b64}
                        })),
                        Err(_) => None,
                    };
                }
                Some(serde_json::json!({"type": "document", "source": {"type": "url", "url": url}}))
            } else {
                doc.data.as_ref().map(|data| {
                    let mime = doc.media_type.as_deref().unwrap_or("application/pdf");
                    let b64 = BASE64_STANDARD.encode(data);
                    serde_json::json!({"type": "document", "source": {"type": "base64", "media_type": mime, "data": b64}})
                })
            }
        }
        ContentPart::Audio(_) => Some(
            serde_json::json!({"type": "text", "text": "[Audio content not supported by this provider]"}),
        ),
        ContentPart::Other { .. } => None,
    }
}

/// Convert unified messages to Anthropic API messages.
///
/// Handles: role mapping, content block translation, strict alternation
/// (merging consecutive same-role messages), and tool results in user messages.
async fn translate_messages(messages: &[&Message]) -> Vec<ApiMessage> {
    let mut api_messages: Vec<ApiMessage> = Vec::new();

    for msg in messages {
        let role = match msg.role {
            Role::Assistant => "assistant",
            // Tool results go in user messages for Anthropic
            Role::User | Role::Tool => "user",
            // System and Developer are extracted separately
            Role::System | Role::Developer => continue,
        };

        let mut content = Vec::new();
        for part in &msg.content {
            if let Some(block) = content_part_to_api(part).await {
                content.push(block);
            }
        }

        if content.is_empty() {
            continue;
        }

        // Enforce strict user/assistant alternation by merging consecutive same-role
        // messages
        if let Some(last) = api_messages.last_mut() {
            if last.role == role {
                last.content.extend(content);
                continue;
            }
        }

        api_messages.push(ApiMessage {
            role: role.to_string(),
            content,
        });
    }

    api_messages
}

/// Translate unified `ToolDefinition` to Anthropic format.
fn translate_tools(tools: &[ToolDefinition]) -> Vec<ApiToolDef> {
    tools
        .iter()
        .map(|t| ApiToolDef {
            name:          t.name.clone(),
            description:   t.description.clone(),
            input_schema:  t.parameters.clone(),
            cache_control: None,
        })
        .collect()
}

/// Translate unified `ToolChoice` to Anthropic's `tool_choice` JSON.
fn translate_tool_choice(choice: &ToolChoice) -> Option<serde_json::Value> {
    match choice {
        ToolChoice::Auto => Some(serde_json::json!({"type": "auto"})),
        // Anthropic does not support tool_choice none with tools present.
        // The caller should omit tools from the request instead.
        ToolChoice::None => None,
        ToolChoice::Required => Some(serde_json::json!({"type": "any"})),
        ToolChoice::Named { tool_name } => {
            Some(serde_json::json!({"type": "tool", "name": tool_name}))
        }
    }
}

// --- Structured output (response_format) helpers ---

const SYNTHETIC_TOOL_NAME: &str = "json_output";

/// Apply `response_format` to the Anthropic API request by mutating tools,
/// `tool_choice`, and system.
///
/// For `JsonSchema`: injects a synthetic tool with the given schema and forces
/// the model to call it. For `JsonObject`: appends a JSON instruction to the
/// system prompt. For `Text`: no-op.
fn apply_response_format(
    request: &Request,
    api_tools: &mut Option<Vec<ApiToolDef>>,
    tool_choice: &mut Option<serde_json::Value>,
    system: &mut Option<serde_json::Value>,
) {
    let Some(format) = &request.response_format else {
        return;
    };

    match format.kind {
        ResponseFormatType::JsonSchema => {
            let schema = format
                .json_schema
                .clone()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            let synthetic_tool = ApiToolDef {
                name:          SYNTHETIC_TOOL_NAME.to_string(),
                description:   "Output the requested structured data".to_string(),
                input_schema:  schema,
                cache_control: None,
            };
            match api_tools {
                Some(tools) => tools.push(synthetic_tool),
                None => *api_tools = Some(vec![synthetic_tool]),
            }
            *tool_choice = Some(serde_json::json!({"type": "tool", "name": SYNTHETIC_TOOL_NAME}));
        }
        ResponseFormatType::JsonObject => {
            let json_instruction = "\n\nYou must respond with valid JSON only, no other text.";
            match system {
                Some(serde_json::Value::Array(blocks)) => {
                    // Append to the last text block's text
                    if let Some(last) = blocks.last_mut() {
                        if let Some(text) = last.get("text").and_then(serde_json::Value::as_str) {
                            let mut new_text = text.to_string();
                            new_text.push_str(json_instruction);
                            last["text"] = serde_json::Value::String(new_text);
                        }
                    } else {
                        blocks.push(
                            serde_json::json!({"type": "text", "text": json_instruction.trim()}),
                        );
                    }
                }
                Some(serde_json::Value::String(s)) => {
                    s.push_str(json_instruction);
                }
                None => {
                    *system = Some(serde_json::Value::String(
                        json_instruction.trim().to_string(),
                    ));
                }
                _ => {}
            }
        }
        ResponseFormatType::Text => {}
    }
}

/// Convert synthetic `tool_use` content blocks back to text content parts.
///
/// When `response_format` uses `JsonSchema` mode, the model responds with a
/// `tool_use` block for our synthetic tool. We extract its arguments as a JSON
/// text string.
fn convert_synthetic_tool_to_text(content_parts: Vec<ContentPart>) -> Vec<ContentPart> {
    content_parts
        .into_iter()
        .map(|part| match &part {
            ContentPart::ToolCall(tc) if tc.name == SYNTHETIC_TOOL_NAME => {
                ContentPart::text(tc.arguments.to_string())
            }
            _ => part,
        })
        .collect()
}

/// Check if the request uses `JsonSchema` `response_format`.
fn uses_json_schema_format(request: &Request) -> bool {
    request
        .response_format
        .as_ref()
        .is_some_and(|f| matches!(f.kind, ResponseFormatType::JsonSchema))
}

/// Convert a streaming event for `JsonSchema` mode: `tool_use` events for the
/// synthetic tool become text events, and the Finish event gets its content
/// parts and `finish_reason` adjusted.
fn convert_stream_event_for_json_schema(event: StreamEvent) -> StreamEvent {
    match event {
        StreamEvent::ToolCallStart { tool_call } if tool_call.name == SYNTHETIC_TOOL_NAME => {
            StreamEvent::TextStart { text_id: None }
        }
        StreamEvent::ToolCallDelta { tool_call } if tool_call.name == SYNTHETIC_TOOL_NAME => {
            // The delta's arguments field contains the partial JSON string
            let delta = match &tool_call.arguments {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            StreamEvent::TextDelta {
                delta,
                text_id: None,
            }
        }
        StreamEvent::ToolCallEnd { tool_call } if tool_call.name == SYNTHETIC_TOOL_NAME => {
            StreamEvent::TextEnd { text_id: None }
        }
        StreamEvent::Finish {
            mut response,
            usage,
            ..
        } => {
            response.message.content =
                convert_synthetic_tool_to_text(std::mem::take(&mut response.message.content));
            response.finish_reason = FinishReason::Stop;
            StreamEvent::Finish {
                finish_reason: FinishReason::Stop,
                usage,
                response,
            }
        }
        other => other,
    }
}

// --- Prompt caching helpers ---

const CACHE_BETA_HEADER: &str = "prompt-caching-2024-07-31";
const FAST_MODE_BETA_HEADER: &str = "fast-mode-2026-02-01";

/// Check whether auto-caching is disabled via `provider_options`.
///
/// Returns `true` if caching should be applied (the default).
/// Only returns `false` if `provider_options.anthropic.auto_cache` is
/// explicitly `false`. Extract the `thinking` configuration from
/// `provider_options.anthropic.thinking`.
fn extract_thinking_config(
    provider_options: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    provider_options
        .and_then(|opts| opts.get("anthropic"))
        .and_then(|anthropic| anthropic.get("thinking"))
        .cloned()
}

/// Map a reasoning effort level to a thinking `budget_tokens` value for models
/// that don't support the `output_config.effort` parameter (e.g.
/// claude-sonnet-4-5).
fn effort_to_budget_tokens(effort: &str, max_tokens: i64) -> i64 {
    let budget = match effort {
        "low" => max_tokens / 4,
        "high" => max_tokens * 3 / 4,
        "xhigh" => max_tokens * 7 / 8,
        "max" => max_tokens,
        _ => max_tokens / 2, // "medium" or unknown
    };
    // Anthropic requires budget_tokens >= 1024
    budget.max(1024)
}

fn is_auto_cache_enabled(provider_options: Option<&serde_json::Value>) -> bool {
    provider_options
        .and_then(|opts| opts.get("anthropic"))
        .and_then(|anthropic| anthropic.get("auto_cache"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

/// Wrap a system prompt string as an array of content blocks with
/// `cache_control` on the last block.
fn system_with_cache_control(system: &str) -> serde_json::Value {
    serde_json::json!([{
        "type": "text",
        "text": system,
        "cache_control": {"type": "ephemeral"}
    }])
}

/// Add `cache_control` to the last tool definition.
fn apply_cache_control_to_last_tool(tools: &mut [ApiToolDef]) {
    if let Some(last) = tools.last_mut() {
        last.cache_control = Some(CacheControl::ephemeral());
    }
}

/// Add `cache_control` to the last content block of the second-to-last user
/// message.
///
/// In a multi-turn conversation, the conversation prefix (everything before the
/// latest user turn) is stable and benefits from caching. We find the last user
/// message before the final one and annotate its last content block.
fn apply_cache_control_to_conversation_prefix(messages: &mut [ApiMessage]) {
    // Find all user message indices
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();

    // We need at least 2 user messages to have a "prefix" user message
    if user_indices.len() < 2 {
        return;
    }

    // The second-to-last user message is the one to cache
    let target_idx = user_indices[user_indices.len() - 2];
    if let Some(serde_json::Value::Object(map)) = messages[target_idx].content.last_mut() {
        map.insert(
            "cache_control".to_string(),
            serde_json::json!({"type": "ephemeral"}),
        );
    }
}

/// Collect beta headers from `provider_options` and merge with the caching
/// header when auto-caching is active.
const CONTEXT_1M_BETA_HEADER: &str = "context-1m-2025-08-07";

fn build_beta_header(
    provider_options: Option<&serde_json::Value>,
    include_cache_header: bool,
    include_fast_mode_header: bool,
    include_1m_context: bool,
) -> Option<String> {
    let mut headers: Vec<String> = Vec::new();

    // Add user-provided beta headers
    if let Some(beta_array) = provider_options
        .and_then(|opts| opts.get("anthropic"))
        .and_then(|anthropic| anthropic.get("beta_headers"))
        .and_then(serde_json::Value::as_array)
    {
        headers.extend(
            beta_array
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from),
        );
    }

    // Add prompt-caching header if caching is active and not already present
    if include_cache_header && !headers.iter().any(|h| h == CACHE_BETA_HEADER) {
        headers.push(CACHE_BETA_HEADER.to_string());
    }

    // Add fast-mode header if speed=fast and not already present
    if include_fast_mode_header && !headers.iter().any(|h| h == FAST_MODE_BETA_HEADER) {
        headers.push(FAST_MODE_BETA_HEADER.to_string());
    }

    // Add 1M context header for models with >= 1M context window
    if include_1m_context && !headers.iter().any(|h| h == CONTEXT_1M_BETA_HEADER) {
        headers.push(CONTEXT_1M_BETA_HEADER.to_string());
    }

    if headers.is_empty() {
        None
    } else {
        Some(headers.join(","))
    }
}

// --- Streaming types and helpers ---

/// The type of the current content block being streamed.
#[derive(Clone)]
enum ContentBlockKind {
    Text,
    ToolUse { id: String, name: String },
    Thinking { signature: Option<String> },
}

/// Accumulated state across SSE events during streaming.
struct StreamAccumulator {
    id:                String,
    model:             String,
    content_parts:     Vec<ContentPart>,
    usage:             TokenCounts,
    finish_reason:     FinishReason,
    /// The kind of the current content block, set by `content_block_start`.
    current_block:     Option<ContentBlockKind>,
    /// Accumulated text for the current text block.
    current_text:      String,
    /// Accumulated thinking text for the current thinking block.
    current_thinking:  String,
    /// Accumulated raw JSON arguments for the current `tool_use` block.
    current_tool_args: String,
    /// Rate limit info parsed from the initial HTTP response headers.
    rate_limit:        Option<RateLimitInfo>,
}

impl StreamAccumulator {
    fn new(rate_limit: Option<RateLimitInfo>) -> Self {
        Self {
            id: String::new(),
            model: String::new(),
            content_parts: Vec::new(),
            usage: TokenCounts::default(),
            finish_reason: FinishReason::Stop,
            current_block: None,
            current_text: String::new(),
            current_thinking: String::new(),
            current_tool_args: String::new(),
            rate_limit,
        }
    }

    /// Build the final `Response` from accumulated state, consuming content
    /// parts.
    fn take_response(&mut self) -> Response {
        let content_parts = std::mem::take(&mut self.content_parts);
        Response {
            id:            self.id.clone(),
            model:         self.model.clone(),
            provider:      "anthropic".to_string(),
            message:       Message {
                role:         Role::Assistant,
                content:      content_parts,
                name:         None,
                tool_call_id: None,
            },
            finish_reason: self.finish_reason.clone(),
            usage:         self.usage.clone(),
            raw:           None,
            warnings:      vec![],
            rate_limit:    self.rate_limit.clone(),
        }
    }
}

impl StreamAccumulator {
    fn handle_message_start(&mut self, data: &serde_json::Value) -> Vec<StreamEvent> {
        if let Some(message) = data.get("message") {
            if let Some(id) = message.get("id").and_then(serde_json::Value::as_str) {
                self.id = id.to_string();
            }
            if let Some(model) = message.get("model").and_then(serde_json::Value::as_str) {
                self.model = model.to_string();
            }
            if let Some(usage) = message.get("usage") {
                self.usage.input_tokens = usage
                    .get("input_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                self.usage.cache_read_tokens = usage
                    .get("cache_read_input_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                self.usage.cache_write_tokens = usage
                    .get("cache_creation_input_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
            }
        }
        vec![StreamEvent::StreamStart]
    }

    fn handle_content_block_start(&mut self, data: &serde_json::Value) -> Vec<StreamEvent> {
        let block_type = data
            .get("content_block")
            .and_then(|b| b.get("type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        let index = data
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let text_id = Some(format!("block_{index}"));

        match block_type {
            "text" => {
                self.current_block = Some(ContentBlockKind::Text);
                self.current_text.clear();
                vec![StreamEvent::TextStart { text_id }]
            }
            "tool_use" => {
                let content_block = data.get("content_block");
                let id = content_block
                    .and_then(|b| b.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = content_block
                    .and_then(|b| b.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.current_block = Some(ContentBlockKind::ToolUse {
                    id:   id.clone(),
                    name: name.clone(),
                });
                self.current_tool_args.clear();
                vec![StreamEvent::ToolCallStart {
                    tool_call: ToolCall::new(id, name, serde_json::json!({})),
                }]
            }
            "thinking" => {
                let signature = data
                    .get("content_block")
                    .and_then(|b| b.get("signature"))
                    .and_then(serde_json::Value::as_str)
                    .map(String::from);
                self.current_block = Some(ContentBlockKind::Thinking { signature });
                self.current_thinking.clear();
                vec![StreamEvent::ReasoningStart]
            }
            _ => vec![],
        }
    }

    fn handle_content_block_delta(&mut self, data: &serde_json::Value) -> Vec<StreamEvent> {
        let delta = data.get("delta");
        let delta_type = delta
            .and_then(|d| d.get("type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        match delta_type {
            "text_delta" => {
                let text = delta
                    .and_then(|d| d.get("text"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                self.current_text.push_str(text);

                let index = data
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);

                vec![StreamEvent::TextDelta {
                    delta:   text.to_string(),
                    text_id: Some(format!("block_{index}")),
                }]
            }
            "input_json_delta" => {
                let partial_json = delta
                    .and_then(|d| d.get("partial_json"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                self.current_tool_args.push_str(partial_json);

                if let Some(ContentBlockKind::ToolUse { id, name }) = &self.current_block {
                    vec![StreamEvent::ToolCallDelta {
                        tool_call: ToolCall::new(
                            id.clone(),
                            name.clone(),
                            serde_json::json!(partial_json),
                        ),
                    }]
                } else {
                    vec![]
                }
            }
            "thinking_delta" => {
                let thinking = delta
                    .and_then(|d| d.get("thinking"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                self.current_thinking.push_str(thinking);
                vec![StreamEvent::ReasoningDelta {
                    delta: thinking.to_string(),
                }]
            }
            "signature_delta" => {
                let signature = delta
                    .and_then(|d| d.get("signature"))
                    .and_then(serde_json::Value::as_str)
                    .map(String::from);
                if let Some(ContentBlockKind::Thinking {
                    signature: ref mut sig,
                }) = self.current_block
                {
                    *sig = signature;
                }
                vec![]
            }
            _ => vec![],
        }
    }

    fn handle_content_block_stop(&mut self, data: &serde_json::Value) -> Vec<StreamEvent> {
        let current_block = self.current_block.take();
        match current_block {
            Some(ContentBlockKind::Text) => {
                let text = std::mem::take(&mut self.current_text);
                self.content_parts.push(ContentPart::text(&text));

                let index = data
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);

                vec![StreamEvent::TextEnd {
                    text_id: Some(format!("block_{index}")),
                }]
            }
            Some(ContentBlockKind::ToolUse { id, name }) => {
                let raw_args = std::mem::take(&mut self.current_tool_args);
                let arguments =
                    serde_json::from_str(&raw_args).unwrap_or_else(|_| serde_json::json!({}));
                let mut tool_call = ToolCall::new(id, name, arguments);
                tool_call.raw_arguments = Some(raw_args);
                self.content_parts
                    .push(ContentPart::ToolCall(tool_call.clone()));
                vec![StreamEvent::ToolCallEnd { tool_call }]
            }
            Some(ContentBlockKind::Thinking { signature }) => {
                let thinking_text = std::mem::take(&mut self.current_thinking);
                // Prefer signature from content_block_stop if available,
                // fall back to one captured at content_block_start.
                let stop_signature = data
                    .get("content_block")
                    .and_then(|b| b.get("signature"))
                    .and_then(serde_json::Value::as_str)
                    .map(String::from);
                self.content_parts.push(ContentPart::Thinking(ThinkingData {
                    text:      thinking_text,
                    signature: stop_signature.or(signature),
                    redacted:  false,
                }));
                vec![StreamEvent::ReasoningEnd]
            }
            None => vec![],
        }
    }

    fn handle_message_delta(&mut self, data: &serde_json::Value) {
        if let Some(delta) = data.get("delta") {
            let stop_reason = delta.get("stop_reason").and_then(serde_json::Value::as_str);
            self.finish_reason = map_finish_reason(stop_reason);
        }
        if let Some(usage) = data.get("usage") {
            self.usage.output_tokens = usage
                .get("output_tokens")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
        }
    }

    fn handle_message_stop(&mut self) -> Vec<StreamEvent> {
        let reasoning_tokens = estimate_reasoning_tokens(&self.content_parts).unwrap_or(0);
        self.usage.reasoning_tokens = reasoning_tokens;
        self.usage.output_tokens = self.usage.output_tokens.saturating_sub(reasoning_tokens);
        let response = self.take_response();
        vec![StreamEvent::Finish {
            finish_reason: response.finish_reason.clone(),
            usage:         response.usage.clone(),
            response:      Box::new(response),
        }]
    }
}

/// Process a single SSE event and return zero or more `StreamEvent`s.
fn process_sse_event(
    event_type: &str,
    data: &serde_json::Value,
    acc: &mut StreamAccumulator,
) -> Vec<StreamEvent> {
    match event_type {
        "message_start" => acc.handle_message_start(data),
        "content_block_start" => acc.handle_content_block_start(data),
        "content_block_delta" => acc.handle_content_block_delta(data),
        "content_block_stop" => acc.handle_content_block_stop(data),
        "message_delta" => {
            acc.handle_message_delta(data);
            vec![]
        }
        "message_stop" => acc.handle_message_stop(),
        _ => vec![],
    }
}

// --- SSE reader ---

enum SseResult {
    Event {
        event_type: String,
        data:       String,
    },
    Done,
    Error(Error),
}

struct SseReaderState {
    line_reader:      super::common::LineReader,
    accumulator:      StreamAccumulator,
    pending_events:   std::collections::VecDeque<StreamEvent>,
    /// When true, `tool_use` events for the synthetic tool are converted to
    /// text events.
    json_schema_mode: bool,
}

impl SseReaderState {
    fn new(
        http_resp: fabro_http::Response,
        rate_limit: Option<RateLimitInfo>,
        json_schema_mode: bool,
        stream_read_timeout: Option<std::time::Duration>,
    ) -> Self {
        Self {
            line_reader: super::common::LineReader::new(http_resp, stream_read_timeout),
            accumulator: StreamAccumulator::new(rate_limit),
            pending_events: std::collections::VecDeque::new(),
            json_schema_mode,
        }
    }

    /// Read the next complete SSE event from the byte stream.
    ///
    /// SSE events are separated by double newlines. Each event has optional
    /// `event:` and `data:` lines.
    async fn next_sse_event(&mut self) -> SseResult {
        loop {
            match self.line_reader.read_next_chunk("\n\n").await {
                Ok(Some(event_block)) => {
                    if let Some(result) = Self::parse_event_block(&event_block) {
                        return result;
                    }
                    // No data in this block (e.g. heartbeat comment); keep
                    // reading.
                }
                Ok(None) => return SseResult::Done,
                Err(e) => return SseResult::Error(e),
            }
        }
    }

    /// Parse an SSE event block into an `SseResult`.
    ///
    /// Returns `None` for blocks with no `data:` lines (e.g. heartbeat
    /// comments).
    fn parse_event_block(event_block: &str) -> Option<SseResult> {
        let mut event_type = String::new();
        let mut data_parts: Vec<String> = Vec::new();

        for line in event_block.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                event_type = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_parts.push(rest.trim().to_string());
            }
            // Ignore other SSE fields (id:, retry:, comments starting with :)
        }

        // Skip events with no data (e.g. heartbeat comments).
        if data_parts.is_empty() {
            return None;
        }

        let data = data_parts.join("\n");
        Some(SseResult::Event { event_type, data })
    }
}

/// Known `provider_options.anthropic` keys that are already handled by the
/// adapter and should not be merged into the request body a second time.
const KNOWN_ANTHROPIC_OPTION_KEYS: &[&str] = &["thinking", "auto_cache", "beta_headers"];

/// Serialize the API request and merge any unknown `provider_options.anthropic`
/// keys.
fn merge_provider_options(
    api_request: &ApiRequest,
    provider_options: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut body = serde_json::to_value(api_request).unwrap_or_else(|_| serde_json::json!({}));

    if let Some(anthropic_opts) = provider_options.and_then(|opts| opts.get("anthropic")) {
        if let (Some(base), Some(overrides)) = (body.as_object_mut(), anthropic_opts.as_object()) {
            for (key, value) in overrides {
                if !KNOWN_ANTHROPIC_OPTION_KEYS.contains(&key.as_str()) {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
    }

    body
}

/// Build an Anthropic API request and HTTP request builder for the given
/// unified request.
async fn build_api_request(
    adapter: &Adapter,
    request: &Request,
    stream: bool,
) -> (ApiRequest, fabro_http::RequestBuilder) {
    let (system, other_messages) = extract_system_prompt(&request.messages);
    let mut api_messages = translate_messages(&other_messages).await;

    let mut omit_tools = false;
    let tool_choice_json = request.tool_choice.as_ref().and_then(|tc| {
        if matches!(tc, ToolChoice::None) {
            omit_tools = true;
            None
        } else {
            translate_tool_choice(tc)
        }
    });

    let mut api_tools = if omit_tools {
        None
    } else {
        request.tools.as_ref().map(|t| translate_tools(t))
    };

    let auto_cache = is_auto_cache_enabled(request.provider_options.as_ref());

    let mut system_value = system.and_then(|s| {
        if s.trim().is_empty() {
            None
        } else if auto_cache {
            Some(system_with_cache_control(&s))
        } else {
            Some(serde_json::Value::String(s))
        }
    });

    // Apply response_format (may inject synthetic tool or system prompt suffix)
    let mut tool_choice_json = tool_choice_json;
    apply_response_format(
        request,
        &mut api_tools,
        &mut tool_choice_json,
        &mut system_value,
    );

    if auto_cache {
        if let Some(ref mut tools) = api_tools {
            apply_cache_control_to_last_tool(tools);
        }
        apply_cache_control_to_conversation_prefix(&mut api_messages);
    }

    let explicit_thinking = extract_thinking_config(request.provider_options.as_ref());

    // Check whether this model supports the `output_config.effort` parameter.
    // Older reasoning models (e.g. claude-sonnet-4-5) need `thinking` with
    // `budget_tokens` instead.
    let model_info = fabro_model::Catalog::builtin().get(&request.model);
    let supports_effort = model_info.is_none_or(|m| m.features.effort);

    let mut resolved_max_tokens = request
        .max_tokens
        .or_else(|| model_info.and_then(|m| m.limits.max_output))
        .unwrap_or(65536);

    let (thinking, output_config) = if let Some(effort) = &request.reasoning_effort {
        if supports_effort {
            (
                explicit_thinking,
                Some(serde_json::json!({"effort": effort.as_str()})),
            )
        } else if explicit_thinking.is_none() {
            // Convert effort level to a thinking budget for models that don't
            // support the effort parameter (e.g. claude-sonnet-4-5).
            let budget = effort_to_budget_tokens(effort.as_str(), resolved_max_tokens);
            if resolved_max_tokens <= budget {
                resolved_max_tokens = budget + 1024;
            }
            (
                Some(serde_json::json!({"type": "enabled", "budget_tokens": budget})),
                None,
            )
        } else {
            // thinking already configured via provider_options; skip output_config
            (explicit_thinking, None)
        }
    } else {
        // Auto-set adaptive thinking for known effort-capable models when no
        // explicit thinking config or reasoning_effort is provided.
        let thinking = explicit_thinking.or_else(|| {
            if model_info.is_some_and(|m| m.features.effort) {
                Some(serde_json::json!({"type": "adaptive"}))
            } else {
                None
            }
        });
        (thinking, None)
    };

    let is_fast = request.speed.as_deref() == Some("fast");

    let api_request = ApiRequest {
        model: request.model.clone(),
        messages: api_messages,
        max_tokens: resolved_max_tokens,
        system: system_value,
        temperature: request.temperature,
        top_p: request.top_p,
        stop_sequences: request.stop_sequences.clone(),
        tools: api_tools,
        tool_choice: tool_choice_json,
        thinking,
        output_config,
        speed: request.speed.clone(),
        metadata: request.metadata.clone(),
        stream,
    };

    let url = adapter.messages_url();
    let mut req_builder = adapter.http.client.post(&url);
    // Apply default_headers first so adapter-specific headers can override
    for (key, value) in &adapter.http.default_headers {
        req_builder = req_builder.header(key, value);
    }

    if adapter.provider_name == "anthropic" {
        req_builder = req_builder
            .header("x-api-key", &adapter.http.api_key)
            .header("anthropic-version", "2023-06-01");

        let include_1m_context = model_info.is_some_and(|m| m.context_window() >= 1_000_000);
        if let Some(beta_str) = build_beta_header(
            request.provider_options.as_ref(),
            auto_cache,
            is_fast,
            include_1m_context,
        ) {
            req_builder = req_builder.header("anthropic-beta", beta_str);
        }
    } else {
        req_builder = req_builder.bearer_auth(&adapter.http.api_key);
    }

    let req_builder = req_builder.json(&merge_provider_options(
        &api_request,
        request.provider_options.as_ref(),
    ));
    (api_request, req_builder)
}

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn complete(&self, request: &Request) -> Result<Response, Error> {
        if let Some(tc) = &request.tool_choice {
            validate_tool_choice(self, tc)?;
        }

        // Non-Anthropic providers (e.g. Kimi) require stream=true even for
        // blocking calls.  Collect the stream into a single Response.
        if self.provider_name != "anthropic" {
            return self.complete_via_stream(request).await;
        }

        let (_api_request, req_builder) = build_api_request(self, request, false).await;

        let mut req = req_builder;
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }
        let (body, headers) = send_and_read_response(req, &self.provider_name, "type").await?;

        let api_resp: ApiResponse = serde_json::from_str(&body).map_err(|e| {
            Error::network(
                format!("failed to parse {} response: {e}", self.provider_name),
                e,
            )
        })?;

        let content_parts: Vec<ContentPart> = api_resp
            .content
            .iter()
            .filter_map(parse_content_block)
            .collect();

        // If we used JsonSchema mode, convert the synthetic tool call back to text
        let content_parts = if uses_json_schema_format(request) {
            convert_synthetic_tool_to_text(content_parts)
        } else {
            content_parts
        };

        let finish_reason = if uses_json_schema_format(request) {
            // The model was forced to call a tool, so stop_reason is "tool_use",
            // but from the caller's perspective, the request completed normally.
            FinishReason::Stop
        } else {
            map_finish_reason(api_resp.stop_reason.as_deref())
        };
        let reasoning_tokens = estimate_reasoning_tokens(&content_parts).unwrap_or(0);

        Ok(Response {
            id: api_resp.id,
            model: api_resp.model,
            provider: self.provider_name.clone(),
            message: Message {
                role:         Role::Assistant,
                content:      content_parts,
                name:         None,
                tool_call_id: None,
            },
            finish_reason,
            usage: TokenCounts {
                input_tokens: api_resp.usage.input_tokens,
                output_tokens: api_resp
                    .usage
                    .output_tokens
                    .saturating_sub(reasoning_tokens),
                reasoning_tokens,
                cache_read_tokens: api_resp.usage.cache_read_input_tokens.unwrap_or(0),
                cache_write_tokens: api_resp.usage.cache_creation_input_tokens.unwrap_or(0),
            },
            raw: serde_json::from_str(&body).ok(),
            warnings: vec![],
            rate_limit: parse_rate_limit_headers(&headers),
        })
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        if let Some(tc) = &request.tool_choice {
            validate_tool_choice(self, tc)?;
        }
        let (_api_request, req_builder) = build_api_request(self, request, true).await;

        let http_resp = req_builder
            .send()
            .await
            .map_err(|e| Error::network(e.to_string(), e))?;

        let status = http_resp.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(http_resp.headers());
            let body = http_resp
                .text()
                .await
                .map_err(|e| Error::network(e.to_string(), e))?;
            let (msg, code, raw) = parse_error_body(&body, "type");
            return Err(error_from_status_code(
                status.as_u16(),
                msg,
                self.provider_name.clone(),
                code,
                raw,
                retry_after,
            ));
        }

        let rate_limit = parse_rate_limit_headers(http_resp.headers());
        let json_schema_mode = uses_json_schema_format(request);
        let stream_read_timeout = self.http.stream_read_timeout;

        let stream = stream::unfold(
            SseReaderState::new(http_resp, rate_limit, json_schema_mode, stream_read_timeout),
            |mut state| async move {
                loop {
                    // Drain any buffered events first.
                    if let Some(event) = state.pending_events.pop_front() {
                        let event = if state.json_schema_mode {
                            convert_stream_event_for_json_schema(event)
                        } else {
                            event
                        };
                        return Some((Ok(event), state));
                    }

                    // Read more SSE data from the byte stream.
                    match state.next_sse_event().await {
                        SseResult::Event { event_type, data } => {
                            let parsed: serde_json::Value = match serde_json::from_str(&data) {
                                Ok(v) => v,
                                Err(e) => {
                                    return Some((
                                        Err(Error::stream_error(
                                            format!("failed to parse SSE data: {e}"),
                                            e,
                                        )),
                                        state,
                                    ));
                                }
                            };
                            let events =
                                process_sse_event(&event_type, &parsed, &mut state.accumulator);
                            state.pending_events.extend(events);
                            // Loop to drain from pending_events.
                        }
                        SseResult::Done => return None,
                        SseResult::Error(err) => return Some((Err(err), state)),
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }

    fn supports_tool_choice(&self, mode: &str) -> bool {
        matches!(mode, "auto" | "none" | "required" | "named")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AudioData, DocumentData, ReasoningEffort, ResponseFormat};

    #[test]
    fn adapter_with_name() {
        let adapter = Adapter::new("key").with_name("kimi");
        assert_eq!(adapter.name(), "kimi");
    }

    #[test]
    fn adapter_default_name() {
        let adapter = Adapter::new("key");
        assert_eq!(adapter.name(), "anthropic");
    }

    #[test]
    fn auto_cache_enabled_by_default() {
        assert!(is_auto_cache_enabled(None));
    }

    #[test]
    fn auto_cache_enabled_when_true() {
        let opts = serde_json::json!({"anthropic": {"auto_cache": true}});
        assert!(is_auto_cache_enabled(Some(&opts)));
    }

    #[test]
    fn auto_cache_disabled_when_false() {
        let opts = serde_json::json!({"anthropic": {"auto_cache": false}});
        assert!(!is_auto_cache_enabled(Some(&opts)));
    }

    #[test]
    fn auto_cache_enabled_when_key_missing() {
        let opts = serde_json::json!({"anthropic": {}});
        assert!(is_auto_cache_enabled(Some(&opts)));
    }

    #[test]
    fn auto_cache_enabled_when_anthropic_missing() {
        let opts = serde_json::json!({"openai": {}});
        assert!(is_auto_cache_enabled(Some(&opts)));
    }

    #[test]
    fn system_prompt_cache_control_wraps_as_array() {
        let result = system_with_cache_control("You are helpful.");
        let arr = result.as_array().expect("should be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "You are helpful.");
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn tool_cache_control_applied_to_last_tool() {
        let mut tools = vec![
            ApiToolDef {
                name:          "tool_a".to_string(),
                description:   "first".to_string(),
                input_schema:  serde_json::json!({}),
                cache_control: None,
            },
            ApiToolDef {
                name:          "tool_b".to_string(),
                description:   "second".to_string(),
                input_schema:  serde_json::json!({}),
                cache_control: None,
            },
        ];
        apply_cache_control_to_last_tool(&mut tools);

        assert!(tools[0].cache_control.is_none());
        assert!(tools[1].cache_control.is_some());
        assert_eq!(tools[1].cache_control.as_ref().unwrap().kind, "ephemeral");
    }

    #[test]
    fn tool_cache_control_empty_slice() {
        let mut tools: Vec<ApiToolDef> = vec![];
        apply_cache_control_to_last_tool(&mut tools);
        assert!(tools.is_empty());
    }

    #[test]
    fn tool_cache_control_single_tool() {
        let mut tools = vec![ApiToolDef {
            name:          "only_tool".to_string(),
            description:   "the one".to_string(),
            input_schema:  serde_json::json!({}),
            cache_control: None,
        }];
        apply_cache_control_to_last_tool(&mut tools);
        assert!(tools[0].cache_control.is_some());
    }

    #[test]
    fn conversation_prefix_cache_control_with_two_user_messages() {
        let mut messages = vec![
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
            },
            ApiMessage {
                role:    "assistant".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hi there"})],
            },
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "How are you?"})],
            },
        ];

        apply_cache_control_to_conversation_prefix(&mut messages);

        // First user message should have cache_control
        assert_eq!(messages[0].content[0]["cache_control"]["type"], "ephemeral");
        // Last user message should NOT have cache_control
        assert!(messages[2].content[0].get("cache_control").is_none());
        // Assistant message should NOT have cache_control
        assert!(messages[1].content[0].get("cache_control").is_none());
    }

    #[test]
    fn conversation_prefix_cache_control_with_multiple_content_blocks() {
        let mut messages = vec![
            ApiMessage {
                role:    "user".to_string(),
                content: vec![
                    serde_json::json!({"type": "text", "text": "Part 1"}),
                    serde_json::json!({"type": "text", "text": "Part 2"}),
                ],
            },
            ApiMessage {
                role:    "assistant".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Reply"})],
            },
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Follow up"})],
            },
        ];

        apply_cache_control_to_conversation_prefix(&mut messages);

        // Only the LAST content block of the first user message should have
        // cache_control
        assert!(messages[0].content[0].get("cache_control").is_none());
        assert_eq!(messages[0].content[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn conversation_prefix_cache_control_single_user_message() {
        let mut messages = vec![ApiMessage {
            role:    "user".to_string(),
            content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
        }];

        apply_cache_control_to_conversation_prefix(&mut messages);

        // With only one user message, no cache_control should be added
        assert!(messages[0].content[0].get("cache_control").is_none());
    }

    #[test]
    fn conversation_prefix_cache_control_no_user_messages() {
        let mut messages: Vec<ApiMessage> = vec![];
        // Should not panic on empty messages
        apply_cache_control_to_conversation_prefix(&mut messages);
    }

    #[test]
    fn conversation_prefix_cache_control_three_user_messages() {
        let mut messages = vec![
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "First"})],
            },
            ApiMessage {
                role:    "assistant".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Reply 1"})],
            },
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Second"})],
            },
            ApiMessage {
                role:    "assistant".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Reply 2"})],
            },
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Third"})],
            },
        ];

        apply_cache_control_to_conversation_prefix(&mut messages);

        // Only the second-to-last user message (index 2) should get cache_control
        assert!(messages[0].content[0].get("cache_control").is_none());
        assert_eq!(messages[2].content[0]["cache_control"]["type"], "ephemeral");
        assert!(messages[4].content[0].get("cache_control").is_none());
    }

    #[test]
    fn beta_header_includes_cache_header() {
        let result = build_beta_header(None, true, false, false);
        assert_eq!(result, Some(CACHE_BETA_HEADER.to_string()));
    }

    #[test]
    fn beta_header_no_cache_no_user_headers() {
        let result = build_beta_header(None, false, false, false);
        assert_eq!(result, None);
    }

    #[test]
    fn beta_header_merges_user_headers_with_cache() {
        let opts = serde_json::json!({
            "anthropic": {
                "beta_headers": ["interleaved-thinking-2025-05-14"]
            }
        });
        let result = build_beta_header(Some(&opts), true, false, false);
        assert_eq!(
            result,
            Some(format!(
                "interleaved-thinking-2025-05-14,{CACHE_BETA_HEADER}"
            ))
        );
    }

    #[test]
    fn beta_header_no_duplicate_cache_header() {
        let opts = serde_json::json!({
            "anthropic": {
                "beta_headers": [CACHE_BETA_HEADER]
            }
        });
        let result = build_beta_header(Some(&opts), true, false, false);
        // Should not duplicate the header
        assert_eq!(result, Some(CACHE_BETA_HEADER.to_string()));
    }

    #[test]
    fn beta_header_user_headers_only_when_cache_disabled() {
        let opts = serde_json::json!({
            "anthropic": {
                "beta_headers": ["interleaved-thinking-2025-05-14"]
            }
        });
        let result = build_beta_header(Some(&opts), false, false, false);
        assert_eq!(result, Some("interleaved-thinking-2025-05-14".to_string()));
    }

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

    #[test]
    fn system_prompt_as_string_when_cache_disabled() {
        let system = "You are helpful.".to_string();
        let value = serde_json::Value::String(system);
        assert_eq!(value.as_str(), Some("You are helpful."));
    }

    #[test]
    fn api_request_serialization_with_cached_system() {
        let api_request = ApiRequest {
            model:          "claude-sonnet-4-20250514".to_string(),
            messages:       vec![ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
            }],
            max_tokens:     4096,
            system:         Some(system_with_cache_control("You are helpful.")),
            temperature:    None,
            top_p:          None,
            stop_sequences: None,
            tools:          None,
            tool_choice:    None,
            thinking:       None,
            output_config:  None,
            speed:          None,
            metadata:       None,
            stream:         false,
        };

        let json = serde_json::to_value(&api_request).expect("should serialize");
        let system = json.get("system").expect("system should be present");
        let arr = system.as_array().expect("system should be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
    }

    #[tokio::test]
    async fn build_api_request_omits_whitespace_only_system_prompt() {
        let adapter = Adapter::new("test-key");
        let request = Request {
            messages: vec![Message::system("   \n\t"), Message::user("Hello")],
            ..make_base_request()
        };

        let (api_request, _req_builder) = build_api_request(&adapter, &request, false).await;
        assert!(
            api_request.system.is_none(),
            "whitespace-only system prompts should be omitted"
        );
    }

    fn make_base_request() -> Request {
        Request {
            model:            "claude-sonnet-4-20250514".to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         Some("anthropic".to_string()),
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       Some(128),
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        }
    }

    fn make_request_with_format(format: ResponseFormat) -> Request {
        Request {
            provider: None,
            response_format: Some(format),
            max_tokens: None,
            ..make_base_request()
        }
    }

    #[test]
    fn response_format_json_schema_injects_synthetic_tool() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        });
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonSchema,
            json_schema: Some(schema.clone()),
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system: Option<serde_json::Value> = None;

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let tools = tools.expect("tools should be set");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, SYNTHETIC_TOOL_NAME);
        assert_eq!(tools[0].input_schema, schema);

        let tc = tool_choice.expect("tool_choice should be set");
        assert_eq!(tc["type"], "tool");
        assert_eq!(tc["name"], SYNTHETIC_TOOL_NAME);

        // System should not be modified
        assert!(system.is_none());
    }

    #[test]
    fn response_format_json_schema_appends_to_existing_tools() {
        let schema = serde_json::json!({"type": "object"});
        let mut request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonSchema,
            json_schema: Some(schema),
            strict:      false,
        });
        request.tools = Some(vec![ToolDefinition {
            name:        "existing_tool".to_string(),
            description: "An existing tool".to_string(),
            parameters:  serde_json::json!({}),
        }]);

        let mut tools: Option<Vec<ApiToolDef>> =
            Some(translate_tools(request.tools.as_ref().unwrap()));
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system: Option<serde_json::Value> = None;

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let tools = tools.expect("tools should be set");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "existing_tool");
        assert_eq!(tools[1].name, SYNTHETIC_TOOL_NAME);
    }

    #[test]
    fn response_format_json_object_appends_to_string_system() {
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonObject,
            json_schema: None,
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system = Some(serde_json::Value::String("You are helpful.".to_string()));

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let sys = system.expect("system should be set");
        let text = sys.as_str().expect("should be a string");
        assert!(text.contains("You are helpful."));
        assert!(text.contains("valid JSON"));

        // Tools should not be modified
        assert!(tools.is_none());
        assert!(tool_choice.is_none());
    }

    #[test]
    fn response_format_json_object_sets_system_when_none() {
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonObject,
            json_schema: None,
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system: Option<serde_json::Value> = None;

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let sys = system.expect("system should be set");
        let text = sys.as_str().expect("should be a string");
        assert!(text.contains("valid JSON"));
    }

    #[test]
    fn response_format_json_object_appends_to_array_system() {
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonObject,
            json_schema: None,
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system = Some(system_with_cache_control("You are helpful."));

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let sys = system.expect("system should be set");
        let arr = sys.as_array().expect("should be an array");
        let text = arr[0]["text"].as_str().expect("should have text");
        assert!(text.contains("You are helpful."));
        assert!(text.contains("valid JSON"));
    }

    #[test]
    fn response_format_text_is_noop() {
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::Text,
            json_schema: None,
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system: Option<serde_json::Value> = None;

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        assert!(tools.is_none());
        assert!(tool_choice.is_none());
        assert!(system.is_none());
    }

    #[test]
    fn convert_synthetic_tool_to_text_replaces_synthetic_tool() {
        let parts = vec![ContentPart::ToolCall(ToolCall::new(
            "id1",
            SYNTHETIC_TOOL_NAME,
            serde_json::json!({"name": "Alice"}),
        ))];
        let result = convert_synthetic_tool_to_text(parts);
        assert_eq!(result.len(), 1);
        match &result[0] {
            ContentPart::Text(text) => {
                assert!(text.contains("Alice"));
            }
            _ => panic!("expected Text, got {:?}", result[0]),
        }
    }

    #[test]
    fn convert_synthetic_tool_to_text_preserves_other_tool_calls() {
        let parts = vec![ContentPart::ToolCall(ToolCall::new(
            "id1",
            "real_tool",
            serde_json::json!({"key": "value"}),
        ))];
        let result = convert_synthetic_tool_to_text(parts);
        assert_eq!(result.len(), 1);
        match &result[0] {
            ContentPart::ToolCall(tc) => {
                assert_eq!(tc.name, "real_tool");
            }
            _ => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn convert_stream_event_converts_tool_start_for_synthetic() {
        let event = StreamEvent::ToolCallStart {
            tool_call: ToolCall::new("id1", SYNTHETIC_TOOL_NAME, serde_json::json!({})),
        };
        let result = convert_stream_event_for_json_schema(event);
        assert!(matches!(result, StreamEvent::TextStart { .. }));
    }

    #[test]
    fn convert_stream_event_preserves_real_tool_start() {
        let event = StreamEvent::ToolCallStart {
            tool_call: ToolCall::new("id1", "real_tool", serde_json::json!({})),
        };
        let result = convert_stream_event_for_json_schema(event);
        assert!(matches!(result, StreamEvent::ToolCallStart { .. }));
    }

    #[test]
    fn convert_stream_event_converts_tool_delta_for_synthetic() {
        let event = StreamEvent::ToolCallDelta {
            tool_call: ToolCall::new("id1", SYNTHETIC_TOOL_NAME, serde_json::json!("{\"name\"")),
        };
        let result = convert_stream_event_for_json_schema(event);
        match result {
            StreamEvent::TextDelta { delta, .. } => {
                assert_eq!(delta, "{\"name\"");
            }
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn convert_stream_event_converts_finish_reason() {
        let response = Box::new(Response {
            id:            "test".to_string(),
            model:         "claude".to_string(),
            provider:      "anthropic".to_string(),
            message:       Message {
                role:         Role::Assistant,
                content:      vec![ContentPart::ToolCall(ToolCall::new(
                    "id1",
                    SYNTHETIC_TOOL_NAME,
                    serde_json::json!({"data": "value"}),
                ))],
                name:         None,
                tool_call_id: None,
            },
            finish_reason: FinishReason::ToolCalls,
            usage:         TokenCounts::default(),
            raw:           None,
            warnings:      vec![],
            rate_limit:    None,
        });
        let event = StreamEvent::Finish {
            finish_reason: FinishReason::ToolCalls,
            usage: TokenCounts::default(),
            response,
        };
        let result = convert_stream_event_for_json_schema(event);
        match result {
            StreamEvent::Finish {
                finish_reason,
                response,
                ..
            } => {
                assert_eq!(finish_reason, FinishReason::Stop);
                assert_eq!(response.finish_reason, FinishReason::Stop);
                // Content should be converted from tool call to text
                assert!(matches!(&response.message.content[0], ContentPart::Text(_)));
            }
            _ => panic!("expected Finish"),
        }
    }

    #[tokio::test]
    async fn document_url_translates_to_url_source() {
        let part = ContentPart::Document(DocumentData {
            url:        Some("https://example.com/doc.pdf".to_string()),
            data:       None,
            media_type: None,
            file_name:  None,
        });
        let result = content_part_to_api(&part)
            .await
            .expect("should produce JSON");
        assert_eq!(result["type"], "document");
        assert_eq!(result["source"]["type"], "url");
        assert_eq!(result["source"]["url"], "https://example.com/doc.pdf");
    }

    #[tokio::test]
    async fn document_base64_data_translates_to_base64_source() {
        let part = ContentPart::Document(DocumentData {
            url:        None,
            data:       Some(vec![0x25, 0x50, 0x44, 0x46]),
            media_type: Some("application/pdf".to_string()),
            file_name:  Some("test.pdf".to_string()),
        });
        let result = content_part_to_api(&part)
            .await
            .expect("should produce JSON");
        assert_eq!(result["type"], "document");
        assert_eq!(result["source"]["type"], "base64");
        assert_eq!(result["source"]["media_type"], "application/pdf");
        assert!(result["source"]["data"].as_str().is_some());
    }

    #[tokio::test]
    async fn document_base64_defaults_to_pdf_mime() {
        let part = ContentPart::Document(DocumentData {
            url:        None,
            data:       Some(vec![1, 2, 3]),
            media_type: None,
            file_name:  None,
        });
        let result = content_part_to_api(&part)
            .await
            .expect("should produce JSON");
        assert_eq!(result["source"]["media_type"], "application/pdf");
    }

    /// Regression test: deprecated beta header values must not be sent.
    /// The Anthropic API rejects requests containing these old headers.
    #[test]
    fn beta_header_rejects_deprecated_values() {
        let deprecated = [
            "extended-thinking-2025-04-14",
            "max-tokens-3-5-sonnet-2025-04-14",
        ];

        // No user headers — only cache header should appear
        let header = build_beta_header(None, true, false, false).unwrap_or_default();
        for dep in &deprecated {
            assert!(
                !header.contains(dep),
                "default header must not contain deprecated value {dep}"
            );
        }

        // With a valid user header
        let opts = serde_json::json!({
            "anthropic": {
                "beta_headers": ["interleaved-thinking-2025-05-14"]
            }
        });
        let header = build_beta_header(Some(&opts), true, false, false).unwrap_or_default();
        for dep in &deprecated {
            assert!(
                !header.contains(dep),
                "header with user values must not contain deprecated value {dep}"
            );
        }
    }

    #[test]
    fn merge_provider_options_passes_through_unknown_keys() {
        let api_request = ApiRequest {
            model:          "claude-sonnet-4-20250514".to_string(),
            messages:       vec![ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
            }],
            max_tokens:     4096,
            system:         None,
            temperature:    None,
            top_p:          None,
            stop_sequences: None,
            tools:          None,
            tool_choice:    None,
            thinking:       None,
            output_config:  None,
            speed:          None,
            metadata:       None,
            stream:         false,
        };

        let opts = serde_json::json!({
            "anthropic": {
                "top_k": 40,
                "custom_field": "value"
            }
        });
        let body = merge_provider_options(&api_request, Some(&opts));
        assert_eq!(body["top_k"], 40);
        assert_eq!(body["custom_field"], "value");
    }

    #[test]
    fn merge_provider_options_skips_known_keys() {
        let api_request = ApiRequest {
            model:          "claude-sonnet-4-20250514".to_string(),
            messages:       vec![ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
            }],
            max_tokens:     4096,
            system:         None,
            temperature:    None,
            top_p:          None,
            stop_sequences: None,
            tools:          None,
            tool_choice:    None,
            thinking:       None,
            output_config:  None,
            speed:          None,
            metadata:       None,
            stream:         false,
        };

        let opts = serde_json::json!({
            "anthropic": {
                "thinking": {"type": "enabled", "budget_tokens": 10000},
                "auto_cache": false,
                "beta_headers": ["some-header"],
                "top_k": 40
            }
        });
        let body = merge_provider_options(&api_request, Some(&opts));
        // Known keys should not be merged (they are handled separately)
        assert!(body.get("auto_cache").is_none());
        assert!(body.get("beta_headers").is_none());
        // thinking is handled by the ApiRequest struct directly, should not be
        // double-merged
        assert!(body["thinking"].is_null());
        // Unknown keys should be merged
        assert_eq!(body["top_k"], 40);
    }

    #[tokio::test]
    async fn audio_produces_text_fallback() {
        let part = ContentPart::Audio(AudioData {
            url:        Some("https://example.com/audio.wav".to_string()),
            data:       None,
            media_type: None,
        });
        let result = content_part_to_api(&part)
            .await
            .expect("should produce JSON");
        assert_eq!(result["type"], "text");
        assert_eq!(
            result["text"],
            "[Audio content not supported by this provider]"
        );
    }

    #[tokio::test]
    async fn build_api_request_maps_reasoning_effort_to_output_config() {
        let adapter = Adapter::new("test-key");
        let request = Request {
            reasoning_effort: Some(ReasoningEffort::Medium),
            ..make_base_request()
        };

        let (api_request, _req_builder) = build_api_request(&adapter, &request, false).await;
        assert_eq!(
            api_request.output_config,
            Some(serde_json::json!({"effort": "medium"}))
        );
    }

    #[tokio::test]
    async fn build_api_request_omits_output_config_when_no_reasoning_effort() {
        let adapter = Adapter::new("test-key");
        let request = make_base_request();

        let (api_request, _req_builder) = build_api_request(&adapter, &request, false).await;
        assert!(api_request.output_config.is_none());
    }

    #[tokio::test]
    async fn build_api_request_sets_speed() {
        let adapter = Adapter::new("test-key");
        let request = Request {
            speed: Some("fast".to_string()),
            ..make_base_request()
        };

        let (api_request, _req_builder) = build_api_request(&adapter, &request, false).await;
        assert_eq!(api_request.speed, Some("fast".to_string()));
    }

    #[tokio::test]
    async fn build_api_request_injects_fast_mode_beta_header() {
        let adapter = Adapter::new("test-key");
        let request = Request {
            speed: Some("fast".to_string()),
            ..make_base_request()
        };

        let (_api_request, req_builder) = build_api_request(&adapter, &request, false).await;
        let built = req_builder.build().expect("should build request");
        let beta = built
            .headers()
            .get("anthropic-beta")
            .expect("anthropic-beta header should be present")
            .to_str()
            .unwrap();
        assert!(
            beta.contains(FAST_MODE_BETA_HEADER),
            "beta header should contain fast-mode header, got: {beta}"
        );
    }
    #[test]
    fn beta_header_includes_both_cache_and_fast_mode() {
        let result = build_beta_header(None, true, true, false);
        let header = result.expect("should produce a header");
        assert!(
            header.contains(CACHE_BETA_HEADER),
            "should contain cache header"
        );
        assert!(
            header.contains(FAST_MODE_BETA_HEADER),
            "should contain fast-mode header"
        );
    }

    #[test]
    fn effort_to_budget_tokens_xhigh_maps_to_seven_eighths() {
        assert_eq!(effort_to_budget_tokens("xhigh", 16_000), 14_000);
    }

    #[test]
    fn effort_to_budget_tokens_max_maps_to_full_budget() {
        assert_eq!(effort_to_budget_tokens("max", 16_000), 16_000);
    }

    #[tokio::test]
    async fn build_api_request_falls_back_to_thinking_budget_for_non_effort_model() {
        let adapter = Adapter::new("test-key");
        let request = Request {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: Some(16_000),
            reasoning_effort: Some(ReasoningEffort::XHigh),
            ..make_base_request()
        };

        let (api_request, _req_builder) = build_api_request(&adapter, &request, false).await;
        assert!(
            api_request.output_config.is_none(),
            "non-effort models must not receive output_config"
        );
        let thinking = api_request
            .thinking
            .expect("thinking must be set for fallback path");
        assert_eq!(thinking["type"], "enabled");
        assert_eq!(thinking["budget_tokens"], 14_000);
    }
}
