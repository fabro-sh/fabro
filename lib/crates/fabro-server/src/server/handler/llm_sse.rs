//! Shared SSE plumbing for endpoints that proxy LLM `StreamEvent`s.
//!
//! `POST /api/v1/completions` and `POST /api/v1/playground/chat` both run an
//! LLM stream and forward every `StreamEvent` to the browser as a
//! `stream_event` SSE frame. This module owns that framing so the two
//! endpoints cannot drift: serialization failures and stream errors are
//! shaped into the same `{"type": "error", ...}` frame vocabulary, the
//! stream ends when the LLM stream ends or the server shuts down, and a
//! `ping` keep-alive frame goes out every 15 seconds.

use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use fabro_llm::types::StreamEvent;
use futures_util::{Stream, StreamExt};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::error;

/// Forward LLM `StreamEvent`s as `stream_event` SSE frames until the LLM
/// stream ends or `shutdown` fires.
pub(super) fn stream_response(
    stream: impl Stream<Item = Result<StreamEvent, fabro_llm::Error>> + Send + 'static,
    shutdown: CancellationToken,
) -> Response {
    let sse_stream = stream.map(|event| match event {
        Ok(ref evt) => match serde_json::to_string(evt) {
            Ok(json) => Ok::<_, Infallible>(Event::default().event("stream_event").data(json)),
            Err(e) => Ok(Event::default().event("stream_event").data(
                json!({
                    "type": "error",
                    "error": {"Stream": {"message": format!("failed to serialize event: {e}")}},
                    "raw": null
                })
                .to_string(),
            )),
        },
        Err(e) => {
            error!(error = %e, "LLM stream event error");
            Ok(Event::default().event("stream_event").data(
                json!({
                    "type": "error",
                    "error": {"Stream": {"message": e.to_string()}},
                    "raw": null
                })
                .to_string(),
            ))
        }
    });
    let sse_stream = sse_stream.take_until(shutdown.cancelled_owned());

    Sse::new(sse_stream)
        .keep_alive(
            KeepAlive::new().interval(Duration::from_secs(15)).event(
                Event::default()
                    .event("ping")
                    .data(json!({"type": "ping"}).to_string()),
            ),
        )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;

    async fn body_text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read SSE body");
        String::from_utf8(bytes.to_vec()).expect("SSE body is UTF-8")
    }

    #[tokio::test]
    async fn forwards_events_as_stream_event_frames() {
        let stream = futures_util::stream::iter(vec![
            Ok(StreamEvent::StreamStart),
            Ok(StreamEvent::TextDelta {
                delta:   "hi".to_string(),
                text_id: None,
            }),
        ]);
        let body = body_text(stream_response(stream, CancellationToken::new())).await;

        assert!(body.contains("event: stream_event"), "body: {body}");
        assert!(body.contains(r#""type":"stream_start""#), "body: {body}");
        assert!(body.contains(r#""delta":"hi""#), "body: {body}");
    }

    #[tokio::test]
    async fn shapes_stream_errors_into_error_frames() {
        let stream = futures_util::stream::iter(vec![Err(fabro_llm::Error::Interrupt {
            message: "boom".to_string(),
        })]);
        let body = body_text(stream_response(stream, CancellationToken::new())).await;

        assert!(body.contains("event: stream_event"), "body: {body}");
        assert!(body.contains(r#""type":"error""#), "body: {body}");
        assert!(body.contains("boom"), "body: {body}");
    }

    #[tokio::test]
    async fn shutdown_token_ends_the_stream() {
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let stream = futures_util::stream::pending::<Result<StreamEvent, fabro_llm::Error>>();
        let body = body_text(stream_response(stream, shutdown)).await;

        assert!(
            !body.contains("stream_event"),
            "cancelled stream should emit no frames, body: {body}"
        );
    }
}
