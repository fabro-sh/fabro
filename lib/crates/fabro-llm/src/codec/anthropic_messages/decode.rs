//! Response decoding: Anthropic Messages body → canonical `Response`.

use serde::Deserialize;

use super::SYNTHETIC_TOOL_NAME;
use super::wire::{ApiResponse, ApiUsage, CountTokensResponse};
use crate::codec::CodecCtx;
use crate::error::{Error, ProviderErrorDetail, ProviderErrorKind};
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Request, Response, ResponseFormatType, Role,
    ThinkingData, TokenCounts, ToolCall,
};

pub(super) fn token_counts_from_api_usage(usage: &ApiUsage) -> TokenCounts {
    // Anthropic does not expose a separate billed thinking/reasoning token
    // count. Thinking tokens are billed as part of `output_tokens`. When
    // Anthropic adds a real thinking token field, wire it through and subtract
    // it here.
    TokenCounts {
        input_tokens:       usage.input_tokens,
        output_tokens:      usage.output_tokens,
        reasoning_tokens:   0,
        cache_read_tokens:  usage.cache_read_input_tokens.unwrap_or(0),
        cache_write_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
    }
}

pub(super) fn map_finish_reason(stop_reason: Option<&str>) -> FinishReason {
    match stop_reason {
        Some("end_turn" | "stop_sequence") | None => FinishReason::Stop,
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCalls,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

pub(super) fn parse_content_block(block: &serde_json::Value) -> Option<ContentPart> {
    match block.get("type")?.as_str()? {
        "text" => Some(ContentPart::text(block.get("text")?.as_str()?)),
        "tool_use" => Some(ContentPart::ToolCall(ToolCall::new(
            block.get("id")?.as_str()?,
            block.get("name")?.as_str()?,
            block.get("input")?.clone(),
        ))),
        "thinking" => Some(ContentPart::Thinking(ThinkingData {
            text:      block.get("thinking")?.as_str()?.to_string(),
            signature: block
                .get("signature")
                .and_then(serde_json::Value::as_str)
                .map(String::from),
            redacted:  false,
        })),
        "redacted_thinking" => Some(ContentPart::Thinking(ThinkingData {
            text:      block
                .get("data")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            signature: None,
            redacted:  true,
        })),
        _ => None,
    }
}

/// Convert synthetic `tool_use` content blocks back to text content parts.
///
/// When `response_format` uses `JsonSchema` mode, the model responds with a
/// `tool_use` block for our synthetic tool. We extract its arguments as a JSON
/// text string.
pub(super) fn convert_synthetic_tool_to_text(content_parts: Vec<ContentPart>) -> Vec<ContentPart> {
    content_parts
        .into_iter()
        .map(|part| match &part {
            ContentPart::ToolCall(tc) if tc.name == SYNTHETIC_TOOL_NAME => {
                ContentPart::text(tc.arguments.to_string())
            }
            _ => part,
        })
        .collect()
}

/// Check if the request uses `JsonSchema` `response_format`.
pub(super) fn uses_json_schema_format(request: &Request) -> bool {
    request
        .response_format
        .as_ref()
        .is_some_and(|f| matches!(f.kind, ResponseFormatType::JsonSchema))
}

/// Map a refusal stop reason (Claude Fable 5) to a content-filter provider
/// error. Shared by the response decoder and the stream decoder; the
/// `error_code = "refusal"` marker is what makes it failover-eligible.
pub(super) fn refusal_error(
    provider_name: &str,
    model: &str,
    raw: serde_json::Value,
    stop_details: Option<&serde_json::Value>,
) -> Error {
    let model_label = if model.is_empty() { "The model" } else { model };
    let message = stop_details
        .and_then(|details| details.get("explanation"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || format!("{model_label} refused the request"),
            |explanation| format!("{model_label} refused the request: {explanation}"),
        );

    Error::Provider {
        kind:   ProviderErrorKind::ContentFilter,
        detail: Box::new(ProviderErrorDetail {
            message,
            provider: provider_name.to_string(),
            status_code: None,
            error_code: Some("refusal".to_string()),
            retry_after: None,
            raw: Some(raw),
        }),
    }
}

pub(super) fn decode_response(
    body: &str,
    ctx: &CodecCtx<'_>,
    rate_limit: Option<RateLimitInfo>,
) -> Result<Response, Error> {
    let raw: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        Error::network(
            format!("failed to parse {} response: {e}", ctx.provider_name),
            e,
        )
    })?;
    let api_resp = ApiResponse::deserialize(&raw).map_err(|e| {
        Error::network(
            format!("failed to parse {} response: {e}", ctx.provider_name),
            e,
        )
    })?;

    if api_resp.stop_reason.as_deref() == Some("refusal") {
        return Err(refusal_error(
            ctx.provider_name,
            &api_resp.model,
            raw,
            api_resp.stop_details.as_ref(),
        ));
    }

    let content_parts: Vec<ContentPart> = api_resp
        .content
        .iter()
        .filter_map(parse_content_block)
        .collect();

    // If we used JsonSchema mode, convert the synthetic tool call back to text.
    let json_schema_mode = uses_json_schema_format(ctx.request);
    let content_parts = if json_schema_mode {
        convert_synthetic_tool_to_text(content_parts)
    } else {
        content_parts
    };

    let finish_reason = if json_schema_mode {
        // The model was forced to call a tool, so stop_reason is "tool_use",
        // but from the caller's perspective, the request completed normally.
        FinishReason::Stop
    } else {
        map_finish_reason(api_resp.stop_reason.as_deref())
    };

    Ok(Response {
        id: api_resp.id,
        model: api_resp.model,
        provider: ctx.provider_name.to_string(),
        message: Message {
            role:         Role::Assistant,
            content:      content_parts,
            name:         None,
            tool_call_id: None,
        },
        finish_reason,
        usage: token_counts_from_api_usage(&api_resp.usage),
        raw: Some(raw),
        warnings: vec![],
        rate_limit,
    })
}

pub(super) fn decode_count_tokens(body: &str) -> Result<i64, Error> {
    let response: CountTokensResponse =
        serde_json::from_str(body).map_err(|e| Error::Configuration {
            message: format!("failed to parse token count response: {e}"),
            source:  None,
        })?;
    Ok(response.input_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_token_counts_leaves_reasoning_zero_and_output_full() {
        let body = serde_json::json!({
            "id": "msg_test",
            "model": "claude-sonnet-4-5",
            "content": [
                { "type": "thinking", "thinking": "summary text", "signature": "" },
                { "type": "text", "text": "answer" }
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 50,
                "output_tokens": 1200,
                "cache_read_input_tokens": 9000,
                "cache_creation_input_tokens": 1000
            }
        });
        let api: ApiResponse = serde_json::from_value(body).unwrap();
        let usage = token_counts_from_api_usage(&api.usage);

        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 9000);
        assert_eq!(usage.cache_write_tokens, 1000);
        assert_eq!(usage.output_tokens, 1200);
        assert_eq!(usage.reasoning_tokens, 0);
        assert_eq!(usage.total_tokens(), 11_250);
    }

    #[test]
    fn convert_synthetic_tool_to_text_replaces_synthetic_tool() {
        let parts = vec![ContentPart::ToolCall(ToolCall::new(
            "id1",
            SYNTHETIC_TOOL_NAME,
            serde_json::json!({"name": "Alice"}),
        ))];
        let result = convert_synthetic_tool_to_text(parts);
        assert_eq!(result.len(), 1);
        match &result[0] {
            ContentPart::Text(text) => {
                assert!(text.contains("Alice"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn convert_synthetic_tool_to_text_preserves_other_tool_calls() {
        let parts = vec![ContentPart::ToolCall(ToolCall::new(
            "id1",
            "real_tool",
            serde_json::json!({"key": "value"}),
        ))];
        let result = convert_synthetic_tool_to_text(parts);
        assert_eq!(result.len(), 1);
        match &result[0] {
            ContentPart::ToolCall(tc) => {
                assert_eq!(tc.name, "real_tool");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }
}
