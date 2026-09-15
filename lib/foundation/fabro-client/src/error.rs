use anyhow::{Result, anyhow};
use fabro_util::exit::{ErrorExt, ExitClass};
use fabro_util::version::FABRO_VERSION;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFailure {
    pub status: fabro_http::StatusCode,
    pub code:   Option<String>,
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

fn build_structured_error(
    error: anyhow::Error,
    status: fabro_http::StatusCode,
    code: Option<String>,
) -> StructuredApiError {
    let failure = ApiFailure { status, code };
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
            let mut code = None;
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                let (detail, parsed_code) = parse_error_response_value(&value);
                code = parsed_code;
                if let Some(detail) = detail {
                    return build_structured_error(anyhow!("{detail}"), status, code);
                }
            }
            let error = if body.is_empty() {
                anyhow!("request failed with status {status}")
            } else {
                anyhow!("request failed with status {status}: {body}")
            };
            build_structured_error(error, status, code)
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
            let mut code = None;
            if let Ok(value) = serde_json::to_value(response.into_inner()) {
                let (detail, parsed_code) = parse_error_response_value(&value);
                code = parsed_code;
                if let Some(detail) = detail {
                    return build_structured_error(anyhow!("{detail}"), status, code);
                }
            }
            build_structured_error(anyhow!("request failed with status {status}"), status, code)
        }
        progenitor_client::Error::UnexpectedResponse(response) => {
            let status = response.status();
            build_structured_error(anyhow!("request failed with status {status}"), status, None)
        }
        progenitor_client::Error::InvalidResponsePayload(body, source) => StructuredApiError {
            error:   schema_mismatch_error(&body, &source),
            failure: None,
        },
        other => StructuredApiError {
            error:   anyhow::Error::new(other),
            failure: None,
        },
    }
}

/// Maximum number of body characters echoed back when a response fails to
/// deserialize. Enough to recognize the payload, short enough that the
/// remediation hint is not scrolled off the terminal.
const SCHEMA_MISMATCH_BODY_PREVIEW: usize = 200;

/// The server answered successfully but its payload does not fit the schema
/// this CLI was generated against. In practice that nearly always means the
/// two are on different versions, so lead with that rather than dumping the
/// whole body and leaving the reader to guess.
fn schema_mismatch_error(body: &[u8], source: &serde_json::Error) -> anyhow::Error {
    let text = String::from_utf8_lossy(body);
    let preview: String = text.chars().take(SCHEMA_MISMATCH_BODY_PREVIEW).collect();
    let ellipsis = if text.chars().nth(SCHEMA_MISMATCH_BODY_PREVIEW).is_some() {
        "..."
    } else {
        ""
    };

    anyhow!(
        "server response did not match the schema this CLI expects: {source}\n\n\
         This usually means the CLI and the server are running different versions. \
         This CLI is {FABRO_VERSION}; run `fabro version` to compare it with the server, \
         then upgrade the older side with `fabro upgrade` (add `--prerelease` if the \
         server runs a nightly build).\n\n\
         Response body started with: {preview}{ellipsis}"
    )
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
        ApiError, ApiFailure, FABRO_VERSION, api_failure_for, classify_api_error, map_api_error,
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

    // A payload the server sent happily but the generated client cannot fit
    // into the type it was built against, which is what a version skew looks
    // like from the CLI's side.
    fn invalid_payload_error(body: &str) -> progenitor_client::Error<serde_json::Value> {
        let source = serde_json::from_str::<u32>(body)
            .expect_err("an object should not deserialize into the expected type");
        progenitor_client::Error::InvalidResponsePayload(
            bytes::Bytes::copy_from_slice(body.as_bytes()),
            source,
        )
    }

    #[test]
    fn map_api_error_explains_schema_mismatch_as_a_version_skew() {
        let err = map_api_error(invalid_payload_error(r#"{"title":"some run"}"#));
        let rendered = format!("{err}");

        assert!(
            rendered.contains("did not match the schema this CLI expects"),
            "{rendered}"
        );
        assert!(rendered.contains("fabro version"), "{rendered}");
        assert!(rendered.contains("fabro upgrade"), "{rendered}");
        assert!(rendered.contains("--prerelease"), "{rendered}");
        assert!(rendered.contains(FABRO_VERSION), "{rendered}");
    }

    #[test]
    fn schema_mismatch_truncates_the_response_body() {
        let body = format!(r#"{{"title":"{}"}}"#, "x".repeat(5_000));
        let err = map_api_error(invalid_payload_error(&body));
        let rendered = format!("{err}");

        assert!(rendered.contains("Response body started with:"), "{rendered}");
        assert!(rendered.contains("..."), "{rendered}");
        assert!(
            rendered.len() < 1_000,
            "message should stay readable, got {} chars:\n{rendered}",
            rendered.len()
        );
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
