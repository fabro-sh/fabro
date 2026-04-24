use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_http::HeaderMap;
use fabro_static::EnvVars;
use tokio::{fs, time};
use tracing::warn;

use crate::error::{Error, error_from_status_code};
use crate::types::{Message, RateLimitInfo, Role};

/// Parse an error response body, extracting the message and error code.
///
/// `error_code_field` is the JSON field name for the error code (e.g. "type" or
/// "status").
#[must_use]
pub fn parse_error_body(
    body: &str,
    error_code_field: &str,
) -> (String, Option<String>, Option<serde_json::Value>) {
    serde_json::from_str::<serde_json::Value>(body).map_or_else(
        |_| (body.to_string(), None, None),
        |v| {
            let message = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                // Codex endpoint returns {"detail": "..."} instead of {"error": {"message": "..."}}
                .or_else(|| v.get("detail").and_then(serde_json::Value::as_str))
                .unwrap_or("Unknown error")
                .to_string();
            let error_code = v
                .get("error")
                .and_then(|e| e.get(error_code_field))
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            (message, error_code, Some(v))
        },
    )
}

/// Extract system and developer messages from a message list.
///
/// Returns the joined system prompt and the remaining messages.
/// Per spec, Developer role messages are merged with system messages
/// for Anthropic and Gemini.
#[must_use]
pub fn extract_system_prompt(messages: &[Message]) -> (Option<String>, Vec<&Message>) {
    let mut system_parts = Vec::new();
    let mut other = Vec::new();
    for msg in messages {
        if msg.role == Role::System || msg.role == Role::Developer {
            let text = msg.text();
            if !text.trim().is_empty() {
                system_parts.push(text);
            }
        } else {
            other.push(msg);
        }
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n"))
    };
    (system, other)
}

/// Check if a URL string looks like a local file path.
#[must_use]
pub fn is_file_path(url: &str) -> bool {
    url.starts_with('/') || url.starts_with("./") || url.starts_with("~/")
}

/// Infer MIME type from a file extension.
#[must_use]
pub fn mime_from_extension(path: &str) -> &str {
    match path.rsplit('.').next().map(str::to_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("heic") => "image/heic",
        Some("heif") => "image/heif",
        Some("pdf") => "application/pdf",
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mp3",
        _ => "application/octet-stream",
    }
}

/// Load a local file, returning (`base64_data`, `mime_type`).
/// Expands ~ to home directory.
///
/// # Errors
/// Returns an error if the file cannot be read.
#[expect(
    clippy::disallowed_methods,
    reason = "Attachment path expansion supports the conventional HOME env var."
)]
pub async fn load_file_as_base64(path: &str) -> Result<(String, String), std::io::Error> {
    let expanded = path.strip_prefix("~/").map_or_else(
        || path.to_string(),
        |rest| {
            let home = std::env::var(EnvVars::HOME).unwrap_or_else(|_| "/".to_string());
            format!("{home}/{rest}")
        },
    );
    let data = fs::read(&expanded).await.map_err(|err| {
        std::io::Error::new(err.kind(), format!("read attachment {expanded}: {err}"))
    })?;
    let mime = mime_from_extension(&expanded).to_string();
    let b64 = BASE64_STANDARD.encode(&data);
    Ok((b64, mime))
}

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

/// Send an HTTP request, read the response body, and return it along with the
/// response headers.
///
/// Returns an error on non-success status.
///
/// # Errors
///
/// Returns `Error::Network` on connection failure or `Error::Provider` on
/// non-success status.
pub async fn send_and_read_response(
    request: fabro_http::RequestBuilder,
    provider: &str,
    error_code_field: &str,
) -> Result<(String, HeaderMap), Error> {
    let http_resp = request.send().await.map_err(|e| {
        if e.is_timeout() {
            warn!(provider = %provider, error = %e, "Provider request timed out");
            Error::request_timeout(format!("{provider}: {e}"), e)
        } else {
            warn!(provider = %provider, error = %e, "Provider network error");
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
        warn!(provider = %provider, status = status.as_u16(), "Provider returned error");
        let (msg, code, raw) = parse_error_body(&body, error_code_field);
        return Err(error_from_status_code(
            status.as_u16(),
            msg,
            provider.to_string(),
            code,
            raw,
            retry_after,
        ));
    }

    Ok((body, headers))
}

/// Shared line reader for SSE streams.
///
/// Buffers bytes from a `fabro_http::Response` and splits them by a
/// configurable delimiter (e.g. `"\n"` for Gemini/OpenAI-compatible, `"\n\n"`
/// for Anthropic/OpenAI SSE event blocks).
pub struct LineReader {
    response:            fabro_http::Response,
    buffer:              String,
    stream_read_timeout: Option<std::time::Duration>,
}

impl LineReader {
    pub fn new(
        response: fabro_http::Response,
        stream_read_timeout: Option<std::time::Duration>,
    ) -> Self {
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
    use crate::types::ContentPart;

    #[test]
    fn is_file_path_absolute() {
        assert!(is_file_path("/tmp/image.png"));
        assert!(is_file_path("/home/user/photo.jpg"));
    }

    #[test]
    fn is_file_path_relative() {
        assert!(is_file_path("./image.png"));
        assert!(is_file_path("./subdir/photo.jpg"));
    }

    #[test]
    fn is_file_path_tilde() {
        assert!(is_file_path("~/image.png"));
        assert!(is_file_path("~/Documents/photo.jpg"));
    }

    #[test]
    fn is_file_path_url() {
        assert!(!is_file_path("https://example.com/image.png"));
        assert!(!is_file_path("http://example.com/image.png"));
        assert!(!is_file_path("data:image/png;base64,abc"));
    }

    #[test]
    fn mime_from_extension_known() {
        assert_eq!(mime_from_extension("photo.png"), "image/png");
        assert_eq!(mime_from_extension("photo.jpg"), "image/jpeg");
        assert_eq!(mime_from_extension("photo.jpeg"), "image/jpeg");
        assert_eq!(mime_from_extension("photo.gif"), "image/gif");
        assert_eq!(mime_from_extension("photo.webp"), "image/webp");
        assert_eq!(mime_from_extension("doc.pdf"), "application/pdf");
    }

    #[test]
    fn mime_from_extension_unknown() {
        assert_eq!(mime_from_extension("file.xyz"), "application/octet-stream");
        assert_eq!(mime_from_extension("noext"), "application/octet-stream");
    }

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

    // --- parse_error_body ---

    #[test]
    fn parse_error_body_valid_json() {
        let body = r#"{"error":{"message":"rate limited","type":"rate_limit_error"}}"#;
        let (msg, code, raw) = parse_error_body(body, "type");
        assert_eq!(msg, "rate limited");
        assert_eq!(code.as_deref(), Some("rate_limit_error"));
        assert!(raw.is_some());
    }

    #[test]
    fn parse_error_body_missing_error_field() {
        let body = r#"{"status":"fail"}"#;
        let (msg, code, raw) = parse_error_body(body, "type");
        assert_eq!(msg, "Unknown error");
        assert_eq!(code, None);
        assert!(raw.is_some());
    }

    #[test]
    fn parse_error_body_not_json() {
        let body = "Internal Server Error";
        let (msg, code, raw) = parse_error_body(body, "type");
        assert_eq!(msg, "Internal Server Error");
        assert_eq!(code, None);
        assert!(raw.is_none());
    }

    #[test]
    fn parse_error_body_different_code_field() {
        let body = r#"{"error":{"message":"bad","status":"INVALID_ARGUMENT"}}"#;
        let (msg, code, _) = parse_error_body(body, "status");
        assert_eq!(msg, "bad");
        assert_eq!(code.as_deref(), Some("INVALID_ARGUMENT"));
    }

    #[test]
    fn parse_error_body_no_message() {
        let body = r#"{"error":{"type":"server_error"}}"#;
        let (msg, code, _) = parse_error_body(body, "type");
        assert_eq!(msg, "Unknown error");
        assert_eq!(code.as_deref(), Some("server_error"));
    }

    // --- extract_system_prompt ---

    #[test]
    fn extract_system_prompt_no_system() {
        let msgs = vec![Message::user("hello")];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys, None);
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn extract_system_prompt_system_only() {
        let msgs = vec![Message::system("Be helpful"), Message::user("hi")];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys.as_deref(), Some("Be helpful"));
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].role, Role::User);
    }

    #[test]
    fn extract_system_prompt_multiple_system() {
        let msgs = vec![
            Message::system("Rule 1"),
            Message::system("Rule 2"),
            Message::user("hi"),
        ];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys.as_deref(), Some("Rule 1\nRule 2"));
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn extract_system_prompt_developer_role() {
        let dev = Message {
            role:         Role::Developer,
            content:      vec![ContentPart::text("dev instructions")],
            name:         None,
            tool_call_id: None,
        };
        let msgs = vec![dev, Message::user("hi")];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys.as_deref(), Some("dev instructions"));
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn extract_system_prompt_ignores_whitespace_system_and_developer() {
        let dev = Message {
            role:         Role::Developer,
            content:      vec![ContentPart::text(" \n\t ")],
            name:         None,
            tool_call_id: None,
        };
        let msgs = vec![Message::system("   "), dev, Message::user("hi")];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys, None);
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].role, Role::User);
    }

    #[test]
    fn extract_system_prompt_empty() {
        let msgs: Vec<Message> = vec![];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys, None);
        assert!(other.is_empty());
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
}
