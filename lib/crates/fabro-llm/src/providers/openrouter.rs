//! OpenRouter adapter (`OpenAI`-Chat-Completions compatible with extensions).
//!
//! `OpenRouter` (<https://openrouter.ai>) is a gateway over many model
//! providers that speaks the `OpenAI` Chat Completions wire protocol but
//! adds extra top-level request fields (`provider`, `models`,
//! `transforms`, `plugins`, `reasoning`) and an inline authoritative
//! `usage.cost` in responses. This adapter sits on top of the shared
//! [`super::openai_chat`] module, using
//! [`ChatHooks`](super::openai_chat::ChatHooks) to layer OpenRouter-specific
//! behavior on top of the generic wire layer.
//!
//! - `mutate_request`: translates typed `Request.reasoning_effort` into OR's
//!   `{reasoning: {effort: ...}}` JSON shape.
//! - `enrich_response`: pulls `usage.cost` out of the raw response into
//!   `Response.cost_usd` with `CostSource::Authoritative`.
//!
//! Typed OpenRouter routing controls (`openrouter_provider_sort`, etc.)
//! flow in through `Request.provider_options["openrouter"]` — the
//! existing `merge_provider_options` path in `openai_chat::request`
//! handles pasting them into the request body.

use std::sync::Arc;

use fabro_model::Catalog;

use crate::error::Error;
use crate::provider::{
    ProviderAdapter, StreamEventStream, validate_standard_speed, validate_tool_choice,
};
use crate::providers::openai_chat::{self, ChatHooks};
use crate::types::{AdapterTimeout, CostSource, Request, Response};

/// `OpenRouter` adapter. Speaks the Chat Completions wire protocol with
/// OpenRouter-specific request/response translations applied via
/// [`ChatHooks`].
pub struct Adapter {
    pub(crate) http: super::http_api::HttpApi,
    provider_name:   String,
    catalog:         Option<Arc<Catalog>>,
}

impl Adapter {
    #[must_use]
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::new_optional_auth(Some(api_key.into()), base_url)
    }

    #[must_use]
    pub fn new_optional_auth(api_key: Option<String>, base_url: impl Into<String>) -> Self {
        Self {
            http:          super::http_api::HttpApi::new_optional(api_key, base_url),
            provider_name: "openrouter".to_string(),
            catalog:       None,
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    #[must_use]
    pub fn with_default_headers(self, headers: std::collections::HashMap<String, String>) -> Self {
        Self {
            http: self.http.with_default_headers(headers),
            ..self
        }
    }

    #[must_use]
    pub fn with_catalog(mut self, catalog: Arc<Catalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    #[must_use]
    pub fn with_timeout(self, timeout: AdapterTimeout) -> Self {
        Self {
            http: self.http.with_timeout(timeout),
            ..self
        }
    }

    /// Build a `fabro_http::RequestBuilder` with default headers and auth.
    fn build_request(&self, url: &str) -> fabro_http::RequestBuilder {
        let mut req = self.http.client.post(url);
        // Apply default_headers first so adapter-specific headers can override.
        for (key, value) in &self.http.default_headers {
            req = req.header(key, value);
        }
        if let Some(api_key) = &self.http.api_key {
            req = req.bearer_auth(api_key);
        }
        req
    }
}

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn validate_request(&self, request: &Request) -> Result<(), Error> {
        validate_standard_speed(self, request)?;
        if let Some(tc) = &request.tool_choice {
            validate_tool_choice(self, tc)?;
        }
        Ok(())
    }

    async fn complete(&self, request: &Request) -> Result<Response, Error> {
        self.validate_request(request)?;
        openai_chat::complete(
            &self.http,
            |url| self.build_request(url),
            self.catalog.as_deref(),
            &self.provider_name,
            request,
            openrouter_hooks(),
        )
        .await
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        self.validate_request(request)?;
        openai_chat::stream(
            &self.http,
            |url| self.build_request(url),
            self.catalog.clone(),
            &self.provider_name,
            request,
            openrouter_hooks(),
        )
        .await
    }
}

fn openrouter_hooks() -> ChatHooks {
    ChatHooks {
        mutate_request:  Some(or_mutate_request),
        enrich_response: Some(or_enrich_response),
    }
}

/// Translate the typed `Request.reasoning_effort` field into OR's
/// `{reasoning: {effort: "..."}}` shape, unless the user has already
/// set `reasoning` explicitly via `provider_options.openrouter`.
fn or_mutate_request(body: &mut serde_json::Value, request: &Request) {
    if body.get("reasoning").is_some() {
        return;
    }
    if let Some(effort) = request.reasoning_effort {
        body["reasoning"] = serde_json::json!({
            "effort": <&'static str>::from(effort),
        });
    }
}

/// Pull OpenRouter's inline `usage.cost` (USD) into `Response.cost_usd`
/// with `CostSource::Authoritative`. Overrides any catalog-estimated
/// cost.
fn or_enrich_response(response: &mut Response, raw: &serde_json::Value) {
    if let Some(cost) = raw
        .get("usage")
        .and_then(|u| u.get("cost"))
        .and_then(serde_json::Value::as_f64)
    {
        response.cost_usd = Some(cost);
        response.cost_source = Some(CostSource::Authoritative);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FinishReason, Message, ReasoningEffort, TokenCounts};

    fn minimal_request() -> Request {
        Request {
            model:            "anthropic/claude-sonnet-4-6".to_string(),
            messages:         vec![Message::user("Hello")],
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
        }
    }

    fn empty_response() -> Response {
        Response {
            id:            "resp_1".into(),
            model:         "anthropic/claude-sonnet-4-6".into(),
            provider:      "openrouter".into(),
            message:       Message::assistant(""),
            finish_reason: FinishReason::Stop,
            usage:         TokenCounts::default(),
            cost_usd:      None,
            cost_source:   None,
            raw:           None,
            warnings:      vec![],
            rate_limit:    None,
        }
    }

    #[test]
    fn or_mutate_request_translates_typed_reasoning_effort() {
        let mut request = minimal_request();
        request.reasoning_effort = Some(ReasoningEffort::High);
        let mut body = serde_json::json!({ "model": "x", "messages": [] });
        or_mutate_request(&mut body, &request);
        assert_eq!(body["reasoning"], serde_json::json!({ "effort": "high" }));
    }

    #[test]
    fn or_mutate_request_preserves_explicit_provider_options_reasoning() {
        let mut request = minimal_request();
        request.reasoning_effort = Some(ReasoningEffort::High);
        // Body already carries an explicit `reasoning` from
        // provider_options merging.
        let mut body = serde_json::json!({
            "model": "x",
            "messages": [],
            "reasoning": { "max_tokens": 1024 },
        });
        or_mutate_request(&mut body, &request);
        assert_eq!(
            body["reasoning"],
            serde_json::json!({ "max_tokens": 1024 }),
            "explicit reasoning should not be clobbered by typed effort translation",
        );
    }

    #[test]
    fn or_mutate_request_no_op_without_reasoning_effort() {
        let request = minimal_request();
        let mut body = serde_json::json!({ "model": "x", "messages": [] });
        let snapshot = body.clone();
        or_mutate_request(&mut body, &request);
        assert_eq!(body, snapshot);
    }

    #[test]
    fn or_enrich_response_extracts_usage_cost() {
        let mut response = empty_response();
        let raw = serde_json::json!({
            "id": "resp_1",
            "usage": { "prompt_tokens": 10, "completion_tokens": 20, "cost": 0.0042 }
        });
        or_enrich_response(&mut response, &raw);
        assert_eq!(response.cost_usd, Some(0.0042));
        assert_eq!(response.cost_source, Some(CostSource::Authoritative));
    }

    #[test]
    fn or_enrich_response_overrides_estimated_cost() {
        let mut response = empty_response();
        response.cost_usd = Some(0.01);
        response.cost_source = Some(CostSource::Estimated);
        let raw = serde_json::json!({
            "usage": { "prompt_tokens": 10, "completion_tokens": 20, "cost": 0.005 }
        });
        or_enrich_response(&mut response, &raw);
        assert_eq!(response.cost_usd, Some(0.005));
        assert_eq!(response.cost_source, Some(CostSource::Authoritative));
    }

    #[test]
    fn or_enrich_response_no_op_without_usage_cost() {
        let mut response = empty_response();
        response.cost_usd = Some(0.01);
        response.cost_source = Some(CostSource::Estimated);
        let raw = serde_json::json!({
            "usage": { "prompt_tokens": 10, "completion_tokens": 20 }
        });
        or_enrich_response(&mut response, &raw);
        // Unchanged: enrich is a no-op when usage.cost is absent.
        assert_eq!(response.cost_usd, Some(0.01));
        assert_eq!(response.cost_source, Some(CostSource::Estimated));
    }
}
