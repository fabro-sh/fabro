use std::sync::Arc;

use fabro_model::Catalog;

use crate::attachments::{self, AttachmentPolicy};
use crate::codec::gemini_generate::GeminiGenerate;
use crate::codec::{Codec, CodecCtx, CodecParams, EncodedRequest};
use crate::error::Error;
use crate::provider::{
    ProviderAdapter, StreamEventStream, validate_standard_speed, validate_tool_choice,
};
use crate::providers::common::{self as common};
use crate::token_count::{InputTokenCount, InputTokenCountMethod};
use crate::transport::{self, HttpTransport, SseFraming};
use crate::types::{AdapterTimeout, Request, Response};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Provider adapter for the Google Gemini `generateContent` API.
///
/// A thin transport shell over the `gemini_generate` codec: it owns auth
/// (`x-goog-api-key`), base URL, and the streaming byte loop. All wire
/// translation — including the model-in-path endpoints — lives in the codec.
/// Gemini has no route variance (single auth scheme, count-tokens always
/// available, no forced streaming), so there is no route config.
pub struct Adapter {
    pub(crate) http: HttpTransport,
    provider_name:   String,
    catalog:         Option<Arc<Catalog>>,
}

impl Adapter {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::new_optional_auth(Some(api_key.into()))
    }

    #[must_use]
    pub fn new_optional_auth(api_key: Option<String>) -> Self {
        Self {
            http:          HttpTransport::new_optional(api_key, DEFAULT_BASE_URL),
            provider_name: "gemini".to_string(),
            catalog:       None,
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.http.base_url = base_url.into();
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

    /// Build the canonical request for the codec, resolving file-backed
    /// attachments to inline data first. Borrowed when nothing needs loading.
    async fn resolve_request<'a>(&self, request: &'a Request) -> std::borrow::Cow<'a, Request> {
        // Gemini loads all three attachment kinds inline.
        let policy = AttachmentPolicy {
            images:    true,
            documents: true,
            audio:     true,
        };
        attachments::resolve(request, policy).await
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
            model: common::catalog_model(self.catalog.as_deref(), &request.model),
            params,
        }
    }

    /// Apply the base URL, auth (`x-goog-api-key`), and codec-emitted headers
    /// to an encoded request.
    fn build_http_request(&self, encoded: &EncodedRequest) -> fabro_http::RequestBuilder {
        let url = format!("{}{}", self.http.base_url, encoded.endpoint);
        let mut req = self.http.client.post(&url);
        if let Some(api_key) = &self.http.api_key {
            req = req.header("x-goog-api-key", api_key);
        }
        for (key, value) in &self.http.default_headers {
            req = req.header(key, value);
        }
        for (key, value) in &encoded.headers {
            req = req.header(key, value);
        }
        req.json(&encoded.body)
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

    async fn count_input_tokens(
        &self,
        request: &Request,
    ) -> Result<Option<InputTokenCount>, Error> {
        self.validate_request(request)?;

        let resolved = self.resolve_request(request).await;
        let codec = GeminiGenerate;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let params = CodecParams::default();
        let ctx = self.codec_ctx(&resolved, &deployment_id, &params);

        let Some(encoded) = codec.encode_count_tokens(&ctx).transpose()? else {
            return Ok(None);
        };

        let mut req = self.build_http_request(&encoded);
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }
        let (body, _headers) =
            transport::send_for_body(req, "input_token_count", &codec, &ctx).await?;
        let input_tokens = codec.decode_count_tokens(&body)?;

        Ok(Some(InputTokenCount {
            input_tokens,
            method: InputTokenCountMethod::ProviderApi,
            provider: self.provider_name.clone(),
            model: request.model.clone(),
            warnings: vec![],
        }))
    }

    async fn complete(&self, request: &Request) -> Result<Response, Error> {
        self.validate_request(request)?;

        let resolved = self.resolve_request(request).await;
        let codec = GeminiGenerate;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let params = CodecParams::default();
        let ctx = self.codec_ctx(&resolved, &deployment_id, &params);

        let encoded = codec.encode(&ctx, false)?;
        let mut req = self.build_http_request(&encoded);
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }
        transport::complete_via_http(req, &codec, &ctx).await
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        self.validate_request(request)?;

        let resolved = self.resolve_request(request).await;
        let codec = GeminiGenerate;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let params = CodecParams::default();
        let ctx = self.codec_ctx(&resolved, &deployment_id, &params);

        let encoded = codec.encode(&ctx, true)?;
        transport::stream_via_http(
            self.build_http_request(&encoded),
            &codec,
            &ctx,
            SseFraming::DataLines,
            self.http.stream_read_timeout,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use httpmock::prelude::*;

    use super::*;
    use crate::types::Message;

    fn minimal_request() -> Request {
        Request {
            model:            "gemini-2.0-flash".to_string(),
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

    #[tokio::test]
    async fn count_input_tokens_posts_generate_content_request_and_parses_response() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/models/gemini-2.0-flash:countTokens")
                .header("x-goog-api-key", "test-key");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({"totalTokens": 456}));
        });
        let adapter = Adapter::new("test-key").with_base_url(server.base_url());

        let count = adapter
            .count_input_tokens(&minimal_request())
            .await
            .unwrap()
            .expect("gemini should count tokens");

        mock.assert();
        assert_eq!(count.input_tokens, 456);
        assert_eq!(count.method, InputTokenCountMethod::ProviderApi);
    }
}
