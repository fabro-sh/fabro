//! Streaming decoder: Gemini SSE chunks → canonical `StreamEvent`s.
//!
//! Byte reading and line framing live in the transport; this decoder is fed
//! framed `RawEvent`s carrying bare `data:` payloads (Gemini uses data-only
//! SSE — no event types, no `[DONE]` sentinel). Gemini has no terminal wire
//! event, so `finish()` synthesizes the `Finish` from accumulated state
//! unconditionally at byte-stream end.

use super::decode::{map_finish_reason, parse_usage};
use super::wire::ApiResponse;
use crate::codec::{CodecCtx, RawEvent, StreamDecoder};
use crate::error::Error;
use crate::types::{
    ContentPart, Message, RateLimitInfo, Response, Role, StreamEvent, ThinkingData, TokenCounts,
    ToolCall,
};

/// Accumulated state across SSE chunks during streaming.
pub(super) struct SseAccumulator {
    /// Requested model, stamped into the synthesized final `Response`.
    model:                  String,
    /// Configured provider name stamped into the final `Response.provider`.
    provider:               String,
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

impl SseAccumulator {
    pub(super) fn new(ctx: &CodecCtx<'_>, rate_limit: Option<RateLimitInfo>) -> Self {
        Self {
            model: ctx.request.model.clone(),
            provider: ctx.provider_name.to_string(),
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

    /// Extract stream events from a parsed SSE chunk.
    fn process_chunk(&mut self, chunk: &ApiResponse) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        if !self.stream_started {
            self.stream_started = true;
            events.push(StreamEvent::StreamStart);
        }

        let parts = chunk
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.content.as_ref())
            .and_then(|c| c.parts.as_ref());

        if let Some(parts) = parts {
            for part in parts {
                let is_thought = part
                    .get("thought")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);

                if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                    if is_thought {
                        if !self.reasoning_started {
                            self.reasoning_started = true;
                            events.push(StreamEvent::ReasoningStart);
                        }
                        self.accumulated_thinking.push_str(text);
                        events.push(StreamEvent::ReasoningDelta {
                            delta: text.to_string(),
                        });
                    } else {
                        // Transition from reasoning to text: close reasoning segment.
                        if self.reasoning_started {
                            self.reasoning_started = false;
                            events.push(StreamEvent::ReasoningEnd);
                        }
                        if !self.text_started {
                            self.text_started = true;
                            events.push(StreamEvent::TextStart {
                                text_id: Some(self.text_id.clone()),
                            });
                        }
                        self.accumulated_text.push_str(text);
                        events.push(StreamEvent::text_delta(text, Some(self.text_id.clone())));
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
                    // Preserve thought_signature for Gemini 3 models (sibling of
                    // functionCall)
                    if let Some(sig) = part.get("thoughtSignature") {
                        tool_call.provider_metadata =
                            Some(serde_json::json!({"thoughtSignature": sig}));
                    }

                    // Gemini delivers function calls as complete objects in a single
                    // chunk.
                    events.push(StreamEvent::ToolCallStart {
                        tool_call: tool_call.clone(),
                    });
                    events.push(StreamEvent::ToolCallEnd {
                        tool_call: tool_call.clone(),
                    });
                    self.accumulated_tool_calls.push(tool_call);
                }
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
                events.push(StreamEvent::ReasoningEnd);
            }
            if self.text_started {
                events.push(StreamEvent::TextEnd {
                    text_id: Some(self.text_id.clone()),
                });
            }
        }

        events
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
            provider:      self.provider.clone(),
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

impl StreamDecoder for SseAccumulator {
    fn on_event(&mut self, ev: RawEvent<'_>) -> Result<Vec<StreamEvent>, Error> {
        // Parse the JSON chunk.
        let chunk: ApiResponse = serde_json::from_str(ev.data).map_err(|e| {
            Error::stream_error(format!("failed to parse Gemini SSE chunk: {e}"), e)
        })?;

        let events = self.process_chunk(&chunk);

        // Track usage from every chunk; the final one will have the totals.
        if let Some(ref usage_meta) = chunk.usage_metadata {
            self.usage = parse_usage(Some(usage_meta));
        }

        // Extract finish reason from the candidate if present.
        let candidate_finish = chunk
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.finish_reason.clone());
        if let Some(reason) = candidate_finish {
            self.finish_reason_str = Some(reason);
        }

        Ok(events)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        // Gemini has no terminal wire event: synthesize the Finish from
        // accumulated state, exactly once, at byte-stream end.
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        vec![self.build_finish_event()]
    }
}
