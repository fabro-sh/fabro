//! Streaming Chat Completions SSE parsing and finish-event assembly.

use futures::{StreamExt, stream};

use super::hooks::ChatHooks;
use super::translate::{map_finish_reason, parse_tool_arguments};
use super::wire::{AccumulatedToolCall, StreamChunk};
use crate::error::{Error, error_from_status_code};
use crate::provider::StreamEventStream;
use crate::providers::common::{
    LineReader, parse_error_body, parse_rate_limit_headers, parse_retry_after,
};
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Response, Role, StreamEvent, ThinkingData,
    TokenCounts, ToolCall,
};

/// State for flattening batched events into individual stream events.
pub(crate) struct FlattenState {
    pub(crate) inner:
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<StreamEvent>, Error>> + Send>>,
    pub(crate) pending: Vec<StreamEvent>,
}

/// Accumulated state while processing the SSE stream.
pub(crate) struct StreamState {
    pub(crate) line_reader:           LineReader,
    pub(crate) provider_name:         String,
    pub(crate) model:                 String,
    pub(crate) response_id:           String,
    pub(crate) response_model:        String,
    pub(crate) accumulated_text:      String,
    pub(crate) accumulated_reasoning: String,
    pub(crate) tool_calls:            Vec<AccumulatedToolCall>,
    pub(crate) usage:                 TokenCounts,
    pub(crate) finish_reason:         FinishReason,
    pub(crate) text_started:          bool,
    pub(crate) done:                  bool,
    /// True after `finish_events()` has been called (guards against
    /// duplicates).
    pub(crate) finished:              bool,
    pub(crate) rate_limit:            Option<RateLimitInfo>,
    /// Hooks (e.g. for enriching the final `Response` with provider-specific
    /// fields like OpenRouter's `usage.cost`).
    pub(crate) hooks:                 ChatHooks,
    /// Raw JSON of the most recent chunk that carried a `usage` block.
    /// Captured so that [`ChatHooks::enrich_response`] can pull
    /// provider-specific fields (e.g. `usage.cost`) out of the stream.
    pub(crate) last_usage_raw:        Option<serde_json::Value>,
    /// Names of tools on the request marked custom (freeform), used by
    /// [`parse_tool_arguments`] to preserve raw non-JSON arguments.
    pub(crate) custom_tool_names:     Vec<String>,
}

impl StreamState {
    pub(crate) fn new(
        response: fabro_http::Response,
        provider_name: String,
        model: String,
        rate_limit: Option<RateLimitInfo>,
        stream_read_timeout: Option<std::time::Duration>,
        hooks: ChatHooks,
        custom_tool_names: Vec<String>,
    ) -> Self {
        Self {
            line_reader: LineReader::new(response, stream_read_timeout),
            provider_name,
            model,
            response_id: String::new(),
            response_model: String::new(),
            accumulated_text: String::new(),
            accumulated_reasoning: String::new(),
            tool_calls: Vec::new(),
            usage: TokenCounts::default(),
            finish_reason: FinishReason::Stop,
            text_started: false,
            done: false,
            finished: false,
            rate_limit,
            hooks,
            last_usage_raw: None,
            custom_tool_names,
        }
    }

    /// Read the next complete line from the SSE byte stream.
    pub(crate) async fn next_line(&mut self) -> Result<Option<String>, Error> {
        if self.done {
            return Ok(None);
        }
        if let Some(line) = self.line_reader.read_next_chunk("\n").await? {
            Ok(Some(line))
        } else {
            self.done = true;
            Ok(None)
        }
    }

    /// Process a parsed SSE chunk and return events to emit, if any.
    ///
    /// `raw` is the same chunk re-parsed as `serde_json::Value` — when the
    /// chunk carries `usage`, it gets cached so that
    /// [`ChatHooks::enrich_response`] can read provider-specific fields out
    /// of it in [`Self::finish_events`].
    pub(crate) fn process_chunk(
        &mut self,
        chunk: &StreamChunk,
        raw: &serde_json::Value,
    ) -> Option<Vec<StreamEvent>> {
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
                cache_read_tokens: usage
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens)
                    .unwrap_or(0),
                cache_write_tokens: usage.cache_write_tokens.unwrap_or(0),
                ..TokenCounts::default()
            };
            self.last_usage_raw = Some(raw.clone());
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

    /// Generate the final events when `[DONE]` is received.
    pub(crate) fn finish_events(&mut self) -> Vec<StreamEvent> {
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

        let mut response = Response {
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

        if let Some(enrich) = self.hooks.enrich_response {
            let raw = self
                .last_usage_raw
                .clone()
                .unwrap_or(serde_json::Value::Null);
            enrich(&mut response, &raw);
        }

        events.push(StreamEvent::finish(
            self.finish_reason.clone(),
            self.usage.clone(),
            response,
        ));

        events
    }
}

/// Build the streaming `StreamEventStream` from an HTTP response that has
/// already been confirmed as success-status. Handles SSE parsing, chunk
/// accumulation, and final-event assembly.
pub(crate) fn run_stream(
    http_resp: fabro_http::Response,
    provider_name: String,
    model: String,
    rate_limit: Option<RateLimitInfo>,
    stream_read_timeout: Option<std::time::Duration>,
    hooks: ChatHooks,
    custom_tool_names: Vec<String>,
) -> StreamEventStream {
    let stream = stream::unfold(
        StreamState::new(
            http_resp,
            provider_name,
            model,
            rate_limit,
            stream_read_timeout,
            hooks,
            custom_tool_names,
        ),
        |mut state| async move {
            loop {
                let line = match state.next_line().await {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        // Stream ended without [DONE]. Some providers
                        // (e.g. Minimax) omit the sentinel. Emit
                        // accumulated finish events if we have content
                        // and haven't already emitted them.
                        if !state.finished && (state.text_started || !state.tool_calls.is_empty()) {
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

                let raw: serde_json::Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(e) => {
                        return Some((
                            Err(Error::stream_error(
                                format!("failed to parse SSE chunk: {e}"),
                                e,
                            )),
                            state,
                        ));
                    }
                };
                let chunk: StreamChunk = match serde_json::from_value(raw.clone()) {
                    Ok(c) => c,
                    Err(e) => {
                        return Some((
                            Err(Error::stream_error(
                                format!("failed to parse SSE chunk: {e}"),
                                e,
                            )),
                            state,
                        ));
                    }
                };

                if let Some(events) = state.process_chunk(&chunk, &raw) {
                    return Some((Ok(events), state));
                }
            }
        },
    );

    // Flatten batched events into individual stream events.
    let flat_stream = stream::unfold(
        FlattenState {
            inner:   Box::pin(stream),
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

    Box::pin(flat_stream)
}

/// Drive an already-built `fabro_http::RequestBuilder` to send the chat
/// streaming request and produce the unified [`StreamEventStream`].
pub(crate) async fn send_and_stream(
    req: fabro_http::RequestBuilder,
    provider_name: String,
    model: String,
    stream_read_timeout: Option<std::time::Duration>,
    hooks: ChatHooks,
    custom_tool_names: Vec<String>,
) -> Result<StreamEventStream, Error> {
    let http_resp = req
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
            provider_name,
            code,
            raw,
            retry_after,
        ));
    }

    let rate_limit = parse_rate_limit_headers(http_resp.headers());

    Ok(run_stream(
        http_resp,
        provider_name,
        model,
        rate_limit,
        stream_read_timeout,
        hooks,
        custom_tool_names,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::openai_chat::wire::{AccumulatedToolCall, StreamChunk};

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
        assert!(usage.prompt_tokens_details.is_none());
        assert!(usage.cache_write_tokens.is_none());
    }

    #[test]
    fn stream_chunk_usage_parses_cached_tokens() {
        let json = r#"{"id":"chatcmpl-1","model":"gpt-4","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":80},"cache_write_tokens":12}}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(
            usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            Some(80)
        );
        assert_eq!(usage.cache_write_tokens, Some(12));
    }

    #[test]
    fn stream_state_process_usage_populates_cache_counts() {
        let http_resp =
            fabro_http::Response::from(http::Response::builder().status(200).body("").unwrap());
        let mut state = StreamState::new(
            http_resp,
            "openrouter".into(),
            "model".into(),
            None,
            Some(std::time::Duration::from_secs(30)),
            ChatHooks::NONE,
            Vec::new(),
        );

        let raw: serde_json::Value = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":80},"cache_write_tokens":12}}"#,
        )
        .unwrap();
        let chunk: StreamChunk = serde_json::from_value(raw.clone()).unwrap();
        let _ = state.process_chunk(&chunk, &raw);
        assert_eq!(state.usage.input_tokens, 100);
        assert_eq!(state.usage.output_tokens, 50);
        assert_eq!(state.usage.cache_read_tokens, 80);
        assert_eq!(state.usage.cache_write_tokens, 12);
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
        let http_resp =
            fabro_http::Response::from(http::Response::builder().status(200).body("").unwrap());
        let mut state = StreamState::new(
            http_resp,
            "test".into(),
            "model".into(),
            None,
            Some(std::time::Duration::from_secs(30)),
            ChatHooks::NONE,
            Vec::new(),
        );

        // First text chunk should emit TextStart + TextDelta.
        let raw1: serde_json::Value = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#,
        ).unwrap();
        let chunk1: StreamChunk = serde_json::from_value(raw1.clone()).unwrap();
        let events1 = state.process_chunk(&chunk1, &raw1).unwrap();
        assert_eq!(events1.len(), 2);
        assert!(matches!(events1[0], StreamEvent::TextStart { .. }));
        assert!(matches!(events1[1], StreamEvent::TextDelta { .. }));

        // Second text chunk should emit only TextDelta (no second TextStart).
        let raw2: serde_json::Value = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"content":" world"},"finish_reason":null}]}"#,
        ).unwrap();
        let chunk2: StreamChunk = serde_json::from_value(raw2.clone()).unwrap();
        let events2 = state.process_chunk(&chunk2, &raw2).unwrap();
        assert_eq!(events2.len(), 1);
        assert!(matches!(events2[0], StreamEvent::TextDelta { .. }));

        assert_eq!(state.accumulated_text, "Hello world");
    }

    #[test]
    fn stream_state_process_tool_call_chunks() {
        let http_resp =
            fabro_http::Response::from(http::Response::builder().status(200).body("").unwrap());
        let mut state = StreamState::new(
            http_resp,
            "test".into(),
            "model".into(),
            None,
            Some(std::time::Duration::from_secs(30)),
            ChatHooks::NONE,
            Vec::new(),
        );

        // First tool call chunk (has id and name) -> ToolCallStart.
        let raw1: serde_json::Value = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"fn1","arguments":"{\"k"}}]},"finish_reason":null}]}"#,
        ).unwrap();
        let chunk1: StreamChunk = serde_json::from_value(raw1.clone()).unwrap();
        let events1 = state.process_chunk(&chunk1, &raw1).unwrap();
        assert_eq!(events1.len(), 1);
        assert!(matches!(events1[0], StreamEvent::ToolCallStart { .. }));

        // Subsequent chunk (more arguments) -> ToolCallDelta.
        let raw2: serde_json::Value = serde_json::from_str(
            r#"{"id":"c1","model":"m1","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ey\"}"}}]},"finish_reason":null}]}"#,
        ).unwrap();
        let chunk2: StreamChunk = serde_json::from_value(raw2.clone()).unwrap();
        let events2 = state.process_chunk(&chunk2, &raw2).unwrap();
        assert_eq!(events2.len(), 1);
        assert!(matches!(events2[0], StreamEvent::ToolCallDelta { .. }));

        assert_eq!(state.tool_calls[0].arguments, r#"{"key"}"#);
    }

    #[test]
    fn stream_state_finish_events_text_only() {
        let http_resp =
            fabro_http::Response::from(http::Response::builder().status(200).body("").unwrap());
        let mut state = StreamState::new(
            http_resp,
            "test-provider".into(),
            "test-model".into(),
            None,
            Some(std::time::Duration::from_secs(30)),
            ChatHooks::NONE,
            Vec::new(),
        );
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
        let http_resp =
            fabro_http::Response::from(http::Response::builder().status(200).body("").unwrap());
        let mut state = StreamState::new(
            http_resp,
            "test".into(),
            "model".into(),
            None,
            Some(std::time::Duration::from_secs(30)),
            ChatHooks::NONE,
            Vec::new(),
        );
        state.response_id = "resp-1".into();
        state.tool_calls.push(AccumulatedToolCall {
            id:        "call_1".into(),
            name:      "get_weather".into(),
            arguments: r#"{"city":"SF"}"#.into(),
            started:   true,
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
        let http_resp =
            fabro_http::Response::from(http::Response::builder().status(200).body("").unwrap());
        let mut state = StreamState::new(
            http_resp,
            "test".into(),
            "fallback-model".into(),
            None,
            Some(std::time::Duration::from_secs(30)),
            ChatHooks::NONE,
            Vec::new(),
        );
        // response_model is empty, so finish_events should use the request model.
        let events = state.finish_events();
        match &events[0] {
            StreamEvent::Finish { response, .. } => {
                assert_eq!(response.model, "fallback-model");
            }
            other => panic!("Expected Finish, got {other:?}"),
        }
    }
}
