use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fabro_vault::Error as VaultError;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Workflow(#[from] fabro_workflow::Error),

    #[error(transparent)]
    Agent(#[from] fabro_agent::Error),

    #[error(transparent)]
    Llm(#[from] fabro_llm::Error),

    #[error(transparent)]
    Store(#[from] fabro_store::Error),

    #[error(transparent)]
    Config(#[from] fabro_config::Error),

    #[error(transparent)]
    Vault(#[from] VaultError),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("authentication required")]
    Unauthorized,

    #[error("access denied")]
    Forbidden,

    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("bad gateway: {0}")]
    BadGateway(String),

    #[error("internal server error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Serialize)]
struct ErrorEntry {
    status: String,
    title:  String,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code:   Option<String>,
}

#[derive(Serialize)]
struct ErrorBody {
    errors: Vec<ErrorEntry>,
}

/// Uniform API error response.
///
/// Serializes to `{"errors": [{"status": "4xx", "title": "...", "detail":
/// "..."}]}`.
#[derive(Clone, Debug)]
pub struct ApiError {
    status: StatusCode,
    detail: String,
    code:   Option<String>,
}

impl ApiError {
    pub fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
            code: None,
        }
    }

    pub fn with_code(
        status: StatusCode,
        detail: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            status,
            detail: detail.into(),
            code: Some(code.into()),
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

    pub fn unauthorized_with_code(detail: impl Into<String>, code: impl Into<String>) -> Self {
        Self::with_code(StatusCode::UNAUTHORIZED, detail, code)
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "Access denied.")
    }

    pub fn forbidden_with_code(detail: impl Into<String>, code: impl Into<String>) -> Self {
        Self::with_code(StatusCode::FORBIDDEN, detail, code)
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub(crate) fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }
}

impl From<Error> for ApiError {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRequest(msg) => Self::bad_request(msg),
            Error::Unauthorized => Self::unauthorized(),
            Error::Forbidden => Self::forbidden(),
            Error::NotFound(msg) => Self::not_found(msg),
            Error::Conflict(msg) => Self::new(StatusCode::CONFLICT, msg),
            Error::ServiceUnavailable(msg) => Self::new(StatusCode::SERVICE_UNAVAILABLE, msg),
            Error::BadGateway(msg) => Self::new(StatusCode::BAD_GATEWAY, msg),
            Error::Workflow(err) => Self::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Error::Agent(err) => Self::new(StatusCode::BAD_GATEWAY, err.to_string()),
            Error::Llm(err) => Self::new(StatusCode::BAD_GATEWAY, err.to_string()),
            Error::Store(err) => Self::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Error::Config(err) => Self::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Error::Vault(err) => Self::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Error::Internal(msg) => Self::new(StatusCode::INTERNAL_SERVER_ERROR, msg),
        }
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
                code: self.code,
            }],
        };
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use serde_json::json;

    use super::ApiError;

    #[tokio::test]
    async fn api_error_with_code_serializes_machine_readable_code() {
        let response = ApiError::with_code(
            StatusCode::UNAUTHORIZED,
            "token expired",
            "access_token_expired",
        )
        .into_response();

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should serialize");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("response body should be valid json");

        assert_eq!(
            body,
            json!({
                "errors": [{
                    "status": "401",
                    "title": "Unauthorized",
                    "detail": "token expired",
                    "code": "access_token_expired"
                }]
            })
        );
    }

    #[tokio::test]
    async fn unauthorized_without_code_omits_code_key() {
        let response = ApiError::unauthorized().into_response();

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should serialize");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("response body should be valid json");

        assert_eq!(
            body,
            json!({
                "errors": [{
                    "status": "401",
                    "title": "Unauthorized",
                    "detail": "Authentication required."
                }]
            })
        );
    }
}
