//! The Fabro server completions gateway as a lithos provider adapter.
//!
//! `fabro exec --server` sends every model call to `POST /api/v1/completions`
//! on a Fabro server, which holds the provider credentials and the catalog
//! and is the billing authority. The server returns lithos `Response` JSON
//! and streams lithos `StreamEvent` JSON verbatim, so this adapter decodes
//! the standard types and trusts the cost inside them.
//!
//! Transport (authentication, token refresh, base URL) belongs to the caller
//! through [`GatewayTransport`], so this crate does not depend on the CLI's
//! server client.

use std::time::Duration;

use async_trait::async_trait;
use fabro_http::HeaderMap;
use futures::{StreamExt as _, stream};
use lithos_llm::adapter::{ProviderAdapter, ResolvedCall};
use lithos_llm::catalog::{AdapterId, ProviderId};
use lithos_llm::types::{
    Error, ErrorKind, Response, ResponseStream, RetryClassification, StreamEvent,
};

/// Adapter id reported for gateway routes.
pub const GATEWAY_ADAPTER_ID: &str = "fabro-gateway";

/// How the adapter reaches the server.
#[async_trait]
pub trait GatewayTransport: Send + Sync {
    /// Posts a completion body and returns the raw HTTP response.
    async fn post_completion(
        &self,
        body: serde_json::Value,
    ) -> Result<fabro_http::Response, GatewayError>;
}

/// A failure between the adapter and the server.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// The request never produced an HTTP response.
    #[error("{message}")]
    Transport {
        message: String,
        /// Whether the failure was a missing or rejected Fabro login.
        auth:    bool,
    },
    /// The server answered with an error status.
    #[error("server returned HTTP {status}")]
    Status {
        status:  u16,
        headers: HeaderMap,
        body:    String,
    },
}

pub struct GatewayAdapter {
    id:        AdapterId,
    transport: Box<dyn GatewayTransport>,
}

impl GatewayAdapter {
    #[must_use]
    pub fn new(transport: Box<dyn GatewayTransport>) -> Self {
        Self {
            id: AdapterId::new(GATEWAY_ADAPTER_ID),
            transport,
        }
    }

    fn body(call: &ResolvedCall, stream: bool) -> Result<serde_json::Value, Error> {
        let mut body = serde_json::to_value(call.request()).map_err(|source| {
            Error::new(ErrorKind::InvalidRequest, "failed to serialize request").with_source(source)
        })?;
        // The gateway resolves models itself; send the canonical route so the
        // server and the local catalog agree on the offering.
        body["model"] = serde_json::Value::String(call.route().model().id().to_string());
        body["provider"] = serde_json::Value::String(call.route().provider().id().to_string());
        body["stream"] = serde_json::Value::Bool(stream);
        Ok(body)
    }

    async fn send(&self, call: &ResolvedCall, stream: bool) -> Result<fabro_http::Response, Error> {
        let provider = call.route().provider().id().clone();
        self.transport
            .post_completion(Self::body(call, stream)?)
            .await
            .map_err(|err| gateway_error(err, &provider))
    }
}

fn gateway_error(err: GatewayError, provider: &ProviderId) -> Error {
    match err {
        GatewayError::Transport { message, auth } => {
            let kind = if auth {
                ErrorKind::Authentication
            } else {
                ErrorKind::Network
            };
            let mut error = Error::new(kind, message).with_provider(provider.clone());
            if !auth {
                error = error.with_retry(RetryClassification::Safe);
            }
            error
        }
        GatewayError::Status {
            status,
            headers,
            body,
        } => {
            let (message, code) = parse_server_error_body(&body);
            let kind = match status {
                400 | 422 => ErrorKind::InvalidRequest,
                401 => ErrorKind::Authentication,
                403 => ErrorKind::AccessDenied,
                404 => ErrorKind::NotFound,
                408 | 504 => ErrorKind::Timeout,
                429 => ErrorKind::RateLimit,
                500..=599 => ErrorKind::Server,
                _ => ErrorKind::Provider,
            };
            let mut error = Error::new(kind.clone(), message)
                .with_provider(provider.clone())
                .with_status(status);
            if let Some(code) = code {
                error = error.with_provider_code(code);
            }
            match kind {
                ErrorKind::RateLimit | ErrorKind::Server | ErrorKind::Timeout => {
                    error = error.with_retry(RetryClassification::Safe);
                    if let Some(after) = retry_after(&headers) {
                        error = error
                            .with_retry(RetryClassification::after(after))
                            .with_provider_retry_after(after);
                    }
                }
                _ => {}
            }
            error
        }
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<f64>().ok())
        .map(Duration::from_secs_f64)
}

/// Reads the Fabro API error envelope (`errors[0].detail` / `code`), falling
/// back to the raw body.
#[must_use]
pub fn parse_server_error_body(body: &str) -> (String, Option<String>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return (body.to_string(), None);
    };
    let first = value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .and_then(|errors| errors.first());
    let detail = first
        .and_then(|entry| entry.get("detail"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("detail").and_then(serde_json::Value::as_str))
        .unwrap_or("Unknown error")
        .to_string();
    let code = first
        .and_then(|entry| entry.get("code"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    (detail, code)
}

fn parse_sse_block(block: &str) -> Option<(String, String)> {
    let mut event_type = None;
    let mut data_lines = Vec::new();
    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim());
        }
    }
    let event_type = event_type?;
    (!data_lines.is_empty()).then(|| (event_type, data_lines.join("\n")))
}

fn decode_error(message: String, source: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::new(ErrorKind::StreamDecode, message).with_source(source)
}

#[async_trait]
impl ProviderAdapter for GatewayAdapter {
    fn id(&self) -> &AdapterId {
        &self.id
    }

    async fn complete(&self, call: &ResolvedCall) -> Result<Response, Error> {
        let response = self.send(call, false).await?;
        let body = response.text().await.map_err(|source| {
            Error::new(ErrorKind::Network, "failed to read completion body")
                .with_source(source)
                .with_retry(RetryClassification::Safe)
        })?;
        serde_json::from_str(&body)
            .map_err(|source| decode_error("failed to parse completion response".into(), source))
    }

    async fn stream(&self, call: &ResolvedCall) -> Result<ResponseStream, Error> {
        let response = self.send(call, true).await?;
        let state = SseState {
            buffer: String::new(),
            bytes:  Box::pin(response.bytes_stream()),
        };
        let events = stream::unfold(state, |mut state| async move {
            loop {
                if let Some(position) = state.buffer.find("\n\n") {
                    let block = state.buffer[..position].to_string();
                    state.buffer = state.buffer[position + 2..].to_string();
                    let Some((event_type, data)) = parse_sse_block(&block) else {
                        continue;
                    };
                    if event_type != "stream_event" {
                        continue;
                    }
                    let event = serde_json::from_str::<StreamEvent>(&data).map_err(|source| {
                        decode_error("failed to parse stream event".into(), source)
                    });
                    return Some((event, state));
                }
                match state.bytes.next().await {
                    Some(Ok(chunk)) => state.buffer.push_str(&String::from_utf8_lossy(&chunk)),
                    Some(Err(source)) => {
                        let error = Error::new(ErrorKind::Network, "stream read failed")
                            .with_source(source)
                            .with_retry(RetryClassification::Safe);
                        return Some((Err(error), state));
                    }
                    None => return None,
                }
            }
        });
        Ok(ResponseStream::new(events))
    }
}

type ByteStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<bytes::Bytes, fabro_http::HttpError>> + Send>,
>;

struct SseState {
    buffer: String,
    bytes:  ByteStream,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_error_envelope_is_parsed() {
        let (detail, code) = parse_server_error_body(
            r#"{"errors":[{"status":"429","title":"Too Many","detail":"slow down","code":"rate"}]}"#,
        );
        assert_eq!(detail, "slow down");
        assert_eq!(code.as_deref(), Some("rate"));
        let (detail, code) = parse_server_error_body("plain text");
        assert_eq!(detail, "plain text");
        assert!(code.is_none());
    }

    #[test]
    fn sse_blocks_split_event_and_data() {
        assert_eq!(
            parse_sse_block("event: stream_event\ndata: {\"a\":1}"),
            Some(("stream_event".to_string(), "{\"a\":1}".to_string()))
        );
        assert_eq!(parse_sse_block(": comment"), None);
    }

    #[test]
    fn status_codes_map_to_error_kinds() {
        let provider = ProviderId::new("openai");
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "2".parse().unwrap());
        let error = gateway_error(
            GatewayError::Status {
                status: 429,
                headers,
                body: String::new(),
            },
            &provider,
        );
        assert_eq!(error.kind(), ErrorKind::RateLimit);
        assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));
        let error = gateway_error(
            GatewayError::Transport {
                message: "login required".into(),
                auth:    true,
            },
            &provider,
        );
        assert_eq!(error.kind(), ErrorKind::Authentication);
        assert_eq!(error.retry_classification(), RetryClassification::Never);
    }
}

#[cfg(test)]
mod selection_tests {
    use lithos_llm::ModelSelection;
    use lithos_llm::middleware::CallContext;
    use lithos_llm::resolver::{AvailableProviders, ModelResolver as _};
    use lithos_llm::types::Request;

    use super::*;
    use crate::{selection, test_support};

    #[test]
    fn gateway_body_round_trips_provider_scope_for_catalog_and_opaque_ids() {
        let catalog =
            test_support::test_catalog_with_overlay("[providers.openrouter]\nenabled = true\n");
        let available = AvailableProviders::all(&catalog);
        for model in ["openai/gpt-5.6-sol", "vendor/model:version", "default"] {
            let intent = ModelSelection::from_parts(Some(model), Some("openrouter"));
            let route = selection::policy()
                .resolve_selection(&intent, &catalog, &available)
                .unwrap();
            let request = Request::builder()
                .selection(intent)
                .user("hello")
                .build()
                .unwrap();
            let call = ResolvedCall::new(request, route.clone(), CallContext::new());
            let body = GatewayAdapter::body(&call, false).unwrap();
            let received =
                ModelSelection::from_parts(body["model"].as_str(), body["provider"].as_str());
            let rerouted = selection::policy()
                .resolve_selection(&received, &catalog, &available)
                .unwrap();
            assert_eq!(rerouted.handle(), route.handle());
            assert_eq!(rerouted.api_model(), route.api_model());
        }
    }
}
