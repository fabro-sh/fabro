//! Streaming decoder: Chat Completions SSE chunks → canonical `StreamEvent`s.
//!
//! Byte reading and `data:` framing live in the transport; this decoder is fed
//! already-stripped payloads (including the `[DONE]` sentinel) via `on_event`.

use super::translate::{map_finish_reason, parse_tool_arguments};
use super::wire::{AccumulatedToolCall, StreamChunk};
use crate::codec::{CodecCtx, RawEvent, StreamDecoder};
use crate::error::Error;
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Response, Role, StreamEvent, ThinkingData,
    TokenCounts, ToolCall,
};

/// Accumulated state while decoding the Chat Completions SSE stream.
pub(super) struct StreamState {
    provider_name:         String,
    model:                 String,
    response_id:           String,
    response_model:        String,
    accumulated_text:      String,
    accumulated_reasoning: String,
    tool_calls:            Vec<AccumulatedToolCall>,
    usage:                 TokenCounts,
    finish_reason:         FinishReason,
    text_started:          bool,
    custom_tool_names:     Vec<String>,
    /// True after `finish_events()` has run (guards against duplicates).
    finished:              bool,
    rate_limit:            Option<RateLimitInfo>,
}

impl StreamState {
    pub(super) fn new(ctx: &CodecCtx<'_>, rate_limit: Option<RateLimitInfo>) -> Self {
        Self {
            provider_name: ctx.provider_name.to_string(),
            model: ctx.request.model.clone(),
            response_id: String::new(),
            response_model: String::new(),
            accumulated_text: String::new(),
            accumulated_reasoning: String::new(),
            tool_calls: Vec::new(),
            usage: TokenCounts::default(),
            finish_reason: FinishReason::Stop,
            text_started: false,
            custom_tool_names: super::translate::custom_tool_names(ctx.request),
            finished: false,
            rate_limit,
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
            self.usage = TokenCounts {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                ..TokenCounts::default()
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
                        id:        String::new(),
                        name:      String::new(),
                        arguments: String::new(),
                        started:   false,
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

                let partial_tool_call =
                    ToolCall::new(&accumulated.id, &accumulated.name, serde_json::json!(null));

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

    /// Generate the final events when `[DONE]` (or end-of-stream) is received.
    fn finish_events(&mut self) -> Vec<StreamEvent> {
        self.finished = true;
        let mut events = Vec::new();

        // End text segment if it was started.
        if self.text_started {
            events.push(StreamEvent::TextEnd { text_id: None });
        }

        let mut content_parts = Vec::new();

        // Include reasoning/thinking content if present (Kimi, etc.).
        if !self.accumulated_reasoning.is_empty() {
            content_parts.push(ContentPart::Thinking(ThinkingData {
                text:      std::mem::take(&mut self.accumulated_reasoning),
                signature: None,
                redacted:  false,
            }));
        }

        if !self.accumulated_text.is_empty() {
            content_parts.push(ContentPart::text(&self.accumulated_text));
        }

        for accumulated in &self.tool_calls {
            let arguments = parse_tool_arguments(
                &accumulated.name,
                &accumulated.arguments,
                &self.custom_tool_names,
            );
            let mut tool_call = ToolCall::new(&accumulated.id, &accumulated.name, arguments);
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
            id:            self.response_id.clone(),
            model:         response_model,
            provider:      self.provider_name.clone(),
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
        };

        events.push(StreamEvent::finish(
            self.finish_reason.clone(),
            self.usage.clone(),
            response,
        ));

        events
    }
}

impl StreamDecoder for StreamState {
    fn on_event(&mut self, ev: RawEvent<'_>) -> Result<Vec<StreamEvent>, Error> {
        // Chat Completions uses data-only framing; the `event:` field is unused.
        if ev.data == "[DONE]" {
            return Ok(self.finish_events());
        }

        let chunk: StreamChunk = serde_json::from_str(ev.data)
            .map_err(|e| Error::stream_error(format!("failed to parse SSE chunk: {e}"), e))?;

        Ok(self.process_chunk(&chunk).unwrap_or_default())
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        // Stream ended without `[DONE]`. Some providers (e.g. Minimax) omit the
        // sentinel; emit accumulated finish events if we have content and
        // haven't already finished.
        if !self.finished && (self.text_started || !self.tool_calls.is_empty()) {
            self.finish_events()
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::CodecParams;
    use crate::types::Request;

    /// Build a decoder through `StreamState::new` (with a minimal request) for
    /// unit tests that drive `process_chunk` / `finish_events`.
    fn test_state(provider: &str, model: &str) -> StreamState {
        let request = Request {
            model:            model.to_string(),
            messages:         Vec::new(),
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
        };
        let params = CodecParams;
        let ctx = CodecCtx {
            request:       &request,
            provider_name: provider,
            deployment_id: model,
            model:         None,
            params:        &params,
        };
        StreamState::new(&ctx, None)
    }

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
    }

    #[test]
    fn stream_chunk_finish_reason_parsing() {
        let json = r#"{"id":"chatcmpl-1","model":"gpt-4","choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        let choices = chunk.choices.unwrap();
        assert_eq!(choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn process_text_chunks() {
        let mut state = test_state("test", "model");

        let chunk1: StreamChunk = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#,
        ).unwrap();
        let events1 = state.process_chunk(&chunk1).unwrap();
        assert_eq!(events1.len(), 2);
        assert!(matches!(events1[0], StreamEvent::TextStart { .. }));
        assert!(matches!(events1[1], StreamEvent::TextDelta { .. }));

        let chunk2: StreamChunk = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"content":" world"},"finish_reason":null}]}"#,
        ).unwrap();
        let events2 = state.process_chunk(&chunk2).unwrap();
        assert_eq!(events2.len(), 1);
        assert!(matches!(events2[0], StreamEvent::TextDelta { .. }));

        assert_eq!(state.accumulated_text, "Hello world");
    }

    #[test]
    fn process_tool_call_chunks() {
        let mut state = test_state("test", "model");

        let chunk1: StreamChunk = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"fn1","arguments":"{\"k"}}]},"finish_reason":null}]}"#,
        ).unwrap();
        let events1 = state.process_chunk(&chunk1).unwrap();
        assert_eq!(events1.len(), 1);
        assert!(matches!(events1[0], StreamEvent::ToolCallStart { .. }));

        let chunk2: StreamChunk = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ey\"}"}}]},"finish_reason":null}]}"#,
        ).unwrap();
        let events2 = state.process_chunk(&chunk2).unwrap();
        assert_eq!(events2.len(), 1);
        assert!(matches!(events2[0], StreamEvent::ToolCallDelta { .. }));

        assert_eq!(state.tool_calls[0].arguments, r#"{"key"}"#);
    }

    #[test]
    fn finish_events_text_only() {
        let mut state = test_state("test-provider", "test-model");
        state.response_id = "resp-1".into();
        state.response_model = "gpt-4".into();
        state.accumulated_text = "Hello world".into();
        state.text_started = true;
        state.usage = TokenCounts {
            input_tokens: 5,
            output_tokens: 10,
            ..TokenCounts::default()
        };

        let events = state.finish_events();
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
    fn finish_events_with_tool_calls() {
        let mut state = test_state("test", "model");
        state.response_id = "resp-1".into();
        state.tool_calls.push(AccumulatedToolCall {
            id:        "call_1".into(),
            name:      "get_weather".into(),
            arguments: r#"{"city":"SF"}"#.into(),
            started:   true,
        });

        let events = state.finish_events();
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
    fn uses_request_model_as_fallback() {
        let mut state = test_state("test", "fallback-model");
        let events = state.finish_events();
        match &events[0] {
            StreamEvent::Finish { response, .. } => {
                assert_eq!(response.model, "fallback-model");
            }
            other => panic!("Expected Finish, got {other:?}"),
        }
    }
}
