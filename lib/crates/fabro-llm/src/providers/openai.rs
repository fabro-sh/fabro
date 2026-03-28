use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use futures::{stream, StreamExt};

use crate::error::{error_from_status_code, SdkError};
use crate::provider::{validate_tool_choice, ProviderAdapter, StreamEventStream};
use crate::providers::common::{
    self as common, parse_error_body, parse_rate_limit_headers, parse_retry_after,
    send_and_read_response,
};
use crate::types::{
    AdapterTimeout, ContentPart, FinishReason, Message, RateLimitInfo, Request, Response,
    ResponseFormat, ResponseFormatType, Role, StreamEvent, ToolCall, ToolChoice, ToolDefinition,
    Usage,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Provider adapter for the `OpenAI` Responses API (`/v1/responses`).
///
/// Per spec Section 2.7, this adapter uses the Responses API (not Chat Completions)
/// to properly surface reasoning tokens, built-in tools, and server-side state.
pub struct Adapter {
    pub(crate) http: super::http_api::HttpApi,
    org_id: Option<String>,
    project_id: Option<String>,
    /// When true, always use streaming (required by the Codex endpoint).
    codex_mode: bool,
}

impl Adapter {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: super::http_api::HttpApi::new(api_key, DEFAULT_BASE_URL),
            org_id: None,
            project_id: None,
            codex_mode: false,
        }
    }

    #[must_use]
    pub fn with_codex_mode(mut self) -> Self {
        self.codex_mode = true;
        self
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.http.base_url = base_url.into();
        self
    }

    #[must_use]
    pub fn with_org_id(mut self, org_id: impl Into<String>) -> Self {
        self.org_id = Some(org_id.into());
        self
    }

    #[must_use]
    pub fn with_project_id(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
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

    /// Build a `reqwest::RequestBuilder` with default headers, org/project headers, and auth.
    fn build_request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.client.post(url);
        // Apply default_headers first so adapter-specific headers can override
        for (key, value) in &self.http.default_headers {
            req = req.header(key, value);
        }
        req = req.bearer_auth(&self.http.api_key);
        if let Some(org_id) = &self.org_id {
            req = req.header("OpenAI-Organization", org_id);
        }
        if let Some(project_id) = &self.project_id {
            req = req.header("OpenAI-Project", project_id);
        }
        req
    }

    /// Complete a request by streaming and collecting the final response.
    /// Used for the Codex endpoint which requires `stream: true`.
    async fn complete_via_stream(&self, request: &Request) -> Result<Response, SdkError> {
        use futures::StreamExt;
        let mut event_stream = self.stream(request).await?;
        let mut last_response: Option<Response> = None;
        while let Some(event) = event_stream.next().await {
            if let Ok(StreamEvent::Finish { response, .. }) = event {
                last_response = Some(*response);
                break;
            }
        }
        last_response.ok_or_else(|| SdkError::Network {
            message: "Stream ended without a finish event".into(),
            source: None,
        })
    }
}

// --- Request types (Responses API format) ---

#[derive(serde::Serialize)]
struct ApiRequest {
    model: String,
    input: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<std::collections::HashMap<String, String>>,
    store: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    include: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
}

// --- Response types (Responses API format) ---

#[derive(serde::Deserialize)]
struct ApiResponse {
    id: String,
    model: Option<String>,
    output: Vec<serde_json::Value>,
    status: Option<String>,
    usage: Option<ApiUsage>,
}

#[derive(serde::Deserialize)]
struct ApiUsage {
    input_tokens: i64,
    output_tokens: i64,
    total_tokens: Option<i64>,
    output_tokens_details: Option<OutputTokenDetails>,
    input_tokens_details: Option<InputTokenDetails>,
}

#[derive(serde::Deserialize)]
struct OutputTokenDetails {
    reasoning_tokens: Option<i64>,
}

#[derive(serde::Deserialize)]
struct InputTokenDetails {
    cached_tokens: Option<i64>,
}

/// Map the Responses API status to a `FinishReason`.
fn map_finish_reason(status: Option<&str>, has_tool_calls: bool) -> FinishReason {
    if has_tool_calls {
        return FinishReason::ToolCalls;
    }
    match status {
        Some("completed") | None => FinishReason::Stop,
        Some("incomplete") => FinishReason::Length,
        Some("failed") => FinishReason::Error,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

/// Translate unified messages to Responses API `input` array format.
fn translate_input(messages: &[Message]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut instructions_parts: Vec<String> = Vec::new();
    let mut input: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        match msg.role {
            Role::System | Role::Developer => {
                instructions_parts.push(msg.text());
            }
            Role::User => {
                let content: Vec<serde_json::Value> = msg
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text(text) => {
                            Some(serde_json::json!({"type": "input_text", "text": text}))
                        }
                        ContentPart::Image(img) => {
                            img.url.as_ref().map_or_else(
                                || {
                                    img.data.as_ref().map(|data| {
                                        let mime = img.media_type.as_deref().unwrap_or("image/png");
                                        let b64 = BASE64_STANDARD.encode(data);
                                        serde_json::json!({"type": "input_image", "image_url": format!("data:{mime};base64,{b64}")})
                                    })
                                },
                                |url| {
                                    if common::is_file_path(url) {
                                        match common::load_file_as_base64(url) {
                                            Ok((b64, mime)) => Some(serde_json::json!({"type": "input_image", "image_url": format!("data:{mime};base64,{b64}")})),
                                            Err(_) => None,
                                        }
                                    } else {
                                        Some(serde_json::json!({"type": "input_image", "image_url": url}))
                                    }
                                },
                            )
                        }
                        ContentPart::Audio(_) => {
                            Some(serde_json::json!({"type": "input_text", "text": "[Audio content not supported by this provider]"}))
                        }
                        ContentPart::Document(doc) => {
                            let desc = doc.file_name.as_ref().map_or_else(
                                || "[Document content not supported by this provider]".to_string(),
                                |name| format!("[Document '{name}': content type not supported by this provider]"),
                            );
                            Some(serde_json::json!({"type": "input_text", "text": desc}))
                        }
                        _ => None,
                    })
                    .collect();
                if !content.is_empty() {
                    input.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": content,
                    }));
                }
            }
            Role::Assistant => {
                // If we have a preserved opaque message item (with id/status), use
                // it instead of constructing a new message from Text parts.  This is
                // required so that reasoning items can find their "required following
                // item" during Responses API round-tripping.
                let has_opaque_message = msg.content.iter().any(|p| {
                    matches!(p, ContentPart::Other { kind, .. } if kind == ContentPart::OPENAI_MESSAGE)
                });
                for part in &msg.content {
                    match part {
                        ContentPart::Text(text) if !has_opaque_message => {
                            input.push(serde_json::json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": text}],
                            }));
                        }
                        ContentPart::Text(_) => {
                            // Skip — using preserved opaque message item instead
                        }
                        ContentPart::ToolCall(tc) if !tc.name.is_empty() => {
                            let args = tc
                                .raw_arguments
                                .as_ref()
                                .map_or_else(|| tc.arguments.to_string(), Clone::clone);
                            // Use the item-level ID (fc_xxx) for the `id` field;
                            // fall back to tc.id if no provider_metadata was stored.
                            let item_id = tc
                                .provider_metadata
                                .as_ref()
                                .and_then(|m| m.get("id"))
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(&tc.id);
                            input.push(serde_json::json!({
                                "type": "function_call",
                                "id": item_id,
                                "call_id": tc.id,
                                "name": tc.name,
                                "arguments": args,
                            }));
                        }
                        ContentPart::Other { data, .. } if part.is_opaque_openai() => {
                            input.push(data.clone());
                        }
                        _ => {}
                    }
                }
            }
            Role::Tool => {
                for part in &msg.content {
                    if let ContentPart::ToolResult(tr) = part {
                        let output = tr
                            .content
                            .as_str()
                            .map_or_else(|| tr.content.to_string(), str::to_string);
                        let mut item = serde_json::json!({
                            "type": "function_call_output",
                            "call_id": tr.tool_call_id,
                            "output": output,
                        });
                        if tr.is_error {
                            item["status"] = serde_json::json!("incomplete");
                        }
                        input.push(item);
                    }
                }
            }
        }
    }

    let instructions = if instructions_parts.is_empty() {
        None
    } else {
        Some(instructions_parts.join("\n"))
    };

    (instructions, input)
}

/// Translate unified tool definitions to Responses API tool format.
fn translate_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            })
        })
        .collect()
}

/// Translate unified `ToolChoice` to Responses API format.
fn translate_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!("none"),
        ToolChoice::Required => serde_json::json!("required"),
        ToolChoice::Named { tool_name } => {
            serde_json::json!({"type": "function", "name": tool_name})
        }
    }
}

/// Translate unified `ResponseFormat` to Responses API `text` field.
///
/// The Responses API uses `"text": {"format": {...}}` for structured output.
fn translate_response_format(format: &ResponseFormat) -> Option<serde_json::Value> {
    match format.kind {
        ResponseFormatType::Text => None,
        ResponseFormatType::JsonObject => {
            Some(serde_json::json!({"format": {"type": "json_object"}}))
        }
        ResponseFormatType::JsonSchema => {
            let mut schema_obj = serde_json::json!({
                "type": "json_schema",
                "name": "response",
                "strict": format.strict,
            });
            if let Some(schema) = &format.json_schema {
                schema_obj["schema"] = schema.clone();
            }
            Some(serde_json::json!({"format": schema_obj}))
        }
    }
}

/// Build an `ApiRequest` from a unified `Request`.
///
/// When `codex_mode` is true, unsupported fields (`temperature`, `max_output_tokens`, `top_p`)
/// are omitted and empty instructions are sent as `""` (required by the Codex endpoint).
fn build_api_request(request: &Request, stream: bool, codex_mode: bool) -> ApiRequest {
    let (instructions, input) = translate_input(&request.messages);
    let api_tools = request.tools.as_ref().map(|t| translate_tools(t));
    let tool_choice = request.tool_choice.as_ref().map(translate_tool_choice);
    let reasoning = request
        .reasoning_effort
        .as_ref()
        .map(|effort| serde_json::json!({"effort": effort.as_str()}));
    let text = request
        .response_format
        .as_ref()
        .and_then(translate_response_format);

    let include = if reasoning.is_some() {
        vec!["reasoning.encrypted_content".to_string()]
    } else {
        Vec::new()
    };

    let instructions = if codex_mode {
        Some(instructions.unwrap_or_default())
    } else {
        instructions
    };

    ApiRequest {
        model: request.model.clone(),
        input,
        instructions,
        temperature: if codex_mode {
            None
        } else {
            request.temperature
        },
        max_output_tokens: if codex_mode { None } else { request.max_tokens },
        top_p: if codex_mode { None } else { request.top_p },
        tools: api_tools,
        tool_choice,
        reasoning,
        text,
        stop: request.stop_sequences.clone(),
        metadata: request.metadata.clone(),
        // store: false is required for non-Azure OpenAI endpoints. Reasoning
        // items still round-trip correctly because we request encrypted_content
        // via the `include` field, which embeds them in the response payload
        // rather than relying on server-side storage.
        store: false,
        include,
        stream,
    }
}

/// Serialize an `ApiRequest` to JSON and merge any `provider_options.openai` keys into it.
fn build_request_body(request: &Request, stream: bool, codex_mode: bool) -> serde_json::Value {
    let api_request = build_api_request(request, stream, codex_mode);
    let mut body = serde_json::to_value(&api_request).unwrap_or_else(|_| serde_json::json!({}));

    if let Some(openai_opts) = request
        .provider_options
        .as_ref()
        .and_then(|opts| opts.get("openai"))
    {
        if let (Some(base), Some(overrides)) = (body.as_object_mut(), openai_opts.as_object()) {
            for (key, value) in overrides {
                base.insert(key.clone(), value.clone());
            }
        }
    }

    body
}

/// Parse output items from the Responses API into content parts.
fn parse_output(output: &[serde_json::Value]) -> (Vec<ContentPart>, bool) {
    let mut parts = Vec::new();
    let mut has_tool_calls = false;

    for item in output {
        let item_type = item.get("type").and_then(serde_json::Value::as_str);
        match item_type {
            Some("message") => {
                // Preserve the full message item for Responses API round-tripping.
                // The item's `id` and `status` fields are required so that reasoning
                // items preceding it can find their "required following item."
                parts.push(ContentPart::Other {
                    kind: ContentPart::OPENAI_MESSAGE.to_string(),
                    data: item.clone(),
                });
                if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                    for block in content {
                        if block.get("type").and_then(serde_json::Value::as_str)
                            == Some("output_text")
                        {
                            if let Some(text) =
                                block.get("text").and_then(serde_json::Value::as_str)
                            {
                                parts.push(ContentPart::text(text));
                            }
                        }
                    }
                }
            }
            Some("reasoning") => {
                parts.push(ContentPart::Other {
                    kind: ContentPart::OPENAI_REASONING.to_string(),
                    data: item.clone(),
                });
            }
            Some("function_call") => {
                let item_id = item
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let call_id = item
                    .get("call_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(item_id)
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                // Skip function calls with empty names (e.g. model-internal items)
                if name.is_empty() {
                    continue;
                }
                has_tool_calls = true;
                let args_str = item
                    .get("arguments")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("{}");
                let arguments =
                    serde_json::from_str(args_str).unwrap_or_else(|_| serde_json::json!({}));
                let mut tc = ToolCall::new(call_id, name, arguments);
                tc.raw_arguments = Some(args_str.to_string());
                // Preserve item-level ID (fc_xxx) for Responses API round-trip
                if !item_id.is_empty() {
                    tc.provider_metadata = Some(serde_json::json!({"id": item_id}));
                }
                parts.push(ContentPart::ToolCall(tc));
            }
            _ => {}
        }
    }

    (parts, has_tool_calls)
}

// --- SSE streaming support ---

/// Mutable state carried through SSE stream processing.
struct SseStreamState {
    line_reader: super::common::LineReader,
    model: String,
    response_id: String,
    response_model: String,
    accumulated_text: String,
    tool_calls: Vec<ToolCall>,
    /// Raw reasoning output items to preserve for round-tripping.
    reasoning_items: Vec<serde_json::Value>,
    /// Raw message output items to preserve for round-tripping.
    message_items: Vec<serde_json::Value>,
    usage: Usage,
    finish_reason: FinishReason,
    emitted_start: bool,
    emitted_text_start: bool,
    emitted_reasoning_start: bool,
    raw_response: Option<serde_json::Value>,
    rate_limit: Option<RateLimitInfo>,
}

/// Parse a single SSE message block into an (`event_type`, `data`) pair.
///
/// Each SSE message consists of one or more lines (`event:` and `data:` prefixed).
/// Returns `None` if the block has no `data:` lines.
fn parse_sse_message(message_block: &str) -> Option<(Option<String>, String)> {
    let mut current_event: Option<String> = None;
    let mut current_data = String::new();

    for line in message_block.lines() {
        if let Some(stripped) = line.strip_prefix("event: ") {
            current_event = Some(stripped.to_string());
        } else if let Some(stripped) = line.strip_prefix("event:") {
            current_event = Some(stripped.trim().to_string());
        } else if let Some(stripped) = line.strip_prefix("data: ") {
            if !current_data.is_empty() {
                current_data.push('\n');
            }
            current_data.push_str(stripped);
        } else if let Some(stripped) = line.strip_prefix("data:") {
            if !current_data.is_empty() {
                current_data.push('\n');
            }
            current_data.push_str(stripped.trim());
        }
    }

    if current_data.is_empty() {
        None
    } else {
        Some((current_event, current_data))
    }
}

/// Process the next chunk(s) from the byte stream and return `StreamEvent`s.
async fn process_next_sse_events(state: &mut SseStreamState) -> Result<Vec<StreamEvent>, SdkError> {
    loop {
        match state.line_reader.read_next_chunk("\n\n").await? {
            Some(message_block) => {
                if let Some((event_type, data)) = parse_sse_message(&message_block) {
                    let events = process_sse_event(state, event_type.as_deref(), &data);
                    if !events.is_empty() {
                        return Ok(events);
                    }
                }
                // No data or unhandled event type; keep reading.
            }
            None => return Ok(vec![]),
        }
    }
}

/// Process a single SSE event and return the corresponding `StreamEvent`(s).
fn process_sse_event(
    state: &mut SseStreamState,
    event_type: Option<&str>,
    data: &str,
) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    if !state.emitted_start {
        state.emitted_start = true;
        events.push(StreamEvent::StreamStart);
    }

    let json: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return events,
    };

    // Resolve event type from the `event:` SSE line or from the JSON `type` field.
    let resolved_type = event_type
        .map(str::to_string)
        .or_else(|| {
            json.get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();

    match resolved_type.as_str() {
        "response.created" => handle_response_created(state, &json),
        "response.output_text.delta" => handle_text_delta(state, &json, &mut events),
        "response.function_call_arguments.delta" => {
            handle_tool_call_delta(state, &json, &mut events);
        }
        "response.output_item.done" => handle_output_item_done(state, &json, &mut events),
        "response.completed" => handle_response_completed(state, &json, &mut events),
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(delta) = json.get("delta").and_then(serde_json::Value::as_str) {
                if !state.emitted_reasoning_start {
                    state.emitted_reasoning_start = true;
                    events.push(StreamEvent::ReasoningStart);
                }
                events.push(StreamEvent::ReasoningDelta {
                    delta: delta.to_string(),
                });
            }
        }
        "response.reasoning_summary_part.added" => {
            // Recognized but no-op — ReasoningStart is emitted on the first delta instead.
        }
        _ => {}
    }

    events
}

/// Handle `response.created` by extracting the response ID and model.
fn handle_response_created(state: &mut SseStreamState, json: &serde_json::Value) {
    if let Some(id) = json
        .get("response")
        .and_then(|r| r.get("id"))
        .and_then(serde_json::Value::as_str)
    {
        state.response_id = id.to_string();
    }
    if let Some(model) = json
        .get("response")
        .and_then(|r| r.get("model"))
        .and_then(serde_json::Value::as_str)
    {
        state.response_model = model.to_string();
    }
}

/// Handle `response.output_text.delta` by accumulating text and emitting events.
fn handle_text_delta(
    state: &mut SseStreamState,
    json: &serde_json::Value,
    events: &mut Vec<StreamEvent>,
) {
    if let Some(delta) = json.get("delta").and_then(serde_json::Value::as_str) {
        if !state.emitted_text_start {
            state.emitted_text_start = true;
            events.push(StreamEvent::TextStart { text_id: None });
        }
        state.accumulated_text.push_str(delta);
        events.push(StreamEvent::text_delta(delta, None));
    }
}

/// Handle `response.function_call_arguments.delta` by accumulating args and emitting events.
fn handle_tool_call_delta(
    state: &mut SseStreamState,
    json: &serde_json::Value,
    events: &mut Vec<StreamEvent>,
) {
    let Some(delta) = json.get("delta").and_then(serde_json::Value::as_str) else {
        return;
    };

    let call_id = json
        .get("call_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let item_id = json
        .get("item_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let lookup_id = if call_id.is_empty() {
        &item_id
    } else {
        &call_id
    };

    let tc_index = state.tool_calls.iter().position(|tc| tc.id == *lookup_id);

    if let Some(idx) = tc_index {
        if let Some(ref mut raw) = state.tool_calls[idx].raw_arguments {
            raw.push_str(delta);
        }
    } else {
        let name = json
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut tc = ToolCall::new(lookup_id, name, serde_json::json!({}));
        tc.raw_arguments = Some(delta.to_string());
        // Preserve item-level ID (fc_xxx) for Responses API round-trip
        if !item_id.is_empty() && item_id != *lookup_id {
            tc.provider_metadata = Some(serde_json::json!({"id": item_id}));
        }
        state.tool_calls.push(tc.clone());
        events.push(StreamEvent::ToolCallStart { tool_call: tc });
    }

    let current_tc = state
        .tool_calls
        .iter()
        .find(|tc| tc.id == *lookup_id)
        .cloned()
        .unwrap_or_else(|| ToolCall::new("", "", serde_json::json!({})));

    events.push(StreamEvent::ToolCallDelta {
        tool_call: ToolCall {
            raw_arguments: Some(delta.to_string()),
            ..current_tc
        },
    });
}

/// Handle `response.output_item.done` for text and function call items.
fn handle_output_item_done(
    state: &mut SseStreamState,
    json: &serde_json::Value,
    events: &mut Vec<StreamEvent>,
) {
    let item_type = json
        .get("item")
        .and_then(|i| i.get("type"))
        .and_then(serde_json::Value::as_str);

    match item_type {
        Some("reasoning") => {
            if state.emitted_reasoning_start {
                state.emitted_reasoning_start = false;
                events.push(StreamEvent::ReasoningEnd);
            }
            let item = json.get("item").unwrap_or(json);
            state.reasoning_items.push(item.clone());
        }
        Some("message") => {
            if state.emitted_text_start {
                events.push(StreamEvent::TextEnd { text_id: None });
                state.emitted_text_start = false;
            }
            let item = json.get("item").unwrap_or(json);
            state.message_items.push(item.clone());
        }
        Some("function_call") => {
            let item = json.get("item").unwrap_or(json);
            let item_id = item
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let call_id = item
                .get("call_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(item_id)
                .to_string();
            let name = item
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let args_str = item
                .get("arguments")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("{}");
            let arguments =
                serde_json::from_str(args_str).unwrap_or_else(|_| serde_json::json!({}));

            let mut tc = ToolCall::new(&call_id, &name, arguments);
            tc.raw_arguments = Some(args_str.to_string());
            // Preserve item-level ID (fc_xxx) for Responses API round-trip
            if !item_id.is_empty() {
                tc.provider_metadata = Some(serde_json::json!({"id": item_id}));
            }

            if let Some(existing) = state.tool_calls.iter_mut().find(|t| t.id == call_id) {
                existing.name.clone_from(&name);
                existing.arguments = tc.arguments.clone();
                existing.raw_arguments.clone_from(&tc.raw_arguments);
                existing.provider_metadata.clone_from(&tc.provider_metadata);
            } else {
                state.tool_calls.push(tc.clone());
            }

            events.push(StreamEvent::ToolCallEnd { tool_call: tc });
        }
        _ => {}
    }
}

/// Handle `response.completed` by extracting usage and building the final response.
fn handle_response_completed(
    state: &mut SseStreamState,
    json: &serde_json::Value,
    events: &mut Vec<StreamEvent>,
) {
    let response_data = json.get("response").unwrap_or(json);

    if let Some(usage_data) = response_data.get("usage") {
        if let Ok(u) = serde_json::from_value::<ApiUsage>(usage_data.clone()) {
            state.usage = Usage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                total_tokens: u.total_tokens.unwrap_or(u.input_tokens + u.output_tokens),
                reasoning_tokens: u
                    .output_tokens_details
                    .as_ref()
                    .and_then(|d| d.reasoning_tokens),
                cache_read_tokens: u
                    .input_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens),
                ..Usage::default()
            };
        }
    }

    if let Some(id) = response_data.get("id").and_then(serde_json::Value::as_str) {
        state.response_id = id.to_string();
    }
    if let Some(model) = response_data
        .get("model")
        .and_then(serde_json::Value::as_str)
    {
        state.response_model = model.to_string();
    }

    let status = response_data
        .get("status")
        .and_then(serde_json::Value::as_str);
    let has_tool_calls = !state.tool_calls.is_empty();
    state.finish_reason = map_finish_reason(status, has_tool_calls);

    state.raw_response = Some(response_data.clone());

    let mut content_parts = Vec::new();
    // Reasoning items must precede function calls for Responses API round-trip
    for item in std::mem::take(&mut state.reasoning_items) {
        content_parts.push(ContentPart::Other {
            kind: ContentPart::OPENAI_REASONING.to_string(),
            data: item,
        });
    }
    // Preserve full message output items for Responses API round-tripping
    for item in std::mem::take(&mut state.message_items) {
        content_parts.push(ContentPart::Other {
            kind: ContentPart::OPENAI_MESSAGE.to_string(),
            data: item,
        });
    }
    if !state.accumulated_text.is_empty() {
        content_parts.push(ContentPart::text(&state.accumulated_text));
    }
    for tc in &state.tool_calls {
        // Skip tool calls with empty names (e.g. model-internal items)
        if tc.name.is_empty() {
            continue;
        }
        content_parts.push(ContentPart::ToolCall(tc.clone()));
    }

    let model = if state.response_model.is_empty() {
        state.model.clone()
    } else {
        state.response_model.clone()
    };

    let response = Response {
        id: state.response_id.clone(),
        model,
        provider: "openai".to_string(),
        message: Message {
            role: Role::Assistant,
            content: content_parts,
            name: None,
            tool_call_id: None,
        },
        finish_reason: state.finish_reason.clone(),
        usage: state.usage.clone(),
        raw: state.raw_response.clone(),
        warnings: vec![],
        rate_limit: state.rate_limit.clone(),
    };

    events.push(StreamEvent::finish(
        state.finish_reason.clone(),
        state.usage.clone(),
        response,
    ));
}

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn name(&self) -> &str {
        "openai"
    }

    async fn complete(&self, request: &Request) -> Result<Response, SdkError> {
        // Codex endpoint requires streaming; collect the stream into a response.
        if self.codex_mode {
            return self.complete_via_stream(request).await;
        }

        if let Some(tc) = &request.tool_choice {
            validate_tool_choice(self, tc)?;
        }
        let request_body = build_request_body(request, false, false);
        let url = format!("{}/responses", self.http.base_url);

        let mut req = self.build_request(&url).json(&request_body);
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }
        let (body, headers) = send_and_read_response(req, "openai", "type").await?;

        let api_resp: ApiResponse = serde_json::from_str(&body)
            .map_err(|e| SdkError::network(format!("failed to parse OpenAI response: {e}"), e))?;

        let (content_parts, has_tool_calls) = parse_output(&api_resp.output);
        let finish_reason = map_finish_reason(api_resp.status.as_deref(), has_tool_calls);

        let usage = api_resp
            .usage
            .as_ref()
            .map_or_else(Usage::default, |u| Usage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                total_tokens: u.total_tokens.unwrap_or(u.input_tokens + u.output_tokens),
                reasoning_tokens: u
                    .output_tokens_details
                    .as_ref()
                    .and_then(|d| d.reasoning_tokens),
                cache_read_tokens: u
                    .input_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens),
                ..Usage::default()
            });

        Ok(Response {
            id: api_resp.id,
            model: api_resp.model.unwrap_or_else(|| request.model.clone()),
            provider: "openai".to_string(),
            message: Message {
                role: Role::Assistant,
                content: content_parts,
                name: None,
                tool_call_id: None,
            },
            finish_reason,
            usage,
            raw: serde_json::from_str(&body).ok(),
            warnings: vec![],
            rate_limit: parse_rate_limit_headers(&headers),
        })
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, SdkError> {
        if let Some(tc) = &request.tool_choice {
            validate_tool_choice(self, tc)?;
        }
        let request_body = build_request_body(request, true, self.codex_mode);
        let url = format!("{}/responses", self.http.base_url);

        let http_resp = self
            .build_request(&url)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| SdkError::network(e.to_string(), e))?;

        let status = http_resp.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(http_resp.headers());
            let body = http_resp
                .text()
                .await
                .map_err(|e| SdkError::network(e.to_string(), e))?;
            let (msg, code, raw) = parse_error_body(&body, "type");
            return Err(error_from_status_code(
                status.as_u16(),
                msg,
                "openai".to_string(),
                code,
                raw,
                retry_after,
            ));
        }

        let model = request.model.clone();
        let rate_limit = parse_rate_limit_headers(http_resp.headers());
        let stream_read_timeout = self.http.stream_read_timeout;

        let state = SseStreamState {
            line_reader: super::common::LineReader::new(http_resp, stream_read_timeout),
            model,
            response_id: String::new(),
            response_model: String::new(),
            accumulated_text: String::new(),
            tool_calls: Vec::new(),
            reasoning_items: Vec::new(),
            message_items: Vec::new(),
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
            emitted_start: false,
            emitted_text_start: false,
            emitted_reasoning_start: false,
            raw_response: None,
            rate_limit,
        };

        let stream = stream::unfold(state, |mut state| async move {
            let events = process_next_sse_events(&mut state).await;
            let items: Vec<Result<StreamEvent, SdkError>> = match events {
                Ok(events) if events.is_empty() => return None,
                Ok(events) => events.into_iter().map(Ok).collect(),
                Err(e) => vec![Err(e)],
            };
            Some((stream::iter(items), state))
        })
        .flatten();

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn minimal_request() -> Request {
        Request {
            model: "gpt-4o".to_string(),
            messages: vec![Message::user("Hello")],
            provider: None,
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

    #[test]
    fn build_request_body_includes_metadata() {
        let mut metadata = HashMap::new();
        metadata.insert("user_id".to_string(), "u123".to_string());
        metadata.insert("session".to_string(), "s456".to_string());

        let mut request = minimal_request();
        request.metadata = Some(metadata);

        let body = build_request_body(&request, false, false);
        let meta = body.get("metadata").expect("metadata should be present");
        assert_eq!(meta["user_id"], "u123");
        assert_eq!(meta["session"], "s456");
    }

    #[test]
    fn build_request_body_omits_metadata_when_none() {
        let request = minimal_request();
        let body = build_request_body(&request, false, false);
        assert!(body.get("metadata").is_none());
    }

    #[test]
    fn build_request_body_merges_provider_options_openai() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "openai": {
                "store": true,
                "previous_response_id": "resp_abc123"
            }
        }));

        let body = build_request_body(&request, false, false);
        assert_eq!(body["store"], true);
        assert_eq!(body["previous_response_id"], "resp_abc123");
    }

    #[test]
    fn build_request_body_provider_options_override_fields() {
        let mut request = minimal_request();
        request.temperature = Some(0.5);
        request.provider_options = Some(serde_json::json!({
            "openai": {
                "temperature": 0.9
            }
        }));

        let body = build_request_body(&request, false, false);
        // provider_options should override the base field
        assert_eq!(body["temperature"], 0.9);
    }

    #[test]
    fn build_request_body_ignores_non_openai_provider_options() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "anthropic": {
                "thinking": {"type": "enabled", "budget_tokens": 10000}
            }
        }));

        let body = build_request_body(&request, false, false);
        // anthropic options should not leak into the OpenAI request
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn build_request_body_no_provider_options() {
        let request = minimal_request();
        let body = build_request_body(&request, false, false);
        assert_eq!(body["model"], "gpt-4o");
        // stream field is omitted when false (skip_serializing_if)
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn build_request_body_stream_flag() {
        let request = minimal_request();
        let body = build_request_body(&request, true, false);
        assert!(body["stream"].as_bool().unwrap_or(false));
    }

    #[test]
    fn build_request_body_metadata_and_provider_options_together() {
        let mut metadata = HashMap::new();
        metadata.insert("trace_id".to_string(), "t789".to_string());

        let mut request = minimal_request();
        request.metadata = Some(metadata);
        request.provider_options = Some(serde_json::json!({
            "openai": {
                "store": true
            }
        }));

        let body = build_request_body(&request, false, false);
        assert_eq!(body["metadata"]["trace_id"], "t789");
        assert_eq!(body["store"], true);
    }

    #[test]
    fn adapter_with_org_id_sets_field() {
        let adapter = Adapter::new("sk-test").with_org_id("org-123");
        assert_eq!(adapter.org_id.as_deref(), Some("org-123"));
    }

    #[test]
    fn adapter_with_project_id_sets_field() {
        let adapter = Adapter::new("sk-test").with_project_id("proj-456");
        assert_eq!(adapter.project_id.as_deref(), Some("proj-456"));
    }

    #[test]
    fn adapter_with_default_headers_sets_field() {
        let mut headers = HashMap::new();
        headers.insert("X-Custom".to_string(), "value".to_string());
        let adapter = Adapter::new("sk-test").with_default_headers(headers);
        assert_eq!(
            adapter
                .http
                .default_headers
                .get("X-Custom")
                .map(String::as_str),
            Some("value")
        );
    }

    #[test]
    fn adapter_defaults_have_no_org_project_or_headers() {
        let adapter = Adapter::new("sk-test");
        assert!(adapter.org_id.is_none());
        assert!(adapter.project_id.is_none());
        assert!(adapter.http.default_headers.is_empty());
    }

    #[test]
    fn audio_content_produces_text_fallback() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentPart::Audio(crate::types::AudioData {
                url: Some("https://example.com/audio.wav".to_string()),
                data: None,
                media_type: None,
            })],
            name: None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let content = input[0]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(
            content[0]["text"],
            "[Audio content not supported by this provider]"
        );
    }

    #[test]
    fn document_content_produces_text_fallback_with_filename() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentPart::Document(crate::types::DocumentData {
                url: Some("https://example.com/doc.pdf".to_string()),
                data: None,
                media_type: None,
                file_name: Some("report.pdf".to_string()),
            })],
            name: None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let content = input[0]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(
            content[0]["text"],
            "[Document 'report.pdf': content type not supported by this provider]"
        );
    }

    #[test]
    fn document_content_produces_text_fallback_without_filename() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentPart::Document(crate::types::DocumentData {
                url: None,
                data: Some(vec![1, 2, 3]),
                media_type: None,
                file_name: None,
            })],
            name: None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let content = input[0]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(
            content[0]["text"],
            "[Document content not supported by this provider]"
        );
    }

    #[test]
    fn parse_output_preserves_both_ids_on_function_call() {
        let output = vec![serde_json::json!({
            "type": "function_call",
            "id": "fc_abc123",
            "call_id": "call_xyz789",
            "name": "get_weather",
            "arguments": "{\"location\":\"NYC\"}"
        })];
        let (parts, has_tool_calls) = parse_output(&output);
        assert!(has_tool_calls);
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            ContentPart::ToolCall(tc) => {
                // call_id is used as the ToolCall.id (links to tool results)
                assert_eq!(tc.id, "call_xyz789");
                // item-level id (fc_xxx) is preserved in provider_metadata
                let meta = tc
                    .provider_metadata
                    .as_ref()
                    .expect("provider_metadata should be set");
                assert_eq!(meta["id"], "fc_abc123");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn translate_input_uses_item_id_for_id_field() {
        let mut tc = ToolCall::new(
            "call_xyz789",
            "get_weather",
            serde_json::json!({"location": "NYC"}),
        );
        tc.provider_metadata = Some(serde_json::json!({"id": "fc_abc123"}));

        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall(tc)],
            name: None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let fc = &input[0];
        assert_eq!(fc["type"], "function_call");
        // id field uses the fc_ prefixed item ID
        assert_eq!(fc["id"], "fc_abc123");
        // call_id field uses the call_ prefixed call ID
        assert_eq!(fc["call_id"], "call_xyz789");
    }

    #[test]
    fn translate_input_falls_back_to_tc_id_without_metadata() {
        let tc = ToolCall::new("call_xyz789", "get_weather", serde_json::json!({}));

        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall(tc)],
            name: None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let fc = &input[0];
        // Without provider_metadata, both fields use tc.id
        assert_eq!(fc["id"], "call_xyz789");
        assert_eq!(fc["call_id"], "call_xyz789");
    }

    #[test]
    fn parse_output_preserves_reasoning_items() {
        let output = vec![
            serde_json::json!({
                "type": "reasoning",
                "id": "rs_abc123",
                "summary": [{"type": "summary_text", "text": "Thinking..."}]
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_def456",
                "call_id": "call_789",
                "name": "search",
                "arguments": "{}"
            }),
        ];
        let (parts, has_tool_calls) = parse_output(&output);
        assert!(has_tool_calls);
        assert_eq!(parts.len(), 2);
        // First part is the reasoning item
        match &parts[0] {
            ContentPart::Other { kind, data } => {
                assert_eq!(kind, ContentPart::OPENAI_REASONING);
                assert_eq!(data["type"], "reasoning");
                assert_eq!(data["id"], "rs_abc123");
            }
            other => panic!("expected Other, got {other:?}"),
        }
        // Second part is the function call
        assert!(matches!(&parts[1], ContentPart::ToolCall(_)));
    }

    #[test]
    fn parse_output_preserves_message_items() {
        let output = vec![
            serde_json::json!({
                "type": "reasoning",
                "id": "rs_abc",
                "summary": []
            }),
            serde_json::json!({
                "type": "message",
                "id": "msg_xyz",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hello"}]
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_123",
                "call_id": "call_456",
                "name": "search",
                "arguments": "{}"
            }),
        ];
        let (parts, has_tool_calls) = parse_output(&output);
        assert!(has_tool_calls);
        // reasoning + openai_message + text + function_call
        assert_eq!(parts.len(), 4);
        assert!(
            matches!(&parts[0], ContentPart::Other { kind, .. } if kind == ContentPart::OPENAI_REASONING)
        );
        assert!(
            matches!(&parts[1], ContentPart::Other { kind, data } if kind == ContentPart::OPENAI_MESSAGE && data["id"] == "msg_xyz")
        );
        assert!(matches!(&parts[2], ContentPart::Text(t) if t == "Hello"));
        assert!(matches!(&parts[3], ContentPart::ToolCall(_)));
    }

    #[test]
    fn reasoning_items_round_trip_through_translate_input() {
        let reasoning = serde_json::json!({
            "type": "reasoning",
            "id": "rs_abc123",
            "summary": [{"type": "summary_text", "text": "Thinking..."}]
        });
        let mut tc = ToolCall::new("call_789", "search", serde_json::json!({}));
        tc.provider_metadata = Some(serde_json::json!({"id": "fc_def456"}));

        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentPart::Other {
                    kind: ContentPart::OPENAI_REASONING.to_string(),
                    data: reasoning,
                },
                ContentPart::ToolCall(tc),
            ],
            name: None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        assert_eq!(input.len(), 2);
        // Reasoning item is emitted first
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["id"], "rs_abc123");
        // Function call follows
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["id"], "fc_def456");
        assert_eq!(input[1]["call_id"], "call_789");
    }

    #[test]
    fn reasoning_message_function_call_round_trip() {
        // Simulates an assistant turn with reasoning + text + tool call.
        // The opaque message item (with id/status) must be used instead of
        // constructing a new one from Text, so the reasoning item can find
        // its "required following item."
        let reasoning = serde_json::json!({
            "type": "reasoning",
            "id": "rs_xyz789",
            "summary": [{"type": "summary_text", "text": "Let me check..."}]
        });
        let opaque_message = serde_json::json!({
            "type": "message",
            "id": "msg_abc123",
            "status": "completed",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Checking now."}]
        });
        let mut tc = ToolCall::new("call_001", "shell", serde_json::json!({"cmd": "ls"}));
        tc.provider_metadata = Some(serde_json::json!({"id": "fc_def456"}));

        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentPart::Other {
                    kind: ContentPart::OPENAI_REASONING.to_string(),
                    data: reasoning,
                },
                ContentPart::Other {
                    kind: ContentPart::OPENAI_MESSAGE.to_string(),
                    data: opaque_message,
                },
                ContentPart::text("Checking now."),
                ContentPart::ToolCall(tc),
            ],
            name: None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        assert_eq!(input.len(), 3);
        // Reasoning first
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["id"], "rs_xyz789");
        // Opaque message with id/status (not a reconstructed one)
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["id"], "msg_abc123");
        assert_eq!(input[1]["status"], "completed");
        // Function call last
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["id"], "fc_def456");
    }

    #[test]
    fn text_without_opaque_message_still_constructs_message() {
        // For non-OpenAI turns or turns without preserved message items,
        // Text parts should still produce a constructed message.
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentPart::text("Hello")],
            name: None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "assistant");
        // No id field on constructed messages
        assert!(input[0].get("id").is_none());
    }

    #[test]
    fn parse_output_round_trips_function_call_ids() {
        // Simulate a response from the Responses API
        let output = vec![serde_json::json!({
            "type": "function_call",
            "id": "fc_item1",
            "call_id": "call_001",
            "name": "search",
            "arguments": "{\"q\":\"test\"}"
        })];
        let (parts, _) = parse_output(&output);

        // Now translate back to input format
        let msg = Message {
            role: Role::Assistant,
            content: parts,
            name: None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let fc = &input[0];

        // The round-tripped function call should have correct IDs
        assert_eq!(fc["id"], "fc_item1");
        assert_eq!(fc["call_id"], "call_001");
    }

    #[test]
    fn build_request_body_includes_stop_sequences() {
        let mut request = minimal_request();
        request.stop_sequences = Some(vec!["END".to_string(), "STOP".to_string()]);

        let body = build_request_body(&request, false, false);
        let stop = body.get("stop").expect("stop should be present");
        let arr = stop.as_array().expect("stop should be an array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], "END");
        assert_eq!(arr[1], "STOP");
    }

    #[test]
    fn build_request_body_omits_stop_when_none() {
        let request = minimal_request();
        let body = build_request_body(&request, false, false);
        assert!(body.get("stop").is_none());
    }

    fn empty_sse_state() -> SseStreamState {
        let http_resp = http::Response::builder().status(200).body("").unwrap();
        let response = reqwest::Response::from(http_resp);
        SseStreamState {
            line_reader: crate::providers::common::LineReader::new(response, None),
            model: String::new(),
            response_id: String::new(),
            response_model: String::new(),
            accumulated_text: String::new(),
            tool_calls: Vec::new(),
            reasoning_items: Vec::new(),
            message_items: Vec::new(),
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
            emitted_start: true,
            emitted_text_start: false,
            emitted_reasoning_start: false,
            raw_response: None,
            rate_limit: None,
        }
    }

    #[test]
    fn reasoning_summary_delta_emits_reasoning_events() {
        let mut state = empty_sse_state();
        let data = r#"{"type":"response.reasoning_summary_text.delta","delta":"Let me think"}"#;
        let events = process_sse_event(
            &mut state,
            Some("response.reasoning_summary_text.delta"),
            data,
        );
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::ReasoningStart));
        assert!(
            matches!(events[1], StreamEvent::ReasoningDelta { ref delta } if delta == "Let me think")
        );
    }

    #[test]
    fn reasoning_text_delta_emits_reasoning_events() {
        let mut state = empty_sse_state();

        // First delta: should emit ReasoningStart + ReasoningDelta
        let data1 = r#"{"type":"response.reasoning_text.delta","delta":"Step 1"}"#;
        let events1 = process_sse_event(&mut state, Some("response.reasoning_text.delta"), data1);
        assert_eq!(events1.len(), 2);
        assert!(matches!(events1[0], StreamEvent::ReasoningStart));
        assert!(
            matches!(events1[1], StreamEvent::ReasoningDelta { ref delta } if delta == "Step 1")
        );

        // Second delta: should NOT emit duplicate ReasoningStart
        let data2 = r#"{"type":"response.reasoning_text.delta","delta":"Step 2"}"#;
        let events2 = process_sse_event(&mut state, Some("response.reasoning_text.delta"), data2);
        assert_eq!(events2.len(), 1);
        assert!(
            matches!(events2[0], StreamEvent::ReasoningDelta { ref delta } if delta == "Step 2")
        );
    }

    #[test]
    fn reasoning_end_emitted_on_item_done() {
        let mut state = empty_sse_state();
        state.emitted_reasoning_start = true;

        let data = r#"{"item":{"type":"reasoning","id":"rs_abc","summary":[]}}"#;
        let events = process_sse_event(&mut state, Some("response.output_item.done"), data);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::ReasoningEnd));
        assert!(!state.emitted_reasoning_start);
        assert_eq!(state.reasoning_items.len(), 1);
    }
}
