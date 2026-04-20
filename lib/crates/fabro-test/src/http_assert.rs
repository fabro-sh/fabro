use std::fmt::{Display, Write};

use axum::body::to_bytes;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;

const BODY_PREVIEW_LIMIT: usize = 4096;
const BINARY_HEX_PREVIEW_BYTES: usize = 64;

pub async fn expect_axum_status(
    response: Response,
    expected: StatusCode,
    context: impl Display,
) -> Response {
    let context = context.to_string();
    let status = response.status();
    if status == expected {
        return response;
    }

    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should fit in memory");
    panic!(
        "{}",
        format_status_mismatch(
            &context,
            &format_status_list(&[expected]),
            status,
            None,
            &headers,
            &bytes,
        )
    );
}

pub async fn assert_axum_status(response: Response, expected: StatusCode, context: impl Display) {
    let _ = expect_axum_status(response, expected, context).await;
}

pub async fn expect_axum_status_in(
    response: Response,
    expected: &[StatusCode],
    context: impl Display,
) -> Response {
    let context = context.to_string();
    let status = response.status();
    if expected.contains(&status) {
        return response;
    }

    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should fit in memory");
    panic!(
        "{}",
        format_status_mismatch(
            &context,
            &format_status_list(expected),
            status,
            None,
            &headers,
            &bytes,
        )
    );
}

pub async fn assert_axum_status_in(
    response: Response,
    expected: &[StatusCode],
    context: impl Display,
) {
    let _ = expect_axum_status_in(response, expected, context).await;
}

pub async fn expect_axum_json(
    response: Response,
    expected: StatusCode,
    context: impl Display,
) -> serde_json::Value {
    let context = context.to_string();
    let response = expect_axum_status(response, expected, &context).await;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should fit in memory");
    parse_json_or_panic(&context, status, None, &headers, &bytes)
}

pub async fn expect_axum_text(
    response: Response,
    expected: StatusCode,
    context: impl Display,
) -> String {
    let context = context.to_string();
    let response = expect_axum_status(response, expected, &context).await;
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should fit in memory");
    String::from_utf8_lossy(&bytes).into_owned()
}

pub async fn expect_axum_bytes(
    response: Response,
    expected: StatusCode,
    context: impl Display,
) -> Vec<u8> {
    let context = context.to_string();
    let response = expect_axum_status(response, expected, &context).await;
    to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should fit in memory")
        .to_vec()
}

pub async fn expect_reqwest_status(
    response: fabro_http::Response,
    expected: StatusCode,
    context: impl Display,
) -> fabro_http::Response {
    let context = context.to_string();
    let status = response.status();
    if status == expected {
        return response;
    }

    let url = response.url().clone();
    let headers = response.headers().clone();
    let bytes = response
        .bytes()
        .await
        .expect("response body should fit in memory");
    panic!(
        "{}",
        format_status_mismatch(
            &context,
            &format_status_list(&[expected]),
            status,
            Some(url.as_str()),
            &headers,
            bytes.as_ref(),
        )
    );
}

pub async fn assert_reqwest_status(
    response: fabro_http::Response,
    expected: StatusCode,
    context: impl Display,
) {
    let _ = expect_reqwest_status(response, expected, context).await;
}

pub async fn expect_reqwest_status_in(
    response: fabro_http::Response,
    expected: &[StatusCode],
    context: impl Display,
) -> fabro_http::Response {
    let context = context.to_string();
    let status = response.status();
    if expected.contains(&status) {
        return response;
    }

    let url = response.url().clone();
    let headers = response.headers().clone();
    let bytes = response
        .bytes()
        .await
        .expect("response body should fit in memory");
    panic!(
        "{}",
        format_status_mismatch(
            &context,
            &format_status_list(expected),
            status,
            Some(url.as_str()),
            &headers,
            bytes.as_ref(),
        )
    );
}

pub async fn assert_reqwest_status_in(
    response: fabro_http::Response,
    expected: &[StatusCode],
    context: impl Display,
) {
    let _ = expect_reqwest_status_in(response, expected, context).await;
}

pub async fn expect_reqwest_json(
    response: fabro_http::Response,
    expected: StatusCode,
    context: impl Display,
) -> serde_json::Value {
    let context = context.to_string();
    let response = expect_reqwest_status(response, expected, &context).await;
    let status = response.status();
    let url = response.url().clone();
    let headers = response.headers().clone();
    let bytes = response
        .bytes()
        .await
        .expect("response body should fit in memory");
    parse_json_or_panic(
        &context,
        status,
        Some(url.as_str()),
        &headers,
        bytes.as_ref(),
    )
}

pub async fn expect_reqwest_text(
    response: fabro_http::Response,
    expected: StatusCode,
    context: impl Display,
) -> String {
    let context = context.to_string();
    let response = expect_reqwest_status(response, expected, &context).await;
    response.text().await.expect("response text should read")
}

pub async fn expect_reqwest_bytes(
    response: fabro_http::Response,
    expected: StatusCode,
    context: impl Display,
) -> Vec<u8> {
    let context = context.to_string();
    let response = expect_reqwest_status(response, expected, &context).await;
    response
        .bytes()
        .await
        .expect("response body should fit in memory")
        .to_vec()
}

fn parse_json_or_panic(
    context: &str,
    status: StatusCode,
    url: Option<&str>,
    headers: &HeaderMap,
    bytes: &[u8],
) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|error| {
        panic!(
            "{}",
            format_json_parse_failure(context, status, url, headers, bytes, &error)
        )
    })
}

fn format_json_parse_failure(
    context: &str,
    status: StatusCode,
    url: Option<&str>,
    headers: &HeaderMap,
    bytes: &[u8],
    error: &serde_json::Error,
) -> String {
    let mut lines = vec![
        context.to_string(),
        "expected a JSON response body".to_string(),
        format!("status: {status}"),
    ];
    if let Some(url) = url {
        lines.push(format!("url: {url}"));
    }
    if let Some(content_type) = header_value(headers, header::CONTENT_TYPE) {
        lines.push(format!("content-type: {content_type}"));
    }
    lines.push(format!("parse error: {error}"));
    lines.push("body:".to_string());
    lines.push(format_body_preview(headers, bytes));
    lines.join("\n")
}

fn format_status_mismatch(
    context: &str,
    expected: &str,
    actual: StatusCode,
    url: Option<&str>,
    headers: &HeaderMap,
    bytes: &[u8],
) -> String {
    let mut lines = vec![
        context.to_string(),
        format!("expected status: {expected}"),
        format!("actual status: {actual}"),
    ];
    if let Some(url) = url {
        lines.push(format!("url: {url}"));
    }
    if let Some(content_type) = header_value(headers, header::CONTENT_TYPE) {
        lines.push(format!("content-type: {content_type}"));
    }
    if let Some(location) = header_value(headers, header::LOCATION) {
        lines.push(format!("location: {location}"));
    }
    lines.push("body:".to_string());
    lines.push(format_body_preview(headers, bytes));
    lines.join("\n")
}

fn format_status_list(statuses: &[StatusCode]) -> String {
    statuses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" or ")
}

fn header_value(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn format_body_preview(headers: &HeaderMap, bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "<empty body>".to_string();
    }

    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) {
        let rendered = serde_json::to_string_pretty(&json).expect("JSON value should pretty print");
        return truncate_text(&rendered, BODY_PREVIEW_LIMIT);
    }

    if std::str::from_utf8(bytes).is_ok() || content_type_is_textual(headers) {
        let rendered = String::from_utf8_lossy(bytes);
        return truncate_text(&rendered, BODY_PREVIEW_LIMIT);
    }

    let preview_len = bytes.len().min(BINARY_HEX_PREVIEW_BYTES);
    let hex = bytes[..preview_len]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut line = format!("binary body ({} bytes)", bytes.len());
    if bytes.len() > preview_len {
        let _ = write!(line, " [truncated to first {preview_len} bytes]");
    }
    line.push('\n');
    line.push_str(&hex);
    line
}

fn content_type_is_textual(headers: &HeaderMap) -> bool {
    let Some(content_type) = header_value(headers, header::CONTENT_TYPE) else {
        return false;
    };
    let content_type = content_type.to_ascii_lowercase();
    content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript")
        || content_type.contains("svg")
        || content_type.contains("x-www-form-urlencoded")
}

fn truncate_text(text: &str, byte_limit: usize) -> String {
    if text.len() <= byte_limit {
        return text.to_string();
    }

    let mut end = byte_limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[truncated from {} bytes to {} bytes]",
        &text[..end],
        text.len(),
        byte_limit
    )
}

#[cfg(test)]
mod tests {
    use std::panic;

    use axum::body::Body;
    use axum::http::HeaderValue;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router, serve};
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::runtime::Runtime;

    use super::*;

    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(message) => (*message).to_string(),
                Err(_) => "<non-string panic>".to_string(),
            },
        }
    }

    fn catch_async_panic(future: impl std::future::Future<Output = ()>) -> String {
        let runtime = Runtime::new().expect("test runtime should build");
        let payload = panic::catch_unwind(panic::AssertUnwindSafe(|| runtime.block_on(future)))
            .expect_err("future should panic");
        panic_message(payload)
    }

    async fn catch_task_panic(
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> String {
        let join_error = tokio::spawn(future).await.expect_err("future should panic");
        panic_message(join_error.into_panic())
    }

    async fn start_test_server(router: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("test listener should expose local addr");
        tokio::spawn(async move {
            serve(listener, router)
                .await
                .expect("test server should stay alive");
        });
        format!("http://{addr}")
    }

    #[test]
    fn axum_mismatch_pretty_prints_json_body() {
        let response = (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "boom", "details": { "request_id": "req-123" } })),
        )
            .into_response();

        let panic = catch_async_panic(async move {
            let _ = expect_axum_status(response, StatusCode::OK, "GET /api/v1/runs/1/graph").await;
        });

        assert!(panic.contains("GET /api/v1/runs/1/graph"));
        assert!(panic.contains("expected status: 200 OK"));
        assert!(panic.contains("actual status: 500 Internal Server Error"));
        assert!(panic.contains("\"error\": \"boom\""));
        assert!(panic.contains("\"request_id\": \"req-123\""));
    }

    #[test]
    fn axum_mismatch_pretty_prints_text_body() {
        let response = (
            StatusCode::BAD_REQUEST,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            "bad request body",
        )
            .into_response();

        let panic = catch_async_panic(async move {
            let _ = expect_axum_status(response, StatusCode::OK, "GET /health").await;
        });

        assert!(panic.contains("bad request body"));
        assert!(panic.contains("content-type: text/plain; charset=utf-8"));
    }

    #[test]
    fn axum_mismatch_describes_binary_body() {
        let response = (
            StatusCode::BAD_GATEWAY,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            )],
            vec![0, 159, 146, 150, 1, 2, 3, 4],
        )
            .into_response();

        let panic = catch_async_panic(async move {
            let _ = expect_axum_status(response, StatusCode::OK, "GET /blob").await;
        });

        assert!(panic.contains("binary body (8 bytes)"));
        assert!(panic.contains("00 9f 92 96 01 02 03 04"));
    }

    #[test]
    fn axum_status_preserves_successful_response() {
        let response = (
            StatusCode::OK,
            [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))],
            "still here",
        )
            .into_response();

        let runtime = Runtime::new().expect("test runtime should build");
        runtime.block_on(async move {
            let response = expect_axum_status(response, StatusCode::OK, "GET /ok").await;
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .expect("content-type should exist");
            assert_eq!(content_type, "text/plain");

            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body should fit in memory");
            assert_eq!(&body[..], b"still here");
        });
    }

    #[test]
    fn axum_status_in_accepts_multiple_statuses() {
        let response = (StatusCode::NO_CONTENT, Body::empty()).into_response();

        let runtime = Runtime::new().expect("test runtime should build");
        runtime.block_on(async move {
            let response = expect_axum_status_in(
                response,
                &[StatusCode::OK, StatusCode::NO_CONTENT],
                "DELETE /api/v1/runs/1",
            )
            .await;
            let status = response.status();
            assert_eq!(status, StatusCode::NO_CONTENT);
        });
    }

    #[test]
    fn axum_json_parse_failure_includes_raw_body() {
        let response = (
            StatusCode::OK,
            [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))],
            "not-json",
        )
            .into_response();

        let panic = catch_async_panic(async move {
            let _ = expect_axum_json(response, StatusCode::OK, "GET /api/v1/settings").await;
        });

        assert!(panic.contains("expected a JSON response body"));
        assert!(panic.contains("not-json"));
    }

    #[tokio::test]
    async fn reqwest_mismatch_includes_url_and_json_body() {
        let base_url = start_test_server(Router::new().route(
            "/fail",
            get(|| async {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "boom", "request_id": "req-9" })),
                )
            }),
        ))
        .await;
        let client = fabro_http::test_http_client().expect("test client should build");
        let response = client
            .get(format!("{base_url}/fail"))
            .send()
            .await
            .expect("request should complete");

        let panic = catch_task_panic(async move {
            let _ = expect_reqwest_status(response, StatusCode::OK, "GET /fail").await;
        })
        .await;

        assert!(panic.contains("GET /fail"));
        assert!(panic.contains("url:"));
        assert!(panic.contains("/fail"));
        assert!(panic.contains("\"error\": \"boom\""));
        assert!(panic.contains("\"request_id\": \"req-9\""));
    }

    #[tokio::test]
    async fn reqwest_status_preserves_successful_response() {
        let base_url = start_test_server(Router::new().route("/ok", get(|| async { "ok" }))).await;
        let client = fabro_http::test_http_client().expect("test client should build");
        let response = client
            .get(format!("{base_url}/ok"))
            .send()
            .await
            .expect("request should complete");

        let response = expect_reqwest_status(response, StatusCode::OK, "GET /ok").await;
        let text = response.text().await.expect("text body should read");
        assert_eq!(text, "ok");
    }

    #[tokio::test]
    async fn reqwest_status_in_accepts_multiple_statuses() {
        let base_url = start_test_server(Router::new().route(
            "/delete",
            get(|| async { (StatusCode::NO_CONTENT, Body::empty()) }),
        ))
        .await;
        let client = fabro_http::test_http_client().expect("test client should build");
        let response = client
            .get(format!("{base_url}/delete"))
            .send()
            .await
            .expect("request should complete");

        let response = expect_reqwest_status_in(
            response,
            &[StatusCode::OK, StatusCode::NO_CONTENT],
            "DELETE /delete",
        )
        .await;
        let status = response.status();
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn reqwest_json_parse_failure_includes_raw_body() {
        let base_url = start_test_server(Router::new().route(
            "/text",
            get(|| async {
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))],
                    "not-json",
                )
            }),
        ))
        .await;
        let client = fabro_http::test_http_client().expect("test client should build");
        let response = client
            .get(format!("{base_url}/text"))
            .send()
            .await
            .expect("request should complete");

        let panic = catch_task_panic(async move {
            let _ = expect_reqwest_json(response, StatusCode::OK, "GET /text").await;
        })
        .await;

        assert!(panic.contains("expected a JSON response body"));
        assert!(panic.contains("not-json"));
    }

    #[test]
    fn truncated_bodies_say_they_are_truncated() {
        let long_text = "x".repeat(5000);
        let response = (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))],
            long_text,
        )
            .into_response();

        let panic = catch_async_panic(async move {
            let _ = expect_axum_status(response, StatusCode::OK, "GET /too-large").await;
        });

        assert!(panic.contains("[truncated"));
    }
}
