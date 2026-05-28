use std::sync::Arc;

use fabro_model::Catalog;

use crate::error::Error;
use crate::provider::{
    ProviderAdapter, StreamEventStream, validate_standard_speed, validate_tool_choice,
};
use crate::providers::openai_chat::{self, ChatHooks};
use crate::types::{AdapterTimeout, Request, Response};

/// `OpenAI`-compatible Chat Completions adapter (Section 7.10).
///
/// Use this for third-party services (vLLM, Ollama, Together AI, Groq, etc.)
/// that implement the `OpenAI` Chat Completions API (`/v1/chat/completions`).
///
/// Does NOT support reasoning tokens, built-in tools, or other Responses API
/// features. Use the primary `OpenAiAdapter` for `OpenAI`'s own API.
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
            provider_name: "openai-compatible".to_string(),
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
        // Apply default_headers first so adapter-specific headers can override
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
            ChatHooks::NONE,
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
            ChatHooks::NONE,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::openai_chat::request::build_chat_request_with_catalog;
    use crate::types::Message;

    fn minimal_request() -> Request {
        Request {
            model:            "llama-3.1-70b".to_string(),
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

    fn build_api_request(
        request: &Request,
        stream: Option<bool>,
        provider_name: &str,
    ) -> serde_json::Value {
        build_chat_request_with_catalog(request, stream, provider_name, None, ChatHooks::NONE)
    }

    #[test]
    fn provider_options_matching_name_merged() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "groq": {
                "frequency_penalty": 0.5,
                "presence_penalty": 0.3
            }
        }));

        let body = build_api_request(&request, None, "groq");
        assert_eq!(body["frequency_penalty"], 0.5);
        assert_eq!(body["presence_penalty"], 0.3);
    }

    #[test]
    fn provider_options_different_name_ignored() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "together": {
                "repetition_penalty": 1.2
            }
        }));

        let body = build_api_request(&request, None, "groq");
        assert!(body.get("repetition_penalty").is_none());
    }

    #[test]
    fn provider_options_uses_adapter_name() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "together": {
                "repetition_penalty": 1.2
            }
        }));

        let body = build_api_request(&request, None, "together");
        assert_eq!(body["repetition_penalty"], 1.2);
    }

    #[test]
    fn provider_options_preserves_standard_fields() {
        let mut request = minimal_request();
        request.temperature = Some(0.7);
        request.max_tokens = Some(200);
        request.provider_options = Some(serde_json::json!({
            "groq": {
                "frequency_penalty": 0.5
            }
        }));

        let body = build_api_request(&request, Some(true), "groq");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_tokens"], 200);
        assert_eq!(body["stream"], true);
        assert_eq!(body["frequency_penalty"], 0.5);
    }

    #[test]
    fn provider_options_can_override_model() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "groq": {
                "model": "custom-model"
            }
        }));

        let body = build_api_request(&request, None, "groq");
        assert_eq!(body["model"], "custom-model");
    }
}
