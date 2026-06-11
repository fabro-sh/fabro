//! Streaming decoder: OpenAI Responses SSE events → canonical `StreamEvent`s.
//!
//! Byte reading and SSE block framing live in the transport; this decoder is
//! fed framed `RawEvent`s. The event type is resolved from the SSE `event:`
//! line or the JSON `type` field. The Responses API finishes via
//! `response.completed` / `response.incomplete`; byte-stream end synthesizes
//! nothing, so `finish()` returns an empty list.

use serde::Deserialize;

use super::decode::{map_finish_reason, token_counts_from_api_usage, tool_call_from_item};
use super::wire::ApiUsage;
use crate::codec::{CodecCtx, RawEvent, StreamDecoder};
use crate::error::{Error, ProviderErrorDetail, ProviderErrorKind};
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Response, Role, StreamEvent, TokenCounts,
    ToolCall,
};

/// Map an OpenAI stream `error` / `response.failed` payload to a provider
/// error, classifying on `code` falling back to `type`.
fn provider_error_from_openai_error_json(error: &serde_json::Value, provider: &str) -> Error {
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
            .or_else(|| json.get("type").and_then(serde_json::Value::as_str))
            .unwrap_or_default();

        match resolved_type {
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
            .unwrap_or("");
        let item_id = json
            .get("item_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let lookup_id = if call_id.is_empty() { item_id } else { call_id };

        let idx = if let Some(idx) = self.tool_calls.iter().position(|tc| tc.id == lookup_id) {
            let tc = &mut self.tool_calls[idx];
            if let Some(raw) = &mut tc.raw_arguments {
                raw.push_str(delta);
            }
            // Custom tool input is its raw string; keep `arguments` in sync as
            // it accumulates.
            if tool_type == "custom" {
                if let serde_json::Value::String(args) = &mut tc.arguments {
                    args.push_str(delta);
                }
            }
            idx
        } else {
            let name = json
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
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
            if !item_id.is_empty() && item_id != lookup_id {
                tc.provider_metadata = Some(serde_json::json!({"id": item_id}));
            }
            events.push(StreamEvent::ToolCallStart {
                tool_call: tc.clone(),
            });
            self.tool_calls.push(tc);
            self.tool_calls.len() - 1
        };

        // The delta event carries the call identity, the arguments
        // accumulated so far, and this chunk in `raw_arguments`.
        let current = &self.tool_calls[idx];
        let mut tool_call = ToolCall::new(&*current.id, &*current.name, current.arguments.clone());
        tool_call.tool_type = tool_type.to_string();
        tool_call.raw_arguments = Some(delta.to_string());
        tool_call
            .provider_metadata
            .clone_from(&current.provider_metadata);

        events.push(StreamEvent::ToolCallDelta { tool_call });
    }

    /// Handle `response.output_item.done` for text and function call items.
    fn handle_output_item_done(&mut self, json: &serde_json::Value, events: &mut Vec<StreamEvent>) {
        let item = json.get("item").unwrap_or(json);
        let item_type = item.get("type").and_then(serde_json::Value::as_str);

        match item_type {
            Some("reasoning") => {
                if self.emitted_reasoning_start {
                    self.emitted_reasoning_start = false;
                    events.push(StreamEvent::ReasoningEnd);
                }
                self.reasoning_items.push(item.clone());
            }
            Some("message") => {
                if self.emitted_text_start {
                    events.push(StreamEvent::TextEnd { text_id: None });
                    self.emitted_text_start = false;
                }
                self.message_items.push(item.clone());
            }
            Some(t @ ("function_call" | "custom_tool_call")) => {
                let tc = tool_call_from_item(item, t == "custom_tool_call");

                if let Some(existing) = self.tool_calls.iter_mut().find(|c| c.id == tc.id) {
                    existing.name.clone_from(&tc.name);
                    existing.tool_type.clone_from(&tc.tool_type);
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
            if let Ok(u) = ApiUsage::deserialize(usage_data) {
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
            content_parts.push(ContentPart::text(std::mem::take(
                &mut self.accumulated_text,
            )));
        }
        for tc in std::mem::take(&mut self.tool_calls) {
            // Skip tool calls with empty names (e.g. model-internal items)
            if tc.name.is_empty() {
                continue;
            }
            content_parts.push(ContentPart::ToolCall(tc));
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
            raw: Some(response_data.clone()),
            warnings: vec![],
            rate_limit: self.rate_limit.clone(),
            cost_usd: None,
            cost_source: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an accumulator without threading a `CodecCtx`/`Request`: the test
    /// module sees the private fields, so the few that matter are set
    /// directly. `emitted_start` is true so event assertions don't see the
    /// initial `StreamStart`.
    fn empty_accumulator() -> SseAccumulator {
        SseAccumulator {
            model:                   String::new(),
            provider:                "openai".to_string(),
            response_id:             String::new(),
            response_model:          String::new(),
            accumulated_text:        String::new(),
            tool_calls:              Vec::new(),
            reasoning_items:         Vec::new(),
            message_items:           Vec::new(),
            usage:                   TokenCounts::default(),
            finish_reason:           FinishReason::Stop,
            emitted_start:           true,
            emitted_text_start:      false,
            emitted_reasoning_start: false,
            rate_limit:              None,
        }
    }

    fn on_event(
        acc: &mut SseAccumulator,
        event: Option<&str>,
        data: &str,
    ) -> Result<Vec<StreamEvent>, Error> {
        acc.on_event(RawEvent { event, data })
    }

    #[test]
    fn token_counts_disjoint_with_cache_and_reasoning() {
        let mut acc = empty_accumulator();
        let body = serde_json::json!({
            "response": {
                "id": "resp_test",
                "model": "gpt-5",
                "output": [],
                "status": "completed",
                "usage": {
                    "input_tokens": 200,
                    "input_tokens_details": { "cached_tokens": 180 },
                    "output_tokens": 500,
                    "output_tokens_details": { "reasoning_tokens": 300 },
                    "total_tokens": 700
                }
            }
        });
        let mut events = Vec::new();

        acc.handle_response_completed(&body, &mut events);

        assert_eq!(acc.usage.input_tokens, 20);
        assert_eq!(acc.usage.cache_read_tokens, 180);
        assert_eq!(acc.usage.output_tokens, 200);
        assert_eq!(acc.usage.reasoning_tokens, 300);
        assert_eq!(acc.usage.cache_write_tokens, 0);
        assert_eq!(acc.usage.total_tokens(), 700);
    }

    #[test]
    fn custom_tool_call_streaming_delta_accumulates_raw_input() {
        let mut acc = empty_accumulator();
        let first = r#"{
            "type": "response.custom_tool_call_input.delta",
            "item_id": "ctc_abc",
            "call_id": "call_001",
            "delta": "*** Begin"
        }"#;
        let second = r#"{
            "type": "response.custom_tool_call_input.delta",
            "item_id": "ctc_abc",
            "call_id": "call_001",
            "delta": " Patch\n"
        }"#;

        let first_events = on_event(
            &mut acc,
            Some("response.custom_tool_call_input.delta"),
            first,
        )
        .expect("first custom delta should parse");
        let second_events = on_event(
            &mut acc,
            Some("response.custom_tool_call_input.delta"),
            second,
        )
        .expect("second custom delta should parse");

        assert!(matches!(
            first_events.iter().find(|event| matches!(event, StreamEvent::ToolCallStart { .. })),
            Some(StreamEvent::ToolCallStart { tool_call })
                if tool_call.id == "call_001" && tool_call.tool_type == "custom"
        ));
        assert!(matches!(
            second_events.last(),
            Some(StreamEvent::ToolCallDelta { tool_call })
                if tool_call.raw_arguments.as_deref() == Some(" Patch\n")
                    && tool_call.tool_type == "custom"
        ));
        assert_eq!(
            acc.tool_calls[0].raw_arguments.as_deref(),
            Some("*** Begin Patch\n")
        );
    }

    #[test]
    fn custom_tool_call_output_item_done_emits_tool_call_end() {
        let mut acc = empty_accumulator();
        let patch = "*** Begin Patch\n*** Add File: hello.txt\n+hello\n*** End Patch\n";
        let data = serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "custom_tool_call",
                "id": "ctc_abc",
                "call_id": "call_001",
                "name": "apply_patch",
                "input": patch,
            }
        });

        let events = on_event(
            &mut acc,
            Some("response.output_item.done"),
            &data.to_string(),
        )
        .expect("custom output item should parse");

        assert!(matches!(
            events.last(),
            Some(StreamEvent::ToolCallEnd { tool_call })
                if tool_call.id == "call_001"
                    && tool_call.name == "apply_patch"
                    && tool_call.tool_type == "custom"
                    && tool_call.raw_arguments.as_deref() == Some(patch)
        ));
    }

    #[test]
    fn error_event_with_insufficient_quota_returns_provider_error() {
        let mut acc = empty_accumulator();
        let data = r#"{
            "type": "error",
            "error": {
                "type": "insufficient_quota",
                "code": "insufficient_quota",
                "message": "You exceeded your current quota.",
                "param": null
            }
        }"#;

        let err = on_event(&mut acc, Some("error"), data)
            .expect_err("error event should fail the stream");

        match err {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::QuotaExceeded);
                assert!(detail.message.contains("exceeded your current quota"));
                assert_eq!(detail.error_code.as_deref(), Some("insufficient_quota"));
                assert!(detail.raw.is_some());
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn error_event_classifies_on_type_when_code_absent() {
        let mut acc = empty_accumulator();
        let data = r#"{
            "type": "error",
            "error": {
                "type": "insufficient_quota",
                "message": "You exceeded your current quota."
            }
        }"#;

        let err = on_event(&mut acc, Some("error"), data)
            .expect_err("error event should fail the stream");

        match err {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::QuotaExceeded);
                assert_eq!(detail.error_code.as_deref(), Some("insufficient_quota"));
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn response_failed_event_with_server_error_returns_provider_error() {
        let mut acc = empty_accumulator();
        let data = r#"{
            "type": "response.failed",
            "response": {
                "status": "failed",
                "error": {
                    "type": "server_error",
                    "code": "server_error",
                    "message": "The server had an error while processing your request."
                }
            }
        }"#;

        let err = on_event(&mut acc, Some("response.failed"), data)
            .expect_err("response.failed should fail the stream");

        match err {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::Server);
                assert!(detail.message.contains("server had an error"));
                assert_eq!(detail.error_code.as_deref(), Some("server_error"));
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn response_incomplete_preserves_partial_text() {
        let mut acc = empty_accumulator();

        on_event(
            &mut acc,
            Some("response.created"),
            r#"{"type":"response.created","response":{"id":"resp_123","model":"gpt-5.4"}}"#,
        )
        .expect("created event should parse");
        on_event(
            &mut acc,
            Some("response.output_text.delta"),
            r#"{"type":"response.output_text.delta","delta":"Hel"}"#,
        )
        .expect("first delta should parse");
        on_event(
            &mut acc,
            Some("response.output_text.delta"),
            r#"{"type":"response.output_text.delta","delta":"lo"}"#,
        )
        .expect("second delta should parse");

        let events = on_event(
            &mut acc,
            Some("response.incomplete"),
            r#"{
                "type": "response.incomplete",
                "response": {
                    "id": "resp_123",
                    "model": "gpt-5.4",
                    "status": "incomplete"
                }
            }"#,
        )
        .expect("incomplete response should finish normally");

        let finish = events
            .last()
            .expect("incomplete response should emit finish");
        match finish {
            StreamEvent::Finish {
                finish_reason,
                response,
                ..
            } => {
                assert_eq!(finish_reason.clone(), FinishReason::Length);
                assert_eq!(response.text(), "Hello");
            }
            other => panic!("expected finish event, got {other:?}"),
        }
    }

    #[test]
    fn error_event_with_invalid_api_key_returns_authentication_error() {
        let mut acc = empty_accumulator();
        let data = r#"{
            "type": "error",
            "error": {
                "type": "invalid_api_key",
                "code": "invalid_api_key",
                "message": "Incorrect API key provided."
            }
        }"#;

        let err = on_event(&mut acc, Some("error"), data)
            .expect_err("error event should fail the stream");

        match err {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::Authentication);
                assert_eq!(detail.error_code.as_deref(), Some("invalid_api_key"));
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn error_event_with_rate_limit_error_returns_rate_limit() {
        let mut acc = empty_accumulator();
        let data = r#"{
            "type": "error",
            "error": {
                "type": "rate_limit_error",
                "message": "Too many requests."
            }
        }"#;

        let err = on_event(&mut acc, Some("error"), data)
            .expect_err("error event should fail the stream");

        match err {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::RateLimit);
                assert_eq!(detail.error_code.as_deref(), Some("rate_limit_error"));
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn error_event_with_unknown_invalid_prefix_returns_invalid_request() {
        let mut acc = empty_accumulator();
        let data = r#"{
            "type": "error",
            "error": {
                "type": "invalid_prompt",
                "code": "invalid_prompt",
                "message": "Prompt is invalid."
            }
        }"#;

        let err = on_event(&mut acc, Some("error"), data)
            .expect_err("error event should fail the stream");

        match err {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::InvalidRequest);
                assert_eq!(detail.error_code.as_deref(), Some("invalid_prompt"));
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn error_event_with_unknown_code_falls_back_to_server_with_message() {
        let mut acc = empty_accumulator();
        let data = r#"{
            "type": "error",
            "error": {
                "type": "unexpected_stream_failure",
                "code": "unexpected_stream_failure",
                "message": "Unexpected stream failure."
            }
        }"#;

        let err = on_event(&mut acc, Some("error"), data)
            .expect_err("error event should fail the stream");

        match err {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::Server);
                assert_eq!(detail.message, "Unexpected stream failure.");
                assert_eq!(
                    detail.error_code.as_deref(),
                    Some("unexpected_stream_failure")
                );
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_summary_delta_emits_reasoning_events() {
        let mut acc = empty_accumulator();
        let data = r#"{"type":"response.reasoning_summary_text.delta","delta":"Let me think"}"#;
        let events = on_event(
            &mut acc,
            Some("response.reasoning_summary_text.delta"),
            data,
        )
        .expect("reasoning summary delta should parse");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::ReasoningStart));
        assert!(
            matches!(events[1], StreamEvent::ReasoningDelta { ref delta } if delta == "Let me think")
        );
    }

    #[test]
    fn reasoning_text_delta_emits_reasoning_events() {
        let mut acc = empty_accumulator();

        // First delta: should emit ReasoningStart + ReasoningDelta
        let data1 = r#"{"type":"response.reasoning_text.delta","delta":"Step 1"}"#;
        let events1 = on_event(&mut acc, Some("response.reasoning_text.delta"), data1)
            .expect("first reasoning delta should parse");
        assert_eq!(events1.len(), 2);
        assert!(matches!(events1[0], StreamEvent::ReasoningStart));
        assert!(
            matches!(events1[1], StreamEvent::ReasoningDelta { ref delta } if delta == "Step 1")
        );

        // Second delta: should NOT emit duplicate ReasoningStart
        let data2 = r#"{"type":"response.reasoning_text.delta","delta":"Step 2"}"#;
        let events2 = on_event(&mut acc, Some("response.reasoning_text.delta"), data2)
            .expect("second reasoning delta should parse");
        assert_eq!(events2.len(), 1);
        assert!(
            matches!(events2[0], StreamEvent::ReasoningDelta { ref delta } if delta == "Step 2")
        );
    }

    #[test]
    fn reasoning_end_emitted_on_item_done() {
        let mut acc = empty_accumulator();
        acc.emitted_reasoning_start = true;

        let data = r#"{"item":{"type":"reasoning","id":"rs_abc","summary":[]}}"#;
        let events = on_event(&mut acc, Some("response.output_item.done"), data)
            .expect("output item done should parse");
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::ReasoningEnd));
        assert!(!acc.emitted_reasoning_start);
        assert_eq!(acc.reasoning_items.len(), 1);
    }
}
