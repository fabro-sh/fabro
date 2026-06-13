use std::sync::Arc;

use fabro_model::Catalog;

use crate::codec::openai_compatible::OpenAiCompatible;
use crate::codec::{Codec, CodecCtx, CodecParams};
use crate::error::Error;
use crate::provider::{
    ProviderAdapter, StreamEventStream, validate_standard_speed, validate_tool_choice,
};
use crate::providers::common::api_model_id;
use crate::transport::{self, HttpTransport, SseFraming};
use crate::types::{AdapterTimeout, Request, Response};

/// `OpenAI`-compatible Chat Completions adapter (Section 7.10).
///
/// Use this for third-party services (vLLM, Ollama, Together AI, Groq, etc.)
/// that implement the `OpenAI` Chat Completions API (`/v1/chat/completions`).
///
/// Does NOT support reasoning tokens, built-in tools, or other Responses API
/// features. Use the primary `OpenAiAdapter` for `OpenAI`'s own API.
///
/// This is a thin transport shell over the `openai_compatible` codec: it owns
/// auth, base URL, and the streaming byte loop, and delegates all wire
/// translation to the codec.
pub struct Adapter {
    pub(crate) http: HttpTransport,
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
            http:          HttpTransport::new_optional(api_key, base_url),
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

    /// Resolve the wire model id (catalog `api_id`, falling back to the
    /// requested model).
    fn deployment_id(&self, request: &Request) -> String {
        api_model_id(self.catalog.as_deref(), &request.model)
    }

    /// Build the borrowed codec context. `deployment_id` and `params` are
    /// created by the caller so their borrows outlive the context.
    fn codec_ctx<'a>(
        &'a self,
        request: &'a Request,
        deployment_id: &'a str,
        params: &'a CodecParams,
    ) -> CodecCtx<'a> {
        CodecCtx {
            request,
            provider_name: &self.provider_name,
            deployment_id,
            model: None,
            params,
        }
    }

    /// Encode `ctx.request` through the codec and assemble the HTTP request:
    /// base URL + codec endpoint, default headers, auth, body, and dialect
    /// headers.
    fn encoded_request(
        &self,
        codec: &OpenAiCompatible,
        ctx: &CodecCtx<'_>,
        stream: bool,
    ) -> Result<fabro_http::RequestBuilder, Error> {
        let encoded = codec.encode(ctx, stream)?;
        let url = format!("{}{}", self.http.base_url, encoded.endpoint);
        let mut req = self.build_request(&url).json(&encoded.body);
        for (key, value) in &encoded.headers {
            req = req.header(key, value);
        }
        Ok(req)
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

        let codec = OpenAiCompatible;
        let deployment_id = self.deployment_id(request);
        let params = CodecParams::default();
        let ctx = self.codec_ctx(request, &deployment_id, &params);

        let mut req = self.encoded_request(&codec, &ctx, false)?;
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }

        transport::complete_via_http(req, &codec, &ctx).await
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        self.validate_request(request)?;

        let codec = OpenAiCompatible;
        let deployment_id = self.deployment_id(request);
        let params = CodecParams::default();
        let ctx = self.codec_ctx(request, &deployment_id, &params);

        let req = self.encoded_request(&codec, &ctx, true)?;
        transport::stream_via_http(
            req,
            &codec,
            &ctx,
            SseFraming::DataLines,
            self.http.stream_read_timeout,
        )
        .await
    }
}
