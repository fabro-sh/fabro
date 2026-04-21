use anyhow::{Result, anyhow};
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFailure {
    pub status: fabro_http::StatusCode,
    pub code:   Option<String>,
}

pub struct StructuredApiError {
    pub error:   anyhow::Error,
    pub failure: Option<ApiFailure>,
}

pub struct ApiError {
    pub status:  fabro_http::StatusCode,
    pub headers: fabro_http::HeaderMap,
    pub body:    String,
    failure:     ApiFailure,
}

impl ApiError {
    pub fn api_failure(&self) -> &ApiFailure {
        &self.failure
    }
}

pub fn parse_error_response_value(value: &serde_json::Value) -> (Option<String>, Option<String>) {
    let first = value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .and_then(|errors| errors.first());
    let detail = first
        .and_then(|entry| entry.get("detail"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let code = first
        .and_then(|entry| entry.get("code"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    (detail, code)
}

pub async fn classify_api_error<E>(err: progenitor_client::Error<E>) -> StructuredApiError
where
    E: serde::Serialize + std::fmt::Debug,
{
    match err {
        progenitor_client::Error::UnexpectedResponse(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let mut code = None;
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                let (detail, parsed_code) = parse_error_response_value(&value);
                code = parsed_code;
                if let Some(detail) = detail {
                    return StructuredApiError {
                        error:   anyhow!("{detail}"),
                        failure: Some(ApiFailure { status, code }),
                    };
                }
            }
            let error = if body.is_empty() {
                anyhow!("request failed with status {status}")
            } else {
                anyhow!("request failed with status {status}: {body}")
            };
            StructuredApiError {
                error,
                failure: Some(ApiFailure { status, code }),
            }
        }
        other => map_api_error_structured(other),
    }
}

fn map_api_error_structured<E>(err: progenitor_client::Error<E>) -> StructuredApiError
where
    E: serde::Serialize + std::fmt::Debug,
{
    match err {
        progenitor_client::Error::ErrorResponse(response) => {
            let status = response.status();
            let mut code = None;
            if let Ok(value) = serde_json::to_value(response.into_inner()) {
                let (detail, parsed_code) = parse_error_response_value(&value);
                code = parsed_code;
                if let Some(detail) = detail {
                    return StructuredApiError {
                        error:   anyhow!("{detail}"),
                        failure: Some(ApiFailure { status, code }),
                    };
                }
            }
            StructuredApiError {
                error:   anyhow!("request failed with status {status}"),
                failure: Some(ApiFailure { status, code }),
            }
        }
        progenitor_client::Error::UnexpectedResponse(response) => StructuredApiError {
            error:   anyhow!("request failed with status {}", response.status()),
            failure: Some(ApiFailure {
                status: response.status(),
                code:   None,
            }),
        },
        other => StructuredApiError {
            error:   anyhow!("{other}"),
            failure: None,
        },
    }
}

pub fn map_api_error<E>(err: progenitor_client::Error<E>) -> anyhow::Error
where
    E: serde::Serialize + std::fmt::Debug,
{
    map_api_error_structured(err).error
}

pub fn raw_response_failure_error(failure: &ApiError) -> anyhow::Error {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&failure.body) {
        let (detail, _) = parse_error_response_value(&value);
        if let Some(detail) = detail {
            return anyhow!("{detail}");
        }
    }

    if failure.body.is_empty() {
        return anyhow!("request failed with status {}", failure.status);
    }

    anyhow!(
        "request failed with status {}: {}",
        failure.status,
        failure.body
    )
}

pub async fn classify_http_response(
    response: fabro_http::Response,
) -> Result<std::result::Result<fabro_http::Response, ApiError>> {
    if response.status().is_success() {
        return Ok(Ok(response));
    }
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.text().await.unwrap_or_default();
    let mut code = None;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
        let (_, parsed_code) = parse_error_response_value(&value);
        code = parsed_code;
    }

    Ok(Err(ApiError {
        status,
        headers,
        body,
        failure: ApiFailure { status, code },
    }))
}

pub fn is_not_found_error<E>(err: &progenitor_client::Error<E>) -> bool
where
    E: serde::Serialize + std::fmt::Debug,
{
    match err {
        progenitor_client::Error::ErrorResponse(response) => {
            response.status() == fabro_http::StatusCode::NOT_FOUND
        }
        progenitor_client::Error::UnexpectedResponse(response) => {
            response.status() == fabro_http::StatusCode::NOT_FOUND
        }
        _ => false,
    }
}

pub fn convert_type<TInput, TOutput>(value: TInput) -> Result<TOutput>
where
    TInput: serde::Serialize,
    TOutput: DeserializeOwned,
{
    serde_json::from_value(serde_json::to_value(value)?).map_err(Into::into)
}
