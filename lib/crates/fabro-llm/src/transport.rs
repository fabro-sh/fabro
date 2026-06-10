//! The HTTP transport shared by every provider adapter: how request bytes
//! travel, not what they say.
//!
//! A transport owns the HTTP client, timeouts, the streaming byte loop, and
//! SSE framing. It knows nothing about wire dialects — bodies, endpoints, and
//! error shapes arrive from (and return to) a [`Codec`]. Adapters shrink to
//! auth + route config composed over these helpers.
//!
//! The split mirrors `codec/mod.rs`: a codec knows *what the bytes say*; this
//! module knows *how they travel*.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use fabro_http::HeaderMap;
use futures::stream;
use tokio::time;
use tracing::warn;

use crate::codec::{Codec, CodecCtx, RawEvent, StreamDecoder};
use crate::error::Error;
use crate::provider::StreamEventStream;
use crate::types::{AdapterTimeout, RateLimitInfo, Response, StreamEvent};

// --- HTTP client + configuration
// ----------------------------------------------

/// Shared HTTP infrastructure for provider adapters.
///
/// Holds the API key, base URL, reqwest client, default headers, and timeout
/// configuration that every provider needs. Provider-specific fields live on
/// the adapter struct itself.
pub(crate) struct HttpTransport {
    pub(crate) api_key:             Option<String>,
    pub(crate) base_url:            String,
    pub(crate) default_headers:     HashMap<String, String>,
    pub(crate) client:              fabro_http::HttpClient,
    pub(crate) request_timeout:     Option<Duration>,
    pub(crate) stream_read_timeout: Option<Duration>,
}

impl HttpTransport {
    fn build_client(timeout: AdapterTimeout) -> fabro_http::HttpClient {
        fabro_http::HttpClientBuilder::new()
            .connect_timeout(Duration::from_secs_f64(timeout.connect))
            .build()
            .expect("LLM HTTP client should build")
    }

    #[must_use]
    pub(crate) fn new_optional(api_key: Option<String>, base_url: impl Into<String>) -> Self {
        let timeout = AdapterTimeout::default();
        let client = Self::build_client(timeout);
        Self {
            api_key,
            base_url: base_url.into(),
            default_headers: HashMap::new(),
            client,
            request_timeout: timeout.request.map(Duration::from_secs_f64),
            stream_read_timeout: timeout.stream_read.map(Duration::from_secs_f64),
        }
    }

    #[must_use]
    pub(crate) fn with_timeout(mut self, timeout: AdapterTimeout) -> Self {
        self.client = Self::build_client(timeout);
        self.request_timeout = timeout.request.map(Duration::from_secs_f64);
        self.stream_read_timeout = timeout.stream_read.map(Duration::from_secs_f64);
        self
    }

    #[must_use]
    pub(crate) fn with_default_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.default_headers = headers;
        self
    }
}

// --- Response header parsing
// ---------------------------------------------------

/// Extract the `Retry-After` header value from an HTTP response as seconds.
#[must_use]
pub fn parse_retry_after(headers: &HeaderMap) -> Option<f64> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
}

/// Parse `x-ratelimit-*` headers into a `RateLimitInfo`.
///
/// Returns `None` if no rate limit headers are present.
#[must_use]
pub fn parse_rate_limit_headers(headers: &HeaderMap) -> Option<RateLimitInfo> {
    fn header_i64(headers: &HeaderMap, name: &str) -> Option<i64> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
    }

    fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    }

    let requests_remaining = header_i64(headers, "x-ratelimit-remaining-requests");
    let requests_limit = header_i64(headers, "x-ratelimit-limit-requests");
    let tokens_remaining = header_i64(headers, "x-ratelimit-remaining-tokens");
    let tokens_limit = header_i64(headers, "x-ratelimit-limit-tokens");
    let reset_at = header_str(headers, "x-ratelimit-reset-requests")
        .or_else(|| header_str(headers, "x-ratelimit-reset-tokens"));

    if requests_remaining.is_none()
        && requests_limit.is_none()
        && tokens_remaining.is_none()
        && tokens_limit.is_none()
        && reset_at.is_none()
    {
        return None;
    }

    Some(RateLimitInfo {
        requests_remaining,
        requests_limit,
        tokens_remaining,
        tokens_limit,
        reset_at,
    })
}

// --- Blocking requests
// -----------------------------------------------------------

/// Send a blocking request and decode the response through the codec:
/// `send_for_body` + rate-limit headers + [`Codec::decode_response`].
pub(crate) async fn complete_via_http(
    request: fabro_http::RequestBuilder,
    codec: &dyn Codec,
    ctx: &CodecCtx<'_>,
) -> Result<Response, Error> {
    let (body, headers) = send_for_body(request, "provider_request", codec, ctx).await?;
    let rate_limit = parse_rate_limit_headers(&headers);
    codec.decode_response(&body, ctx, rate_limit)
}

/// Send an HTTP request and read the response body plus headers, mapping
/// non-2xx responses through [`Codec::decode_error`]. `operation` tags the
/// warning logs (`provider_request`, `input_token_count`).
pub(crate) async fn send_for_body(
    request: fabro_http::RequestBuilder,
    operation: &str,
    codec: &dyn Codec,
    ctx: &CodecCtx<'_>,
) -> Result<(String, HeaderMap), Error> {
    let provider = ctx.provider_name;
    let http_resp = request.send().await.map_err(|e| {
        if e.is_timeout() {
            warn!(provider = %provider, operation = %operation, error = %e, "Provider request timed out");
            Error::request_timeout(format!("{provider}: {e}"), e)
        } else {
            warn!(provider = %provider, operation = %operation, error = %e, "Provider network error");
            Error::network(e.to_string(), e)
        }
    })?;

    let status = http_resp.status();
    let retry_after = parse_retry_after(http_resp.headers());
    let headers = http_resp.headers().clone();
    let body = http_resp
        .text()
        .await
        .map_err(|e| Error::network(e.to_string(), e))?;

    if !status.is_success() {
        warn!(provider = %provider, operation = %operation, status = status.as_u16(), "Provider returned error");
        return Err(codec.decode_error(status.as_u16(), &body, ctx, retry_after));
    }

    Ok((body, headers))
}

// --- Streaming
// -------------------------------------------------------------------

/// How a route frames its SSE byte stream into decoder events.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SseFraming {
    /// `\n\n`-delimited blocks carrying `event:` + `data:` lines (anthropic,
    /// openai responses).
    EventBlocks,
    /// Newline-delimited `data:` lines; comments, blank lines, and non-data
    /// fields are skipped (openai_compatible, gemini).
    DataLines,
}

impl SseFraming {
    fn delimiter(self) -> &'static str {
        match self {
            Self::EventBlocks => "\n\n",
            Self::DataLines => "\n",
        }
    }
}

/// Send a streaming request and decode its SSE byte stream through the
/// codec's [`StreamDecoder`]. A non-2xx response is mapped through
/// [`Codec::decode_error`] before any bytes flow.
pub(crate) async fn stream_via_http(
    request: fabro_http::RequestBuilder,
    codec: &dyn Codec,
    ctx: &CodecCtx<'_>,
    framing: SseFraming,
    stream_read_timeout: Option<Duration>,
) -> Result<StreamEventStream, Error> {
    let http_resp = request
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
        return Err(codec.decode_error(status.as_u16(), &body, ctx, retry_after));
    }

    let rate_limit = parse_rate_limit_headers(http_resp.headers());
    let decoder = codec.stream_decoder(ctx, rate_limit);
    Ok(decode_sse_stream(
        http_resp,
        decoder,
        framing,
        stream_read_timeout,
    ))
}

/// State driving the streaming byte loop: the codec's decoder plus the line
/// reader, with a buffer that flattens batched events into individual items.
struct StreamLoop {
    decoder:          Box<dyn StreamDecoder>,
    line_reader:      LineReader,
    /// Events decoded but not yet yielded.
    pending:          VecDeque<StreamEvent>,
    /// Byte stream exhausted.
    done:             bool,
    /// `finish()` already drained.
    finished_emitted: bool,
}

/// Drive `decoder` over the SSE byte stream of `response`: frame each chunk,
/// feed it to the decoder, flatten batched events, and drain
/// [`StreamDecoder::finish`] at byte-stream end.
fn decode_sse_stream(
    response: fabro_http::Response,
    decoder: Box<dyn StreamDecoder>,
    framing: SseFraming,
    stream_read_timeout: Option<Duration>,
) -> StreamEventStream {
    let out = stream::unfold(
        StreamLoop {
            decoder,
            line_reader: LineReader::new(response, stream_read_timeout),
            pending: VecDeque::new(),
            done: false,
            finished_emitted: false,
        },
        move |mut state| async move {
            loop {
                if let Some(event) = state.pending.pop_front() {
                    return Some((Ok(event), state));
                }

                if state.done {
                    if state.finished_emitted {
                        return None;
                    }
                    state.finished_emitted = true;
                    state.pending.extend(state.decoder.finish());
                    if state.pending.is_empty() {
                        return None;
                    }
                    continue;
                }

                match state.line_reader.read_next_chunk(framing.delimiter()).await {
                    Ok(Some(chunk)) => {
                        let Some((event, data)) = frame_sse_chunk(framing, &chunk) else {
                            continue;
                        };
                        match state.decoder.on_event(RawEvent { event, data: &data }) {
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
    Box::pin(out)
}

/// Frame one delimiter-separated chunk into an SSE `(event, data)` pair.
/// Returns `None` for chunks with no payload to decode: heartbeat comments,
/// blank lines, non-data fields, and empty `data:` payloads.
fn frame_sse_chunk(framing: SseFraming, chunk: &str) -> Option<(Option<&str>, Cow<'_, str>)> {
    match framing {
        SseFraming::EventBlocks => parse_sse_block(chunk),
        SseFraming::DataLines => {
            let data = chunk.trim().strip_prefix("data:")?.trim();
            if data.is_empty() {
                return None;
            }
            Some((None, Cow::Borrowed(data)))
        }
    }
}

/// Parse an SSE event block (lines within a `\n\n`-delimited chunk) into
/// `(event_type, data)`. Multi-line `data:` payloads are joined with `\n`;
/// the common single-line case borrows from the block. Returns `None` for
/// blocks with no non-empty payload (e.g. heartbeat comments).
pub(crate) fn parse_sse_block(block: &str) -> Option<(Option<&str>, Cow<'_, str>)> {
    let mut event: Option<&str> = None;
    let mut data: Option<Cow<'_, str>> = None;

    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.trim();
            data = Some(match data {
                None => Cow::Borrowed(rest),
                Some(prev) => {
                    let mut joined = prev.into_owned();
                    joined.push('\n');
                    joined.push_str(rest);
                    Cow::Owned(joined)
                }
            });
        }
    }

    let data = data?;
    if data.is_empty() {
        return None;
    }
    Some((event, data))
}

// --- Byte-stream reading -----------------------------------------------------

/// Shared line reader for SSE streams.
///
/// Buffers bytes from a `fabro_http::Response` and splits them by a
/// configurable delimiter (e.g. `"\n"` for Gemini/OpenAI-compatible, `"\n\n"`
/// for Anthropic/OpenAI SSE event blocks).
pub struct LineReader {
    response:            fabro_http::Response,
    buffer:              String,
    stream_read_timeout: Option<Duration>,
}

impl LineReader {
    pub fn new(response: fabro_http::Response, stream_read_timeout: Option<Duration>) -> Self {
        Self {
            response,
            buffer: String::new(),
            stream_read_timeout,
        }
    }

    /// Read the next complete segment delimited by `delimiter`.
    ///
    /// Returns `Ok(Some(segment))` for each complete segment, `Ok(None)` when
    /// the stream is exhausted, or `Err` on I/O or timeout errors.  When the
    /// stream ends with data remaining in the buffer, the leftover is returned
    /// as a final segment.
    pub async fn read_next_chunk(&mut self, delimiter: &str) -> Result<Option<String>, Error> {
        loop {
            if let Some(pos) = self.buffer.find(delimiter) {
                let segment = self.buffer[..pos].to_string();
                self.buffer = self.buffer[pos + delimiter.len()..].to_string();
                return Ok(Some(segment));
            }

            let chunk_result = match self.stream_read_timeout {
                Some(timeout) => time::timeout(timeout, self.response.chunk()).await,
                None => Ok(self.response.chunk().await),
            };
            match chunk_result {
                Ok(Ok(Some(bytes))) => {
                    let text = String::from_utf8_lossy(&bytes);
                    self.buffer.push_str(&text);
                }
                Ok(Ok(None)) => {
                    if self.buffer.is_empty() {
                        return Ok(None);
                    }
                    let remaining = std::mem::take(&mut self.buffer);
                    return Ok(Some(remaining));
                }
                Ok(Err(e)) => {
                    return Err(Error::stream_error(e.to_string(), e));
                }
                Err(_) => {
                    warn!("Stream read timed out waiting for next event");
                    return Err(Error::Stream {
                        message: "stream read timed out waiting for next event".to_string(),
                        source:  None,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rate_limit_headers_all_present() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining-requests", "99".parse().unwrap());
        headers.insert("x-ratelimit-limit-requests", "100".parse().unwrap());
        headers.insert("x-ratelimit-remaining-tokens", "9000".parse().unwrap());
        headers.insert("x-ratelimit-limit-tokens", "10000".parse().unwrap());
        headers.insert(
            "x-ratelimit-reset-requests",
            "2024-01-01T00:00:00Z".parse().unwrap(),
        );

        let info = parse_rate_limit_headers(&headers).unwrap();
        assert_eq!(info.requests_remaining, Some(99));
        assert_eq!(info.requests_limit, Some(100));
        assert_eq!(info.tokens_remaining, Some(9000));
        assert_eq!(info.tokens_limit, Some(10000));
        assert_eq!(info.reset_at.as_deref(), Some("2024-01-01T00:00:00Z"));
    }

    #[test]
    fn parse_rate_limit_headers_none_present() {
        let headers = HeaderMap::new();
        assert!(parse_rate_limit_headers(&headers).is_none());
    }

    #[test]
    fn parse_rate_limit_headers_partial() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining-requests", "50".parse().unwrap());

        let info = parse_rate_limit_headers(&headers).unwrap();
        assert_eq!(info.requests_remaining, Some(50));
        assert_eq!(info.requests_limit, None);
        assert_eq!(info.tokens_remaining, None);
        assert_eq!(info.tokens_limit, None);
        assert_eq!(info.reset_at, None);
    }

    #[test]
    fn parse_rate_limit_headers_reset_tokens_fallback() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit-tokens", "5000".parse().unwrap());
        headers.insert(
            "x-ratelimit-reset-tokens",
            "2024-06-01T12:00:00Z".parse().unwrap(),
        );

        let info = parse_rate_limit_headers(&headers).unwrap();
        assert_eq!(info.tokens_limit, Some(5000));
        assert_eq!(info.reset_at.as_deref(), Some("2024-06-01T12:00:00Z"));
    }

    #[test]
    fn parse_rate_limit_headers_invalid_values_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-ratelimit-remaining-requests",
            "not-a-number".parse().unwrap(),
        );
        headers.insert("x-ratelimit-limit-tokens", "10000".parse().unwrap());

        let info = parse_rate_limit_headers(&headers).unwrap();
        assert_eq!(info.requests_remaining, None);
        assert_eq!(info.tokens_limit, Some(10000));
    }

    // --- parse_retry_after ---

    #[test]
    fn parse_retry_after_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "2.5".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(2.5));
    }

    #[test]
    fn parse_retry_after_missing() {
        let headers = HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn parse_retry_after_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "not-a-number".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn parse_retry_after_integer() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "5".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(5.0));
    }

    // --- frame_sse_chunk: event blocks ---

    #[test]
    fn parse_sse_block_event_and_data() {
        let block = "event: message_start\ndata: {\"a\":1}";
        let (event, data) = parse_sse_block(block).unwrap();
        assert_eq!(event, Some("message_start"));
        assert_eq!(data, "{\"a\":1}");
    }

    #[test]
    fn parse_sse_block_data_without_event() {
        let block = "data: {\"a\":1}";
        let (event, data) = parse_sse_block(block).unwrap();
        assert_eq!(event, None);
        assert_eq!(data, "{\"a\":1}");
    }

    #[test]
    fn parse_sse_block_joins_multiple_data_lines() {
        let block = "event: e\ndata: line1\ndata: line2";
        let (event, data) = parse_sse_block(block).unwrap();
        assert_eq!(event, Some("e"));
        assert_eq!(data, "line1\nline2");
    }

    #[test]
    fn parse_sse_block_skips_comment_only_block() {
        assert!(parse_sse_block(": heartbeat").is_none());
        assert!(parse_sse_block("event: ping").is_none());
        assert!(parse_sse_block("").is_none());
    }

    #[test]
    fn parse_sse_block_skips_empty_data_payload() {
        assert!(parse_sse_block("data:").is_none());
        assert!(parse_sse_block("event: e\ndata: ").is_none());
    }

    #[test]
    fn parse_sse_block_trims_crlf() {
        let block = "event: e\r\ndata: {\"a\":1}\r";
        let (event, data) = parse_sse_block(block).unwrap();
        assert_eq!(event, Some("e"));
        assert_eq!(data, "{\"a\":1}");
    }

    // --- frame_sse_chunk: data lines ---

    #[test]
    fn data_lines_strips_prefix_and_trims() {
        let (event, data) = frame_sse_chunk(SseFraming::DataLines, "data: {\"a\":1}\r").unwrap();
        assert_eq!(event, None);
        assert_eq!(data, "{\"a\":1}");
    }

    #[test]
    fn data_lines_passes_done_sentinel() {
        let (_, data) = frame_sse_chunk(SseFraming::DataLines, "data: [DONE]").unwrap();
        assert_eq!(data, "[DONE]");
    }

    #[test]
    fn data_lines_skips_comments_blanks_and_other_fields() {
        assert!(frame_sse_chunk(SseFraming::DataLines, ": keep-alive").is_none());
        assert!(frame_sse_chunk(SseFraming::DataLines, "").is_none());
        assert!(frame_sse_chunk(SseFraming::DataLines, "event: x").is_none());
        assert!(frame_sse_chunk(SseFraming::DataLines, "data:").is_none());
    }
}
