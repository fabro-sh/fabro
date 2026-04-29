use std::fmt;

use axum::body::{Body, HttpBody as _, to_bytes};
use axum::extract::Request;
use axum::http::header::HeaderName;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use serde_json::{Value, json};
use tracing::warn;
use uuid::Uuid;

const BODY_REWRITE_LIMIT: usize = 1 << 20;
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone, Copy, Debug)]
pub(crate) struct RequestId(pub Uuid);

impl RequestId {
    pub(crate) fn render(self) -> String {
        self.0.to_string()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

pub(crate) async fn layer(mut req: Request, next: Next) -> Response {
    let request_id = RequestId(Uuid::new_v4());
    req.extensions_mut().insert(request_id);

    let mut response = next.run(req).await;
    response.headers_mut().insert(
        REQUEST_ID_HEADER,
        HeaderValue::from_str(&request_id.render()).expect("uuid should be a valid header value"),
    );
    if response.status().as_u16() >= 400 && is_json_response(&response) {
        inject_request_id_into_body(response, request_id).await
    } else {
        response
    }
}

fn is_json_response(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(';').next().is_some_and(|media_type| {
                media_type.trim().eq_ignore_ascii_case("application/json")
            })
        })
}

async fn inject_request_id_into_body(response: Response, request_id: RequestId) -> Response {
    match response.body().size_hint().upper() {
        Some(size) if size <= BODY_REWRITE_LIMIT as u64 => {}
        _ => return response,
    }

    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, BODY_REWRITE_LIMIT).await {
        Ok(bytes) => bytes,
        Err(err) => {
            warn!(
                request_id = %request_id,
                ?err,
                "request_id middleware: failed to buffer response body for rewrite"
            );
            parts.headers.remove(header::CONTENT_LENGTH);
            return Response::from_parts(parts, Body::empty());
        }
    };

    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Response::from_parts(parts, Body::from(bytes)),
    };

    let Value::Object(object) = &mut value else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    let rendered = request_id.render();
    if let Some(errors) = object.get_mut("errors").and_then(Value::as_array_mut) {
        for error in errors {
            if let Value::Object(error) = error {
                error.insert("request_id".to_owned(), json!(rendered));
            }
        }
    }
    object.insert("request_id".to_owned(), json!(rendered));

    let new_bytes = serde_json::to_vec(&value).unwrap_or_else(|_| bytes.to_vec());
    parts.headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&new_bytes.len().to_string())
            .expect("content length should be a valid header value"),
    );
    Response::from_parts(parts, Body::from(new_bytes))
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router, middleware};
    use bytes::Bytes;
    use futures_util::stream;
    use serde_json::json;
    use tower::ServiceExt as _;
    use uuid::Uuid;

    async fn ok_handler() -> impl IntoResponse {
        StatusCode::OK
    }

    async fn api_error_handler() -> impl IntoResponse {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "errors": [
                    {
                        "status": "400",
                        "title": "Bad Request",
                        "detail": "invalid input"
                    },
                    {
                        "status": "400",
                        "title": "Bad Request",
                        "detail": "missing field"
                    }
                ]
            })),
        )
    }

    async fn legacy_error_handler() -> impl IntoResponse {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "login required" })),
        )
    }

    async fn stale_request_id_handler() -> impl IntoResponse {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "request_id": "stale-top-level",
                "errors": [
                    {
                        "status": "400",
                        "title": "Bad Request",
                        "detail": "invalid input",
                        "request_id": "stale-entry"
                    }
                ]
            })),
        )
    }

    async fn text_error_handler() -> impl IntoResponse {
        (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            "plain error",
        )
    }

    async fn malformed_json_handler() -> impl IntoResponse {
        (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"#,
        )
    }

    async fn mixed_case_json_handler() -> impl IntoResponse {
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "Application/JSON; charset=utf-8")
            .body(Body::from(r#"{"error":"mixed case"}"#))
            .unwrap()
    }

    async fn oversized_json_handler() -> impl IntoResponse {
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(vec![b'a'; super::BODY_REWRITE_LIMIT + 1]))
            .unwrap()
    }

    async fn unknown_size_json_handler() -> impl IntoResponse {
        let stream = stream::once(async {
            Ok::<_, std::convert::Infallible>(Bytes::from_static(b"{\"error\":\"streamed\"}"))
        });
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from_stream(stream))
            .unwrap()
    }

    async fn send(request: Request<Body>) -> axum::response::Response {
        Router::new()
            .route("/", get(ok_handler))
            .route("/api-error", get(api_error_handler))
            .route("/legacy-error", get(legacy_error_handler))
            .route("/stale-request-id", get(stale_request_id_handler))
            .route("/text-error", get(text_error_handler))
            .route("/malformed-json", get(malformed_json_handler))
            .route("/mixed-case-json", get(mixed_case_json_handler))
            .route("/oversized-json", get(oversized_json_handler))
            .route("/unknown-size-json", get(unknown_size_json_handler))
            .layer(middleware::from_fn(super::layer))
            .oneshot(request)
            .await
            .expect("request should complete")
    }

    fn request_id_header(response: &axum::response::Response) -> String {
        let request_id = response
            .headers()
            .get("x-request-id")
            .expect("request id header should be set")
            .to_str()
            .expect("request id should be ascii")
            .to_owned();
        Uuid::parse_str(&request_id).expect("request id should be a hyphenated uuid");
        request_id
    }

    async fn response_bytes(response: axum::response::Response) -> Bytes {
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should buffer")
    }

    async fn response_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response_bytes(response).await;
        serde_json::from_slice(&bytes).expect("body should remain JSON")
    }

    #[tokio::test]
    async fn sets_request_id_header_on_success_response() {
        let response = send(Request::builder().uri("/").body(Body::empty()).unwrap()).await;

        assert_eq!(response.status(), StatusCode::OK);
        request_id_header(&response);

        let bytes = response_bytes(response).await;
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn overwrites_inbound_request_id_header() {
        let response = send(
            Request::builder()
                .uri("/")
                .header("x-request-id", "GARBAGE-SHOULD-BE-IGNORED")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        let request_id = request_id_header(&response);
        assert_ne!(request_id, "GARBAGE-SHOULD-BE-IGNORED");
    }

    #[tokio::test]
    async fn injects_request_id_into_api_error_body() {
        let response = send(
            Request::builder()
                .uri("/api-error")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let request_id = request_id_header(&response);

        let body = response_json(response).await;

        assert_eq!(body["request_id"], request_id);
        let errors = body["errors"]
            .as_array()
            .expect("errors should be an array");
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().all(|error| error["request_id"] == request_id));
    }

    #[tokio::test]
    async fn injects_request_id_into_legacy_json_error_body() {
        let response = send(
            Request::builder()
                .uri("/legacy-error")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let request_id = request_id_header(&response);
        let body = response_json(response).await;

        assert_eq!(body["error"], "login required");
        assert_eq!(body["request_id"], request_id);
    }

    #[tokio::test]
    async fn overwrites_stale_request_ids_in_json_error_body() {
        let response = send(
            Request::builder()
                .uri("/stale-request-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        let request_id = request_id_header(&response);
        let body = response_json(response).await;

        assert_eq!(body["request_id"], request_id);
        assert_eq!(body["errors"][0]["request_id"], request_id);
    }

    #[tokio::test]
    async fn leaves_non_json_error_body_untouched() {
        let response = send(
            Request::builder()
                .uri("/text-error")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        request_id_header(&response);
        assert_eq!(
            response_bytes(response).await,
            Bytes::from_static(b"plain error")
        );
    }

    #[tokio::test]
    async fn leaves_malformed_json_error_body_untouched() {
        let response = send(
            Request::builder()
                .uri("/malformed-json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        request_id_header(&response);
        assert_eq!(
            response_bytes(response).await,
            Bytes::from_static(br#"{"error":"#)
        );
    }

    #[tokio::test]
    async fn injects_request_id_into_mixed_case_json_error_body() {
        let response = send(
            Request::builder()
                .uri("/mixed-case-json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        let request_id = request_id_header(&response);
        let body = response_json(response).await;

        assert_eq!(body["error"], "mixed case");
        assert_eq!(body["request_id"], request_id);
    }

    #[tokio::test]
    async fn leaves_oversized_json_error_body_untouched() {
        let response = send(
            Request::builder()
                .uri("/oversized-json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        request_id_header(&response);
        let bytes = response_bytes(response).await;
        assert_eq!(bytes.len(), super::BODY_REWRITE_LIMIT + 1);
        assert!(bytes.iter().all(|byte| *byte == b'a'));
    }

    #[tokio::test]
    async fn leaves_unknown_size_json_error_body_untouched() {
        let response = send(
            Request::builder()
                .uri("/unknown-size-json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        request_id_header(&response);
        assert_eq!(
            response_bytes(response).await,
            Bytes::from_static(b"{\"error\":\"streamed\"}")
        );
    }
}
