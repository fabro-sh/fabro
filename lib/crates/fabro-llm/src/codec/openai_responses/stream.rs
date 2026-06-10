//! Streaming decoder: OpenAI Responses SSE events → canonical `StreamEvent`s.
//!
//! Byte reading and SSE block framing live in the transport; this decoder is
//! fed framed `RawEvent`s. The event type is resolved from the SSE `event:`
//! line or the JSON `type` field. The Responses API finishes via
//! `response.completed` / `response.incomplete`; byte-stream end synthesizes
//! nothing, so `finish()` returns an empty list.

use super::decode::{map_finish_reason, token_counts_from_api_usage};
use super::wire::ApiUsage;
use crate::codec::{CodecCtx, RawEvent, StreamDecoder};
use crate::error::{Error, ProviderErrorDetail, ProviderErrorKind};
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Response, Role, StreamEvent, TokenCounts,
    ToolCall,
};

/// Map an OpenAI stream `error` / `response.failed` payload to a provider
/// error, classifying on `code` falling back to `type`.
pub(super) fn provider_error_from_openai_error_json(
    error: &serde_json::Value,
    provider: &str,
) -> Error {
    let classifier = error
        .get("code")
        .and_then(serde_json::Value::as_str)
        .filter(|code| !code.is_empty())
        .or_else(|| {
            error
                .get("type")
                .and_then(serde_json::Value::as_str)
                .filter(|error_type| !error_type.is_empty())
        });
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .filter(|message| !message.is_empty())
        .map_or_else(|| "OpenAI stream error".to_string(), str::to_string);

    let kind = match classifier {
        Some("insufficient_quota" | "billing_hard_limit_reached") => {
            ProviderErrorKind::QuotaExceeded
        }
        Some("rate_limit_error" | "rate_limit_exceeded" | "too_many_requests") => {
            ProviderErrorKind::RateLimit
        }
        Some("authentication_error" | "invalid_api_key" | "invalid_authentication") => {
            ProviderErrorKind::Authentication
        }
        Some(
            "access_denied" | "account_deactivated" | "permission_denied" | "permission_error",
        ) => ProviderErrorKind::AccessDenied,
        Some("content_filter" | "content_policy_violation") => ProviderErrorKind::ContentFilter,
        Some("context_length_exceeded") => ProviderErrorKind::ContextLength,
        Some("server_error" | "internal_error" | "service_unavailable" | "engine_overloaded") => {
            ProviderErrorKind::Server
        }
        Some(code) if code.ends_with("_not_found") => ProviderErrorKind::NotFound,
        Some(code)
            if code.starts_with("invalid_")
                || code.starts_with("unsupported_")
                || code.ends_with("_too_large")
                || code.ends_with("_too_long") =>
        {
            ProviderErrorKind::InvalidRequest
        }
        Some(_) | None => ProviderErrorKind::Server,
    };

    Error::Provider {
        kind,
        detail: Box::new(ProviderErrorDetail {
            message,
            provider: provider.to_string(),
            status_code: None,
            error_code: classifier.map(str::to_string),
            retry_after: None,
            raw: Some(error.clone()),
        }),
    }
}

/// Accumulated state across SSE events during streaming.
pub(super) struct SseAccumulator {
    /// Requested model, used as the fallback when the response omits one.
    model:                   String,
    /// Configured provider name stamped into responses and error details.
    provider:                String,
    response_id:             String,
    response_model:          String,
    accumulated_text:        String,
    tool_calls:              Vec<ToolCall>,
    /// Raw reasoning output items to preserve for round-tripping.
    reasoning_items:         Vec<serde_json::Value>,
    /// Raw message output items to preserve for round-tripping.
    message_items:           Vec<serde_json::Value>,
    usage:                   TokenCounts,
    finish_reason:           FinishReason,
    emitted_start:           bool,
    emitted_text_start:      bool,
    emitted_reasoning_start: bool,
    raw_response:            Option<serde_json::Value>,
    rate_limit:              Option<RateLimitInfo>,
}

impl SseAccumulator {
    pub(super) fn new(ctx: &CodecCtx<'_>, rate_limit: Option<RateLimitInfo>) -> Self {
        Self {
            model: ctx.request.model.clone(),
            provider: ctx.provider_name.to_string(),
            response_id: String::new(),
            response_model: String::new(),
            accumulated_text: String::new(),
            tool_calls: Vec::new(),
            reasoning_items: Vec::new(),
            message_items: Vec::new(),
            usage: TokenCounts::default(),
            finish_reason: FinishReason::Stop,
            emitted_start: false,
            emitted_text_start: false,
            emitted_reasoning_start: false,
            raw_response: None,
            rate_limit,
        }
    }

    /// Process a single SSE event and return the corresponding
    /// `StreamEvent`(s).
    fn process_sse_event(
        &mut self,
        event_type: Option<&str>,
        data: &str,
    ) -> Result<Vec<StreamEvent>, Error> {
        let mut events = Vec::new();

        if !self.emitted_start {
            self.emitted_start = true;
            events.push(StreamEvent::StreamStart);
        }

        let json: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return Ok(events),
        };

        // Resolve event type from the `event:` SSE line or from the JSON `type`
        // field.
        let resolved_type = event_type
            .map(str::to_string)
            .or_else(|| {
                json.get("type")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();

        match resolved_type.as_str() {
            "error" => {
                let error = json.get("error").unwrap_or(&json);
                return Err(provider_error_from_openai_error_json(error, &self.provider));
            }
            "response.created" => self.handle_response_created(&json),
            "response.output_text.delta" => self.handle_text_delta(&json, &mut events),
            "response.function_call_arguments.delta" => {
                self.handle_tool_call_delta(&json, &mut events, "function");
            }
            "response.custom_tool_call_input.delta" => {
                self.handle_tool_call_delta(&json, &mut events, "custom");
            }
            "response.output_item.done" => self.handle_output_item_done(&json, &mut events),
            "response.completed" | "response.incomplete" => {
                self.handle_response_completed(&json, &mut events);
            }
            "response.failed" => {
                let error = json
                    .get("response")
                    .and_then(|response| response.get("error"))
                    .unwrap_or(&json);
                return Err(provider_error_from_openai_error_json(error, &self.provider));
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = json.get("delta").and_then(serde_json::Value::as_str) {
                    if !self.emitted_reasoning_start {
                        self.emitted_reasoning_start = true;
                        events.push(StreamEvent::ReasoningStart);
                    }
                    events.push(StreamEvent::ReasoningDelta {
                        delta: delta.to_string(),
                    });
                }
            }
            // response.reasoning_summary_part.added and other unrecognized
            // events are no-ops
            _ => {}
        }

        Ok(events)
    }

    /// Handle `response.created` by extracting the response ID and model.
    fn handle_response_created(&mut self, json: &serde_json::Value) {
        if let Some(id) = json
            .get("response")
            .and_then(|r| r.get("id"))
            .and_then(serde_json::Value::as_str)
        {
            self.response_id = id.to_string();
        }
        if let Some(model) = json
            .get("response")
            .and_then(|r| r.get("model"))
            .and_then(serde_json::Value::as_str)
        {
            self.response_model = model.to_string();
        }
    }

    /// Handle `response.output_text.delta` by accumulating text and emitting
    /// events.
    fn handle_text_delta(&mut self, json: &serde_json::Value, events: &mut Vec<StreamEvent>) {
        if let Some(delta) = json.get("delta").and_then(serde_json::Value::as_str) {
            if !self.emitted_text_start {
                self.emitted_text_start = true;
                events.push(StreamEvent::TextStart { text_id: None });
            }
            self.accumulated_text.push_str(delta);
            events.push(StreamEvent::text_delta(delta, None));
        }
    }

    /// Handle `response.function_call_arguments.delta` /
    /// `response.custom_tool_call_input.delta` by accumulating args and
    /// emitting events.
    fn handle_tool_call_delta(
        &mut self,
        json: &serde_json::Value,
        events: &mut Vec<StreamEvent>,
        tool_type: &str,
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

        let tc_index = self.tool_calls.iter().position(|tc| tc.id == *lookup_id);

        if let Some(idx) = tc_index {
            if let Some(ref mut raw) = self.tool_calls[idx].raw_arguments {
                raw.push_str(delta);
                if tool_type == "custom" {
                    self.tool_calls[idx].arguments = serde_json::json!(raw.clone());
                }
            }
        } else {
            let name = json
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut tc = ToolCall::new(
                lookup_id,
                name,
                if tool_type == "custom" {
                    serde_json::json!(delta)
                } else {
                    serde_json::json!({})
                },
            );
            tc.tool_type = tool_type.to_string();
            tc.raw_arguments = Some(delta.to_string());
            // Preserve item-level ID (fc_xxx) for Responses API round-trip
            if !item_id.is_empty() && item_id != *lookup_id {
                tc.provider_metadata = Some(serde_json::json!({"id": item_id}));
            }
            self.tool_calls.push(tc.clone());
            events.push(StreamEvent::ToolCallStart { tool_call: tc });
        }

        let current_tc = self
            .tool_calls
            .iter()
            .find(|tc| tc.id == *lookup_id)
            .cloned()
            .unwrap_or_else(|| ToolCall::new("", "", serde_json::json!({})));

        events.push(StreamEvent::ToolCallDelta {
            tool_call: ToolCall {
                tool_type: tool_type.to_string(),
                raw_arguments: Some(delta.to_string()),
                ..current_tc
            },
        });
    }

    /// Handle `response.output_item.done` for text and function call items.
    fn handle_output_item_done(&mut self, json: &serde_json::Value, events: &mut Vec<StreamEvent>) {
        let item_type = json
            .get("item")
            .and_then(|i| i.get("type"))
            .and_then(serde_json::Value::as_str);

        match item_type {
            Some("reasoning") => {
                if self.emitted_reasoning_start {
                    self.emitted_reasoning_start = false;
                    events.push(StreamEvent::ReasoningEnd);
                }
                let item = json.get("item").unwrap_or(json);
                self.reasoning_items.push(item.clone());
            }
            Some("message") => {
                if self.emitted_text_start {
                    events.push(StreamEvent::TextEnd { text_id: None });
                    self.emitted_text_start = false;
                }
                let item = json.get("item").unwrap_or(json);
                self.message_items.push(item.clone());
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

                if let Some(existing) = self.tool_calls.iter_mut().find(|t| t.id == call_id) {
                    existing.name.clone_from(&name);
                    existing.arguments = tc.arguments.clone();
                    existing.raw_arguments.clone_from(&tc.raw_arguments);
                    existing.provider_metadata.clone_from(&tc.provider_metadata);
                } else {
                    self.tool_calls.push(tc.clone());
                }

                events.push(StreamEvent::ToolCallEnd { tool_call: tc });
            }
            Some("custom_tool_call") => {
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
                let raw_input = item
                    .get("input")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");

                let mut tc = ToolCall::new(&call_id, &name, serde_json::json!(raw_input));
                tc.tool_type = "custom".to_string();
                tc.raw_arguments = Some(raw_input.to_string());
                if !item_id.is_empty() {
                    tc.provider_metadata = Some(serde_json::json!({"id": item_id}));
                }

                if let Some(existing) = self.tool_calls.iter_mut().find(|t| t.id == call_id) {
                    existing.name.clone_from(&name);
                    existing.tool_type = "custom".to_string();
                    existing.arguments = tc.arguments.clone();
                    existing.raw_arguments.clone_from(&tc.raw_arguments);
                    existing.provider_metadata.clone_from(&tc.provider_metadata);
                } else {
                    self.tool_calls.push(tc.clone());
                }

                events.push(StreamEvent::ToolCallEnd { tool_call: tc });
            }
            _ => {}
        }
    }

    /// Handle `response.completed` / `response.incomplete` by extracting usage
    /// and building the final response.
    fn handle_response_completed(
        &mut self,
        json: &serde_json::Value,
        events: &mut Vec<StreamEvent>,
    ) {
        let response_data = json.get("response").unwrap_or(json);

        if let Some(usage_data) = response_data.get("usage") {
            if let Ok(u) = serde_json::from_value::<ApiUsage>(usage_data.clone()) {
                self.usage = token_counts_from_api_usage(Some(&u));
            }
        }

        if let Some(id) = response_data.get("id").and_then(serde_json::Value::as_str) {
            self.response_id = id.to_string();
        }
        if let Some(model) = response_data
            .get("model")
            .and_then(serde_json::Value::as_str)
        {
            self.response_model = model.to_string();
        }

        let status = response_data
            .get("status")
            .and_then(serde_json::Value::as_str);
        let has_tool_calls = !self.tool_calls.is_empty();
        self.finish_reason = map_finish_reason(status, has_tool_calls);

        self.raw_response = Some(response_data.clone());

        let mut content_parts = Vec::new();
        // Reasoning items must precede function calls for Responses API
        // round-trip
        for item in std::mem::take(&mut self.reasoning_items) {
            content_parts.push(ContentPart::Other {
                kind: ContentPart::OPENAI_REASONING.to_string(),
                data: item,
            });
        }
        // Preserve full message output items for Responses API round-tripping
        for item in std::mem::take(&mut self.message_items) {
            content_parts.push(ContentPart::Other {
                kind: ContentPart::OPENAI_MESSAGE.to_string(),
                data: item,
            });
        }
        if !self.accumulated_text.is_empty() {
            content_parts.push(ContentPart::text(&self.accumulated_text));
        }
        for tc in &self.tool_calls {
            // Skip tool calls with empty names (e.g. model-internal items)
            if tc.name.is_empty() {
                continue;
            }
            content_parts.push(ContentPart::ToolCall(tc.clone()));
        }

        let model = if self.response_model.is_empty() {
            self.model.clone()
        } else {
            self.response_model.clone()
        };

        let response = Response {
            id: self.response_id.clone(),
            model,
            provider: self.provider.clone(),
            message: Message {
                role:         Role::Assistant,
                content:      content_parts,
                name:         None,
                tool_call_id: None,
            },
            finish_reason: self.finish_reason.clone(),
            usage: self.usage.clone(),
            raw: self.raw_response.clone(),
            warnings: vec![],
            rate_limit: self.rate_limit.clone(),
        };

        events.push(StreamEvent::finish(
            self.finish_reason.clone(),
            self.usage.clone(),
            response,
        ));
    }
}

impl StreamDecoder for SseAccumulator {
    fn on_event(&mut self, ev: RawEvent<'_>) -> Result<Vec<StreamEvent>, Error> {
        self.process_sse_event(ev.event, ev.data)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        // The Responses API finishes via `response.completed`/`.incomplete`;
        // nothing is synthesized at byte-stream end.
        Vec::new()
    }
}
