use anyhow::{Result, anyhow};
use fabro_util::exit::{ErrorExt, ExitClass};
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFailure {
    pub status: fabro_http::StatusCode,
    pub code:   Option<String>,
    /// The error entry's `meta`: structured details specific to `code`.
    pub meta:   Option<serde_json::Value>,
}

impl ApiFailure {
    #[must_use]
    pub fn new(status: fabro_http::StatusCode, code: Option<String>) -> Self {
        Self {
            status,
            code,
            meta: None,
        }
    }
}

/// The first entry of an `ErrorResponse` body, as far as a client acts on
/// it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedErrorEntry {
    pub detail: Option<String>,
    pub code:   Option<String>,
    pub meta:   Option<serde_json::Value>,
}

// Transparent wrapper that attaches an ApiFailure to an anyhow error while
// preserving Display/Debug of the inner error. Discoverable via downcast_ref
// so callers can branch on HTTP status without substring matching.
struct TaggedFailure {
    failure: ApiFailure,
    inner:   anyhow::Error,
}

impl std::fmt::Debug for TaggedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner, f)
    }
}

impl std::fmt::Display for TaggedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl std::error::Error for TaggedFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

pub fn tag_with_failure(err: anyhow::Error, failure: ApiFailure) -> anyhow::Error {
    anyhow::Error::new(TaggedFailure {
        failure,
        inner: err,
    })
}

pub fn api_failure_for(err: &anyhow::Error) -> Option<&ApiFailure> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<TaggedFailure>())
        .map(|tagged| &tagged.failure)
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
    let entry = parse_error_response_entry(value);
    (entry.detail, entry.code)
}

/// The first error entry of an `ErrorResponse` body: its detail, code and
/// `meta`, each absent when the body does not carry it.
pub fn parse_error_response_entry(value: &serde_json::Value) -> ParsedErrorEntry {
    let first = value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .and_then(|errors| errors.first());
    let text = |field: &str| {
        first
            .and_then(|entry| entry.get(field))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    };
    ParsedErrorEntry {
        detail: text("detail"),
        code:   text("code"),
        meta:   first
            .and_then(|entry| entry.get("meta"))
            .filter(|meta| meta.is_object())
            .cloned(),
    }
}

fn classify_from_status(err: anyhow::Error, status: fabro_http::StatusCode) -> anyhow::Error {
    if status == fabro_http::StatusCode::UNAUTHORIZED {
        err.classify(ExitClass::AuthRequired)
    } else {
        err
    }
}

fn build_structured_error(
    error: anyhow::Error,
    status: fabro_http::StatusCode,
    entry: ParsedErrorEntry,
) -> StructuredApiError {
    let failure = ApiFailure {
        status,
        code: entry.code,
        meta: entry.meta,
    };
    let tagged = tag_with_failure(error, failure.clone());
    StructuredApiError {
        error:   classify_from_status(tagged, status),
        failure: Some(failure),
    }
}

pub async fn classify_api_error<E>(err: progenitor_client::Error<E>) -> StructuredApiError
where
    E: serde::Serialize + std::fmt::Debug + Send + Sync + 'static,
{
    match err {
        progenitor_client::Error::UnexpectedResponse(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let mut entry = ParsedErrorEntry::default();
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                entry = parse_error_response_entry(&value);
                if let Some(detail) = entry.detail.take() {
                    return build_structured_error(anyhow!("{detail}"), status, entry);
                }
            }
            let error = if body.is_empty() {
                anyhow!("request failed with status {status}")
            } else {
                anyhow!("request failed with status {status}: {body}")
            };
            build_structured_error(error, status, entry)
        }
        other => map_api_error_structured(other),
    }
}

fn map_api_error_structured<E>(err: progenitor_client::Error<E>) -> StructuredApiError
where
    E: serde::Serialize + std::fmt::Debug + Send + Sync + 'static,
{
    match err {
        progenitor_client::Error::ErrorResponse(response) => {
            let status = response.status();
            let mut entry = ParsedErrorEntry::default();
            if let Ok(value) = serde_json::to_value(response.into_inner()) {
                entry = parse_error_response_entry(&value);
                if let Some(detail) = entry.detail.take() {
                    return build_structured_error(anyhow!("{detail}"), status, entry);
                }
            }
            build_structured_error(
                anyhow!("request failed with status {status}"),
                status,
                entry,
            )
        }
        progenitor_client::Error::UnexpectedResponse(response) => {
            let status = response.status();
            build_structured_error(
                anyhow!("request failed with status {status}"),
                status,
                ParsedErrorEntry::default(),
            )
        }
        other => StructuredApiError {
            error:   anyhow::Error::new(other),
            failure: None,
        },
    }
}

pub fn map_api_error<E>(err: progenitor_client::Error<E>) -> anyhow::Error
where
    E: serde::Serialize + std::fmt::Debug + Send + Sync + 'static,
{
    map_api_error_structured(err).error
}

pub fn raw_response_failure_error(failure: &ApiError) -> anyhow::Error {
    let base = if let Ok(value) = serde_json::from_str::<serde_json::Value>(&failure.body) {
        let (detail, _) = parse_error_response_value(&value);
        if let Some(detail) = detail {
            anyhow!("{detail}")
        } else if failure.body.is_empty() {
            anyhow!("request failed with status {}", failure.status)
        } else {
            anyhow!(
                "request failed with status {}: {}",
                failure.status,
                failure.body
            )
        }
    } else if failure.body.is_empty() {
        anyhow!("request failed with status {}", failure.status)
    } else {
        anyhow!(
            "request failed with status {}: {}",
            failure.status,
            failure.body
        )
    };
    classify_from_status(
        tag_with_failure(base, failure.failure.clone()),
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
    let entry = serde_json::from_str::<serde_json::Value>(&body)
        .map(|value| parse_error_response_entry(&value))
        .unwrap_or_default();

    Ok(Err(ApiError {
        status,
        headers,
        body,
        failure: ApiFailure {
            status,
            code: entry.code,
            meta: entry.meta,
        },
    }))
}

pub fn is_not_found_error(err: &anyhow::Error) -> bool {
    api_failure_for(err).is_some_and(|failure| failure.status == fabro_http::StatusCode::NOT_FOUND)
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
    use httpmock::MockServer;
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::{
        ApiError, ApiFailure, api_failure_for, classify_api_error, map_api_error,
        raw_response_failure_error,
    };

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
            failure: ApiFailure::new(status, Some(code.to_string())),
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
    fn map_api_error_uses_structured_detail_and_preserves_code() {
        let err = map_api_error(error_response(
            fabro_http::StatusCode::UNPROCESSABLE_ENTITY,
            "missing field `dirty` at line 1 column 2834",
            "invalid_manifest",
        ));

        assert_eq!(
            err.to_string(),
            "missing field `dirty` at line 1 column 2834"
        );
        let failure = api_failure_for(&err).expect("error should carry API failure metadata");
        assert_eq!(failure.status, fabro_http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(failure.code.as_deref(), Some("invalid_manifest"));
    }

    #[test]
    fn map_api_error_carries_the_entry_meta() {
        let response = progenitor_client::ResponseValue::new(
            json!({
                "errors": [{
                    "detail": "run is leased",
                    "code": "petri_run_leased",
                    "meta": { "owner": "worker-1" },
                }]
            }),
            fabro_http::StatusCode::CONFLICT,
            fabro_http::HeaderMap::new(),
        );
        let err =
            map_api_error(progenitor_client::Error::<serde_json::Value>::ErrorResponse(response));

        let failure = api_failure_for(&err).expect("error should carry API failure metadata");
        assert_eq!(failure.code.as_deref(), Some("petri_run_leased"));
        assert_eq!(failure.meta, Some(json!({ "owner": "worker-1" })));
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

    #[tokio::test]
    async fn classify_api_error_preserves_plain_text_unexpected_response_body() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/plain-error");
            then.status(422)
                .header("Content-Type", "text/plain")
                .body("Failed to deserialize request: missing field `dirty` at line 1 column 2834");
        });

        let response = fabro_http::test_http_client()
            .unwrap()
            .get(format!("{}/plain-error", server.base_url()))
            .send()
            .await
            .expect("mock server should return a response");
        let err = classify_api_error(
            progenitor_client::Error::<serde_json::Value>::UnexpectedResponse(response),
        )
        .await
        .error;

        let message = err.to_string();
        assert!(
            message.contains("request failed with status 422 Unprocessable Entity"),
            "expected status in message, got: {message}"
        );
        assert!(
            message.contains("missing field `dirty`"),
            "expected response body in message, got: {message}"
        );
        let failure = api_failure_for(&err).expect("error should carry API failure metadata");
        assert_eq!(failure.status, fabro_http::StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn map_api_error_preserves_communication_error_source_chain() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/v1/runs", listener.local_addr().unwrap());
        drop(listener);

        let client = fabro_http::test_http_client().unwrap();
        let reqwest_error = client
            .get(url)
            .send()
            .await
            .expect_err("closed local port should refuse connection");

        let err = map_api_error(
            progenitor_client::Error::<serde_json::Value>::CommunicationError(reqwest_error),
        );
        let chain = err
            .chain()
            .map(ToString::to_string)
            .collect::<Vec<String>>();

        assert!(
            chain.len() >= 2,
            "expected communication error source chain, got {chain:#?}"
        );
        assert!(
            chain
                .iter()
                .any(|cause| cause.contains("error sending request")),
            "expected reqwest error in chain, got {chain:#?}"
        );
    }
}
