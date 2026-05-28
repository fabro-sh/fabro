//! Parse a non-streaming Chat Completions HTTP response body into a unified
//! [`Response`](crate::types::Response).

use fabro_http::HeaderMap;

use super::hooks::ChatHooks;
use super::translate::{custom_tool_names, map_finish_reason, parse_tool_arguments};
use super::wire::ApiResponse;
use crate::error::{Error, ProviderErrorDetail, ProviderErrorKind};
use crate::providers::common::parse_rate_limit_headers;
use crate::types::{
    ContentPart, Message, Request, Response, Role, ThinkingData, TokenCounts, ToolCall,
};

/// Parse a non-streaming Chat Completions response body into a unified
/// [`Response`]. Applies [`ChatHooks::enrich_response`] after constructing
/// the response, before returning it.
pub(crate) fn parse_chat_response(
    body: &str,
    headers: &HeaderMap,
    provider_name: &str,
    request: &Request,
    hooks: ChatHooks,
) -> Result<Response, Error> {
    let api_resp: ApiResponse = serde_json::from_str(body)
        .map_err(|e| Error::network(format!("failed to parse response: {e}"), e))?;

    let choice = api_resp.choices.first().ok_or_else(|| Error::Provider {
        kind:   ProviderErrorKind::Server,
        detail: Box::new(ProviderErrorDetail::new(
            "no choices in response",
            provider_name,
        )),
    })?;

    let mut content_parts = Vec::new();
    if let Some(reasoning) = &choice.message.reasoning_content {
        if !reasoning.is_empty() {
            content_parts.push(ContentPart::Thinking(ThinkingData {
                text:      reasoning.clone(),
                signature: None,
                redacted:  false,
            }));
        }
    }
    if let Some(text) = &choice.message.content {
        if !text.is_empty() {
            content_parts.push(ContentPart::text(text));
        }
    }
    if let Some(tool_calls) = &choice.message.tool_calls {
        let custom_tool_names = custom_tool_names(request);
        for tc in tool_calls {
            let arguments = parse_tool_arguments(
                &tc.function.name,
                &tc.function.arguments,
                &custom_tool_names,
            );
            let mut tool_call = ToolCall::new(&tc.id, &tc.function.name, arguments);
            tool_call.raw_arguments = Some(tc.function.arguments.clone());
            content_parts.push(ContentPart::ToolCall(tool_call));
        }
    }

    let finish_reason = map_finish_reason(choice.finish_reason.as_deref());

    let usage = api_resp
        .usage
        .as_ref()
        .map_or_else(TokenCounts::default, |u| TokenCounts {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cache_read_tokens: u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0),
            cache_write_tokens: u.cache_write_tokens.unwrap_or(0),
            ..TokenCounts::default()
        });

    let raw: Option<serde_json::Value> = serde_json::from_str(body).ok();

    let mut response = Response {
        id: api_resp.id,
        model: api_resp.model,
        provider: provider_name.to_string(),
        message: Message {
            role:         Role::Assistant,
            content:      content_parts,
            name:         None,
            tool_call_id: None,
        },
        finish_reason,
        usage,
        raw: raw.clone(),
        warnings: vec![],
        rate_limit: parse_rate_limit_headers(headers),
    };

    if let Some(enrich) = hooks.enrich_response {
        let raw_for_enrich = raw.unwrap_or(serde_json::Value::Null);
        enrich(&mut response, &raw_for_enrich);
    }

    Ok(response)
}
