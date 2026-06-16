//! Provider adapter for Amazon Bedrock (Converse/ConverseStream).
//!
//! A thin transport shell over the `bedrock_converse` codec: it owns auth
//! (SigV4 signing or a bearer Bedrock API key), the region derivation, and
//! the AWS event-stream byte loop. All wire translation lives in the codec;
//! one codec serves every Converse-capable family because AWS translates the
//! envelope server-side.

pub(crate) mod eventstream;
pub(crate) mod sigv4;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use eventstream::FrameDecoder;
use fabro_auth::ApiKeyHeader;
use fabro_model::Catalog;
use futures::stream;
use sigv4::Sigv4Signer;
use tokio::sync::OnceCell;
use tokio::time;

use crate::adapter_registry::AdapterConfig;
#[cfg(test)]
use crate::adapter_registry::AdapterKindOptions;
use crate::attachments::{self, AttachmentPolicy};
use crate::codec::bedrock_converse::BedrockConverse;
use crate::codec::{Codec, CodecCtx, CodecParams, EncodedRequest, RawEvent, StreamDecoder};
use crate::error::Error;
use crate::provider::{self, ProviderAdapter, StreamEventStream};
use crate::providers::common::{self as common};
use crate::transport::{self, HttpTransport};
use crate::types::{AdapterTimeout, Request, Response, StreamEvent};

/// How the adapter authenticates to Bedrock.
pub(crate) enum BedrockAuth {
    /// Bedrock API key, sent as an `Authorization: Bearer` token.
    ApiKey(String),
    /// SigV4 signing. The signer (holding the AWS default credential chain)
    /// is resolved on first use and cached; the chain itself re-resolves
    /// expiring credentials per request. Tests pre-seed the cell with a
    /// static signer.
    Sigv4(OnceCell<Sigv4Signer>),
}

/// Build a boxed Bedrock adapter from a resolved [`AdapterConfig`].
///
/// Kept in this module (rather than the generic adapter registry) so that
/// Bedrock-specific construction stays encapsulated here. The auth mode is
/// implied by the resolved credential: an `aws_sigv4` credential signs with
/// the AWS chain; a static token is sent as a bearer API key.
pub(crate) fn build(config: AdapterConfig) -> Result<Arc<dyn ProviderAdapter>, Error> {
    let base_url = config
        .base_url
        .clone()
        .ok_or_else(|| Error::Configuration {
            message: format!(
                "bedrock provider '{}' requires a base_url (the Bedrock runtime endpoint)",
                config.provider_id
            ),
            source:  None,
        })?;
    let adapter = match config.auth_header {
        Some(ApiKeyHeader::AwsSigv4) => Adapter::new_sigv4(base_url)?,
        Some(ApiKeyHeader::Bearer(token)) => Adapter::new_api_key(token, base_url)?,
        Some(ApiKeyHeader::Custom { name, .. }) => {
            return Err(Error::Configuration {
                message: format!(
                    "bedrock provider '{}' does not support custom auth header '{}' (use bearer \
                     credentials or aws_sigv4)",
                    config.provider_id, name
                ),
                source:  None,
            });
        }
        None => {
            return Err(Error::Configuration {
                message: format!(
                    "bedrock provider '{}' has no resolved credential (configure `aws_sigv4` or \
                     an API key)",
                    config.provider_id
                ),
                source:  None,
            });
        }
    };
    let mut adapter = adapter.with_name(config.provider_id);
    if !config.extra_headers.is_empty() {
        adapter = adapter.with_default_headers(config.extra_headers);
    }
    if let Some(catalog) = config.catalog {
        adapter = adapter.with_catalog(catalog);
    }
    Ok(Arc::new(adapter))
}

/// Provider adapter for Amazon Bedrock.
pub struct Adapter {
    pub(crate) http: HttpTransport,
    provider_name:   String,
    region:          String,
    auth:            BedrockAuth,
    catalog:         Option<Arc<Catalog>>,
}

impl Adapter {
    /// Construct an adapter that authenticates with a Bedrock API key.
    /// `base_url` is the Bedrock runtime endpoint; the signing region is
    /// parsed from it.
    pub fn new_api_key(
        token: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, Error> {
        Self::with_auth(base_url, BedrockAuth::ApiKey(token.into()))
    }

    /// Construct a SigV4 adapter. Credentials resolve lazily from the AWS
    /// default chain on the first request, so construction stays synchronous.
    pub fn new_sigv4(base_url: impl Into<String>) -> Result<Self, Error> {
        Self::with_auth(base_url, BedrockAuth::Sigv4(OnceCell::new()))
    }

    fn with_auth(base_url: impl Into<String>, auth: BedrockAuth) -> Result<Self, Error> {
        let base_url = base_url.into();
        let region = region_from_base_url(&base_url)?;
        Ok(Self {
            http: HttpTransport::new_optional(None, base_url),
            provider_name: "bedrock".to_string(),
            region,
            auth,
            catalog: None,
        })
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    #[must_use]
    pub fn with_catalog(mut self, catalog: Arc<Catalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    #[must_use]
    pub fn with_default_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.http = self.http.with_default_headers(headers);
        self
    }

    #[must_use]
    pub fn with_timeout(self, timeout: AdapterTimeout) -> Self {
        Self {
            http: self.http.with_timeout(timeout),
            ..self
        }
    }

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

    /// Resolve file-backed attachments to inline data first: Converse takes
    /// inline image and document bytes (no URL sources).
    async fn resolve_request<'a>(&self, request: &'a Request) -> std::borrow::Cow<'a, Request> {
        let policy = AttachmentPolicy {
            images:    true,
            documents: true,
            audio:     false,
        };
        attachments::resolve(request, policy).await
    }

    /// Build the signed/bearer HTTP request for an encoded Converse call.
    async fn build_http_request(
        &self,
        encoded: &EncodedRequest,
        stream: bool,
    ) -> Result<fabro_http::RequestBuilder, Error> {
        let url = format!("{}{}", self.http.base_url, encoded.endpoint);
        let body = serde_json::to_vec(&encoded.body).map_err(|e| Error::Configuration {
            message: format!("failed to serialize converse request: {e}"),
            source:  None,
        })?;

        let mut req = self.http.client.post(&url);
        for (key, value) in &self.http.default_headers {
            req = req.header(key, value);
        }
        for (key, value) in &encoded.headers {
            req = req.header(key, value);
        }

        req = match &self.auth {
            BedrockAuth::ApiKey(token) => req.bearer_auth(token).body(body),
            BedrockAuth::Sigv4(cell) => {
                let signer = cell
                    .get_or_try_init(Sigv4Signer::from_default_chain)
                    .await?;
                signer.sign_post(req, &self.region, &url, body).await?
            }
        };

        req = req.header("content-type", "application/json");
        if stream {
            req = req.header("accept", "application/vnd.amazon.eventstream");
        }
        if let Some(t) = self.http.request_timeout {
            if !stream {
                req = req.timeout(t);
            }
        }
        Ok(req)
    }
}

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn complete(&self, request: &Request) -> Result<Response, Error> {
        self.validate_request(request)?;

        let resolved = self.resolve_request(request).await;
        let codec = BedrockConverse;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let params = CodecParams::default();
        let ctx = self.codec_ctx(&resolved, &deployment_id, &params);

        let encoded = codec.encode(&ctx, false)?;
        let req = self.build_http_request(&encoded, false).await?;
        transport::complete_via_http(req, &codec, &ctx).await
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        self.validate_request(request)?;

        let resolved = self.resolve_request(request).await;
        let codec = BedrockConverse;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let params = CodecParams::default();
        let ctx = self.codec_ctx(&resolved, &deployment_id, &params);

        let encoded = codec.encode(&ctx, true)?;
        let req = self.build_http_request(&encoded, true).await?;

        let http_resp = req
            .send()
            .await
            .map_err(|e| Error::network(e.to_string(), e))?;
        let status = http_resp.status();
        if !status.is_success() {
            let retry_after = transport::parse_retry_after(http_resp.headers());
            let body = http_resp
                .text()
                .await
                .map_err(|e| Error::network(e.to_string(), e))?;
            return Err(codec.decode_error(status.as_u16(), &body, &ctx, retry_after));
        }

        let rate_limit = transport::parse_rate_limit_headers(http_resp.headers());
        let decoder = codec.stream_decoder(&ctx, rate_limit);
        Ok(decode_eventstream(
            http_resp,
            decoder,
            self.http.stream_read_timeout,
        ))
    }

    fn supports_tool_choice(&self, mode: &str) -> bool {
        // Converse has no `none` tool choice on the wire.
        matches!(mode, "auto" | "required" | "named")
    }

    fn validate_request(&self, request: &Request) -> Result<(), Error> {
        if let Some(tool_choice) = &request.tool_choice {
            provider::validate_tool_choice(self, tool_choice)?;
        }
        Ok(())
    }
}

/// State driving the event-stream byte loop: the codec's decoder plus the
/// frame decoder, with a buffer that flattens batched events.
struct EventStreamLoop {
    response: fabro_http::Response,
    frames:   FrameDecoder,
    decoder:  Box<dyn StreamDecoder>,
    pending:  VecDeque<StreamEvent>,
    done:     bool,
    /// `finish()` already drained.
    finished: bool,
    timeout:  Option<Duration>,
}

/// Drive `decoder` over the AWS event-stream byte stream of `response`: the
/// event-stream sibling of the transport's shared SSE loop, anticipated by
/// the transport consolidation notes.
fn decode_eventstream(
    response: fabro_http::Response,
    decoder: Box<dyn StreamDecoder>,
    timeout: Option<Duration>,
) -> StreamEventStream {
    let out = stream::unfold(
        EventStreamLoop {
            response,
            frames: FrameDecoder::new(),
            decoder,
            pending: VecDeque::new(),
            done: false,
            finished: false,
            timeout,
        },
        move |mut state| async move {
            loop {
                if let Some(event) = state.pending.pop_front() {
                    return Some((Ok(event), state));
                }

                if state.done {
                    if state.finished {
                        return None;
                    }
                    state.finished = true;
                    state.pending.extend(state.decoder.finish());
                    if state.pending.is_empty() {
                        return None;
                    }
                    continue;
                }

                let chunk_result = match state.timeout {
                    Some(timeout) => time::timeout(timeout, state.response.chunk()).await,
                    None => Ok(state.response.chunk().await),
                };
                match chunk_result {
                    Ok(Ok(Some(bytes))) => {
                        let frames = match state.frames.push(&bytes) {
                            Ok(frames) => frames,
                            Err(e) => return Some((Err(e), state)),
                        };
                        for frame in frames {
                            let raw = RawEvent {
                                event: Some(frame.event_type.as_str()),
                                data:  frame.payload.as_str(),
                            };
                            match state.decoder.on_event(raw) {
                                Ok(events) => state.pending.extend(events),
                                Err(e) => return Some((Err(e), state)),
                            }
                        }
                    }
                    Ok(Ok(None)) => state.done = true,
                    Ok(Err(e)) => {
                        return Some((Err(Error::stream_error(e.to_string(), e)), state));
                    }
                    Err(_) => {
                        return Some((
                            Err(Error::Stream {
                                message: "stream read timed out waiting for next event".to_string(),
                                source:  None,
                            }),
                            state,
                        ));
                    }
                }
            }
        },
    );
    Box::pin(out)
}

/// Derive the AWS region from a Bedrock runtime endpoint URL.
///
/// The region is a SigV4 signing parameter, so it is parsed from the
/// configured base URL rather than carried as a separate AWS-specific config
/// field. It is validated as `[a-z0-9-]` since it ultimately appears in a
/// signed request.
fn region_from_base_url(base_url: &str) -> Result<String, Error> {
    let invalid = || Error::Configuration {
        message: format!(
            "bedrock base_url '{base_url}' is not a recognized Bedrock runtime endpoint \
             (expected https://bedrock-runtime[-fips].<region>.amazonaws.com[.cn])"
        ),
        source:  None,
    };
    #[expect(
        clippy::disallowed_types,
        reason = "Bedrock region derivation needs URL host parsing; the raw URL is not logged or rendered."
    )]
    let parsed = fabro_http::Url::parse(base_url).map_err(|_| invalid())?;
    let host = parsed.host_str().ok_or_else(invalid)?;
    let rest = host
        .strip_prefix("bedrock-runtime-fips.")
        .or_else(|| host.strip_prefix("bedrock-runtime."))
        .ok_or_else(invalid)?;
    let region = rest
        .strip_suffix(".amazonaws.com.cn")
        .or_else(|| rest.strip_suffix(".amazonaws.com"))
        .ok_or_else(invalid)?;
    let valid = !region.is_empty()
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(region.to_string())
    } else {
        Err(invalid())
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use httpmock::prelude::*;

    use super::*;
    use crate::types::{FinishReason, Message};

    fn make_request(model: &str) -> Request {
        Request {
            model:            model.to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         Some("bedrock".to_string()),
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       Some(64),
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        }
    }

    /// Adapter pointed at httpmock: region parsing only applies to real
    /// bedrock-runtime URLs, so the test constructor sets the region field
    /// directly.
    fn test_adapter(server: &MockServer) -> Adapter {
        Adapter {
            http:          HttpTransport::new_optional(None, server.base_url()),
            provider_name: "bedrock".to_string(),
            region:        "us-east-1".to_string(),
            auth:          BedrockAuth::ApiKey("test-bedrock-key".to_string()),
            catalog:       None,
        }
    }

    #[test]
    fn region_parses_from_standard_endpoint() {
        assert_eq!(
            region_from_base_url("https://bedrock-runtime.eu-west-1.amazonaws.com").unwrap(),
            "eu-west-1"
        );
    }

    #[test]
    fn region_parses_from_fips_endpoint() {
        assert_eq!(
            region_from_base_url("https://bedrock-runtime-fips.us-gov-west-1.amazonaws.com")
                .unwrap(),
            "us-gov-west-1"
        );
    }

    #[test]
    fn region_parses_from_china_endpoint() {
        assert_eq!(
            region_from_base_url("https://bedrock-runtime.cn-north-1.amazonaws.com.cn").unwrap(),
            "cn-north-1"
        );
    }

    #[test]
    fn region_rejects_non_bedrock_hosts() {
        for url in [
            "https://example.com",
            "https://bedrock.us-east-1.amazonaws.com",
            "https://bedrock-runtime.amazonaws.com",
        ] {
            assert!(region_from_base_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn region_normalizes_hostname_case() {
        assert_eq!(
            region_from_base_url("https://bedrock-runtime.US-EAST-1.amazonaws.com").unwrap(),
            "us-east-1"
        );
    }

    #[tokio::test]
    async fn complete_posts_converse_body_with_bearer_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/model/us.anthropic.claude-sonnet-4-6/converse")
                .header("authorization", "Bearer test-bedrock-key")
                .json_body_includes(
                    r#"{"messages":[{"role":"user","content":[{"text":"Hello"}]}],"inferenceConfig":{"maxTokens":64}}"#,
                );
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "output": {"message": {"role": "assistant", "content": [{"text": "Hi!"}]}},
                    "stopReason": "end_turn",
                    "usage": {"inputTokens": 8, "outputTokens": 2, "totalTokens": 10}
                }));
        });

        let adapter = test_adapter(&server);
        let response = adapter
            .complete(&make_request("us.anthropic.claude-sonnet-4-6"))
            .await
            .unwrap();

        mock.assert();
        assert_eq!(response.text(), "Hi!");
        assert_eq!(response.finish_reason, FinishReason::Stop);
        assert_eq!(response.usage.input_tokens, 8);
        assert_eq!(response.provider, "bedrock");
    }

    #[tokio::test]
    async fn complete_applies_default_headers() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/model/m/converse")
                .header("x-fabro-test", "present");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
                    "stopReason": "end_turn",
                    "usage": {"inputTokens": 1, "outputTokens": 1, "totalTokens": 2}
                }));
        });

        let adapter = test_adapter(&server).with_default_headers(HashMap::from([(
            "x-fabro-test".to_string(),
            "present".to_string(),
        )]));
        let response = adapter.complete(&make_request("m")).await.unwrap();

        mock.assert();
        assert_eq!(response.text(), "ok");
    }

    #[test]
    fn factory_rejects_custom_auth_header() {
        let result = build(AdapterConfig {
            provider_id:   "bedrock".to_string(),
            auth_header:   Some(ApiKeyHeader::Custom {
                name:  "x-api-key".to_string(),
                value: "secret".to_string(),
            }),
            base_url:      Some("https://bedrock-runtime.us-east-1.amazonaws.com".to_string()),
            extra_headers: HashMap::new(),
            kind_options:  AdapterKindOptions::None,
            catalog:       None,
        });

        let Err(err) = result else {
            panic!("expected custom auth header to be rejected");
        };
        assert!(
            err.to_string()
                .contains("does not support custom auth header")
        );
    }

    #[tokio::test]
    async fn complete_signs_with_sigv4_when_configured() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/model/m/converse")
                .header_exists("authorization")
                .header_exists("x-amz-date");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
                    "stopReason": "end_turn",
                    "usage": {"inputTokens": 1, "outputTokens": 1, "totalTokens": 2}
                }));
        });

        let mut adapter = test_adapter(&server);
        let cell = OnceCell::new();
        cell.set(Sigv4Signer::from_static("AKIDEXAMPLE", "secret", None))
            .ok();
        adapter.auth = BedrockAuth::Sigv4(cell);

        let response = adapter.complete(&make_request("m")).await.unwrap();
        mock.assert();
        assert_eq!(response.text(), "ok");
    }

    #[tokio::test]
    async fn stream_decodes_eventstream_frames() {
        let server = MockServer::start();
        let body = eventstream::tests::build_stream_body(&[
            ("messageStart", r#"{"role":"assistant"}"#),
            (
                "contentBlockDelta",
                r#"{"delta":{"text":"Hel"},"contentBlockIndex":0}"#,
            ),
            (
                "contentBlockDelta",
                r#"{"delta":{"text":"lo"},"contentBlockIndex":0}"#,
            ),
            ("contentBlockStop", r#"{"contentBlockIndex":0}"#),
            ("messageStop", r#"{"stopReason":"end_turn"}"#),
            (
                "metadata",
                r#"{"usage":{"inputTokens":9,"outputTokens":3,"totalTokens":12}}"#,
            ),
        ]);
        server.mock(|when, then| {
            when.method(POST)
                .path("/model/m/converse-stream")
                .header("accept", "application/vnd.amazon.eventstream");
            then.status(200)
                .header("content-type", "application/vnd.amazon.eventstream")
                .body(body);
        });

        let adapter = test_adapter(&server);
        let mut stream = adapter.stream(&make_request("m")).await.unwrap();

        let mut text = String::new();
        let mut finish: Option<Response> = None;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                StreamEvent::TextDelta { delta, .. } => text.push_str(&delta),
                StreamEvent::Finish { response, .. } => finish = Some(*response),
                _ => {}
            }
        }
        assert_eq!(text, "Hello");
        let response = finish.expect("stream should finish");
        assert_eq!(response.text(), "Hello");
        assert_eq!(response.usage.input_tokens, 9);
    }

    #[tokio::test]
    async fn stream_surfaces_http_error_before_bytes() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/model/m/converse-stream");
            then.status(429)
                .json_body(serde_json::json!({"message": "Too many requests"}));
        });

        let adapter = test_adapter(&server);
        let Err(err) = adapter.stream(&make_request("m")).await else {
            panic!("expected an HTTP error before any stream bytes");
        };
        assert_eq!(err.status_code(), Some(429));
    }

    #[test]
    fn tool_choice_none_is_rejected() {
        let server = MockServer::start();
        let adapter = test_adapter(&server);
        assert!(!adapter.supports_tool_choice("none"));
        assert!(adapter.supports_tool_choice("auto"));
    }
}
