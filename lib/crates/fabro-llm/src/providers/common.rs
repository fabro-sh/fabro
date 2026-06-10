use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_http::HeaderMap;
use fabro_model::{Catalog, Model};
use fabro_static::EnvVars;
use tokio::{fs, time};
use tracing::warn;

use crate::error::{Error, error_from_status_code};
use crate::types::{Message, RateLimitInfo, Role};

#[must_use]
pub fn catalog_model<'a>(catalog: Option<&'a Catalog>, model: &str) -> Option<&'a Model> {
    catalog.and_then(|catalog| catalog.get(model))
}

#[must_use]
pub fn api_model_id(catalog: Option<&Catalog>, model: &str) -> String {
    catalog
        .and_then(|catalog| catalog.model_settings(model))
        .map_or_else(|| model.to_string(), |settings| settings.api_id.clone())
}

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
pub async fn load_file_bytes(path: &str) -> Result<(Vec<u8>, String), std::io::Error> {
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
    Ok((data, mime))
}

/// Read a file and return base64-encoded contents plus the inferred MIME type.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub async fn load_file_as_base64(path: &str) -> Result<(String, String), std::io::Error> {
    let (data, mime) = load_file_bytes(path).await?;
    Ok((BASE64_STANDARD.encode(&data), mime))
}

// Transport pieces moved to `crate::transport`; re-exported here because
// fabro-cli imports them from this path (frozen public surface).
pub use crate::transport::{LineReader, parse_rate_limit_headers, parse_retry_after};

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
    send_and_read_response_with_operation(request, provider, error_code_field, "provider_request")
        .await
}

pub(crate) async fn send_and_read_response_with_operation(
    request: fabro_http::RequestBuilder,
    provider: &str,
    error_code_field: &str,
    operation: &str,
) -> Result<(String, HeaderMap), Error> {
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
}
