#![expect(
    clippy::disallowed_types,
    reason = "SSE transport needs URL parsing for internal request routing; error messages omit raw URLs"
)]

use std::future::Future;

use anyhow::{Context as _, Result, anyhow};
use fabro_http::{Url, header};
use futures::{StreamExt as _, TryStreamExt as _};
use rmcp::RoleClient;
use rmcp::model::ServerJsonRpcMessage;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use sse_stream::{Sse, SseStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

const MAX_SSE_MESSAGE_BYTES: usize = 1024 * 1024;

pub(crate) struct SseClientTransport {
    client:      fabro_http::HttpClient,
    endpoint_rx: watch::Receiver<Option<String>>,
    messages_rx: mpsc::Receiver<ServerJsonRpcMessage>,
    stream_task: Option<JoinHandle<()>>,
}

impl SseClientTransport {
    pub(crate) fn new(url: &str, client: fabro_http::HttpClient) -> Result<Self> {
        let (endpoint_tx, endpoint_rx) = watch::channel(None);
        let (messages_tx, messages_rx) = mpsc::channel(64);
        let sse_url = Url::parse(url).context("invalid SSE MCP URL")?;
        let stream_client = client.clone();

        let stream_task = tokio::spawn(async move {
            if let Err(err) =
                read_sse_stream(stream_client, sse_url, endpoint_tx, messages_tx).await
            {
                tracing::warn!(error = %err, "SSE MCP stream ended");
            }
        });

        Ok(Self {
            client,
            endpoint_rx,
            messages_rx,
            stream_task: Some(stream_task),
        })
    }
}

impl Drop for SseClientTransport {
    fn drop(&mut self) {
        if let Some(stream_task) = &self.stream_task {
            stream_task.abort();
        }
    }
}

impl Transport<RoleClient> for SseClientTransport {
    type Error = SseClientError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let client = self.client.clone();
        let mut endpoint_rx = self.endpoint_rx.clone();
        async move {
            let endpoint = wait_for_endpoint(&mut endpoint_rx).await?;
            client
                .post(endpoint)
                .header(header::CONTENT_TYPE, "application/json")
                .json(&item)
                .send()
                .await
                .map_err(SseClientError::from_error)?
                .error_for_status()
                .map_err(SseClientError::from_error)?;
            Ok(())
        }
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.messages_rx.recv()
    }

    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        if let Some(stream_task) = self.stream_task.take() {
            stream_task.abort();
        }
        Ok(())
    }
}

async fn wait_for_endpoint(
    endpoint_rx: &mut watch::Receiver<Option<String>>,
) -> std::result::Result<String, SseClientError> {
    loop {
        if let Some(endpoint) = endpoint_rx.borrow().clone() {
            return Ok(endpoint);
        }
        endpoint_rx
            .changed()
            .await
            .map_err(|_| SseClientError::EndpointUnavailable)?;
    }
}

async fn read_sse_stream(
    client: fabro_http::HttpClient,
    sse_url: Url,
    endpoint_tx: watch::Sender<Option<String>>,
    messages_tx: mpsc::Sender<ServerJsonRpcMessage>,
) -> std::result::Result<(), SseClientError> {
    let request = client
        .get(sse_url.clone())
        .header(header::ACCEPT, "text/event-stream");
    let response = request
        .send()
        .await
        .map_err(SseClientError::from_error)?
        .error_for_status()
        .map_err(SseClientError::from_error)?;
    let mut size_guard = SseSizeGuard::default();
    let byte_stream = response.bytes_stream().map(move |chunk| {
        let chunk = chunk.map_err(SseClientError::from_error)?;
        size_guard.check_chunk(&chunk)?;
        Ok::<_, SseClientError>(chunk)
    });
    let mut stream = SseStream::from_byte_stream(byte_stream);

    while let Some(event) = stream
        .try_next()
        .await
        .map_err(SseClientError::from_error)?
    {
        handle_sse_event(event, &sse_url, &endpoint_tx, &messages_tx).await?;
    }

    Ok(())
}

async fn handle_sse_event(
    event: Sse,
    sse_url: &Url,
    endpoint_tx: &watch::Sender<Option<String>>,
    messages_tx: &mpsc::Sender<ServerJsonRpcMessage>,
) -> std::result::Result<(), SseClientError> {
    let data = event.data.unwrap_or_default();
    match event.event.as_deref() {
        Some("endpoint") => {
            let endpoint =
                resolve_endpoint_url(sse_url, data.trim()).context("invalid SSE MCP endpoint")?;
            let _ = endpoint_tx.send(Some(endpoint.to_string()));
        }
        None | Some("" | "message") => {
            if data.trim().is_empty() {
                return Ok(());
            }
            let message: ServerJsonRpcMessage =
                serde_json::from_str(&data).context("invalid SSE MCP JSON-RPC message")?;
            messages_tx
                .send(message)
                .await
                .map_err(|_| SseClientError::ReceiverClosed)?;
        }
        _ => {}
    }
    Ok(())
}

fn resolve_endpoint_url(sse_url: &Url, endpoint: &str) -> Result<Url> {
    if endpoint.starts_with("//") {
        return Err(anyhow!("SSE MCP endpoint must not be protocol-relative"));
    }

    let resolved = if endpoint.starts_with('/') {
        let (path, query) = endpoint
            .split_once('?')
            .map_or((endpoint, None), |(path, query)| (path, Some(query)));
        let base_path = sse_url.path().trim_end_matches('/');
        let prefix = base_path.rsplit_once('/').map_or("", |(prefix, _)| prefix);
        let mut url = sse_url.clone();
        url.set_path(&format!("{prefix}{path}"));
        url.set_query(query);
        url
    } else {
        sse_url.join(endpoint)?
    };

    if resolved.origin() != sse_url.origin() {
        return Err(anyhow!("SSE MCP endpoint origin must match SSE URL origin"));
    }

    Ok(resolved)
}

#[derive(Default)]
struct SseSizeGuard {
    current_event_bytes: usize,
    current_line_bytes:  usize,
}

impl SseSizeGuard {
    fn check_chunk(&mut self, chunk: &[u8]) -> std::result::Result<(), SseClientError> {
        for byte in chunk {
            self.current_event_bytes = self
                .current_event_bytes
                .checked_add(1)
                .filter(|bytes| *bytes <= MAX_SSE_MESSAGE_BYTES)
                .ok_or(SseClientError::MessageTooLarge {
                    max_bytes: MAX_SSE_MESSAGE_BYTES,
                })?;

            match *byte {
                b'\n' => {
                    if self.current_line_bytes == 0 {
                        self.current_event_bytes = 0;
                    }
                    self.current_line_bytes = 0;
                }
                b'\r' => {}
                _ => {
                    self.current_line_bytes += 1;
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SseClientError {
    #[error(transparent)]
    Source(#[from] anyhow::Error),
    #[error("SSE MCP endpoint was not received before the stream closed")]
    EndpointUnavailable,
    #[error("SSE MCP receiver closed")]
    ReceiverClosed,
    #[error("SSE MCP message exceeds maximum size of {max_bytes} bytes")]
    MessageTooLarge { max_bytes: usize },
}

impl SseClientError {
    fn from_error(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Source(anyhow::Error::new(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse_url() -> Url {
        Url::parse("https://srv.example.com/sse").unwrap()
    }

    #[test]
    fn accepts_relative_path_with_query() {
        let resolved = resolve_endpoint_url(&sse_url(), "/messages?sessionId=abc").unwrap();
        assert_eq!(
            resolved.as_str(),
            "https://srv.example.com/messages?sessionId=abc"
        );
    }

    #[test]
    fn accepts_same_origin_absolute_url() {
        let resolved =
            resolve_endpoint_url(&sse_url(), "https://srv.example.com/messages?s=1").unwrap();
        assert_eq!(resolved.as_str(), "https://srv.example.com/messages?s=1");
    }

    #[test]
    fn accepts_same_origin_default_port() {
        let resolved =
            resolve_endpoint_url(&sse_url(), "https://srv.example.com:443/messages").unwrap();
        assert_eq!(resolved.origin(), sse_url().origin());
    }

    #[test]
    fn rejects_cross_host_absolute_url() {
        let err = resolve_endpoint_url(&sse_url(), "https://evil.example/steal").unwrap_err();
        assert!(
            err.to_string().contains("origin"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_scheme_downgrade() {
        let err = resolve_endpoint_url(&sse_url(), "http://srv.example.com/messages").unwrap_err();
        assert!(
            err.to_string().contains("origin"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_port_mismatch() {
        let err =
            resolve_endpoint_url(&sse_url(), "https://srv.example.com:8443/messages").unwrap_err();
        assert!(
            err.to_string().contains("origin"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_protocol_relative_url() {
        let err = resolve_endpoint_url(&sse_url(), "//evil.example/steal").unwrap_err();
        assert!(
            err.to_string().contains("protocol-relative"),
            "unexpected error: {err}"
        );
    }
}
