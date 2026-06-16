//! Streaming decoder: ConverseStream events → canonical `StreamEvent`s.
//!
//! Event names arrive in the transport's `RawEvent::event` (the frame's
//! `:event-type` header); payloads are the event JSON. The documented
//! sequence is `messageStart` → per content block (`contentBlockStart`
//! [tool use only] → `contentBlockDelta`* → `contentBlockStop`) →
//! `messageStop{stopReason}` → `metadata{usage}`. Usage arrives ONLY in the
//! terminal `metadata` event, which is also where the final `Finish` is
//! synthesized.

use std::collections::BTreeMap;

use serde_json::Value;

use super::decode::{map_stop_reason, token_counts_from_usage};
use crate::codec::{CodecCtx, RawEvent, StreamDecoder, parse_tool_arguments_or_empty};
use crate::error::Error;
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Response, Role, StreamEvent, ThinkingData,
    TokenCounts, ToolCall,
};

/// Per-content-block accumulation state, keyed by `contentBlockIndex`.
enum BlockState {
    Text(String),
    Reasoning {
        text:      String,
        signature: Option<String>,
        redacted:  Option<String>,
    },
    ToolUse {
        id:    String,
        name:  String,
        input: String,
    },
}

/// Accumulated state while decoding one ConverseStream response.
pub(super) struct ConverseStreamDecoder {
    provider_name: String,
    model:         String,
    blocks:        BTreeMap<u64, BlockState>,
    /// Completed blocks in arrival order, for the final response message.
    parts:         Vec<ContentPart>,
    finish_reason: FinishReason,
    usage:         TokenCounts,
    text_started:  bool,
    finished:      bool,
    rate_limit:    Option<RateLimitInfo>,
}

impl ConverseStreamDecoder {
    pub(super) fn new(ctx: &CodecCtx<'_>, rate_limit: Option<RateLimitInfo>) -> Self {
        Self {
            provider_name: ctx.provider_name.to_string(),
            model: ctx.request.model.clone(),
            blocks: BTreeMap::new(),
            parts: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: TokenCounts::default(),
            text_started: false,
            finished: false,
            rate_limit,
        }
    }

    fn block_index(payload: &Value) -> u64 {
        payload
            .get("contentBlockIndex")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    fn on_block_start(&mut self, payload: &Value) -> Vec<StreamEvent> {
        let index = Self::block_index(payload);
        if let Some(tool_use) = payload.pointer("/start/toolUse") {
            let id = tool_use
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let name = tool_use
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let started = ToolCall::new(&id, &name, Value::Null);
            self.blocks.insert(index, BlockState::ToolUse {
                id,
                name,
                input: String::new(),
            });
            return vec![StreamEvent::ToolCallStart { tool_call: started }];
        }
        Vec::new()
    }

    fn on_block_delta(&mut self, payload: &Value) -> Vec<StreamEvent> {
        let index = Self::block_index(payload);
        let Some(delta) = payload.get("delta") else {
            return Vec::new();
        };

        if let Some(text) = delta.get("text").and_then(Value::as_str) {
            if text.is_empty() {
                return Vec::new();
            }
            let mut events = Vec::new();
            if !self.text_started {
                self.text_started = true;
                events.push(StreamEvent::TextStart { text_id: None });
            }
            match self
                .blocks
                .entry(index)
                .or_insert_with(|| BlockState::Text(String::new()))
            {
                BlockState::Text(buffer) => buffer.push_str(text),
                // A text delta against a non-text block: tolerate by ignoring
                // the mismatch rather than corrupting tool/reasoning state.
                _ => return events,
            }
            events.push(StreamEvent::text_delta(text, None));
            return events;
        }

        if let Some(input) = delta.pointer("/toolUse/input").and_then(Value::as_str) {
            if let Some(BlockState::ToolUse {
                id,
                name,
                input: buffer,
            }) = self.blocks.get_mut(&index)
            {
                buffer.push_str(input);
                let partial = ToolCall::new(id.as_str(), name.as_str(), Value::Null);
                return vec![StreamEvent::ToolCallDelta { tool_call: partial }];
            }
            return Vec::new();
        }

        if let Some(reasoning) = delta.get("reasoningContent") {
            let entry = self
                .blocks
                .entry(index)
                .or_insert_with(|| BlockState::Reasoning {
                    text:      String::new(),
                    signature: None,
                    redacted:  None,
                });
            let BlockState::Reasoning {
                text,
                signature,
                redacted,
            } = entry
            else {
                return Vec::new();
            };
            let mut events = Vec::new();
            if text.is_empty() && signature.is_none() && redacted.is_none() {
                events.push(StreamEvent::ReasoningStart);
            }
            // Streaming reasoning deltas carry text/signature as FLAT union
            // members (unlike the nested request-side reasoningText block).
            if let Some(fragment) = reasoning.get("text").and_then(Value::as_str) {
                text.push_str(fragment);
                events.push(StreamEvent::ReasoningDelta {
                    delta: fragment.to_string(),
                });
            }
            if let Some(sig) = reasoning.get("signature").and_then(Value::as_str) {
                *signature = Some(sig.to_string());
            }
            if let Some(blob) = reasoning.get("redactedContent").and_then(Value::as_str) {
                *redacted = Some(blob.to_string());
            }
            return events;
        }

        Vec::new()
    }

    fn on_block_stop(&mut self, payload: &Value) -> Vec<StreamEvent> {
        let index = Self::block_index(payload);
        let Some(block) = self.blocks.remove(&index) else {
            return Vec::new();
        };
        match block {
            BlockState::Text(text) => {
                let mut events = Vec::new();
                if self.text_started {
                    self.text_started = false;
                    events.push(StreamEvent::TextEnd { text_id: None });
                }
                if !text.is_empty() {
                    self.parts.push(ContentPart::text(&text));
                }
                events
            }
            BlockState::Reasoning {
                text,
                signature,
                redacted,
            } => {
                let part = if let Some(blob) = redacted {
                    ThinkingData {
                        text:      blob,
                        signature: None,
                        redacted:  true,
                    }
                } else {
                    ThinkingData {
                        text,
                        signature,
                        redacted: false,
                    }
                };
                self.parts.push(ContentPart::Thinking(part));
                vec![StreamEvent::ReasoningEnd]
            }
            BlockState::ToolUse { id, name, input } => {
                // A no-argument tool call streams no input fragments, leaving
                // the buffer empty; canonically that is an empty object, not
                // null (matching the anthropic/openai codecs, and what Bedrock
                // wants back on re-encode).
                let arguments = parse_tool_arguments_or_empty(&input);
                let mut tool_call = ToolCall::new(&id, &name, arguments);
                tool_call.raw_arguments = Some(input);
                self.parts.push(ContentPart::ToolCall(tool_call.clone()));
                vec![StreamEvent::ToolCallEnd { tool_call }]
            }
        }
    }

    /// Build the final `Finish` from accumulated state.
    fn finish_event(&mut self) -> StreamEvent {
        self.finished = true;
        // Flush any blocks that never saw a contentBlockStop.
        let dangling: Vec<u64> = self.blocks.keys().copied().collect();
        for index in dangling {
            let _ = self.on_block_stop(&serde_json::json!({ "contentBlockIndex": index }));
        }

        let response = Response {
            id:            uuid::Uuid::new_v4().to_string(),
            model:         self.model.clone(),
            provider:      self.provider_name.clone(),
            message:       Message {
                role:         Role::Assistant,
                content:      std::mem::take(&mut self.parts),
                name:         None,
                tool_call_id: None,
            },
            finish_reason: self.finish_reason.clone(),
            usage:         self.usage.clone(),
            raw:           None,
            warnings:      vec![],
            rate_limit:    self.rate_limit.clone(),
            cost_usd:      None,
            cost_source:   None,
        };
        StreamEvent::finish(self.finish_reason.clone(), self.usage.clone(), response)
    }
}

impl StreamDecoder for ConverseStreamDecoder {
    fn on_event(&mut self, ev: RawEvent<'_>) -> Result<Vec<StreamEvent>, Error> {
        let Some(event_type) = ev.event else {
            return Ok(Vec::new());
        };
        let payload: Value = serde_json::from_str(ev.data)
            .map_err(|e| Error::stream_error(format!("converse stream event json: {e}"), e))?;

        Ok(match event_type {
            "messageStart" => vec![StreamEvent::StreamStart],
            "contentBlockStart" => self.on_block_start(&payload),
            "contentBlockDelta" => self.on_block_delta(&payload),
            "contentBlockStop" => self.on_block_stop(&payload),
            "messageStop" => {
                self.finish_reason =
                    map_stop_reason(payload.get("stopReason").and_then(Value::as_str));
                Vec::new()
            }
            "metadata" => {
                self.usage = token_counts_from_usage(payload.get("usage"));
                vec![self.finish_event()]
            }
            // Tolerate unknown event types — the union grows.
            _ => Vec::new(),
        })
    }

    /// Byte-stream end: `metadata` is the documented terminus, but if the
    /// stream ends without one, synthesize the `Finish` from accumulated
    /// state so callers still receive a response (mirrors the gemini
    /// decoder's unconditional synthesis).
    fn finish(&mut self) -> Vec<StreamEvent> {
        if self.finished {
            return Vec::new();
        }
        vec![self.finish_event()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::CodecParams;
    use crate::types::{Message as RequestMessage, Request};

    fn decoder() -> ConverseStreamDecoder {
        let request = Request {
            model:            "us.anthropic.claude-sonnet-4-6".to_string(),
            messages:         vec![RequestMessage::user("hi")],
            provider:         Some("bedrock".to_string()),
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
        let params = CodecParams::default();
        let ctx = CodecCtx {
            request:       &request,
            provider_name: "bedrock",
            deployment_id: "us.anthropic.claude-sonnet-4-6",
            model:         None,
            params:        &params,
        };
        ConverseStreamDecoder::new(&ctx, None)
    }

    fn feed(decoder: &mut ConverseStreamDecoder, event: &str, data: &str) -> Vec<StreamEvent> {
        decoder
            .on_event(RawEvent {
                event: Some(event),
                data,
            })
            .unwrap()
    }

    #[test]
    fn text_happy_path_finishes_on_metadata() {
        let mut d = decoder();
        assert!(matches!(
            feed(&mut d, "messageStart", r#"{"role":"assistant"}"#)[0],
            StreamEvent::StreamStart
        ));
        let events = feed(
            &mut d,
            "contentBlockDelta",
            r#"{"delta":{"text":"Hel"},"contentBlockIndex":0}"#,
        );
        assert!(matches!(events[0], StreamEvent::TextStart { .. }));
        assert!(matches!(events[1], StreamEvent::TextDelta { .. }));
        feed(
            &mut d,
            "contentBlockDelta",
            r#"{"delta":{"text":"lo"},"contentBlockIndex":0}"#,
        );
        let stop = feed(&mut d, "contentBlockStop", r#"{"contentBlockIndex":0}"#);
        assert!(matches!(stop[0], StreamEvent::TextEnd { .. }));
        assert!(feed(&mut d, "messageStop", r#"{"stopReason":"end_turn"}"#).is_empty());

        let finish = feed(
            &mut d,
            "metadata",
            r#"{"usage":{"inputTokens":12,"outputTokens":5,"totalTokens":17}}"#,
        );
        let StreamEvent::Finish {
            finish_reason,
            usage,
            response,
        } = &finish[0]
        else {
            panic!("expected Finish");
        };
        assert_eq!(*finish_reason, FinishReason::Stop);
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(response.text(), "Hello");
        assert_eq!(response.provider, "bedrock");
        // Byte-stream end after metadata adds nothing.
        assert!(d.finish().is_empty());
    }

    #[test]
    fn no_argument_tool_call_decodes_empty_object_not_null() {
        // A no-arg tool call (e.g. TaskList) streams no input fragments; the
        // arguments must be `{}` so it re-encodes to a valid Converse input.
        let mut d = decoder();
        feed(&mut d, "messageStart", r#"{"role":"assistant"}"#);
        feed(
            &mut d,
            "contentBlockStart",
            r#"{"start":{"toolUse":{"toolUseId":"tool-1","name":"TaskList"}},"contentBlockIndex":0}"#,
        );
        let stop = feed(&mut d, "contentBlockStop", r#"{"contentBlockIndex":0}"#);
        let StreamEvent::ToolCallEnd { tool_call } = &stop[0] else {
            panic!("expected ToolCallEnd");
        };
        assert_eq!(tool_call.arguments, serde_json::json!({}));
        assert!(!tool_call.arguments.is_null());
    }

    #[test]
    fn tool_use_accumulates_string_input_fragments() {
        let mut d = decoder();
        feed(&mut d, "messageStart", r#"{"role":"assistant"}"#);
        let start = feed(
            &mut d,
            "contentBlockStart",
            r#"{"start":{"toolUse":{"toolUseId":"tool-1","name":"search"}},"contentBlockIndex":0}"#,
        );
        assert!(matches!(start[0], StreamEvent::ToolCallStart { .. }));
        feed(
            &mut d,
            "contentBlockDelta",
            r#"{"delta":{"toolUse":{"input":"{\"que"}},"contentBlockIndex":0}"#,
        );
        feed(
            &mut d,
            "contentBlockDelta",
            r#"{"delta":{"toolUse":{"input":"ry\":\"foo\"}"}},"contentBlockIndex":0}"#,
        );
        let stop = feed(&mut d, "contentBlockStop", r#"{"contentBlockIndex":0}"#);
        let StreamEvent::ToolCallEnd { tool_call } = &stop[0] else {
            panic!("expected ToolCallEnd");
        };
        assert_eq!(tool_call.id, "tool-1");
        assert_eq!(tool_call.arguments["query"], "foo");

        feed(&mut d, "messageStop", r#"{"stopReason":"tool_use"}"#);
        let finish = feed(
            &mut d,
            "metadata",
            r#"{"usage":{"inputTokens":1,"outputTokens":1}}"#,
        );
        let StreamEvent::Finish { finish_reason, .. } = &finish[0] else {
            panic!("expected Finish");
        };
        assert_eq!(*finish_reason, FinishReason::ToolCalls);
    }

    #[test]
    fn reasoning_deltas_round_trip_signature() {
        let mut d = decoder();
        let events = feed(
            &mut d,
            "contentBlockDelta",
            r#"{"delta":{"reasoningContent":{"text":"thinking"}},"contentBlockIndex":0}"#,
        );
        assert!(matches!(events[0], StreamEvent::ReasoningStart));
        assert!(matches!(events[1], StreamEvent::ReasoningDelta { .. }));
        feed(
            &mut d,
            "contentBlockDelta",
            r#"{"delta":{"reasoningContent":{"signature":"sig-9"}},"contentBlockIndex":0}"#,
        );
        let stop = feed(&mut d, "contentBlockStop", r#"{"contentBlockIndex":0}"#);
        assert!(matches!(stop[0], StreamEvent::ReasoningEnd));

        let finish = feed(
            &mut d,
            "metadata",
            r#"{"usage":{"inputTokens":1,"outputTokens":1}}"#,
        );
        let StreamEvent::Finish { response, .. } = &finish[0] else {
            panic!("expected Finish");
        };
        let ContentPart::Thinking(thinking) = &response.message.content[0] else {
            panic!("expected thinking part");
        };
        assert_eq!(thinking.text, "thinking");
        assert_eq!(thinking.signature.as_deref(), Some("sig-9"));
    }

    #[test]
    fn stream_end_without_metadata_synthesizes_finish() {
        let mut d = decoder();
        feed(
            &mut d,
            "contentBlockDelta",
            r#"{"delta":{"text":"partial"},"contentBlockIndex":0}"#,
        );
        let events = d.finish();
        let StreamEvent::Finish { response, .. } = &events[0] else {
            panic!("expected synthesized Finish");
        };
        assert_eq!(response.text(), "partial");
        // Synthesis happens once.
        assert!(d.finish().is_empty());
    }

    #[test]
    fn unknown_events_are_tolerated() {
        let mut d = decoder();
        assert!(feed(&mut d, "futureEventKind", r#"{"anything":1}"#).is_empty());
        assert!(
            d.on_event(RawEvent {
                event: None,
                data:  "{}",
            })
            .unwrap()
            .is_empty()
        );
    }
}
