use crate::error::{error_from_status_code, SdkError};
use crate::provider::{ProviderAdapter, StreamEventStream};
use crate::providers::common::LineReader;
use crate::types::{FinishReason, Message, Request, Response, StreamEvent, Usage};
use futures::stream;
use tracing::{debug, error};

/// Provider adapter that routes LLM requests through an fabro server's
/// `/completions` endpoint, delegating to whatever real provider the server
/// is configured with.
pub struct Adapter {
    client: reqwest::Client,
    base_url: String,
    provider_name: String,
}

impl Adapter {
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        provider_name: impl Into<String>,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into(),
            provider_name: provider_name.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Server response deserialization types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ServerCompletionResponse {
    id: String,
    model: String,
    message: Message,
    stop_reason: String,
    usage: ServerUsage,
}

#[derive(serde::Deserialize)]
struct ServerUsage {
    input_tokens: i64,
    output_tokens: i64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn map_stop_reason(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop" => FinishReason::Stop,
        "max_tokens" | "length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolCalls,
        other => FinishReason::Other(other.to_string()),
    }
}

/// Build the JSON request body by serializing the `Request` and injecting
/// the `stream` flag.
fn build_body(request: &Request, stream: bool) -> Result<serde_json::Value, SdkError> {
    let mut body = serde_json::to_value(request).map_err(|e| {
        SdkError::configuration_error(format!("failed to serialize request: {e}"), e)
    })?;
    body["stream"] = serde_json::Value::Bool(stream);
    Ok(body)
}

/// Send a POST request and return the validated response.
///
/// Handles timeout/network error mapping and non-2xx status codes.
async fn send_request(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    provider: &str,
) -> Result<reqwest::Response, SdkError> {
    let http_resp = client.post(url).json(body).send().await.map_err(|e| {
        if e.is_timeout() {
            SdkError::request_timeout(e.to_string(), e)
        } else {
            SdkError::network(e.to_string(), e)
        }
    })?;

    let status = http_resp.status();
    debug!(status = %status, "Fabro server response received");

    if !status.is_success() {
        let status_code = status.as_u16();
        let body = http_resp.text().await.unwrap_or_default();
        error!(status = %status_code, body = %body, "Fabro server request failed");
        return Err(error_from_status_code(
            status_code,
            body,
            provider.to_string(),
            None,
            None,
            None,
        ));
    }

    Ok(http_resp)
}

// ---------------------------------------------------------------------------
// ProviderAdapter implementation
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn complete(&self, request: &Request) -> Result<Response, SdkError> {
        let url = format!("{}/completions", self.base_url);
        debug!(base_url = %url, provider = %self.provider_name, "Sending completion to fabro server");

        let body = build_body(request, false)?;
        let http_resp = send_request(&self.client, &url, &body, &self.provider_name).await?;

        let resp_body = http_resp
            .text()
            .await
            .map_err(|e| SdkError::network(e.to_string(), e))?;

        let server_resp: ServerCompletionResponse =
            serde_json::from_str(&resp_body).map_err(|e| {
                SdkError::stream_error(format!("failed to parse completion response: {e}"), e)
            })?;

        let finish_reason = map_stop_reason(&server_resp.stop_reason);
        let total = server_resp.usage.input_tokens + server_resp.usage.output_tokens;

        Ok(Response {
            id: server_resp.id,
            model: server_resp.model,
            provider: self.provider_name.clone(),
            message: server_resp.message,
            finish_reason,
            usage: Usage {
                input_tokens: server_resp.usage.input_tokens,
                output_tokens: server_resp.usage.output_tokens,
                total_tokens: total,
                ..Default::default()
            },
            raw: None,
            warnings: vec![],
            rate_limit: None,
        })
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, SdkError> {
        let url = format!("{}/completions", self.base_url);
        debug!(base_url = %url, provider = %self.provider_name, "Sending completion to fabro server");

        let body = build_body(request, true)?;
        let http_resp = send_request(&self.client, &url, &body, &self.provider_name).await?;

        let stream = stream::unfold(LineReader::new(http_resp, None), |mut reader| async move {
            loop {
                match reader.read_next_chunk("\n\n").await {
                    Ok(Some(block)) => {
                        if let Some((event_type, data)) = parse_sse_block(&block) {
                            if event_type == "stream_event" {
                                match serde_json::from_str::<StreamEvent>(&data) {
                                    Ok(event) => return Some((Ok(event), reader)),
                                    Err(e) => {
                                        return Some((
                                            Err(SdkError::stream_error(
                                                format!("failed to parse stream event: {e}"),
                                                e,
                                            )),
                                            reader,
                                        ));
                                    }
                                }
                            }
                            // Skip non-stream_event SSE events
                        }
                        // Empty or unparsable block — keep reading.
                    }
                    Ok(None) => return None,
                    Err(e) => return Some((Err(e), reader)),
                }
            }
        });

        Ok(Box::pin(stream))
    }
}

/// Parse a single SSE event block into `(event_type, data)`.
///
/// Returns `None` if the block doesn't contain both an `event:` and `data:` line.
fn parse_sse_block(block: &str) -> Option<(String, String)> {
    let mut event_type = None;
    let mut data_lines: Vec<&str> = Vec::new();

    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim());
        }
    }

    let event_type = event_type?;
    if data_lines.is_empty() {
        return None;
    }
    Some((event_type, data_lines.join("\n")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;
    use futures::StreamExt;
    use httpmock::prelude::*;

    fn make_request() -> Request {
        Request {
            model: "test-model".to_string(),
            messages: vec![Message::user("Hello")],
            provider: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            reasoning_effort: None,
            speed: None,
            metadata: None,
            provider_options: None,
        }
    }

    #[tokio::test]
    async fn stream_parses_sse_events() {
        let server = MockServer::start();

        let sse_body = "\
event: stream_event\n\
data: {\"type\":\"stream_start\"}\n\
\n\
event: stream_event\n\
data: {\"type\":\"text_delta\",\"delta\":\"Hello\",\"text_id\":null}\n\
\n\
event: stream_event\n\
data: {\"type\":\"text_delta\",\"delta\":\" world\",\"text_id\":null}\n\
\n";

        server.mock(|when, then| {
            when.method(POST).path("/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(sse_body);
        });

        let adapter = Adapter::new(reqwest::Client::new(), server.base_url(), "test-provider");

        let mut stream = adapter.stream(&make_request()).await.unwrap();

        // First event: StreamStart
        let event = stream.next().await.unwrap().unwrap();
        assert!(matches!(event, StreamEvent::StreamStart));

        // Second event: TextDelta "Hello"
        let event = stream.next().await.unwrap().unwrap();
        match &event {
            StreamEvent::TextDelta { delta, .. } => assert_eq!(delta, "Hello"),
            other => panic!("expected TextDelta, got {other:?}"),
        }

        // Third event: TextDelta " world"
        let event = stream.next().await.unwrap().unwrap();
        match &event {
            StreamEvent::TextDelta { delta, .. } => assert_eq!(delta, " world"),
            other => panic!("expected TextDelta, got {other:?}"),
        }

        // Stream should end
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn complete_parses_response() {
        let server = MockServer::start();

        let response_json = serde_json::json!({
            "id": "resp-123",
            "model": "test-model",
            "message": {
                "role": "assistant",
                "content": [{"kind": "text", "data": "Hello there!"}],
                "name": null,
                "tool_call_id": null
            },
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });

        server.mock(|when, then| {
            when.method(POST).path("/completions");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(response_json);
        });

        let adapter = Adapter::new(reqwest::Client::new(), server.base_url(), "test-provider");

        let response = adapter.complete(&make_request()).await.unwrap();

        assert_eq!(response.id, "resp-123");
        assert_eq!(response.model, "test-model");
        assert_eq!(response.provider, "test-provider");
        assert_eq!(response.text(), "Hello there!");
        assert_eq!(response.finish_reason, FinishReason::Stop);
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.output_tokens, 5);
        assert_eq!(response.usage.total_tokens, 15);
    }

    #[tokio::test]
    async fn complete_returns_error_on_502() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(POST).path("/completions");
            then.status(502).body("Bad Gateway");
        });

        let adapter = Adapter::new(reqwest::Client::new(), server.base_url(), "test-provider");

        let err = adapter.complete(&make_request()).await.unwrap_err();
        match &err {
            SdkError::Provider { kind, detail } => {
                assert_eq!(*kind, crate::error::ProviderErrorKind::Server);
                assert_eq!(detail.status_code, Some(502));
            }
            other => panic!("expected Provider error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_returns_error_on_502() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(POST).path("/completions");
            then.status(502).body("Bad Gateway");
        });

        let adapter = Adapter::new(reqwest::Client::new(), server.base_url(), "test-provider");

        let result = adapter.stream(&make_request()).await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        match &err {
            SdkError::Provider { kind, detail } => {
                assert_eq!(*kind, crate::error::ProviderErrorKind::Server);
                assert_eq!(detail.status_code, Some(502));
            }
            other => panic!("expected Provider error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_skips_non_stream_event_types() {
        let server = MockServer::start();

        let sse_body = "\
event: ping\n\
data: {}\n\
\n\
event: stream_event\n\
data: {\"type\":\"stream_start\"}\n\
\n";

        server.mock(|when, then| {
            when.method(POST).path("/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(sse_body);
        });

        let adapter = Adapter::new(reqwest::Client::new(), server.base_url(), "test-provider");

        let mut stream = adapter.stream(&make_request()).await.unwrap();

        // The ping event should be skipped, only StreamStart yielded
        let event = stream.next().await.unwrap().unwrap();
        assert!(matches!(event, StreamEvent::StreamStart));

        assert!(stream.next().await.is_none());
    }

    #[test]
    fn map_stop_reason_variants() {
        assert_eq!(map_stop_reason("end_turn"), FinishReason::Stop);
        assert_eq!(map_stop_reason("stop"), FinishReason::Stop);
        assert_eq!(map_stop_reason("max_tokens"), FinishReason::Length);
        assert_eq!(map_stop_reason("length"), FinishReason::Length);
        assert_eq!(map_stop_reason("tool_calls"), FinishReason::ToolCalls);
        assert_eq!(
            map_stop_reason("something_else"),
            FinishReason::Other("something_else".to_string())
        );
    }

    #[test]
    fn parse_sse_block_valid() {
        let block = "event: stream_event\ndata: {\"type\":\"stream_start\"}";
        let (event_type, data) = parse_sse_block(block).unwrap();
        assert_eq!(event_type, "stream_event");
        assert_eq!(data, "{\"type\":\"stream_start\"}");
    }

    #[test]
    fn parse_sse_block_missing_data() {
        let block = "event: stream_event";
        assert!(parse_sse_block(block).is_none());
    }

    #[test]
    fn parse_sse_block_missing_event() {
        let block = "data: {\"type\":\"stream_start\"}";
        assert!(parse_sse_block(block).is_none());
    }

    #[test]
    fn adapter_name() {
        let adapter = Adapter::new(reqwest::Client::new(), "http://localhost", "anthropic");
        assert_eq!(adapter.name(), "anthropic");
    }
}
