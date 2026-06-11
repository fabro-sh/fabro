//! Response decoding: OpenAI Responses API body → canonical `Response`.

use serde::Deserialize;

use super::wire::{ApiResponse, ApiUsage, InputTokensResponse};
use crate::codec::CodecCtx;
use crate::error::Error;
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Response, Role, TokenCounts, ToolCall,
};

pub(super) fn token_counts_from_api_usage(usage: Option<&ApiUsage>) -> TokenCounts {
    usage.map_or_else(TokenCounts::default, |u| {
        let cached_tokens = u
            .input_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0);
        let reasoning_tokens = u
            .output_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens)
            .unwrap_or(0);
        TokenCounts {
            input_tokens: u.input_tokens.saturating_sub(cached_tokens),
            output_tokens: u.output_tokens.saturating_sub(reasoning_tokens),
            reasoning_tokens,
            cache_read_tokens: cached_tokens,
            ..TokenCounts::default()
        }
    })
}

/// Map the Responses API status to a `FinishReason`.
pub(super) fn map_finish_reason(status: Option<&str>, has_tool_calls: bool) -> FinishReason {
    if has_tool_calls {
        return FinishReason::ToolCalls;
    }
    match status {
        Some("completed") | None => FinishReason::Stop,
        Some("incomplete") => FinishReason::Length,
        Some("failed") => FinishReason::Error,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

/// Build a `ToolCall` from a `function_call` / `custom_tool_call` output item.
/// The call-id/item-id round-trip rules live here, shared by the blocking and
/// streaming decode paths.
pub(super) fn tool_call_from_item(item: &serde_json::Value, custom: bool) -> ToolCall {
    let item_id = item
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let call_id = item
        .get("call_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(item_id);
    let name = item
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let mut tc = if custom {
        let raw_input = item
            .get("input")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let mut tc = ToolCall::new(call_id, name, serde_json::json!(raw_input));
        tc.tool_type = "custom".to_string();
        tc.raw_arguments = Some(raw_input.to_string());
        tc
    } else {
        let args_str = item
            .get("arguments")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("{}");
        let arguments = serde_json::from_str(args_str).unwrap_or_else(|_| serde_json::json!({}));
        let mut tc = ToolCall::new(call_id, name, arguments);
        tc.raw_arguments = Some(args_str.to_string());
        tc
    };
    // Preserve item-level ID (fc_xxx) for Responses API round-trip
    if !item_id.is_empty() {
        tc.provider_metadata = Some(serde_json::json!({"id": item_id}));
    }
    tc
}

/// Parse output items from the Responses API into content parts.
pub(super) fn parse_output(output: Vec<serde_json::Value>) -> (Vec<ContentPart>, bool) {
    let mut parts = Vec::new();
    let mut has_tool_calls = false;

    for item in output {
        let item_type = item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        match item_type.as_str() {
            "message" => {
                // Preserve the full message item for Responses API round-tripping.
                // The item's `id` and `status` fields are required so that reasoning
                // items preceding it can find their "required following item."
                let mut texts = Vec::new();
                if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                    for block in content {
                        if block.get("type").and_then(serde_json::Value::as_str)
                            == Some("output_text")
                        {
                            if let Some(text) =
                                block.get("text").and_then(serde_json::Value::as_str)
                            {
                                texts.push(ContentPart::text(text));
                            }
                        }
                    }
                }
                parts.push(ContentPart::Other {
                    kind: ContentPart::OPENAI_MESSAGE.to_string(),
                    data: item,
                });
                parts.extend(texts);
            }
            "reasoning" => {
                parts.push(ContentPart::Other {
                    kind: ContentPart::OPENAI_REASONING.to_string(),
                    data: item,
                });
            }
            "function_call" | "custom_tool_call" => {
                let tc = tool_call_from_item(&item, item_type == "custom_tool_call");
                // Skip tool calls with empty names (e.g. model-internal items)
                if tc.name.is_empty() {
                    continue;
                }
                has_tool_calls = true;
                parts.push(ContentPart::ToolCall(tc));
            }
            _ => {}
        }
    }

    (parts, has_tool_calls)
}

pub(super) fn decode_response(
    body: &str,
    ctx: &CodecCtx<'_>,
    rate_limit: Option<RateLimitInfo>,
) -> Result<Response, Error> {
    let raw: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| Error::network(format!("failed to parse OpenAI response: {e}"), e))?;
    let api_resp = ApiResponse::deserialize(&raw)
        .map_err(|e| Error::network(format!("failed to parse OpenAI response: {e}"), e))?;

    let (content_parts, has_tool_calls) = parse_output(api_resp.output);
    let finish_reason = map_finish_reason(api_resp.status.as_deref(), has_tool_calls);

    let usage = token_counts_from_api_usage(api_resp.usage.as_ref());

    Ok(Response {
        id: api_resp.id,
        model: api_resp.model.unwrap_or_else(|| ctx.request.model.clone()),
        provider: ctx.provider_name.to_string(),
        message: Message {
            role:         Role::Assistant,
            content:      content_parts,
            name:         None,
            tool_call_id: None,
        },
        finish_reason,
        usage,
        raw: Some(raw),
        warnings: vec![],
        rate_limit,
        cost_usd: None,
        cost_source: None,
    })
}

pub(super) fn decode_count_tokens(body: &str) -> Result<i64, Error> {
    let response: InputTokensResponse =
        serde_json::from_str(body).map_err(|e| Error::Configuration {
            message: format!("failed to parse OpenAI input token response: {e}"),
            source:  None,
        })?;

    if response.object != "response.input_tokens" {
        return Err(Error::Configuration {
            message: format!(
                "failed to parse OpenAI input token response: unexpected object '{}'",
                response.object
            ),
            source:  None,
        });
    }

    Ok(response.input_tokens)
}

#[cfg(test)]
mod tests {
    use super::super::encode;
    use super::*;

    #[test]
    fn parse_output_preserves_both_ids_on_function_call() {
        let output = vec![serde_json::json!({
            "type": "function_call",
            "id": "fc_abc123",
            "call_id": "call_xyz789",
            "name": "get_weather",
            "arguments": "{\"location\":\"NYC\"}"
        })];
        let (parts, has_tool_calls) = parse_output(output);
        assert!(has_tool_calls);
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            ContentPart::ToolCall(tc) => {
                // call_id is used as the ToolCall.id (links to tool results)
                assert_eq!(tc.id, "call_xyz789");
                // item-level id (fc_xxx) is preserved in provider_metadata
                let meta = tc
                    .provider_metadata
                    .as_ref()
                    .expect("provider_metadata should be set");
                assert_eq!(meta["id"], "fc_abc123");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_output_preserves_custom_tool_call_raw_input() {
        let patch = "*** Begin Patch\n*** Add File: hello.txt\n+hello\n*** End Patch\n";
        let output = vec![serde_json::json!({
            "type": "custom_tool_call",
            "id": "ctc_abc123",
            "call_id": "call_xyz789",
            "name": "apply_patch",
            "input": patch,
        })];

        let (parts, has_tool_calls) = parse_output(output);

        assert!(has_tool_calls);
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            ContentPart::ToolCall(tc) => {
                assert_eq!(tc.id, "call_xyz789");
                assert_eq!(tc.name, "apply_patch");
                assert_eq!(tc.tool_type, "custom");
                assert_eq!(tc.arguments, serde_json::json!(patch));
                assert_eq!(tc.raw_arguments.as_deref(), Some(patch));
                let meta = tc
                    .provider_metadata
                    .as_ref()
                    .expect("provider metadata should preserve item id");
                assert_eq!(meta["id"], "ctc_abc123");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_output_preserves_reasoning_items() {
        let output = vec![
            serde_json::json!({
                "type": "reasoning",
                "id": "rs_abc123",
                "summary": [{"type": "summary_text", "text": "Thinking..."}]
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_def456",
                "call_id": "call_789",
                "name": "search",
                "arguments": "{}"
            }),
        ];
        let (parts, has_tool_calls) = parse_output(output);
        assert!(has_tool_calls);
        assert_eq!(parts.len(), 2);
        // First part is the reasoning item
        match &parts[0] {
            ContentPart::Other { kind, data } => {
                assert_eq!(kind, ContentPart::OPENAI_REASONING);
                assert_eq!(data["type"], "reasoning");
                assert_eq!(data["id"], "rs_abc123");
            }
            other => panic!("expected Other, got {other:?}"),
        }
        // Second part is the function call
        assert!(matches!(&parts[1], ContentPart::ToolCall(_)));
    }

    #[test]
    fn parse_output_preserves_message_items() {
        let output = vec![
            serde_json::json!({
                "type": "reasoning",
                "id": "rs_abc",
                "summary": []
            }),
            serde_json::json!({
                "type": "message",
                "id": "msg_xyz",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hello"}]
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_123",
                "call_id": "call_456",
                "name": "search",
                "arguments": "{}"
            }),
        ];
        let (parts, has_tool_calls) = parse_output(output);
        assert!(has_tool_calls);
        // reasoning + openai_message + text + function_call
        assert_eq!(parts.len(), 4);
        assert!(
            matches!(&parts[0], ContentPart::Other { kind, .. } if kind == ContentPart::OPENAI_REASONING)
        );
        assert!(
            matches!(&parts[1], ContentPart::Other { kind, data } if kind == ContentPart::OPENAI_MESSAGE && data["id"] == "msg_xyz")
        );
        assert!(matches!(&parts[2], ContentPart::Text(t) if t == "Hello"));
        assert!(matches!(&parts[3], ContentPart::ToolCall(_)));
    }

    #[test]
    fn parse_output_round_trips_function_call_ids() {
        // Simulate a response from the Responses API
        let output = vec![serde_json::json!({
            "type": "function_call",
            "id": "fc_item1",
            "call_id": "call_001",
            "name": "search",
            "arguments": "{\"q\":\"test\"}"
        })];
        let (parts, _) = parse_output(output);

        // Now translate back to input format
        let msg = Message {
            role:         Role::Assistant,
            content:      parts,
            name:         None,
            tool_call_id: None,
        };
        let (_, input) = encode::translate_input(&[msg]);
        let fc = &input[0];

        // The round-tripped function call should have correct IDs
        assert_eq!(fc["id"], "fc_item1");
        assert_eq!(fc["call_id"], "call_001");
    }
}
