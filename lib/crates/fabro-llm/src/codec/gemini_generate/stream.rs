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
            cost_usd:      None,
            cost_source:   None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FinishReason;

    /// Build an accumulator without threading a `CodecCtx`/`Request`: the test
    /// module sees the private fields, so the few that matter are set
    /// directly. `stream_started` is true so event assertions don't see the
    /// initial `StreamStart`.
    fn empty_accumulator() -> SseAccumulator {
        SseAccumulator {
            model:                  "gemini-2.0-flash".to_string(),
            provider:               "gemini".to_string(),
            stream_started:         true,
            text_started:           false,
            reasoning_started:      false,
            accumulated_thinking:   String::new(),
            accumulated_text:       String::new(),
            accumulated_tool_calls: Vec::new(),
            text_id:                "text-1".to_string(),
            usage:                  TokenCounts::default(),
            finish_reason_str:      None,
            finished:               false,
            rate_limit:             None,
        }
    }

    fn on_data(acc: &mut SseAccumulator, data: &str) -> Result<Vec<StreamEvent>, Error> {
        acc.on_event(RawEvent { event: None, data })
    }

    #[test]
    fn first_chunk_emits_stream_start() {
        let mut acc = empty_accumulator();
        acc.stream_started = false;

        let events = on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]}}]}"#,
        )
        .expect("chunk should parse");

        assert!(matches!(events[0], StreamEvent::StreamStart));
        assert!(matches!(events[1], StreamEvent::TextStart { .. }));
    }

    #[test]
    fn text_deltas_accumulate_with_stable_text_id() {
        let mut acc = empty_accumulator();

        let first = on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"text":"Hel"}]}}]}"#,
        )
        .expect("first chunk should parse");
        let second = on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"text":"lo"}]}}]}"#,
        )
        .expect("second chunk should parse");

        assert!(
            matches!(&first[0], StreamEvent::TextStart { text_id: Some(id) } if id == "text-1")
        );
        assert!(
            matches!(&first[1], StreamEvent::TextDelta { delta, text_id: Some(id) } if delta == "Hel" && id == "text-1")
        );
        // Second chunk: no duplicate TextStart.
        assert_eq!(second.len(), 1);
        assert!(matches!(&second[0], StreamEvent::TextDelta { delta, .. } if delta == "lo"));
        assert_eq!(acc.accumulated_text, "Hello");
    }

    #[test]
    fn thought_then_text_transitions_reasoning_to_text() {
        let mut acc = empty_accumulator();

        let thought = on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"text":"Pondering...","thought":true}]}}]}"#,
        )
        .expect("thought chunk should parse");
        let text = on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"text":"Answer"}]}}]}"#,
        )
        .expect("text chunk should parse");

        assert!(matches!(thought[0], StreamEvent::ReasoningStart));
        assert!(
            matches!(&thought[1], StreamEvent::ReasoningDelta { delta } if delta == "Pondering...")
        );
        // Transition closes the reasoning segment before text begins.
        assert!(matches!(text[0], StreamEvent::ReasoningEnd));
        assert!(matches!(text[1], StreamEvent::TextStart { .. }));
        assert!(matches!(&text[2], StreamEvent::TextDelta { delta, .. } if delta == "Answer"));
        assert_eq!(acc.accumulated_thinking, "Pondering...");
    }

    #[test]
    fn function_call_emits_start_and_end_in_one_chunk() {
        let mut acc = empty_accumulator();

        let events = on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_weather","args":{"location":"NYC"}},"thoughtSignature":"sig1"}]}}]}"#,
        )
        .expect("function call chunk should parse");

        assert_eq!(events.len(), 2);
        let (start_tc, end_tc) = match (&events[0], &events[1]) {
            (
                StreamEvent::ToolCallStart { tool_call: start },
                StreamEvent::ToolCallEnd { tool_call: end },
            ) => (start, end),
            other => panic!("expected ToolCallStart + ToolCallEnd, got {other:?}"),
        };
        assert_eq!(start_tc.name, "get_weather");
        assert_eq!(start_tc.id, end_tc.id);
        assert_eq!(
            start_tc.provider_metadata.as_ref().unwrap()["thoughtSignature"],
            "sig1"
        );
        assert_eq!(acc.accumulated_tool_calls.len(), 1);
    }

    #[test]
    fn finish_reason_chunk_emits_text_end_and_records_reason() {
        let mut acc = empty_accumulator();
        on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]}}]}"#,
        )
        .expect("text chunk should parse");

        let events = on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5}}"#,
        )
        .expect("finish chunk should parse");

        assert!(matches!(&events[0], StreamEvent::TextEnd { text_id: Some(id) } if id == "text-1"));
        assert_eq!(acc.finish_reason_str.as_deref(), Some("STOP"));
        assert_eq!(acc.usage.input_tokens, 10);
        assert_eq!(acc.usage.output_tokens, 5);
    }

    #[test]
    fn finish_synthesizes_final_response_exactly_once() {
        let mut acc = empty_accumulator();
        on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}]},"finishReason":"STOP"}]}"#,
        )
        .expect("chunk should parse");

        let events = acc.finish();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Finish {
                finish_reason,
                response,
                ..
            } => {
                assert_eq!(*finish_reason, FinishReason::Stop);
                assert_eq!(response.text(), "Hello");
                assert_eq!(response.provider, "gemini");
                assert_eq!(response.model, "gemini-2.0-flash");
            }
            other => panic!("expected Finish, got {other:?}"),
        }

        // A second finish() (defensive) synthesizes nothing.
        assert!(acc.finish().is_empty());
    }

    #[test]
    fn finish_without_any_finish_reason_still_synthesizes() {
        // Gemini has no terminal wire event; byte-stream end must produce a
        // Finish even when no chunk carried a finishReason.
        let mut acc = empty_accumulator();
        on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"text":"partial"}]}}]}"#,
        )
        .expect("chunk should parse");

        let events = acc.finish();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], StreamEvent::Finish { finish_reason, .. }
            if *finish_reason == FinishReason::Stop)
        );
    }

    #[test]
    fn finish_infers_tool_calls_finish_reason() {
        let mut acc = empty_accumulator();
        on_data(
            &mut acc,
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"search","args":{}}}]},"finishReason":"STOP"}]}"#,
        )
        .expect("chunk should parse");

        let events = acc.finish();
        assert!(
            matches!(&events[0], StreamEvent::Finish { finish_reason, .. }
            if *finish_reason == FinishReason::ToolCalls)
        );
    }

    #[test]
    fn malformed_chunk_yields_stream_error() {
        let mut acc = empty_accumulator();
        let err = on_data(&mut acc, "not json").expect_err("bad chunk should error");
        assert!(matches!(err, Error::Stream { .. }));
    }
}
