//! Response decoding: Converse body → canonical `Response`.

use serde_json::Value;

use crate::codec::CodecCtx;
use crate::error::{Error, error_from_status_code};
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Response, Role, ThinkingData, TokenCounts,
    ToolCall,
};

/// Map a non-2xx Bedrock runtime response to an `Error`, pulling the human
/// reason out of AWS's error envelope. Bedrock uses several shapes for the
/// same field — top-level `message` (SigV4 path) and `Message` (API-key
/// path), occasionally nested `error.message` — and tags the type in
/// `__type`. The generic codec parser only reads `error.message`, so without
/// this these surface as "Unknown error".
pub(super) fn bedrock_error(
    status: u16,
    body: &str,
    provider: &str,
    retry_after: Option<f64>,
) -> Error {
    let raw: Option<Value> = serde_json::from_str(body).ok();
    let message = raw
        .as_ref()
        .and_then(extract_error_message)
        .unwrap_or_else(|| {
            if body.trim().is_empty() {
                "Unknown error".to_string()
            } else {
                body.to_string()
            }
        });
    // `__type` is often an ARN-ish `prefix#ThrottlingException`; keep the tail.
    let code = raw
        .as_ref()
        .and_then(|v| {
            v.get("__type")
                .or_else(|| v.get("code"))
                .and_then(Value::as_str)
        })
        .map(|t| t.rsplit('#').next().unwrap_or(t).to_string());
    error_from_status_code(
        status,
        message,
        provider.to_string(),
        code,
        raw,
        retry_after,
    )
}

fn extract_error_message(v: &Value) -> Option<String> {
    v.get("message")
        .and_then(Value::as_str)
        .or_else(|| v.get("Message").and_then(Value::as_str))
        .or_else(|| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
        })
        .map(String::from)
}

pub(super) fn decode_response(
    body: &str,
    ctx: &CodecCtx<'_>,
    rate_limit: Option<RateLimitInfo>,
) -> Result<Response, Error> {
    let raw: Value = serde_json::from_str(body)
        .map_err(|e| Error::network(format!("failed to parse converse response: {e}"), e))?;

    let content_parts = raw
        .pointer("/output/message/content")
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter_map(decode_content_block).collect())
        .unwrap_or_default();

    let finish_reason = map_stop_reason(raw.get("stopReason").and_then(Value::as_str));
    let usage = token_counts_from_usage(raw.get("usage"));

    Ok(Response {
        // Converse responses carry no id; synthesize one like the gemini
        // codec does so downstream consumers always see a non-empty id.
        id: uuid::Uuid::new_v4().to_string(),
        model: ctx.request.model.clone(),
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

/// Decode one Converse content block into a canonical part. Unknown block
/// kinds are skipped (the union grows: `citationsContent`, `searchResult`,
/// `video`, ...).
pub(super) fn decode_content_block(block: &Value) -> Option<ContentPart> {
    if let Some(text) = block.get("text").and_then(Value::as_str) {
        if text.is_empty() {
            return None;
        }
        return Some(ContentPart::text(text));
    }
    if let Some(tool_use) = block.get("toolUse") {
        let id = tool_use.get("toolUseId").and_then(Value::as_str)?;
        let name = tool_use.get("name").and_then(Value::as_str)?;
        // A no-argument tool call is canonically `{}`, not null (so it
        // re-encodes to a valid Converse `toolUse.input` object).
        let input = match tool_use.get("input") {
            Some(Value::Null) | None => Value::Object(serde_json::Map::new()),
            Some(value) => value.clone(),
        };
        return Some(ContentPart::ToolCall(ToolCall::new(id, name, input)));
    }
    if let Some(reasoning) = block.get("reasoningContent") {
        if let Some(text_block) = reasoning.get("reasoningText") {
            return Some(ContentPart::Thinking(ThinkingData {
                text:      text_block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                signature: text_block
                    .get("signature")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                redacted:  false,
            }));
        }
        if let Some(redacted) = reasoning.get("redactedContent").and_then(Value::as_str) {
            return Some(ContentPart::Thinking(ThinkingData {
                text:      redacted.to_string(),
                signature: None,
                redacted:  true,
            }));
        }
    }
    None
}

/// Map a Converse `stopReason` onto the canonical finish vocabulary.
pub(super) fn map_stop_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        None | Some("end_turn" | "stop_sequence") => FinishReason::Stop,
        Some("max_tokens" | "model_context_window_exceeded") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCalls,
        // `refusal` is the Claude 5 blocking-classifier stop, passed through
        // by Bedrock for Fable-class models.
        Some("guardrail_intervened" | "content_filtered" | "refusal") => {
            FinishReason::ContentFilter
        }
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

/// Converse usage maps directly onto the disjoint buckets: `inputTokens`
/// already excludes cached tokens (documented), so no subtraction applies.
pub(super) fn token_counts_from_usage(usage: Option<&Value>) -> TokenCounts {
    let Some(usage) = usage else {
        return TokenCounts::default();
    };
    let count = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
    TokenCounts {
        input_tokens:       count("inputTokens"),
        output_tokens:      count("outputTokens"),
        reasoning_tokens:   0,
        cache_read_tokens:  count("cacheReadInputTokens"),
        cache_write_tokens: count("cacheWriteInputTokens"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_reasons_map_to_canonical_vocabulary() {
        assert_eq!(map_stop_reason(Some("end_turn")), FinishReason::Stop);
        assert_eq!(map_stop_reason(Some("stop_sequence")), FinishReason::Stop);
        assert_eq!(map_stop_reason(Some("max_tokens")), FinishReason::Length);
        assert_eq!(
            map_stop_reason(Some("model_context_window_exceeded")),
            FinishReason::Length
        );
        assert_eq!(map_stop_reason(Some("tool_use")), FinishReason::ToolCalls);
        assert_eq!(
            map_stop_reason(Some("guardrail_intervened")),
            FinishReason::ContentFilter
        );
        assert_eq!(
            map_stop_reason(Some("content_filtered")),
            FinishReason::ContentFilter
        );
        assert_eq!(
            map_stop_reason(Some("refusal")),
            FinishReason::ContentFilter
        );
        assert_eq!(
            map_stop_reason(Some("malformed_tool_use")),
            FinishReason::Other("malformed_tool_use".to_string())
        );
        assert_eq!(map_stop_reason(None), FinishReason::Stop);
    }

    #[test]
    fn usage_maps_without_subtraction() {
        let usage = serde_json::json!({
            "inputTokens": 30,
            "outputTokens": 628,
            "totalTokens": 658,
            "cacheReadInputTokens": 1024,
            "cacheWriteInputTokens": 512,
        });
        let counts = token_counts_from_usage(Some(&usage));
        assert_eq!(counts.input_tokens, 30);
        assert_eq!(counts.output_tokens, 628);
        assert_eq!(counts.cache_read_tokens, 1024);
        assert_eq!(counts.cache_write_tokens, 512);
        assert_eq!(counts.reasoning_tokens, 0);
    }

    #[test]
    fn bedrock_error_extracts_aws_message_shapes() {
        // SigV4 path: top-level lowercase `message`.
        let sigv4 = bedrock_error(
            403,
            r#"{"message":"Model access is denied due to IAM ..."}"#,
            "bedrock",
            None,
        );
        assert!(
            sigv4.to_string().contains("Model access is denied"),
            "{sigv4}"
        );

        // API-key path: top-level capitalized `Message`.
        let api_key = bedrock_error(
            403,
            r#"{"Message":"Authentication failed: Please make sure your API Key is valid."}"#,
            "bedrock",
            None,
        );
        assert!(
            api_key.to_string().contains("Authentication failed"),
            "{api_key}"
        );

        // `__type` becomes the error code (tail after `#`).
        let typed = bedrock_error(
            429,
            r#"{"__type":"com.amazon.coral.service#ThrottlingException","message":"slow down"}"#,
            "bedrock",
            None,
        );
        let Error::Provider { detail, .. } = &typed else {
            panic!("expected provider error: {typed}");
        };
        assert_eq!(detail.error_code.as_deref(), Some("ThrottlingException"));

        // Garbage body falls back rather than panicking.
        let opaque = bedrock_error(500, "not json", "bedrock", None);
        assert!(opaque.to_string().contains("not json"), "{opaque}");
    }

    #[test]
    fn unknown_content_blocks_are_skipped() {
        assert!(decode_content_block(&serde_json::json!({"citationsContent": {}})).is_none());
        assert!(decode_content_block(&serde_json::json!({"text": ""})).is_none());
    }

    #[test]
    fn reasoning_text_block_round_trips_signature() {
        let block = serde_json::json!({
            "reasoningContent": {
                "reasoningText": { "text": "thinking...", "signature": "sig-1" }
            }
        });
        let Some(ContentPart::Thinking(thinking)) = decode_content_block(&block) else {
            panic!("expected thinking part");
        };
        assert_eq!(thinking.text, "thinking...");
        assert_eq!(thinking.signature.as_deref(), Some("sig-1"));
        assert!(!thinking.redacted);
    }
}
