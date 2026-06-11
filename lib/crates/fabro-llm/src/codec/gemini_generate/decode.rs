//! Response decoding: Gemini `generateContent` body → canonical `Response`,
//! plus the gRPC-status error mapping behind the codec's `decode_error`.

use serde::Deserialize;

use super::wire::{ApiResponse, CountTokensResponse, UsageMetadata};
use crate::codec::CodecCtx;
use crate::error::{
    Error, ProviderErrorDetail, ProviderErrorKind, error_from_grpc_status, error_from_status_code,
};
use crate::types::{
    ContentPart, FinishReason, Message, RateLimitInfo, Response, Role, ThinkingData, TokenCounts,
    ToolCall,
};

/// Map Gemini's finish reason, inferring `ToolCalls` from content when needed.
pub(super) fn map_finish_reason(reason: Option<&str>, has_function_calls: bool) -> FinishReason {
    if has_function_calls {
        return FinishReason::ToolCalls;
    }
    match reason {
        Some("STOP") | None => FinishReason::Stop,
        Some("MAX_TOKENS") => FinishReason::Length,
        Some("SAFETY" | "RECITATION") => FinishReason::ContentFilter,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

pub(super) fn parse_part(part: &serde_json::Value) -> Option<ContentPart> {
    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
        let is_thought = part
            .get("thought")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if is_thought {
            return Some(ContentPart::Thinking(ThinkingData {
                text:      text.to_string(),
                signature: None,
                redacted:  false,
            }));
        }
        return Some(ContentPart::text(text));
    }
    if let Some(fc) = part.get("functionCall") {
        let name = fc.get("name")?.as_str()?.to_string();
        let args = fc
            .get("args")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
        let mut tc = ToolCall::new(uuid::Uuid::new_v4().to_string(), name, args);
        // Preserve thought_signature for Gemini 3 models (sibling of functionCall in
        // the part)
        if let Some(sig) = part.get("thoughtSignature") {
            tc.provider_metadata = Some(serde_json::json!({"thoughtSignature": sig}));
        }
        return Some(ContentPart::ToolCall(tc));
    }
    None
}

/// Check if any parts contain function calls.
pub(super) fn parts_have_function_calls(parts: &[serde_json::Value]) -> bool {
    parts.iter().any(|p| p.get("functionCall").is_some())
}

/// Convert `UsageMetadata` from the Gemini API into a unified `TokenCounts`.
pub(super) fn parse_usage(metadata: Option<&UsageMetadata>) -> TokenCounts {
    metadata.map_or_else(TokenCounts::default, |u| {
        let cache_read_tokens = u.cached_content_token_count.unwrap_or(0);
        let reasoning_tokens = u.thoughts_token_count.unwrap_or(0);
        let tool_use_prompt_tokens = u.tool_use_prompt_token_count.unwrap_or(0);
        TokenCounts {
            input_tokens: u
                .prompt_token_count
                .unwrap_or(0)
                .saturating_sub(cache_read_tokens)
                + tool_use_prompt_tokens,
            output_tokens: u.candidates_token_count.unwrap_or(0),
            reasoning_tokens,
            cache_read_tokens,
            ..TokenCounts::default()
        }
    })
}

/// Map a Gemini error response using gRPC status when available, falling back
/// to HTTP status.
pub(super) fn gemini_error(
    status_code: u16,
    msg: String,
    provider: &str,
    grpc_status: Option<String>,
    raw: Option<serde_json::Value>,
    retry_after: Option<f64>,
) -> Error {
    match grpc_status {
        Some(grpc_code) => error_from_grpc_status(
            &grpc_code,
            msg,
            provider.to_string(),
            Some(grpc_code.clone()),
            raw,
            retry_after,
        ),
        None => error_from_status_code(
            status_code,
            msg,
            provider.to_string(),
            None,
            raw,
            retry_after,
        ),
    }
}

pub(super) fn decode_response(
    body: &str,
    ctx: &CodecCtx<'_>,
    rate_limit: Option<RateLimitInfo>,
) -> Result<Response, Error> {
    let raw: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| Error::network(format!("failed to parse Gemini response: {e}"), e))?;
    let api_resp = ApiResponse::deserialize(&raw)
        .map_err(|e| Error::network(format!("failed to parse Gemini response: {e}"), e))?;

    let candidate = api_resp
        .candidates
        .as_ref()
        .and_then(|c| c.first())
        .ok_or_else(|| Error::Provider {
            kind:   ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail::new(
                "no candidates in Gemini response",
                ctx.provider_name,
            )),
        })?;

    let raw_parts = candidate.content.as_ref().and_then(|c| c.parts.as_ref());

    let content_parts: Vec<ContentPart> = raw_parts
        .map(|parts| parts.iter().filter_map(parse_part).collect())
        .unwrap_or_default();

    // Gemini has no dedicated tool_calls finish reason; infer from parts
    let has_tool_calls = raw_parts.is_some_and(|p| parts_have_function_calls(p));
    let finish_reason = map_finish_reason(candidate.finish_reason.as_deref(), has_tool_calls);

    let usage = parse_usage(api_resp.usage_metadata.as_ref());

    Ok(Response {
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

pub(super) fn decode_count_tokens(body: &str) -> Result<i64, Error> {
    let response: CountTokensResponse =
        serde_json::from_str(body).map_err(|e| Error::Configuration {
            message: format!("failed to parse Gemini token count: {e}"),
            source:  None,
        })?;
    Ok(response.total_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_disjoint_with_cache_thoughts_and_tool_use() {
        let body = serde_json::json!({
            "promptTokenCount": 200,
            "cachedContentTokenCount": 180,
            "candidatesTokenCount": 200,
            "thoughtsTokenCount": 300,
            "toolUsePromptTokenCount": 400
        });
        let meta: UsageMetadata = serde_json::from_value(body).unwrap();
        let usage = parse_usage(Some(&meta));

        assert_eq!(usage.input_tokens, 420);
        assert_eq!(usage.cache_read_tokens, 180);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.reasoning_tokens, 300);
        assert_eq!(usage.cache_write_tokens, 0);
        assert_eq!(usage.total_tokens(), 1100);
    }

    #[test]
    fn gemini_error_uses_grpc_status_when_available() {
        let err = gemini_error(
            400,
            "model not found".into(),
            "gemini",
            Some("NOT_FOUND".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::NotFound,
            ..
        }));

        let err = gemini_error(
            400,
            "bad args".into(),
            "gemini",
            Some("INVALID_ARGUMENT".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::InvalidRequest,
            ..
        }));

        let err = gemini_error(
            429,
            "rate limited".into(),
            "gemini",
            Some("RESOURCE_EXHAUSTED".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::RateLimit,
            ..
        }));

        let err = gemini_error(
            401,
            "bad key".into(),
            "gemini",
            Some("UNAUTHENTICATED".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Authentication,
            ..
        }));

        let err = gemini_error(
            403,
            "denied".into(),
            "gemini",
            Some("PERMISSION_DENIED".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::AccessDenied,
            ..
        }));

        let err = gemini_error(
            504,
            "timeout".into(),
            "gemini",
            Some("DEADLINE_EXCEEDED".into()),
            None,
            None,
        );
        assert!(matches!(err, Error::RequestTimeout { .. }));
    }

    #[test]
    fn gemini_error_falls_back_to_http_status_without_grpc() {
        let err = gemini_error(429, "rate limited".into(), "gemini", None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::RateLimit,
            ..
        }));

        let err = gemini_error(500, "internal".into(), "gemini", None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Server,
            ..
        }));
    }

    #[test]
    fn parse_part_handles_thought_text() {
        let part = serde_json::json!({"text": "Let me think about this...", "thought": true});
        let result = parse_part(&part).expect("should parse thought part");
        match result {
            ContentPart::Thinking(td) => {
                assert_eq!(td.text, "Let me think about this...");
                assert!(td.signature.is_none());
                assert!(!td.redacted);
            }
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[test]
    fn parse_part_text_without_thought_flag() {
        let part = serde_json::json!({"text": "Hello world"});
        let result = parse_part(&part).expect("should parse text part");
        match result {
            ContentPart::Text(text) => assert_eq!(text, "Hello world"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn parse_part_function_call() {
        let part = serde_json::json!({
            "functionCall": {
                "name": "get_weather",
                "args": {"location": "NYC"}
            }
        });
        let result = parse_part(&part).expect("should parse function call");
        match result {
            ContentPart::ToolCall(tc) => {
                assert_eq!(tc.name, "get_weather");
                assert_eq!(tc.arguments, serde_json::json!({"location": "NYC"}));
                assert!(tc.provider_metadata.is_none());
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_part_function_call_with_thought_signature() {
        let part = serde_json::json!({
            "functionCall": {
                "name": "get_weather",
                "args": {"location": "NYC"}
            },
            "thoughtSignature": "abc123sig"
        });
        let result = parse_part(&part).expect("should parse function call with thought signature");
        match result {
            ContentPart::ToolCall(tc) => {
                assert_eq!(tc.name, "get_weather");
                let meta = tc
                    .provider_metadata
                    .expect("provider_metadata should be set");
                assert_eq!(meta["thoughtSignature"], "abc123sig");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_part_thought_false_is_regular_text() {
        let part = serde_json::json!({"text": "Regular text", "thought": false});
        let result = parse_part(&part).expect("should parse text part");
        match result {
            ContentPart::Text(text) => assert_eq!(text, "Regular text"),
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
