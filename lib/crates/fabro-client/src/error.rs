use anyhow::{Result, anyhow};
use fabro_util::exit::{ErrorExt, ExitClass};
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

fn classify_from_status(err: anyhow::Error, status: fabro_http::StatusCode) -> anyhow::Error {
    if status == fabro_http::StatusCode::UNAUTHORIZED {
        err.classify(ExitClass::AuthRequired)
    } else {
        err
    }
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
                        error:   classify_from_status(anyhow!("{detail}"), status),
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
                error:   classify_from_status(error, status),
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
                        error:   classify_from_status(anyhow!("{detail}"), status),
                        failure: Some(ApiFailure { status, code }),
                    };
                }
            }
            StructuredApiError {
                error:   classify_from_status(
                    anyhow!("request failed with status {status}"),
                    status,
                ),
                failure: Some(ApiFailure { status, code }),
            }
        }
        progenitor_client::Error::UnexpectedResponse(response) => StructuredApiError {
            error:   classify_from_status(
                anyhow!("request failed with status {}", response.status()),
                response.status(),
            ),
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
            return classify_from_status(anyhow!("{detail}"), failure.status);
        }
    }

    if failure.body.is_empty() {
        return classify_from_status(
            anyhow!("request failed with status {}", failure.status),
            failure.status,
        );
    }

    classify_from_status(
        anyhow!(
            "request failed with status {}: {}",
            failure.status,
            failure.body
        ),
        failure.status,
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

#[cfg(test)]
mod tests {
    use fabro_util::exit;
    use serde_json::json;

    use super::{ApiError, ApiFailure, map_api_error, raw_response_failure_error};

    fn error_response(
        status: fabro_http::StatusCode,
        detail: &str,
        code: &str,
    ) -> progenitor_client::Error<serde_json::Value> {
        let response = progenitor_client::ResponseValue::new(
            json!({
                "errors": [{
                    "detail": detail,
                    "code": code,
                }]
            }),
            status,
            fabro_http::HeaderMap::new(),
        );
        progenitor_client::Error::ErrorResponse(response)
    }

    fn api_error(status: fabro_http::StatusCode, detail: &str, code: &str) -> ApiError {
        ApiError {
            status,
            headers: fabro_http::HeaderMap::new(),
            body: serde_json::to_string(&json!({
                "errors": [{
                    "detail": detail,
                    "code": code,
                }]
            }))
            .unwrap(),
            failure: ApiFailure {
                status,
                code: Some(code.to_string()),
            },
        }
    }

    #[test]
    fn map_api_error_marks_401_as_auth_required() {
        let err = map_api_error(error_response(
            fabro_http::StatusCode::UNAUTHORIZED,
            "Authentication required.",
            "authentication_required",
        ));
        assert_eq!(exit::exit_code_for(&err), 4);
    }

    #[test]
    fn map_api_error_keeps_500_as_exit_1() {
        let err = map_api_error(error_response(
            fabro_http::StatusCode::INTERNAL_SERVER_ERROR,
            "Server exploded.",
            "server_error",
        ));
        assert_eq!(exit::exit_code_for(&err), 1);
    }

    #[test]
    fn raw_response_failure_error_marks_401_as_auth_required() {
        let err = raw_response_failure_error(&api_error(
            fabro_http::StatusCode::UNAUTHORIZED,
            "Authentication required.",
            "authentication_required",
        ));
        assert_eq!(exit::exit_code_for(&err), 4);
    }

    #[test]
    fn raw_response_failure_error_keeps_403_as_exit_1() {
        let err = raw_response_failure_error(&api_error(
            fabro_http::StatusCode::FORBIDDEN,
            "Forbidden.",
            "forbidden",
        ));
        assert_eq!(exit::exit_code_for(&err), 1);
    }
}
