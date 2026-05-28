//! Shared implementation of the OpenAI Chat Completions wire protocol.
//!
//! Used by [`super::openai_compatible::Adapter`] for vanilla
//! OpenAI-compatible providers (Together, Groq, vLLM, etc.) and (after
//! PR C) by `super::openrouter::Adapter` to layer OR-specific behavior on
//! top via [`ChatHooks`].

pub(crate) mod hooks;
pub(crate) mod request;
pub(crate) mod response;
pub(crate) mod stream;
pub(crate) mod translate;
pub(crate) mod wire;

use fabro_model::Catalog;
pub(crate) use hooks::ChatHooks;

use crate::error::Error;
use crate::provider::StreamEventStream;
use crate::providers::common::send_and_read_response;
use crate::types::{Request, Response};

/// Run a non-streaming Chat Completions request through the shared
/// pipeline, applying the supplied [`ChatHooks`] for request mutation and
/// response enrichment.
///
/// `build_request` is a closure that returns an authenticated
/// `fabro_http::RequestBuilder` for a given URL — each caller (OpenAI
/// compatible, OpenRouter) handles auth header construction itself.
pub(crate) async fn complete(
    http: &super::http_api::HttpApi,
    build_request: impl Fn(&str) -> fabro_http::RequestBuilder + Send,
    catalog: Option<&Catalog>,
    provider_name: &str,
    request: &Request,
    hooks: ChatHooks,
) -> Result<Response, Error> {
    let api_body =
        request::build_chat_request_with_catalog(request, None, provider_name, catalog, hooks);
    let url = format!("{}/chat/completions", http.base_url);

    let mut req = build_request(&url).json(&api_body);
    if let Some(t) = http.request_timeout {
        req = req.timeout(t);
    }
    let (body, headers) = send_and_read_response(req, provider_name, "type").await?;

    response::parse_chat_response(&body, &headers, provider_name, request, hooks)
}

/// Run a streaming Chat Completions request through the shared pipeline.
pub(crate) async fn stream(
    http: &super::http_api::HttpApi,
    build_request: impl Fn(&str) -> fabro_http::RequestBuilder + Send,
    catalog: Option<&Catalog>,
    provider_name: &str,
    request: &Request,
    hooks: ChatHooks,
) -> Result<StreamEventStream, Error> {
    let api_body = request::build_chat_request_with_catalog(
        request,
        Some(true),
        provider_name,
        catalog,
        hooks,
    );
    let url = format!("{}/chat/completions", http.base_url);

    let req = build_request(&url).json(&api_body);

    let custom_tool_names = translate::custom_tool_names(request);
    stream::send_and_stream(
        req,
        provider_name.to_string(),
        request.model.clone(),
        http.stream_read_timeout,
        hooks,
        custom_tool_names,
    )
    .await
}
