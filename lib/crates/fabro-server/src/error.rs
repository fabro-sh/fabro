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
    title: String,
    detail: String,
}

#[derive(Serialize)]
struct ErrorBody {
    errors: Vec<ErrorEntry>,
}

/// Uniform API error response.
///
/// Serializes to `{"errors": [{"status": "4xx", "title": "...", "detail":
/// "..."}]}`.
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
            }],
        };
        (self.status, Json(body)).into_response()
    }
}
