use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Serialize)]
struct ErrorEntry {
    status: String,
    title: String,
    detail: String,
}

#[derive(Serialize)]
struct ErrorBody {
    errors: Vec<ErrorEntry>,
}

/// Uniform API error response.
///
/// Serializes to `{"errors": [{"status": "4xx", "title": "...", "detail": "..."}]}`.
pub struct ApiError {
    status: StatusCode,
    detail: String,
}

impl ApiError {
    pub fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, detail)
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, detail)
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Authentication required.")
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "Access denied.")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let title = self
            .status
            .canonical_reason()
            .unwrap_or("Unknown")
            .to_string();
        let body = ErrorBody {
            errors: vec![ErrorEntry {
                status: self.status.as_u16().to_string(),
                title,
                detail: self.detail,
            }],
        };
        (self.status, Json(body)).into_response()
    }
}
