use std::sync::Arc;

use fabro_model::{Catalog, ReasoningEffortFeature};
use futures::stream;

use crate::attachments::{self, AttachmentPolicy};
use crate::codec::anthropic_messages::{AnthropicMessages, anthropic_option};
use crate::codec::{
    AnthropicVersion, Codec, CodecCtx, CodecParams, EncodedRequest, RawEvent, StreamDecoder,
};
use crate::error::Error;
use crate::provider::{self, ProviderAdapter, StreamEventStream};
use crate::providers::common::{
    self as common, parse_rate_limit_headers, parse_retry_after, send_and_read_response,
};
use crate::token_count::{InputTokenCount, InputTokenCountMethod};
use crate::types::{AdapterTimeout, Request, Response, StreamEvent};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Provider adapter for the Anthropic Messages API.
///
/// A thin transport shell over the `anthropic_messages` codec: it owns auth,
/// base URL, the streaming byte loop, and the route configuration that selects
/// between the direct-Anthropic and Kimi-over-anthropic behaviors. All wire
/// translation lives in the codec.
pub struct Adapter {
    pub(crate) http: super::http_api::HttpApi,
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
            http:          super::http_api::HttpApi::new_optional(api_key, DEFAULT_BASE_URL),
            provider_name: "anthropic".to_string(),
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

    /// Resolve the route configuration for this adapter.
    ///
    /// The direct-Anthropic route (`provider_name == "anthropic"`)
    /// authenticates with `x-api-key`, emits the version + beta headers,
    /// and supports the count-tokens endpoint. Every other name (e.g.
    /// Kimi-over-anthropic) is a bearer-auth route with no anthropic
    /// headers, no count-tokens route, and blocking requests served via
    /// streaming. Resolved once here instead of string-comparing
    /// `provider_name` at each request-time decision.
    fn route_config(&self) -> RouteConfig {
        if self.provider_name == "anthropic" {
            RouteConfig {
                auth:                  AuthScheme::ApiKey,
                codec_params:          CodecParams {
                    anthropic_version: AnthropicVersion::Header("2023-06-01"),
                    anthropic_beta:    true,
                },
                supports_count_tokens: true,
                force_streaming:       false,
            }
        } else {
            RouteConfig {
                auth:                  AuthScheme::Bearer,
                codec_params:          CodecParams::default(),
                supports_count_tokens: false,
                force_streaming:       true,
            }
        }
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

    /// Build the canonical request for the codec, resolving file-backed
    /// attachments to inline data first. Borrowed when nothing needs loading.
    async fn resolve_request<'a>(&self, request: &'a Request) -> std::borrow::Cow<'a, Request> {
        // Anthropic loads images and documents inline; audio falls back to a
        // text placeholder in the codec, so it is not loaded here.
        let policy = AttachmentPolicy {
            images:    true,
            documents: true,
            audio:     false,
        };
        attachments::resolve(request, policy).await
    }

    /// Apply the route base URL, auth, and codec-emitted dialect headers to an
    /// encoded request.
    fn build_http_request(
        &self,
        encoded: &EncodedRequest,
        route: &RouteConfig,
    ) -> fabro_http::RequestBuilder {
        let url = format!("{}{}", self.http.base_url, encoded.endpoint);
        let mut req = self.http.client.post(&url);
        // default_headers first so codec/auth headers can override.
        for (key, value) in &self.http.default_headers {
            req = req.header(key, value);
        }
        match route.auth {
            AuthScheme::ApiKey => {
                if let Some(api_key) = &self.http.api_key {
                    req = req.header("x-api-key", api_key);
                }
            }
            AuthScheme::Bearer => {
                if let Some(api_key) = &self.http.api_key {
                    req = req.bearer_auth(api_key);
                }
            }
        }
        for (key, value) in &encoded.headers {
            req = req.header(key, value);
        }
        req.json(&encoded.body)
    }

    /// Collect a streaming response into a single [`Response`].
    ///
    /// Used by non-Anthropic providers (e.g. Kimi) that require `stream=true`.
    async fn complete_via_stream(&self, request: &Request) -> Result<Response, Error> {
        use futures::StreamExt;

        let mut stream = self.stream(request).await?;
        let mut response: Option<Response> = None;

        while let Some(event) = stream.next().await {
            if let StreamEvent::Finish { response: r, .. } = event? {
                response = Some(*r);
            }
        }

        response.ok_or_else(|| Error::Stream {
            message: "complete_via_stream: stream ended without a Finish event".to_string(),
            source:  None,
        })
    }
}

/// Resolved per-request routing decisions (auth, dialect headers, optional
/// routes) that used to be inline `provider_name == "anthropic"` branches.
struct RouteConfig {
    auth:                  AuthScheme,
    codec_params:          CodecParams,
    supports_count_tokens: bool,
    force_streaming:       bool,
}

enum AuthScheme {
    ApiKey,
    Bearer,
}

/// State driving the streaming byte loop: the codec's decoder plus the line
/// reader, with a buffer that flattens batched events into individual items.
struct StreamLoop {
    decoder:          Box<dyn StreamDecoder>,
    line_reader:      super::common::LineReader,
    pending:          std::collections::VecDeque<StreamEvent>,
    done:             bool,
    finished_emitted: bool,
}

/// The `provider_options.anthropic.thinking.type` value, if any.
fn anthropic_thinking_type(provider_options: Option<&serde_json::Value>) -> Option<&str> {
    anthropic_option(provider_options, "thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(serde_json::Value::as_str)
}

/// Parse an SSE event block (lines separated within a `\n\n`-delimited chunk)
/// into `(event_type, data)`. Returns `None` for blocks with no `data:` lines
/// (e.g. heartbeat comments). Borrows from the block — Anthropic events carry
/// a single `data:` line, so the hot path allocates nothing.
fn parse_sse_block(event_block: &str) -> Option<(&str, std::borrow::Cow<'_, str>)> {
    let mut event_type = "";
    let mut data: Option<std::borrow::Cow<'_, str>> = None;

    for line in event_block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = rest.trim();
        } else if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.trim();
            data = Some(match data {
                None => std::borrow::Cow::Borrowed(rest),
                Some(prev) => {
                    let mut joined = prev.into_owned();
                    joined.push('\n');
                    joined.push_str(rest);
                    std::borrow::Cow::Owned(joined)
                }
            });
        }
    }

    data.map(|data| (event_type, data))
}

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn count_input_tokens(
        &self,
        request: &Request,
    ) -> Result<Option<InputTokenCount>, Error> {
        let route = self.route_config();
        if !route.supports_count_tokens {
            return Ok(None);
        }

        self.validate_request(request)?;
        let resolved = self.resolve_request(request).await;
        let codec = AnthropicMessages;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let ctx = self.codec_ctx(&resolved, &deployment_id, &route.codec_params);

        let Some(encoded) = codec.encode_count_tokens(&ctx).transpose()? else {
            return Ok(None);
        };

        let mut req = self.build_http_request(&encoded, &route);
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }
        let (body, _headers) = send_and_read_response(req, &self.provider_name, "type").await?;
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

        let route = self.route_config();
        // Non-Anthropic providers (e.g. Kimi) require stream=true even for
        // blocking calls. Collect the stream into a single Response.
        if route.force_streaming {
            return self.complete_via_stream(request).await;
        }

        let resolved = self.resolve_request(request).await;
        let codec = AnthropicMessages;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let ctx = self.codec_ctx(&resolved, &deployment_id, &route.codec_params);

        let encoded = codec.encode(&ctx, false)?;
        let mut req = self.build_http_request(&encoded, &route);
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }
        let (body, headers) = send_and_read_response(req, &self.provider_name, "type").await?;
        let rate_limit = parse_rate_limit_headers(&headers);
        codec.decode_response(&body, &ctx, rate_limit)
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        self.validate_request(request)?;

        let route = self.route_config();
        let resolved = self.resolve_request(request).await;
        let codec = AnthropicMessages;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let ctx = self.codec_ctx(&resolved, &deployment_id, &route.codec_params);

        let encoded = codec.encode(&ctx, true)?;
        let http_resp = self
            .build_http_request(&encoded, &route)
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

        let out = stream::unfold(
            StreamLoop {
                decoder,
                line_reader: super::common::LineReader::new(http_resp, stream_read_timeout),
                pending: std::collections::VecDeque::new(),
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
                        let events = state.decoder.finish();
                        if events.is_empty() {
                            return None;
                        }
                        state.pending.extend(events);
                        continue;
                    }

                    match state.line_reader.read_next_chunk("\n\n").await {
                        Ok(Some(block)) => {
                            let Some((event_type, data)) = parse_sse_block(&block) else {
                                continue;
                            };
                            match state.decoder.on_event(RawEvent {
                                event: Some(event_type),
                                data:  &data,
                            }) {
                                Ok(events) => state.pending.extend(events),
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

    fn supports_tool_choice(&self, mode: &str) -> bool {
        matches!(mode, "auto" | "none" | "required" | "named")
    }

    fn validate_request(&self, request: &Request) -> Result<(), Error> {
        if let Some(tool_choice) = &request.tool_choice {
            provider::validate_tool_choice(self, tool_choice)?;
        }

        // Always-adaptive models reject manual enabled/disabled thinking
        // configs at the API, so fail them locally with a clear message
        // instead.
        let model_info = common::catalog_model(self.catalog.as_deref(), &request.model);
        if let Some(model) = model_info
            .filter(|m| m.features.reasoning_effort == ReasoningEffortFeature::AlwaysAdaptive)
        {
            if let Some(kind @ ("enabled" | "disabled")) =
                anthropic_thinking_type(request.provider_options.as_ref())
            {
                return Err(Error::Configuration {
                    message: format!(
                        "{} uses always-on adaptive thinking; provider_options.anthropic.thinking.type = \"{kind}\" is not supported. Omit thinking or set only display options.",
                        model.display_name()
                    ),
                    source:  None,
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use httpmock::prelude::*;

    use super::*;
    use crate::token_count::InputTokenCountMethod;
    use crate::types::{Message, ToolDefinition};

    fn make_base_request() -> Request {
        Request {
            model:            "claude-sonnet-4-20250514".to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         Some("anthropic".to_string()),
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       Some(128),
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        }
    }

    #[test]
    fn adapter_with_name() {
        let adapter = Adapter::new("key").with_name("kimi");
        assert_eq!(adapter.name(), "kimi");
    }

    #[test]
    fn adapter_default_name() {
        let adapter = Adapter::new("key");
        assert_eq!(adapter.name(), "anthropic");
    }

    #[tokio::test]
    async fn count_input_tokens_posts_count_request_and_parses_response() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/messages/count_tokens")
                .header("x-api-key", "test-key")
                .header("anthropic-version", "2023-06-01");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({"input_tokens": 123}));
        });
        let adapter = Adapter::new("test-key").with_base_url(server.base_url());
        let request = Request {
            messages: vec![Message::system("Be concise"), Message::user("Hello")],
            tools: Some(vec![ToolDefinition::function(
                "search",
                "Search files",
                serde_json::json!({"type": "object"}),
            )]),
            ..make_base_request()
        };

        let count = adapter
            .count_input_tokens(&request)
            .await
            .unwrap()
            .expect("anthropic should count tokens");

        mock.assert();
        assert_eq!(count.input_tokens, 123);
        assert_eq!(count.method, InputTokenCountMethod::ProviderApi);
    }
}
