use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_http::HeaderMap;
use futures::stream;

use crate::error::{
    Error, ProviderErrorDetail, ProviderErrorKind, error_from_grpc_status, error_from_status_code,
};
use crate::provider::{ProviderAdapter, StreamEventStream, validate_tool_choice};
use crate::providers::common::{
    self as common, extract_system_prompt, parse_error_body, parse_rate_limit_headers,
    parse_retry_after,
};
use crate::types::{
    AdapterTimeout, ContentPart, FinishReason, Message, RateLimitInfo, Request, Response,
    ResponseFormat, ResponseFormatType, Role, StreamEvent, ThinkingData, TokenCounts, ToolCall,
    ToolChoice, ToolDefinition,
};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Provider adapter for the Google Gemini `generateContent` API.
pub struct Adapter {
    pub(crate) http: super::http_api::HttpApi,
}

impl Adapter {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: super::http_api::HttpApi::new(api_key, DEFAULT_BASE_URL),
        }
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
        }
    }

    #[must_use]
    pub fn with_timeout(self, timeout: AdapterTimeout) -> Self {
        Self {
            http: self.http.with_timeout(timeout),
        }
    }
}

// --- Request types ---

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiRequest {
    contents:           Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config:  Option<GenerationOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools:              Option<Vec<GeminiToolGroup>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config:        Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct Content {
    role:  String,
    parts: Vec<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct SystemInstruction {
    parts: Vec<serde_json::Value>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature:        Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens:  Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p:              Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences:     Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_schema:    Option<serde_json::Value>,
}

/// Gemini groups function declarations under a `tools` array.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiToolGroup {
    function_declarations: Vec<GeminiFunctionDecl>,
}

#[derive(serde::Serialize)]
struct GeminiFunctionDecl {
    name:        String,
    description: String,
    parameters:  serde_json::Value,
}

// --- Response types ---

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiResponse {
    candidates:     Option<Vec<Candidate>>,
    usage_metadata: Option<UsageMetadata>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    content:       Option<CandidateContent>,
    finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct CandidateContent {
    parts: Option<Vec<serde_json::Value>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_field_names)]
struct UsageMetadata {
    prompt_token_count:         Option<i64>,
    candidates_token_count:     Option<i64>,
    thoughts_token_count:       Option<i64>,
    cached_content_token_count: Option<i64>,
}

/// Map Gemini's finish reason, inferring `ToolCalls` from content when needed.
fn map_finish_reason(reason: Option<&str>, has_function_calls: bool) -> FinishReason {
    if has_function_calls {
        return FinishReason::ToolCalls;
    }
    match reason {
        Some("STOP") | None => FinishReason::Stop,
        Some("MAX_TOKENS") => FinishReason::Length,
        Some("SAFETY" | "RECITATION") => FinishReason::ContentFilter,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

fn parse_part(part: &serde_json::Value) -> Option<ContentPart> {
    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
        let is_thought = part
            .get("thought")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if is_thought {
            return Some(ContentPart::Thinking(ThinkingData {
                text:      text.to_string(),
                signature: None,
                redacted:  false,
            }));
        }
        return Some(ContentPart::text(text));
    }
    if let Some(fc) = part.get("functionCall") {
        let name = fc.get("name")?.as_str()?.to_string();
        let args = fc
            .get("args")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
        let mut tc = ToolCall::new(uuid::Uuid::new_v4().to_string(), name, args);
        // Preserve thought_signature for Gemini 3 models (sibling of functionCall in
        // the part)
        if let Some(sig) = part.get("thoughtSignature") {
            tc.provider_metadata = Some(serde_json::json!({"thoughtSignature": sig}));
        }
        return Some(ContentPart::ToolCall(tc));
    }
    None
}

/// Check if any parts contain function calls.
fn parts_have_function_calls(parts: &[serde_json::Value]) -> bool {
    parts.iter().any(|p| p.get("functionCall").is_some())
}

/// Build a mapping from tool call ID to function name by scanning assistant
/// messages.
///
/// Gemini uses function names (not call IDs) in `functionResponse`. Since the
/// adapter generates synthetic UUIDs as tool call IDs, we need this mapping to
/// recover the original function name when sending tool results back.
fn build_tool_call_id_to_name(messages: &[&Message]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for msg in messages {
        if msg.role == Role::Assistant {
            for part in &msg.content {
                if let ContentPart::ToolCall(tc) = part {
                    map.insert(tc.id.clone(), tc.name.clone());
                }
            }
        }
    }
    map
}

/// Translate unified messages to Gemini content format.
fn translate_messages(messages: &[&Message]) -> Vec<Content> {
    let id_to_name = build_tool_call_id_to_name(messages);
    let mut contents: Vec<Content> = Vec::new();

    for msg in messages {
        let role = match msg.role {
            Role::Assistant => "model",
            Role::User | Role::Tool => "user",
            Role::System | Role::Developer => continue,
        };

        let parts: Vec<serde_json::Value> = msg
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(serde_json::json!({"text": text})),
                ContentPart::ToolCall(tc) => {
                    let mut part_json = serde_json::json!({
                        "functionCall": {
                            "name": tc.name,
                            "args": tc.arguments,
                        }
                    });
                    // Re-attach thought_signature as sibling of functionCall
                    if let Some(sig) = tc
                        .provider_metadata
                        .as_ref()
                        .and_then(|m| m.get("thoughtSignature"))
                    {
                        part_json["thoughtSignature"] = sig.clone();
                    }
                    Some(part_json)
                }
                ContentPart::Image(img) => {
                    img.url.as_ref().map_or_else(
                        || {
                            img.data.as_ref().map(|data| {
                                let mime = img.media_type.as_deref().unwrap_or("image/png");
                                let b64 = BASE64_STANDARD.encode(data);
                                serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})
                            })
                        },
                        |url| {
                            if common::is_file_path(url) {
                                match common::load_file_as_base64(url) {
                                    Ok((b64, mime)) => Some(serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})),
                                    Err(_) => None,
                                }
                            } else {
                                let mime = img.media_type.as_deref().unwrap_or("image/png");
                                Some(serde_json::json!({"fileData": {"mimeType": mime, "fileUri": url}}))
                            }
                        },
                    )
                }
                ContentPart::Audio(audio) => {
                    audio.url.as_ref().map_or_else(
                        || {
                            audio.data.as_ref().map(|data| {
                                let mime = audio.media_type.as_deref().unwrap_or("audio/wav");
                                let b64 = BASE64_STANDARD.encode(data);
                                serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})
                            })
                        },
                        |url| {
                            if common::is_file_path(url) {
                                match common::load_file_as_base64(url) {
                                    Ok((b64, mime)) => Some(serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})),
                                    Err(_) => None,
                                }
                            } else {
                                let mime = audio.media_type.as_deref().unwrap_or("audio/wav");
                                Some(serde_json::json!({"fileData": {"mimeType": mime, "fileUri": url}}))
                            }
                        },
                    )
                }
                ContentPart::Document(doc) => {
                    doc.url.as_ref().map_or_else(
                        || {
                            doc.data.as_ref().map(|data| {
                                let mime = doc.media_type.as_deref().unwrap_or("application/pdf");
                                let b64 = BASE64_STANDARD.encode(data);
                                serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})
                            })
                        },
                        |url| {
                            if common::is_file_path(url) {
                                match common::load_file_as_base64(url) {
                                    Ok((b64, mime)) => Some(serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})),
                                    Err(_) => None,
                                }
                            } else {
                                let mime = doc.media_type.as_deref().unwrap_or("application/pdf");
                                Some(serde_json::json!({"fileData": {"mimeType": mime, "fileUri": url}}))
                            }
                        },
                    )
                }
                ContentPart::ToolResult(tr) => {
                    // Gemini's functionResponse uses the function *name*, not the call ID.
                    // Look up the original function name from the tool call mapping.
                    let function_name = id_to_name
                        .get(&tr.tool_call_id)
                        .cloned()
                        .unwrap_or_else(|| tr.tool_call_id.clone());
                    let response = tr.content.as_str().map_or_else(
                        || {
                            if tr.content.is_object() {
                                tr.content.clone()
                            } else {
                                serde_json::json!({"result": tr.content.to_string()})
                            }
                        },
                        |s| serde_json::json!({"result": s}),
                    );
                    Some(serde_json::json!({
                        "functionResponse": {
                            "name": function_name,
                            "response": response,
                        }
                    }))
                }
                _ => None,
            })
            .collect();

        if parts.is_empty() {
            continue;
        }

        contents.push(Content {
            role: role.to_string(),
            parts,
        });
    }

    contents
}

/// Translate unified tool definitions to Gemini's format.
fn translate_tools(tools: &[ToolDefinition]) -> Vec<GeminiToolGroup> {
    vec![GeminiToolGroup {
        function_declarations: tools
            .iter()
            .map(|t| GeminiFunctionDecl {
                name:        t.name.clone(),
                description: t.description.clone(),
                parameters:  t.parameters.clone(),
            })
            .collect(),
    }]
}

/// Translate unified `ToolChoice` to Gemini's `toolConfig`.
fn translate_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!({
            "functionCallingConfig": {"mode": "AUTO"}
        }),
        ToolChoice::None => serde_json::json!({
            "functionCallingConfig": {"mode": "NONE"}
        }),
        ToolChoice::Required => serde_json::json!({
            "functionCallingConfig": {"mode": "ANY"}
        }),
        ToolChoice::Named { tool_name } => serde_json::json!({
            "functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [tool_name],
            }
        }),
    }
}

/// Translate unified `ResponseFormat` to Gemini generation config fields.
///
/// Returns `(response_mime_type, response_schema)`.
fn translate_response_format(
    format: &ResponseFormat,
) -> (Option<String>, Option<serde_json::Value>) {
    match format.kind {
        ResponseFormatType::Text => (None, None),
        ResponseFormatType::JsonObject => (Some("application/json".to_string()), None),
        ResponseFormatType::JsonSchema => (
            Some("application/json".to_string()),
            format.json_schema.clone(),
        ),
    }
}

/// Build the Gemini API request body from a unified `Request`.
///
/// Returns a `serde_json::Value` so that `provider_options.gemini` fields can
/// be merged into the request before sending.
fn build_api_request(request: &Request) -> serde_json::Value {
    let (system_text, other_messages) = extract_system_prompt(&request.messages);

    let system_instruction = system_text.map(|text| SystemInstruction {
        parts: vec![serde_json::json!({"text": text})],
    });

    let contents = translate_messages(&other_messages);

    let (response_mime_type, response_schema) = request
        .response_format
        .as_ref()
        .map_or((None, None), translate_response_format);

    let generation_config = GenerationOptions {
        temperature: request.temperature,
        max_output_tokens: request.max_tokens,
        top_p: request.top_p,
        stop_sequences: request.stop_sequences.clone(),
        response_mime_type,
        response_schema,
    };

    let api_tools = request.tools.as_ref().map(|t| translate_tools(t));
    let tool_config = request.tool_choice.as_ref().map(translate_tool_choice);

    let api_request = ApiRequest {
        contents,
        system_instruction,
        generation_config: Some(generation_config),
        tools: api_tools,
        tool_config,
    };

    let mut body = serde_json::to_value(&api_request).unwrap_or_default();
    merge_provider_options(&mut body, request.provider_options.as_ref());
    apply_default_safety_settings(&mut body);
    body
}

/// Merge `provider_options.gemini` fields into the serialized API request body.
///
/// Known fields like `safety_settings` and `cached_content` are set directly.
/// Any other fields are merged at the top level, allowing pass-through of
/// Gemini-specific options not covered by the unified schema.
fn merge_provider_options(
    body: &mut serde_json::Value,
    provider_options: Option<&serde_json::Value>,
) {
    let Some(gemini_opts) = provider_options.and_then(|opts| opts.get("gemini")) else {
        return;
    };
    let Some(body_map) = body.as_object_mut() else {
        return;
    };
    let Some(gemini_map) = gemini_opts.as_object() else {
        return;
    };

    for (key, value) in gemini_map {
        body_map.insert(key.clone(), value.clone());
    }
}

/// Apply default safety settings if none were provided via provider_options.
fn apply_default_safety_settings(body: &mut serde_json::Value) {
    if body.get("safety_settings").is_some() {
        return;
    }
    if let Some(body_map) = body.as_object_mut() {
        body_map.insert(
            "safety_settings".to_string(),
            serde_json::json!([{
                "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
                "threshold": "BLOCK_ONLY_HIGH"
            }]),
        );
    }
}

/// Convert `UsageMetadata` from the Gemini API into a unified `TokenCounts`.
fn parse_usage(metadata: Option<&UsageMetadata>) -> TokenCounts {
    metadata.map_or_else(TokenCounts::default, |u| {
        let input = u.prompt_token_count.unwrap_or(0);
        let reasoning_tokens = u.thoughts_token_count.unwrap_or(0);
        let output = u
            .candidates_token_count
            .unwrap_or(0)
            .saturating_sub(reasoning_tokens);
        TokenCounts {
            input_tokens: input,
            output_tokens: output,
            reasoning_tokens,
            cache_read_tokens: u.cached_content_token_count.unwrap_or(0),
            ..TokenCounts::default()
        }
    })
}

/// Send an HTTP request and read the Gemini response body.
///
/// Like `send_and_read_response` but uses gRPC status code mapping when
/// available.
async fn send_gemini_response(
    request: fabro_http::RequestBuilder,
) -> Result<(String, HeaderMap), Error> {
    let http_resp = request.send().await.map_err(|e| {
        if e.is_timeout() {
            Error::request_timeout(format!("gemini: {e}"), e)
        } else {
            Error::network(e.to_string(), e)
        }
    })?;

    let status = http_resp.status();
    let retry_after = parse_retry_after(http_resp.headers());
    let headers = http_resp.headers().clone();
    let body = http_resp
        .text()
        .await
        .map_err(|e| Error::network(e.to_string(), e))?;

    if !status.is_success() {
        let (msg, code, raw) = parse_error_body(&body, "status");
        return Err(gemini_error(status.as_u16(), msg, code, raw, retry_after));
    }

    Ok((body, headers))
}

/// Map Gemini error response using gRPC status when available, falling back to
/// HTTP status.
fn gemini_error(
    status_code: u16,
    msg: String,
    grpc_status: Option<String>,
    raw: Option<serde_json::Value>,
    retry_after: Option<f64>,
) -> Error {
    match grpc_status {
        Some(grpc_code) => error_from_grpc_status(
            &grpc_code,
            msg,
            "gemini".to_string(),
            Some(grpc_code.clone()),
            raw,
            retry_after,
        ),
        None => error_from_status_code(
            status_code,
            msg,
            "gemini".to_string(),
            None,
            raw,
            retry_after,
        ),
    }
}

/// Send an HTTP request for streaming and return the `fabro_http::Response`.
///
/// Checks for HTTP errors before returning. On error, reads the body and
/// maps it to `Error` using gRPC status code mapping when available.
async fn send_streaming_request(
    request: fabro_http::RequestBuilder,
) -> Result<fabro_http::Response, Error> {
    let http_resp = request
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
        let (msg, code, raw) = parse_error_body(&body, "status");
        return Err(gemini_error(status.as_u16(), msg, code, raw, retry_after));
    }

    Ok(http_resp)
}

/// Process a stream of SSE chunks from the Gemini `streamGenerateContent`
/// endpoint and yield `StreamEvent` values.
fn process_sse_stream(
    http_resp: fabro_http::Response,
    model: String,
    rate_limit: Option<RateLimitInfo>,
    stream_read_timeout: Option<std::time::Duration>,
) -> StreamEventStream {
    Box::pin(stream::unfold(
        SseStreamState::new(http_resp, model, rate_limit, stream_read_timeout),
        |mut state| async move {
            // If we have buffered events, yield them first.
            if let Some(event) = state.pending_events.pop_front() {
                return Some((Ok(event), state));
            }

            // Read SSE lines until we get a data payload or the stream ends.
            loop {
                let line = match state.read_line().await {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        // Stream ended. Emit Finish if we haven't yet.
                        if !state.finished {
                            state.finished = true;
                            let event = state.build_finish_event();
                            return Some((Ok(event), state));
                        }
                        return None;
                    }
                    Err(e) => return Some((Err(e), state)),
                };

                // SSE format: lines starting with "data:" carry the payload.
                let data = if let Some(stripped) = line.strip_prefix("data:") {
                    stripped.trim()
                } else {
                    // Ignore non-data lines (empty lines, comments, event: lines).
                    continue;
                };

                // Skip empty data lines.
                if data.is_empty() {
                    continue;
                }

                // Parse the JSON chunk.
                let chunk: ApiResponse = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(e) => {
                        return Some((
                            Err(Error::stream_error(
                                format!("failed to parse Gemini SSE chunk: {e}"),
                                e,
                            )),
                            state,
                        ));
                    }
                };

                // Extract events from this chunk.
                state.process_chunk(&chunk);

                // Track usage from every chunk; the final one will have the totals.
                if let Some(ref usage_meta) = chunk.usage_metadata {
                    state.usage = parse_usage(Some(usage_meta));
                }

                // Extract finish reason from the candidate if present.
                let candidate_finish = chunk
                    .candidates
                    .as_ref()
                    .and_then(|c| c.first())
                    .and_then(|c| c.finish_reason.clone());
                if let Some(reason) = candidate_finish {
                    state.finish_reason_str = Some(reason);
                }

                // Yield the first buffered event if any were produced.
                if let Some(event) = state.pending_events.pop_front() {
                    return Some((Ok(event), state));
                }
                // If no events were produced from this chunk, continue reading.
            }
        },
    ))
}

/// Internal state for the SSE stream processor.
struct SseStreamState {
    line_reader:            super::common::LineReader,
    model:                  String,
    /// Events extracted from a chunk but not yet yielded.
    pending_events:         std::collections::VecDeque<StreamEvent>,
    /// Whether we have emitted a `StreamStart` event.
    stream_started:         bool,
    /// Whether we have emitted a `TextStart` event.
    text_started:           bool,
    /// Whether we are currently inside a reasoning (thought) segment.
    reasoning_started:      bool,
    /// Accumulated thinking text across all chunks.
    accumulated_thinking:   String,
    /// Accumulated text across all chunks.
    accumulated_text:       String,
    /// Accumulated tool calls across all chunks.
    accumulated_tool_calls: Vec<ToolCall>,
    /// The `text_id` used for `TextStart`/`TextDelta`/`TextEnd`.
    text_id:                String,
    /// Latest usage metadata (updated per chunk; final chunk has totals).
    usage:                  TokenCounts,
    /// The finish reason string from the candidate, if received.
    finish_reason_str:      Option<String>,
    /// Whether we have emitted the `Finish` event.
    finished:               bool,
    /// Rate limit info parsed from HTTP response headers.
    rate_limit:             Option<RateLimitInfo>,
}

impl SseStreamState {
    fn new(
        http_resp: fabro_http::Response,
        model: String,
        rate_limit: Option<RateLimitInfo>,
        stream_read_timeout: Option<std::time::Duration>,
    ) -> Self {
        Self {
            line_reader: super::common::LineReader::new(http_resp, stream_read_timeout),
            model,
            pending_events: std::collections::VecDeque::new(),
            stream_started: false,
            text_started: false,
            reasoning_started: false,
            accumulated_thinking: String::new(),
            accumulated_text: String::new(),
            accumulated_tool_calls: Vec::new(),
            text_id: uuid::Uuid::new_v4().to_string(),
            usage: TokenCounts::default(),
            finish_reason_str: None,
            finished: false,
            rate_limit,
        }
    }

    /// Read the next complete line from the HTTP byte stream.
    ///
    /// Returns `Ok(None)` when the stream is exhausted.
    async fn read_line(&mut self) -> Result<Option<String>, Error> {
        self.line_reader
            .read_next_chunk("\n")
            .await
            .map(|opt| opt.map(|s| s.trim_end_matches('\r').to_string()))
    }

    /// Extract stream events from a parsed SSE chunk and buffer them.
    fn process_chunk(&mut self, chunk: &ApiResponse) {
        if !self.stream_started {
            self.stream_started = true;
            self.pending_events.push_back(StreamEvent::StreamStart);
        }

        let parts = chunk
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.content.as_ref())
            .and_then(|c| c.parts.as_ref());

        let Some(parts) = parts else {
            return;
        };

        for part in parts {
            let is_thought = part
                .get("thought")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);

            if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                if is_thought {
                    if !self.reasoning_started {
                        self.reasoning_started = true;
                        self.pending_events.push_back(StreamEvent::ReasoningStart);
                    }
                    self.accumulated_thinking.push_str(text);
                    self.pending_events.push_back(StreamEvent::ReasoningDelta {
                        delta: text.to_string(),
                    });
                } else {
                    // Transition from reasoning to text: close reasoning segment.
                    if self.reasoning_started {
                        self.reasoning_started = false;
                        self.pending_events.push_back(StreamEvent::ReasoningEnd);
                    }
                    if !self.text_started {
                        self.text_started = true;
                        self.pending_events.push_back(StreamEvent::TextStart {
                            text_id: Some(self.text_id.clone()),
                        });
                    }
                    self.accumulated_text.push_str(text);
                    self.pending_events
                        .push_back(StreamEvent::text_delta(text, Some(self.text_id.clone())));
                }
            } else if let Some(fc) = part.get("functionCall") {
                let name = fc
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let args = fc
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                let mut tool_call = ToolCall::new(uuid::Uuid::new_v4().to_string(), name, args);
                // Preserve thought_signature for Gemini 3 models (sibling of functionCall)
                if let Some(sig) = part.get("thoughtSignature") {
                    tool_call.provider_metadata =
                        Some(serde_json::json!({"thoughtSignature": sig}));
                }

                // Gemini delivers function calls as complete objects in a single chunk.
                self.pending_events.push_back(StreamEvent::ToolCallStart {
                    tool_call: tool_call.clone(),
                });
                self.pending_events.push_back(StreamEvent::ToolCallEnd {
                    tool_call: tool_call.clone(),
                });
                self.accumulated_tool_calls.push(tool_call);
            }
        }

        // If a finish reason is present on this chunk's candidate, emit TextEnd.
        let has_finish_reason = chunk
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.finish_reason.as_ref())
            .is_some();

        if has_finish_reason {
            if self.reasoning_started {
                self.reasoning_started = false;
                self.pending_events.push_back(StreamEvent::ReasoningEnd);
            }
            if self.text_started {
                self.pending_events.push_back(StreamEvent::TextEnd {
                    text_id: Some(self.text_id.clone()),
                });
            }
        }
    }

    /// Build the final `Finish` event from accumulated state.
    fn build_finish_event(&self) -> StreamEvent {
        let has_tool_calls = !self.accumulated_tool_calls.is_empty();
        let finish_reason = map_finish_reason(self.finish_reason_str.as_deref(), has_tool_calls);

        let mut content_parts: Vec<ContentPart> = Vec::new();
        if !self.accumulated_thinking.is_empty() {
            content_parts.push(ContentPart::Thinking(ThinkingData {
                text:      self.accumulated_thinking.clone(),
                signature: None,
                redacted:  false,
            }));
        }
        if !self.accumulated_text.is_empty() {
            content_parts.push(ContentPart::text(&self.accumulated_text));
        }
        for tc in &self.accumulated_tool_calls {
            content_parts.push(ContentPart::ToolCall(tc.clone()));
        }

        let response = Response {
            id:            uuid::Uuid::new_v4().to_string(),
            model:         self.model.clone(),
            provider:      "gemini".to_string(),
            message:       Message {
                role:         Role::Assistant,
                content:      content_parts,
                name:         None,
                tool_call_id: None,
            },
            finish_reason: finish_reason.clone(),
            usage:         self.usage.clone(),
            raw:           None,
            warnings:      vec![],
            rate_limit:    self.rate_limit.clone(),
        };

        StreamEvent::finish(finish_reason, self.usage.clone(), response)
    }
}

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn name(&self) -> &'static str {
        "gemini"
    }

    async fn complete(&self, request: &Request) -> Result<Response, Error> {
        if let Some(tc) = &request.tool_choice {
            validate_tool_choice(self, tc)?;
        }
        let api_body = build_api_request(request);

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.http.base_url, request.model, self.http.api_key
        );

        let mut req = self.http.client.post(&url);
        for (key, value) in &self.http.default_headers {
            req = req.header(key, value);
        }
        let mut gemini_req = req.json(&api_body);
        if let Some(t) = self.http.request_timeout {
            gemini_req = gemini_req.timeout(t);
        }
        let (body, headers) = send_gemini_response(gemini_req).await?;

        let api_resp: ApiResponse = serde_json::from_str(&body)
            .map_err(|e| Error::network(format!("failed to parse Gemini response: {e}"), e))?;

        let candidate = api_resp
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .ok_or_else(|| Error::Provider {
                kind:   ProviderErrorKind::Server,
                detail: Box::new(ProviderErrorDetail::new(
                    "no candidates in Gemini response",
                    "gemini",
                )),
            })?;

        let raw_parts = candidate.content.as_ref().and_then(|c| c.parts.as_ref());

        let content_parts: Vec<ContentPart> = raw_parts
            .map(|parts| parts.iter().filter_map(parse_part).collect())
            .unwrap_or_default();

        // Gemini has no dedicated tool_calls finish reason; infer from parts
        let has_tool_calls = raw_parts.is_some_and(|p| parts_have_function_calls(p));
        let finish_reason = map_finish_reason(candidate.finish_reason.as_deref(), has_tool_calls);

        let usage = parse_usage(api_resp.usage_metadata.as_ref());

        Ok(Response {
            id: uuid::Uuid::new_v4().to_string(),
            model: request.model.clone(),
            provider: "gemini".to_string(),
            message: Message {
                role:         Role::Assistant,
                content:      content_parts,
                name:         None,
                tool_call_id: None,
            },
            finish_reason,
            usage,
            raw: serde_json::from_str(&body).ok(),
            warnings: vec![],
            rate_limit: parse_rate_limit_headers(&headers),
        })
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        if let Some(tc) = &request.tool_choice {
            validate_tool_choice(self, tc)?;
        }
        let api_body = build_api_request(request);

        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.http.base_url, request.model, self.http.api_key
        );

        let mut req = self.http.client.post(&url);
        for (key, value) in &self.http.default_headers {
            req = req.header(key, value);
        }
        let http_resp = send_streaming_request(req.json(&api_body)).await?;

        let rate_limit = parse_rate_limit_headers(http_resp.headers());
        Ok(process_sse_stream(
            http_resp,
            request.model.clone(),
            rate_limit,
            self.http.stream_read_timeout,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AudioData, DocumentData};

    fn minimal_request() -> Request {
        Request {
            model:            "gemini-2.0-flash".to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         None,
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       None,
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        }
    }

    #[test]
    fn provider_options_none_produces_standard_body() {
        let request = minimal_request();
        let body = build_api_request(&request);
        assert!(body.get("safetySettings").is_none());
        assert!(body.get("cachedContent").is_none());
    }

    #[test]
    fn provider_options_gemini_safety_settings_merged() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "gemini": {
                "safetySettings": [
                    {"category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_NONE"}
                ]
            }
        }));

        let body = build_api_request(&request);
        let safety = body
            .get("safetySettings")
            .expect("safetySettings should be present");
        let arr = safety.as_array().expect("should be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["category"], "HARM_CATEGORY_HARASSMENT");
    }

    #[test]
    fn provider_options_gemini_cached_content_merged() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "gemini": {
                "cachedContent": "projects/my-project/cachedContents/abc123"
            }
        }));

        let body = build_api_request(&request);
        assert_eq!(
            body.get("cachedContent")
                .and_then(serde_json::Value::as_str),
            Some("projects/my-project/cachedContents/abc123")
        );
    }

    #[test]
    fn provider_options_gemini_multiple_fields_merged() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "gemini": {
                "safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_LOW_AND_ABOVE"}],
                "cachedContent": "cache-id",
                "customField": "custom-value"
            }
        }));

        let body = build_api_request(&request);
        assert!(body.get("safetySettings").is_some());
        assert_eq!(
            body.get("cachedContent")
                .and_then(serde_json::Value::as_str),
            Some("cache-id")
        );
        assert_eq!(
            body.get("customField").and_then(serde_json::Value::as_str),
            Some("custom-value")
        );
    }

    #[test]
    fn provider_options_other_provider_ignored() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "anthropic": {
                "auto_cache": false
            }
        }));

        let body = build_api_request(&request);
        assert!(body.get("auto_cache").is_none());
    }

    #[test]
    fn provider_options_gemini_preserves_standard_fields() {
        let mut request = minimal_request();
        request.temperature = Some(0.5);
        request.max_tokens = Some(100);
        request.provider_options = Some(serde_json::json!({
            "gemini": {
                "cachedContent": "cache-id"
            }
        }));

        let body = build_api_request(&request);
        let gen_config = body
            .get("generationConfig")
            .expect("generationConfig should exist");
        assert_eq!(
            gen_config
                .get("temperature")
                .and_then(serde_json::Value::as_f64),
            Some(0.5)
        );
        assert_eq!(
            gen_config
                .get("maxOutputTokens")
                .and_then(serde_json::Value::as_i64),
            Some(100)
        );
        assert_eq!(
            body.get("cachedContent")
                .and_then(serde_json::Value::as_str),
            Some("cache-id")
        );
    }

    #[test]
    fn merge_provider_options_with_non_object_gemini_value() {
        let mut body = serde_json::json!({"contents": []});
        let opts = serde_json::json!({"gemini": "not-an-object"});
        merge_provider_options(&mut body, Some(&opts));
        // Should not crash and body should be unchanged
        assert!(body.get("contents").is_some());
    }

    #[test]
    fn audio_url_translates_to_file_data() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Audio(AudioData {
                url:        Some("https://example.com/audio.wav".to_string()),
                data:       None,
                media_type: Some("audio/wav".to_string()),
            })],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        assert_eq!(contents.len(), 1);
        let part = &contents[0].parts[0];
        assert_eq!(part["fileData"]["mimeType"], "audio/wav");
        assert_eq!(part["fileData"]["fileUri"], "https://example.com/audio.wav");
    }

    #[test]
    fn audio_base64_translates_to_inline_data() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Audio(AudioData {
                url:        None,
                data:       Some(vec![0xFF, 0xFB, 0x90]),
                media_type: None,
            })],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        let part = &contents[0].parts[0];
        assert_eq!(part["inlineData"]["mimeType"], "audio/wav");
        assert!(part["inlineData"]["data"].as_str().is_some());
    }

    #[test]
    fn document_url_translates_to_file_data() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Document(DocumentData {
                url:        Some("https://example.com/doc.pdf".to_string()),
                data:       None,
                media_type: Some("application/pdf".to_string()),
                file_name:  Some("doc.pdf".to_string()),
            })],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        let part = &contents[0].parts[0];
        assert_eq!(part["fileData"]["mimeType"], "application/pdf");
        assert_eq!(part["fileData"]["fileUri"], "https://example.com/doc.pdf");
    }

    #[test]
    fn document_base64_translates_to_inline_data() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Document(DocumentData {
                url:        None,
                data:       Some(vec![0x25, 0x50, 0x44, 0x46]),
                media_type: None,
                file_name:  None,
            })],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        let part = &contents[0].parts[0];
        assert_eq!(part["inlineData"]["mimeType"], "application/pdf");
        assert!(part["inlineData"]["data"].as_str().is_some());
    }

    #[test]
    fn gemini_error_uses_grpc_status_when_available() {
        use crate::error::ProviderErrorKind;

        let err = gemini_error(
            400,
            "model not found".into(),
            Some("NOT_FOUND".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::NotFound,
            ..
        }));

        let err = gemini_error(
            400,
            "bad args".into(),
            Some("INVALID_ARGUMENT".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::InvalidRequest,
            ..
        }));

        let err = gemini_error(
            429,
            "rate limited".into(),
            Some("RESOURCE_EXHAUSTED".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::RateLimit,
            ..
        }));

        let err = gemini_error(
            401,
            "bad key".into(),
            Some("UNAUTHENTICATED".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Authentication,
            ..
        }));

        let err = gemini_error(
            403,
            "denied".into(),
            Some("PERMISSION_DENIED".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::AccessDenied,
            ..
        }));

        let err = gemini_error(
            504,
            "timeout".into(),
            Some("DEADLINE_EXCEEDED".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::RequestTimeout { .. }));
    }

    #[test]
    fn gemini_error_falls_back_to_http_status_without_grpc() {
        use crate::error::ProviderErrorKind;

        let err = gemini_error(429, "rate limited".into(), None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::RateLimit,
            ..
        }));

        let err = gemini_error(500, "internal".into(), None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Server,
            ..
        }));
    }

    #[test]
    fn parse_part_handles_thought_text() {
        let part = serde_json::json!({"text": "Let me think about this...", "thought": true});
        let result = parse_part(&part).expect("should parse thought part");
        match result {
            ContentPart::Thinking(td) => {
                assert_eq!(td.text, "Let me think about this...");
                assert!(td.signature.is_none());
                assert!(!td.redacted);
            }
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[test]
    fn parse_part_text_without_thought_flag() {
        let part = serde_json::json!({"text": "Hello world"});
        let result = parse_part(&part).expect("should parse text part");
        match result {
            ContentPart::Text(text) => assert_eq!(text, "Hello world"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn parse_part_function_call() {
        let part = serde_json::json!({
            "functionCall": {
                "name": "get_weather",
                "args": {"location": "NYC"}
            }
        });
        let result = parse_part(&part).expect("should parse function call");
        match result {
            ContentPart::ToolCall(tc) => {
                assert_eq!(tc.name, "get_weather");
                assert_eq!(tc.arguments, serde_json::json!({"location": "NYC"}));
                assert!(tc.provider_metadata.is_none());
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_part_function_call_with_thought_signature() {
        let part = serde_json::json!({
            "functionCall": {
                "name": "get_weather",
                "args": {"location": "NYC"}
            },
            "thoughtSignature": "abc123sig"
        });
        let result = parse_part(&part).expect("should parse function call with thought signature");
        match result {
            ContentPart::ToolCall(tc) => {
                assert_eq!(tc.name, "get_weather");
                let meta = tc
                    .provider_metadata
                    .expect("provider_metadata should be set");
                assert_eq!(meta["thoughtSignature"], "abc123sig");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn translate_messages_function_call_includes_thought_signature() {
        let mut tc = ToolCall::new(
            "call-1",
            "get_weather",
            serde_json::json!({"location": "NYC"}),
        );
        tc.provider_metadata = Some(serde_json::json!({"thoughtSignature": "sig456"}));

        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tc)],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        assert_eq!(contents.len(), 1);

        let part = &contents[0].parts[0];
        assert!(part.get("functionCall").is_some());
        assert_eq!(part["thoughtSignature"], "sig456");
    }

    #[test]
    fn translate_messages_function_call_without_thought_signature() {
        let tc = ToolCall::new(
            "call-1",
            "get_weather",
            serde_json::json!({"location": "NYC"}),
        );

        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tc)],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        assert_eq!(contents.len(), 1);

        let part = &contents[0].parts[0];
        assert!(part.get("functionCall").is_some());
        assert!(part.get("thoughtSignature").is_none());
    }

    #[test]
    fn parse_part_thought_false_is_regular_text() {
        let part = serde_json::json!({"text": "Regular text", "thought": false});
        let result = parse_part(&part).expect("should parse text part");
        match result {
            ContentPart::Text(text) => assert_eq!(text, "Regular text"),
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
