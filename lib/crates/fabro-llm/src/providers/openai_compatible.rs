use std::collections::VecDeque;
use std::sync::Arc;

use fabro_model::Catalog;
use futures::stream;

use crate::codec::openai_compatible::OpenAiCompatible;
use crate::codec::{Codec, CodecCtx, CodecParams, RawEvent, StreamDecoder};
use crate::error::Error;
use crate::provider::{
    ProviderAdapter, StreamEventStream, validate_standard_speed, validate_tool_choice,
};
use crate::providers::common::{
    api_model_id, parse_rate_limit_headers, parse_retry_after, send_and_read_response,
};
use crate::types::{AdapterTimeout, Request, Response, StreamEvent};

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

/// State driving the streaming byte loop: the codec's decoder plus the line
/// reader, with a small buffer that flattens batched events into individual
/// stream items.
struct StreamLoop {
    decoder:          Box<dyn StreamDecoder>,
    line_reader:      super::common::LineReader,
    /// Events decoded but not yet yielded.
    pending:          VecDeque<StreamEvent>,
    /// Byte stream exhausted.
    done:             bool,
    /// `finish()` already drained.
    finished_emitted: bool,
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
        let params = CodecParams;
        let ctx = self.codec_ctx(request, &deployment_id, &params);

        let mut req = self.encoded_request(&codec, &ctx, false)?;
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }

        let (body, headers) = send_and_read_response(req, &self.provider_name, "type").await?;
        let rate_limit = parse_rate_limit_headers(&headers);
        codec.decode_response(&body, &ctx, rate_limit)
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        self.validate_request(request)?;

        let codec = OpenAiCompatible;
        let deployment_id = self.deployment_id(request);
        let params = CodecParams;
        let ctx = self.codec_ctx(request, &deployment_id, &params);

        let req = self.encoded_request(&codec, &ctx, true)?;
        let http_resp = req
            .send()
            .await
            .map_err(|e| Error::network(e.to_string(), e))?;

        let status = http_resp.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(http_resp.headers());
            let body = http_resp
                .text()
                .await
                .map_err(|e| Error::network(e.to_string(), e))?;
            return Err(codec.decode_error(status.as_u16(), &body, &ctx, retry_after));
        }

        let rate_limit = parse_rate_limit_headers(http_resp.headers());
        let stream_read_timeout = self.http.stream_read_timeout;
        let decoder = codec.stream_decoder(&ctx, rate_limit);
        let line_reader = super::common::LineReader::new(http_resp, stream_read_timeout);

        let out = stream::unfold(
            StreamLoop {
                decoder,
                line_reader,
                pending: VecDeque::new(),
                done: false,
                finished_emitted: false,
            },
            |mut state| async move {
                loop {
                    if let Some(event) = state.pending.pop_front() {
                        return Some((Ok(event), state));
                    }

                    if state.done {
                        if state.finished_emitted {
                            return None;
                        }
                        state.finished_emitted = true;
                        state.pending = state.decoder.finish().into();
                        if state.pending.is_empty() {
                            return None;
                        }
                        continue;
                    }

                    match state.line_reader.read_next_chunk("\n").await {
                        Ok(Some(line)) => {
                            let line = line.trim();
                            if line.is_empty() || line.starts_with(':') {
                                continue;
                            }
                            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                                continue;
                            };
                            match state.decoder.on_event(RawEvent { event: None, data }) {
                                Ok(events) => state.pending = events.into(),
                                Err(e) => return Some((Err(e), state)),
                            }
                        }
                        Ok(None) => state.done = true,
                        Err(e) => return Some((Err(e), state)),
                    }
                }
            },
        );

        Ok(Box::pin(out))
    }
}
