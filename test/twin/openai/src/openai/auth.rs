use axum::{
    Json,
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::state::NamespaceKey;

pub async fn require_bearer_auth(request: Request, next: Next) -> Response {
    match bearer_token_from_headers(request.headers()) {
        Ok(Some(_)) => next.run(request).await,
        Ok(None) | Err(()) => missing_bearer_token_response(),
    }
}

pub fn openai_request_namespace(
    headers: &HeaderMap,
    require_auth: bool,
) -> Result<NamespaceKey, Response> {
    match bearer_token_from_headers(headers) {
        Ok(Some(token)) => Ok(NamespaceKey::Bearer(token)),
        Ok(None) if !require_auth => Ok(NamespaceKey::Global),
        Ok(None) | Err(()) => Err(missing_bearer_token_response()),
    }
}

pub fn admin_request_namespace(headers: &HeaderMap) -> Result<NamespaceKey, Response> {
    match bearer_token_from_headers(headers) {
        Ok(Some(token)) => Ok(NamespaceKey::Bearer(token)),
        Ok(None) => Ok(NamespaceKey::Global),
        Err(()) => Err(missing_bearer_token_response()),
    }
}

fn bearer_token_from_headers(headers: &HeaderMap) -> Result<Option<String>, ()> {
    match headers.get(AUTHORIZATION) {
        Some(value) => parse_bearer_token(value).map(Some).ok_or(()),
        None => Ok(None),
    }
}

fn parse_bearer_token(value: &HeaderValue) -> Option<String> {
    let Ok(value) = value.to_str() else {
        return None;
    };

    let token = value.strip_prefix("Bearer ").map(str::trim)?;

    (!token.is_empty()).then(|| token.to_owned())
}

fn missing_bearer_token_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "message": "missing or empty bearer token",
                "type": "invalid_request_error",
                "param": "Authorization",
                "code": "missing_bearer_token"
            }
        })),
    )
        .into_response()
}
