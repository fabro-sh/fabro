//! Streaming decoder: Anthropic SSE events → canonical `StreamEvent`s.
//!
//! Byte reading and SSE block framing live in the transport; this decoder is
//! fed framed `RawEvent`s (`event:` type + `data:` JSON). Anthropic never
//! synthesizes a finish on byte-stream end — `message_stop` is the finisher —
//! so `finish()` returns nothing.

use super::SYNTHETIC_TOOL_NAME;
use super::decode::{convert_synthetic_tool_to_text, map_finish_reason, uses_json_schema_format};
use crate::codec::{CodecCtx, RawEvent, StreamDecoder};
use crate::error::{Error, ProviderErrorDetail, ProviderErrorKind};
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Response, Role, StreamEvent, ThinkingData,
    TokenCounts, ToolCall,
};

/// The type of the current content block being streamed.
#[derive(Clone)]
enum ContentBlockKind {
    Text,
    ToolUse { id: String, name: String },
    Thinking { signature: Option<String> },
}

/// Accumulated state across SSE events during streaming.
pub(super) struct SseAccumulator {
    id:                String,
    model:             String,
    /// Configured provider name stamped into the final `Response.provider`.
    provider:          String,
    /// When true, synthetic-tool events are rewritten to text events.
    json_schema_mode:  bool,
    content_parts:     Vec<ContentPart>,
    usage:             TokenCounts,
    finish_reason:     FinishReason,
    current_block:     Option<ContentBlockKind>,
    current_text:      String,
    current_thinking:  String,
    current_tool_args: String,
    rate_limit:        Option<RateLimitInfo>,
}

impl SseAccumulator {
    pub(super) fn new(ctx: &CodecCtx<'_>, rate_limit: Option<RateLimitInfo>) -> Self {
        Self {
            id: String::new(),
            model: String::new(),
            provider: ctx.provider_name.to_string(),
            json_schema_mode: uses_json_schema_format(ctx.request),
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

    fn take_response(&mut self) -> Response {
        let content_parts = std::mem::take(&mut self.content_parts);
        Response {
            id:            self.id.clone(),
            model:         self.model.clone(),
            provider:      self.provider.clone(),
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

    fn process_event(&mut self, event_type: &str, data: &serde_json::Value) -> Vec<StreamEvent> {
        match event_type {
            "message_start" => self.handle_message_start(data),
            "content_block_start" => self.handle_content_block_start(data),
            "content_block_delta" => self.handle_content_block_delta(data),
            "content_block_stop" => self.handle_content_block_stop(data),
            "message_delta" => {
                self.handle_message_delta(data);
                vec![]
            }
            "message_stop" => self.handle_message_stop(),
            _ => vec![],
        }
    }

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
                // Prefer signature from content_block_stop if available, fall
                // back to one captured at content_block_start.
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
        let response = self.take_response();
        vec![StreamEvent::Finish {
            finish_reason: response.finish_reason.clone(),
            usage:         response.usage.clone(),
            response:      Box::new(response),
        }]
    }
}

/// Map an Anthropic `error` stream event to a provider error.
fn stream_error_event_to_provider_error(data: &serde_json::Value, provider_name: &str) -> Error {
    let error = data.get("error").unwrap_or(data);
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| data.get("message").and_then(serde_json::Value::as_str))
        .unwrap_or("Unknown Anthropic stream error")
        .to_string();
    let error_code = error
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    let kind = match error_code.as_deref() {
        Some("rate_limit_error") => ProviderErrorKind::RateLimit,
        Some("authentication_error") => ProviderErrorKind::Authentication,
        Some("permission_error") => ProviderErrorKind::AccessDenied,
        Some("not_found_error") => ProviderErrorKind::NotFound,
        Some("invalid_request_error") => ProviderErrorKind::InvalidRequest,
        Some("request_too_large") => ProviderErrorKind::ContextLength,
        // overloaded_error, api_error, and unknown stream errors are transient.
        _ => ProviderErrorKind::Server,
    };

    Error::Provider {
        kind,
        detail: Box::new(ProviderErrorDetail {
            message,
            provider: provider_name.to_string(),
            status_code: None,
            error_code,
            retry_after: None,
            raw: Some(data.clone()),
        }),
    }
}

/// Rewrite a streaming event for `JsonSchema` mode: synthetic-tool events
/// become text events, and the Finish event's content + finish_reason are
/// adjusted.
fn convert_stream_event_for_json_schema(event: StreamEvent) -> StreamEvent {
    match event {
        StreamEvent::ToolCallStart { tool_call } if tool_call.name == SYNTHETIC_TOOL_NAME => {
            StreamEvent::TextStart { text_id: None }
        }
        StreamEvent::ToolCallDelta { tool_call } if tool_call.name == SYNTHETIC_TOOL_NAME => {
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

impl StreamDecoder for SseAccumulator {
    fn on_event(&mut self, ev: RawEvent<'_>) -> Result<Vec<StreamEvent>, Error> {
        let event_type = ev.event.unwrap_or("");
        let data: serde_json::Value = serde_json::from_str(ev.data)
            .map_err(|e| Error::stream_error(format!("failed to parse SSE data: {e}"), e))?;

        if event_type == "error" {
            return Err(stream_error_event_to_provider_error(&data, &self.provider));
        }

        let events = self.process_event(event_type, &data);
        if self.json_schema_mode {
            Ok(events
                .into_iter()
                .map(convert_stream_event_for_json_schema)
                .collect())
        } else {
            Ok(events)
        }
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        // Anthropic relies on `message_stop` to finish; nothing to synthesize.
        Vec::new()
    }
}
