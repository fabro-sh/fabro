use futures::StreamExt;

use crate::error::{error_from_status_code, ProviderErrorDetail, ProviderErrorKind, SdkError};
use crate::provider::{ProviderAdapter, StreamEventStream};
use crate::providers::common::{
    parse_error_body, parse_rate_limit_headers, parse_retry_after, send_and_read_response,
};
use crate::types::{
    ContentPart, FinishReason, Message, Request, Response, ResponseFormat, ResponseFormatType,
    Role, StreamEvent, ThinkingData, ToolCall, ToolChoice, ToolDefinition, Usage,
};

/// `OpenAI`-compatible Chat Completions adapter (Section 7.10).
///
/// Use this for third-party services (vLLM, Ollama, Together AI, Groq, etc.)
/// that implement the `OpenAI` Chat Completions API (`/v1/chat/completions`).
///
/// Does NOT support reasoning tokens, built-in tools, or other Responses API
/// features. Use the primary `OpenAiAdapter` for `OpenAI`'s own API.
pub struct Adapter {
    pub(crate) http: super::http_api::HttpApi,
    provider_name: String,
}

impl Adapter {
    #[must_use]
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            http: super::http_api::HttpApi::new(api_key, base_url),
            provider_name: "openai-compatible".to_string(),
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    #[must_use]
    pub fn with_default_headers(self, headers: std::collections::HashMap<String, String>) -> Self {
        Self { http: self.http.with_default_headers(headers), ..self }
    }

    #[must_use]
    pub fn with_timeout(self, timeout: crate::types::AdapterTimeout) -> Self {
        Self { http: self.http.with_timeout(timeout), ..self }
    }

    /// Build a `reqwest::RequestBuilder` with default headers and auth.
    fn build_request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.client.post(url);
        // Apply default_headers first so adapter-specific headers can override
        for (key, value) in &self.http.default_headers {
            req = req.header(key, value);
        }
        req.bearer_auth(&self.http.api_key)
    }
}

// --- Request types (Chat Completions format) ---

#[derive(serde::Serialize)]
struct ApiRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(serde::Serialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    /// Reasoning/thinking content echoed back for providers that require it (Kimi).
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(serde::Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ChatFunction,
}

#[derive(serde::Serialize)]
struct ChatFunction {
    name: String,
    arguments: String,
}

// --- Response types (non-streaming) ---

#[derive(serde::Deserialize)]
struct ApiResponse {
    id: String,
    model: String,
    choices: Vec<ApiChoice>,
    usage: Option<ApiUsage>,
}

#[derive(serde::Deserialize)]
struct ApiChoice {
    message: ApiChoiceMessage,
    finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiChoiceMessage {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ApiToolCall>>,
}

#[derive(serde::Deserialize)]
struct ApiToolCall {
    id: String,
    function: ApiFunction,
}

#[derive(serde::Deserialize)]
struct ApiFunction {
    name: String,
    arguments: String,
}

#[derive(serde::Deserialize)]
struct ApiUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
}

// --- Streaming response types ---

#[derive(serde::Deserialize)]
struct StreamChunk {
    id: Option<String>,
    model: Option<String>,
    choices: Option<Vec<StreamChoice>>,
    usage: Option<ApiUsage>,
}

#[derive(serde::Deserialize)]
struct StreamChoice {
    delta: Option<StreamDelta>,
    finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct StreamDelta {
    content: Option<String>,
    /// Reasoning/thinking content (used by Kimi and other reasoning models).
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(serde::Deserialize)]
struct StreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<StreamFunction>,
}

#[derive(serde::Deserialize)]
struct StreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

// --- Accumulated tool call state for streaming ---

struct AccumulatedToolCall {
    id: String,
    name: String,
    arguments: String,
    started: bool,
}

fn map_finish_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("stop") | None => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("content_filter") => FinishReason::ContentFilter,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

/// Build the content string from a message's parts, including fallback text
/// for unsupported content types (Audio, Document).
fn content_text_with_fallbacks(parts: &[ContentPart]) -> String {
    let mut segments: Vec<String> = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text(text) => segments.push(text.clone()),
            ContentPart::Audio(_) => {
                segments.push("[Audio content not supported by this provider]".to_string());
            }
            ContentPart::Document(doc) => {
                let desc = doc.file_name.as_ref().map_or_else(
                    || "[Document content not supported by this provider]".to_string(),
                    |name| format!("[Document '{name}': content type not supported by this provider]"),
                );
                segments.push(desc);
            }
            _ => {}
        }
    }
    segments.join("")
}

fn translate_messages(messages: &[Message]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|msg| {
            let role = match msg.role {
                Role::System | Role::Developer => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };

            let mut tool_calls: Vec<ChatToolCall> = Vec::new();
            if msg.role == Role::Assistant {
                for part in &msg.content {
                    if let ContentPart::ToolCall(tc) = part {
                        let arguments = tc
                            .raw_arguments
                            .clone()
                            .unwrap_or_else(|| tc.arguments.to_string());
                        tool_calls.push(ChatToolCall {
                            id: tc.id.clone(),
                            kind: "function".to_string(),
                            function: ChatFunction {
                                name: tc.name.clone(),
                                arguments,
                            },
                        });
                    }
                }
            }

            let text = content_text_with_fallbacks(&msg.content);
            let content = if text.is_empty() { None } else { Some(text) };
            let tool_calls = if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            };

            // Extract reasoning/thinking content for assistant messages.
            let reasoning_content = if msg.role == Role::Assistant {
                let reasoning: String = msg
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Thinking(t) if !t.redacted => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if reasoning.is_empty() {
                    None
                } else {
                    Some(reasoning)
                }
            } else {
                None
            };

            ChatMessage {
                role: role.to_string(),
                content,
                reasoning_content,
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls,
            }
        })
        .collect()
}

fn translate_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect()
}

fn translate_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!("none"),
        ToolChoice::Required => serde_json::json!("required"),
        ToolChoice::Named { tool_name } => {
            serde_json::json!({"type": "function", "function": {"name": tool_name}})
        }
    }
}

/// Translate unified `ResponseFormat` to Chat Completions `response_format`.
fn translate_response_format(format: &ResponseFormat) -> serde_json::Value {
    match format.kind {
        ResponseFormatType::Text => serde_json::json!({"type": "text"}),
        ResponseFormatType::JsonObject => serde_json::json!({"type": "json_object"}),
        ResponseFormatType::JsonSchema => {
            let mut json_schema = serde_json::json!({
                "name": "response",
                "strict": format.strict,
            });
            if let Some(schema) = &format.json_schema {
                json_schema["schema"] = schema.clone();
            }
            serde_json::json!({
                "type": "json_schema",
                "json_schema": json_schema,
            })
        }
    }
}

/// Build the API request body from a unified `Request`.
///
/// Returns a `serde_json::Value` so that `provider_options.<provider_name>` fields
/// can be merged into the request before sending.
fn build_api_request(request: &Request, stream: Option<bool>, provider_name: &str) -> serde_json::Value {
    let chat_messages = translate_messages(&request.messages);
    let tools = request.tools.as_ref().map(|t| translate_tools(t));
    let tool_choice = request.tool_choice.as_ref().map(translate_tool_choice);
    let response_format = request
        .response_format
        .as_ref()
        .map(translate_response_format);

    let api_request = ApiRequest {
        model: request.model.clone(),
        messages: chat_messages,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
        top_p: request.top_p,
        stop: request.stop_sequences.clone(),
        tools,
        tool_choice,
        response_format,
        stream,
    };

    let mut body = serde_json::to_value(&api_request).unwrap_or_default();
    merge_provider_options(&mut body, request.provider_options.as_ref(), provider_name);
    body
}

/// Merge `provider_options.<provider_name>` fields into the serialized API request body.
///
/// The provider name is configurable (e.g. "groq", "together", "openai-compatible"),
/// allowing each instance to have its own namespace in `provider_options`.
fn merge_provider_options(
    body: &mut serde_json::Value,
    provider_options: Option<&serde_json::Value>,
    provider_name: &str,
) {
    let Some(opts) = provider_options.and_then(|opts| opts.get(provider_name)) else {
        return;
    };
    let Some(body_map) = body.as_object_mut() else {
        return;
    };
    let Some(opts_map) = opts.as_object() else {
        return;
    };

    for (key, value) in opts_map {
        body_map.insert(key.clone(), value.clone());
    }
}

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn complete(&self, request: &Request) -> Result<Response, SdkError> {
        if let Some(tc) = &request.tool_choice {
            crate::provider::validate_tool_choice(self, tc)?;
        }
        let api_body = build_api_request(request, None, &self.provider_name);
        let url = format!("{}/chat/completions", self.http.base_url);

        let mut req = self.build_request(&url).json(&api_body);
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }
        let (body, headers) = send_and_read_response(req, &self.provider_name, "type").await?;

        let api_resp: ApiResponse =
            serde_json::from_str(&body).map_err(|e| SdkError::Network {
                message: format!("failed to parse response: {e}"),
            })?;

        let choice = api_resp.choices.first().ok_or_else(|| SdkError::Provider {
            kind: ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail::new(
                "no choices in response",
                &self.provider_name,
            )),
        })?;

        let mut content_parts = Vec::new();
        if let Some(reasoning) = &choice.message.reasoning_content {
            if !reasoning.is_empty() {
                content_parts.push(ContentPart::Thinking(ThinkingData {
                    text: reasoning.clone(),
                    signature: None,
                    redacted: false,
                }));
            }
        }
        if let Some(text) = &choice.message.content {
            if !text.is_empty() {
                content_parts.push(ContentPart::text(text));
            }
        }
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                let arguments = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::json!({}));
                let mut tool_call = ToolCall::new(&tc.id, &tc.function.name, arguments);
                tool_call.raw_arguments = Some(tc.function.arguments.clone());
                content_parts.push(ContentPart::ToolCall(tool_call));
            }
        }

        let finish_reason = map_finish_reason(choice.finish_reason.as_deref());

        let usage = api_resp
            .usage
            .as_ref()
            .map_or_else(Usage::default, |u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
                ..Usage::default()
            });

        Ok(Response {
            id: api_resp.id,
            model: api_resp.model,
            provider: self.provider_name.clone(),
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
            crate::provider::validate_tool_choice(self, tc)?;
        }
        let api_body = build_api_request(request, Some(true), &self.provider_name);
        let url = format!("{}/chat/completions", self.http.base_url);

        let http_resp = self
            .build_request(&url)
            .json(&api_body)
            .send()
            .await
            .map_err(|e| SdkError::Network {
                message: e.to_string(),
            })?;

        let status = http_resp.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(http_resp.headers());
            let body = http_resp
                .text()
                .await
                .map_err(|e| SdkError::Network {
                    message: e.to_string(),
                })?;
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

        let provider_name = self.provider_name.clone();
        let model = request.model.clone();
        let rate_limit = parse_rate_limit_headers(http_resp.headers());
        let stream_read_timeout = self.http.stream_read_timeout;

        let stream = futures::stream::unfold(
            StreamState::new(http_resp, provider_name, model, rate_limit, stream_read_timeout),
            |mut state| async move {
                loop {
                    let line = match state.next_line().await {
                        Ok(Some(line)) => line,
                        Ok(None) => {
                            // Stream ended without [DONE]. Some providers
                            // (e.g. Minimax) omit the sentinel. Emit
                            // accumulated finish events if we have content
                            // and haven't already emitted them.
                            if !state.finished
                                && (state.text_started || !state.tool_calls.is_empty())
                            {
                                let events = state.finish_events();
                                return Some((Ok(events), state));
                            }
                            return None;
                        }
                        Err(e) => return Some((Err(e), state)),
                    };

                    let line = line.trim();
                    if line.is_empty() || line.starts_with(':') {
                        continue;
                    }

                    let data = match line.strip_prefix("data:") {
                        Some(d) => d.trim(),
                        None => continue,
                    };

                    if data == "[DONE]" {
                        let events = state.finish_events();
                        return Some((Ok(events), state));
                    }

                    let chunk: StreamChunk = match serde_json::from_str(data) {
                        Ok(c) => c,
                        Err(e) => {
                            return Some((
                                Err(SdkError::Stream {
                                    message: format!("failed to parse SSE chunk: {e}"),
                                }),
                                state,
                            ));
                        }
                    };

                    if let Some(events) = state.process_chunk(&chunk) {
                        return Some((Ok(events), state));
                    }
                }
            },
        );

        // Flatten batched events into individual stream events.
        let flat_stream = futures::stream::unfold(
            FlattenState {
                inner: Box::pin(stream),
                pending: Vec::new(),
            },
            |mut flatten_state| async {
                loop {
                    if let Some(event) = flatten_state.pending.pop() {
                        return Some((Ok(event), flatten_state));
                    }

                    match flatten_state.inner.next().await {
                        Some(Ok(mut events)) => {
                            // Reverse so we can pop from the end in order.
                            events.reverse();
                            flatten_state.pending = events;
                        }
                        Some(Err(e)) => return Some((Err(e), flatten_state)),
                        None => return None,
                    }
                }
            },
        );

        Ok(Box::pin(flat_stream))
    }
}

/// State for flattening batched events into individual stream events.
struct FlattenState {
    inner: std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<Vec<StreamEvent>, SdkError>> + Send>,
    >,
    pending: Vec<StreamEvent>,
}

/// Accumulated state while processing the SSE stream.
struct StreamState {
    line_reader: super::common::LineReader,
    provider_name: String,
    model: String,
    response_id: String,
    response_model: String,
    accumulated_text: String,
    accumulated_reasoning: String,
    tool_calls: Vec<AccumulatedToolCall>,
    usage: Usage,
    finish_reason: FinishReason,
    text_started: bool,
    done: bool,
    /// True after `finish_events()` has been called (guards against duplicates).
    finished: bool,
    rate_limit: Option<crate::types::RateLimitInfo>,
}

impl StreamState {
    fn new(
        response: reqwest::Response,
        provider_name: String,
        model: String,
        rate_limit: Option<crate::types::RateLimitInfo>,
        stream_read_timeout: Option<std::time::Duration>,
    ) -> Self {
        Self {
            line_reader: super::common::LineReader::new(response, stream_read_timeout),
            provider_name,
            model,
            response_id: String::new(),
            response_model: String::new(),
            accumulated_text: String::new(),
            accumulated_reasoning: String::new(),
            tool_calls: Vec::new(),
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
            text_started: false,
            done: false,
            finished: false,
            rate_limit,
        }
    }

    /// Read the next complete line from the SSE byte stream.
    async fn next_line(&mut self) -> Result<Option<String>, SdkError> {
        if self.done {
            return Ok(None);
        }
        match self.line_reader.read_next_chunk("\n").await? {
            Some(line) => Ok(Some(line)),
            None => {
                self.done = true;
                Ok(None)
            }
        }
    }

    /// Process a parsed SSE chunk and return events to emit, if any.
    fn process_chunk(&mut self, chunk: &StreamChunk) -> Option<Vec<StreamEvent>> {
        // Capture response metadata from the first chunk.
        if let Some(id) = &chunk.id {
            if self.response_id.is_empty() {
                self.response_id.clone_from(id);
            }
        }
        if let Some(model) = &chunk.model {
            if self.response_model.is_empty() {
                self.response_model.clone_from(model);
            }
        }

        // Capture usage if present (often in a dedicated chunk).
        if let Some(usage) = &chunk.usage {
            self.usage = Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                ..Usage::default()
            };
        }

        let choices = chunk.choices.as_ref()?;
        let choice = choices.first()?;

        let mut events = Vec::new();

        // Check for finish_reason.
        if let Some(reason) = &choice.finish_reason {
            self.finish_reason = map_finish_reason(Some(reason.as_str()));
        }

        let delta = choice.delta.as_ref()?;

        // Accumulate reasoning/thinking content (Kimi, etc.).
        if let Some(reasoning) = &delta.reasoning_content {
            if !reasoning.is_empty() {
                self.accumulated_reasoning.push_str(reasoning);
            }
        }

        // Handle text content delta.
        if let Some(content) = &delta.content {
            if !content.is_empty() {
                if !self.text_started {
                    self.text_started = true;
                    events.push(StreamEvent::TextStart { text_id: None });
                }
                self.accumulated_text.push_str(content);
                events.push(StreamEvent::text_delta(content, None));
            }
        }

        // Handle tool call deltas.
        if let Some(tool_calls) = &delta.tool_calls {
            for tc in tool_calls {
                let index = tc.index;

                // Grow the accumulated tool calls vector if needed.
                while self.tool_calls.len() <= index {
                    self.tool_calls.push(AccumulatedToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                        started: false,
                    });
                }

                let accumulated = &mut self.tool_calls[index];

                // First chunk for this tool call carries id and name.
                if let Some(id) = &tc.id {
                    accumulated.id.clone_from(id);
                }
                if let Some(func) = &tc.function {
                    if let Some(name) = &func.name {
                        accumulated.name.clone_from(name);
                    }
                    if let Some(args) = &func.arguments {
                        accumulated.arguments.push_str(args);
                    }
                }

                let partial_tool_call = ToolCall::new(
                    &accumulated.id,
                    &accumulated.name,
                    serde_json::json!(null),
                );

                if accumulated.started {
                    events.push(StreamEvent::ToolCallDelta {
                        tool_call: partial_tool_call,
                    });
                } else {
                    accumulated.started = true;
                    events.push(StreamEvent::ToolCallStart {
                        tool_call: partial_tool_call,
                    });
                }
            }
        }

        if events.is_empty() {
            None
        } else {
            Some(events)
        }
    }

    /// Generate the final events when `[DONE]` is received.
    fn finish_events(&mut self) -> Vec<StreamEvent> {
        self.finished = true;
        let mut events = Vec::new();

        // End text segment if it was started.
        if self.text_started {
            events.push(StreamEvent::TextEnd { text_id: None });
        }

        // End all tool calls with complete data.
        let mut content_parts = Vec::new();

        // Include reasoning/thinking content if present (Kimi, etc.).
        if !self.accumulated_reasoning.is_empty() {
            content_parts.push(ContentPart::Thinking(ThinkingData {
                text: std::mem::take(&mut self.accumulated_reasoning),
                signature: None,
                redacted: false,
            }));
        }

        if !self.accumulated_text.is_empty() {
            content_parts.push(ContentPart::text(&self.accumulated_text));
        }

        for accumulated in &self.tool_calls {
            let arguments = serde_json::from_str(&accumulated.arguments)
                .unwrap_or_else(|_| serde_json::json!({}));
            let mut tool_call =
                ToolCall::new(&accumulated.id, &accumulated.name, arguments);
            tool_call.raw_arguments = Some(accumulated.arguments.clone());

            events.push(StreamEvent::ToolCallEnd {
                tool_call: tool_call.clone(),
            });
            content_parts.push(ContentPart::ToolCall(tool_call));
        }

        // Infer finish reason from tool calls if not explicitly set.
        if !self.tool_calls.is_empty() && self.finish_reason == FinishReason::Stop {
            self.finish_reason = FinishReason::ToolCalls;
        }

        let response_model = if self.response_model.is_empty() {
            self.model.clone()
        } else {
            self.response_model.clone()
        };

        let response = Response {
            id: self.response_id.clone(),
            model: response_model,
            provider: self.provider_name.clone(),
            message: Message {
                role: Role::Assistant,
                content: content_parts,
                name: None,
                tool_call_id: None,
            },
            finish_reason: self.finish_reason.clone(),
            usage: self.usage.clone(),
            raw: None,
            warnings: vec![],
            rate_limit: self.rate_limit.clone(),
        };

        events.push(StreamEvent::finish(
            self.finish_reason.clone(),
            self.usage.clone(),
            response,
        ));

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_chunk_minimax_format() {
        let json = r#"{"id":"abc","choices":[{"index":0,"delta":{"content":"hello","role":"assistant","name":"MiniMax AI","audio_content":""}}],"created":1772268546,"model":"MiniMax-M2.5","object":"chat.completion.chunk","usage":null,"input_sensitive":false,"output_sensitive":false}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        let choices = chunk.choices.unwrap();
        let delta = choices[0].delta.as_ref().unwrap();
        assert_eq!(delta.content.as_deref(), Some("hello"));
    }

    #[test]
    fn stream_chunk_text_delta_parsing() {
        let json = r#"{"id":"chatcmpl-1","model":"gpt-4","choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.id.as_deref(), Some("chatcmpl-1"));
        assert_eq!(chunk.model.as_deref(), Some("gpt-4"));
        let choices = chunk.choices.unwrap();
        assert_eq!(choices.len(), 1);
        let delta = choices[0].delta.as_ref().unwrap();
        assert_eq!(delta.content.as_deref(), Some("Hello"));
        assert!(choices[0].finish_reason.is_none());
    }

    #[test]
    fn stream_chunk_tool_call_parsing() {
        let json = r#"{"id":"chatcmpl-1","model":"gpt-4","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"ci"}}]},"finish_reason":null}]}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        let choices = chunk.choices.unwrap();
        let delta = choices[0].delta.as_ref().unwrap();
        let tc = &delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id.as_deref(), Some("call_1"));
        let func = tc.function.as_ref().unwrap();
        assert_eq!(func.name.as_deref(), Some("get_weather"));
        assert_eq!(func.arguments.as_deref(), Some("{\"ci"));
    }

    #[test]
    fn stream_chunk_usage_parsing() {
        let json = r#"{"id":"chatcmpl-1","model":"gpt-4","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.total_tokens, 30);
    }

    #[test]
    fn stream_chunk_finish_reason_parsing() {
        let json = r#"{"id":"chatcmpl-1","model":"gpt-4","choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        let choices = chunk.choices.unwrap();
        assert_eq!(choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn stream_state_process_text_chunks() {
        let http_resp = reqwest::Response::from(
            http::Response::builder()
                .status(200)
                .body("")
                .unwrap(),
        );
        let mut state = StreamState::new(http_resp, "test".into(), "model".into(), None, Some(std::time::Duration::from_secs(30)));

        // First text chunk should emit TextStart + TextDelta.
        let chunk1: StreamChunk = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#,
        ).unwrap();
        let events1 = state.process_chunk(&chunk1).unwrap();
        assert_eq!(events1.len(), 2);
        assert!(matches!(events1[0], StreamEvent::TextStart { .. }));
        assert!(matches!(events1[1], StreamEvent::TextDelta { .. }));

        // Second text chunk should emit only TextDelta (no second TextStart).
        let chunk2: StreamChunk = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"content":" world"},"finish_reason":null}]}"#,
        ).unwrap();
        let events2 = state.process_chunk(&chunk2).unwrap();
        assert_eq!(events2.len(), 1);
        assert!(matches!(events2[0], StreamEvent::TextDelta { .. }));

        assert_eq!(state.accumulated_text, "Hello world");
    }

    #[test]
    fn stream_state_process_tool_call_chunks() {
        let http_resp = reqwest::Response::from(
            http::Response::builder()
                .status(200)
                .body("")
                .unwrap(),
        );
        let mut state = StreamState::new(http_resp, "test".into(), "model".into(), None, Some(std::time::Duration::from_secs(30)));

        // First tool call chunk (has id and name) -> ToolCallStart.
        let chunk1: StreamChunk = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"fn1","arguments":"{\"k"}}]},"finish_reason":null}]}"#,
        ).unwrap();
        let events1 = state.process_chunk(&chunk1).unwrap();
        assert_eq!(events1.len(), 1);
        assert!(matches!(events1[0], StreamEvent::ToolCallStart { .. }));

        // Subsequent chunk (more arguments) -> ToolCallDelta.
        let chunk2: StreamChunk = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ey\"}"}}]},"finish_reason":null}]}"#,
        ).unwrap();
        let events2 = state.process_chunk(&chunk2).unwrap();
        assert_eq!(events2.len(), 1);
        assert!(matches!(events2[0], StreamEvent::ToolCallDelta { .. }));

        assert_eq!(state.tool_calls[0].arguments, r#"{"key"}"#);
    }

    #[test]
    fn stream_state_finish_events_text_only() {
        let http_resp = reqwest::Response::from(
            http::Response::builder()
                .status(200)
                .body("")
                .unwrap(),
        );
        let mut state = StreamState::new(http_resp, "test-provider".into(), "test-model".into(), None, Some(std::time::Duration::from_secs(30)));
        state.response_id = "resp-1".into();
        state.response_model = "gpt-4".into();
        state.accumulated_text = "Hello world".into();
        state.text_started = true;
        state.usage = Usage {
            input_tokens: 5,
            output_tokens: 10,
            total_tokens: 15,
            ..Usage::default()
        };

        let events = state.finish_events();
        // TextEnd + Finish
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::TextEnd { .. }));
        match &events[1] {
            StreamEvent::Finish {
                finish_reason,
                usage,
                response,
            } => {
                assert_eq!(*finish_reason, FinishReason::Stop);
                assert_eq!(usage.input_tokens, 5);
                assert_eq!(usage.output_tokens, 10);
                assert_eq!(response.text(), "Hello world");
                assert_eq!(response.id, "resp-1");
                assert_eq!(response.model, "gpt-4");
                assert_eq!(response.provider, "test-provider");
            }
            other => panic!("Expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn stream_state_finish_events_with_tool_calls() {
        let http_resp = reqwest::Response::from(
            http::Response::builder()
                .status(200)
                .body("")
                .unwrap(),
        );
        let mut state = StreamState::new(http_resp, "test".into(), "model".into(), None, Some(std::time::Duration::from_secs(30)));
        state.response_id = "resp-1".into();
        state.tool_calls.push(AccumulatedToolCall {
            id: "call_1".into(),
            name: "get_weather".into(),
            arguments: r#"{"city":"SF"}"#.into(),
            started: true,
        });

        let events = state.finish_events();
        // ToolCallEnd + Finish (no TextEnd since text_started is false)
        assert_eq!(events.len(), 2);
        match &events[0] {
            StreamEvent::ToolCallEnd { tool_call } => {
                assert_eq!(tool_call.id, "call_1");
                assert_eq!(tool_call.name, "get_weather");
                assert_eq!(tool_call.raw_arguments.as_deref(), Some(r#"{"city":"SF"}"#));
            }
            other => panic!("Expected ToolCallEnd, got {other:?}"),
        }
        match &events[1] {
            StreamEvent::Finish {
                finish_reason,
                response,
                ..
            } => {
                assert_eq!(*finish_reason, FinishReason::ToolCalls);
                let calls = response.tool_calls();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_weather");
            }
            other => panic!("Expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn stream_state_uses_request_model_as_fallback() {
        let http_resp = reqwest::Response::from(
            http::Response::builder()
                .status(200)
                .body("")
                .unwrap(),
        );
        let mut state = StreamState::new(http_resp, "test".into(), "fallback-model".into(), None, Some(std::time::Duration::from_secs(30)));
        // response_model is empty, so finish_events should use the request model.
        let events = state.finish_events();
        match &events[0] {
            StreamEvent::Finish { response, .. } => {
                assert_eq!(response.model, "fallback-model");
            }
            other => panic!("Expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn api_request_stream_field_serialization() {
        let req = ApiRequest {
            model: "test".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            stream: Some(true),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["stream"], true);

        // When stream is None, it should be omitted.
        let req_no_stream = ApiRequest {
            model: "test".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            stream: None,
        };
        let json_no_stream = serde_json::to_value(&req_no_stream).unwrap();
        assert!(json_no_stream.get("stream").is_none());
    }

    #[test]
    fn translate_assistant_message_with_tool_calls_only() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall(ToolCall::new(
                "call_1",
                "get_weather",
                serde_json::json!({"city": "SF"}),
            ))],
            name: None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        assert_eq!(translated.len(), 1);
        assert_eq!(translated[0].role, "assistant");
        assert!(translated[0].content.is_none());
        let tool_calls = translated[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].kind, "function");
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(tool_calls[0].function.arguments, r#"{"city":"SF"}"#);
    }

    #[test]
    fn translate_assistant_message_with_text_and_tool_calls() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentPart::text("Let me check the weather"),
                ContentPart::ToolCall(ToolCall::new(
                    "call_2",
                    "get_weather",
                    serde_json::json!({"city": "NYC"}),
                )),
            ],
            name: None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        assert_eq!(translated[0].content.as_deref(), Some("Let me check the weather"));
        let tool_calls = translated[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "get_weather");
    }

    #[test]
    fn translate_assistant_message_with_raw_arguments() {
        let mut tc = ToolCall::new("call_3", "search", serde_json::json!({"q": "rust"}));
        tc.raw_arguments = Some(r#"{"q": "rust"}"#.to_string());
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall(tc)],
            name: None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        let tool_calls = translated[0].tool_calls.as_ref().unwrap();
        // Should prefer raw_arguments over serializing arguments
        assert_eq!(tool_calls[0].function.arguments, r#"{"q": "rust"}"#);
    }

    #[test]
    fn translate_tool_message_has_tool_call_id() {
        let msg = Message::tool_result("call_1", serde_json::Value::String("72F and sunny".into()), false);
        let translated = translate_messages(&[msg]);
        assert_eq!(translated[0].role, "tool");
        assert_eq!(translated[0].tool_call_id.as_deref(), Some("call_1"));
        assert!(translated[0].tool_calls.is_none());
    }

    #[test]
    fn translate_user_message_has_no_tool_calls() {
        let msg = Message::user("Hello");
        let translated = translate_messages(&[msg]);
        assert_eq!(translated[0].role, "user");
        assert_eq!(translated[0].content.as_deref(), Some("Hello"));
        assert!(translated[0].tool_calls.is_none());
    }

    #[test]
    fn assistant_tool_calls_serialize_correctly() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall(ToolCall::new(
                "call_1",
                "get_weather",
                serde_json::json!({"city": "SF"}),
            ))],
            name: None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        let json = serde_json::to_value(&translated[0]).unwrap();
        assert!(json.get("content").is_none());
        assert!(json.get("tool_call_id").is_none());
        let tool_calls = json["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
    }

    fn minimal_request() -> Request {
        Request {
            model: "llama-3.1-70b".to_string(),
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
            metadata: None,
            provider_options: None,
        }
    }

    #[test]
    fn provider_options_none_produces_standard_body() {
        let request = minimal_request();
        let body = build_api_request(&request, None, "groq");
        assert_eq!(body["model"], "llama-3.1-70b");
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn provider_options_matching_name_merged() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "groq": {
                "frequency_penalty": 0.5,
                "presence_penalty": 0.3
            }
        }));

        let body = build_api_request(&request, None, "groq");
        assert_eq!(body["frequency_penalty"], 0.5);
        assert_eq!(body["presence_penalty"], 0.3);
    }

    #[test]
    fn provider_options_different_name_ignored() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "together": {
                "repetition_penalty": 1.2
            }
        }));

        let body = build_api_request(&request, None, "groq");
        assert!(body.get("repetition_penalty").is_none());
    }

    #[test]
    fn provider_options_uses_adapter_name() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "together": {
                "repetition_penalty": 1.2
            }
        }));

        let body = build_api_request(&request, None, "together");
        assert_eq!(body["repetition_penalty"], 1.2);
    }

    #[test]
    fn provider_options_preserves_standard_fields() {
        let mut request = minimal_request();
        request.temperature = Some(0.7);
        request.max_tokens = Some(200);
        request.provider_options = Some(serde_json::json!({
            "groq": {
                "frequency_penalty": 0.5
            }
        }));

        let body = build_api_request(&request, Some(true), "groq");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_tokens"], 200);
        assert_eq!(body["stream"], true);
        assert_eq!(body["frequency_penalty"], 0.5);
    }

    #[test]
    fn provider_options_can_override_model() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "groq": {
                "model": "custom-model"
            }
        }));

        let body = build_api_request(&request, None, "groq");
        assert_eq!(body["model"], "custom-model");
    }

    #[test]
    fn merge_provider_options_with_non_object_value() {
        let mut body = serde_json::json!({"model": "test"});
        let opts = serde_json::json!({"groq": "not-an-object"});
        merge_provider_options(&mut body, Some(&opts), "groq");
        // Should not crash and body should be unchanged
        assert_eq!(body["model"], "test");
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
        let translated = translate_messages(&[msg]);
        assert_eq!(
            translated[0].content.as_deref(),
            Some("[Audio content not supported by this provider]")
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
        let translated = translate_messages(&[msg]);
        assert_eq!(
            translated[0].content.as_deref(),
            Some("[Document 'report.pdf': content type not supported by this provider]")
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
        let translated = translate_messages(&[msg]);
        assert_eq!(
            translated[0].content.as_deref(),
            Some("[Document content not supported by this provider]")
        );
    }

    #[test]
    fn mixed_text_and_audio_content_concatenates() {
        let msg = Message {
            role: Role::User,
            content: vec![
                ContentPart::text("Check this: "),
                ContentPart::Audio(crate::types::AudioData {
                    url: None,
                    data: Some(vec![1, 2]),
                    media_type: None,
                }),
            ],
            name: None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        assert_eq!(
            translated[0].content.as_deref(),
            Some("Check this: [Audio content not supported by this provider]")
        );
    }
}
