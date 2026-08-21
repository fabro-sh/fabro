//! Provider error normalization for the Chat Completions wire dialect.

use crate::codec;
use crate::error::{self as llm_error, Error, ProviderErrorDetail, ProviderErrorKind};

const ROUTING_EXHAUSTED_MESSAGE: &str = "Upstream provider routing was exhausted";

/// Structured fields used to classify an OpenAI-compatible provider error.
///
/// OpenRouter's current contract exposes `metadata.error_type`. Older
/// responses can instead expose a list of failed provider attempts. Keep that
/// compatibility input private to the wire boundary so retry code only sees
/// Fabro's normal provider error taxonomy.
struct ParsedProviderError {
    message:           String,
    canonical_code:    Option<String>,
    native_code:       Option<String>,
    provider_code:     Option<String>,
    status_code:       Option<u16>,
    previous_attempts: Option<Vec<u16>>,
    raw:               serde_json::Value,
}

impl ParsedProviderError {
    fn from_raw(raw: serde_json::Value, status_code: Option<u16>) -> Self {
        let error = &raw["error"];
        let metadata = error.get("metadata");

        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .filter(|message| !message.is_empty())
            .unwrap_or("Provider returned error")
            .to_string();
        let canonical_code = metadata
            .and_then(|metadata| metadata.get("error_type"))
            .and_then(nonempty_string);
        let native_code = error.get("type").and_then(nonempty_string).or_else(|| {
            error
                .get("code")
                .filter(|code| code.is_string())
                .and_then(nonempty_string)
        });
        let provider_code = metadata
            .and_then(|metadata| metadata.get("provider_code"))
            .and_then(scalar_string);
        let status_code = status_code.or_else(|| error.get("code").and_then(status_from_value));
        let previous_attempts = metadata
            .and_then(|metadata| metadata.get("previous_errors"))
            .and_then(previous_attempt_statuses);

        Self {
            message,
            canonical_code,
            native_code,
            provider_code,
            status_code,
            previous_attempts,
            raw,
        }
    }

    fn error_code(&self) -> Option<String> {
        self.canonical_code
            .clone()
            .or_else(|| self.native_code.clone())
            .or_else(|| self.provider_code.clone())
    }

    fn routing_was_exhausted(&self) -> bool {
        self.status_code == Some(400)
            && self.canonical_code.is_none()
            && self.previous_attempts.as_ref().is_some_and(|attempts| {
                !attempts.is_empty() && attempts.iter().copied().all(is_transient_status_code)
            })
    }

    fn into_error(self, provider: &str, retry_after: Option<f64>) -> Error {
        if let Some(kind) = self
            .canonical_code
            .as_deref()
            .and_then(kind_from_canonical_code)
        {
            return self.provider_error(kind, provider, retry_after);
        }

        // Do not let legacy routing metadata override a status with a clear
        // meaning of its own.
        if self.status_code.is_some_and(is_unambiguous_status_code) {
            return self.status_error(provider, retry_after);
        }

        if self.routing_was_exhausted() {
            return self.provider_error_with_message(
                ProviderErrorKind::Server,
                ROUTING_EXHAUSTED_MESSAGE,
                provider,
                retry_after,
            );
        }

        if self.status_code.is_some() {
            self.status_error(provider, retry_after)
        } else {
            // An in-band error with no status or canonical code happened after
            // the HTTP stream opened. Treat that unknown provider failure as
            // transient, while retaining all provider detail in `raw`.
            self.provider_error(ProviderErrorKind::Server, provider, retry_after)
        }
    }

    fn status_error(self, provider: &str, retry_after: Option<f64>) -> Error {
        let Some(status_code) = self.status_code else {
            return self.provider_error(ProviderErrorKind::Server, provider, retry_after);
        };
        let error_code = self.error_code();
        llm_error::error_from_status_code(
            status_code,
            self.message,
            provider.to_string(),
            error_code,
            Some(self.raw),
            retry_after,
        )
    }

    fn provider_error(
        self,
        kind: ProviderErrorKind,
        provider: &str,
        retry_after: Option<f64>,
    ) -> Error {
        self.build_provider_error(kind, None, provider, retry_after)
    }

    fn provider_error_with_message(
        self,
        kind: ProviderErrorKind,
        message: &str,
        provider: &str,
        retry_after: Option<f64>,
    ) -> Error {
        self.build_provider_error(kind, Some(message.to_string()), provider, retry_after)
    }

    fn build_provider_error(
        self,
        kind: ProviderErrorKind,
        message: Option<String>,
        provider: &str,
        retry_after: Option<f64>,
    ) -> Error {
        let error_code = self.error_code();
        Error::Provider {
            kind,
            detail: Box::new(ProviderErrorDetail {
                message: message.unwrap_or(self.message),
                provider: provider.to_string(),
                status_code: self.status_code,
                error_code,
                retry_after,
                raw: Some(self.raw),
            }),
        }
    }
}

pub(super) fn decode_http(
    status: u16,
    body: &str,
    provider: &str,
    retry_after: Option<f64>,
) -> Error {
    let (message, code, raw) = codec::parse_error_body(body, "type");
    let Some(raw) = raw else {
        return llm_error::error_from_status_code(
            status,
            message,
            provider.to_string(),
            code,
            None,
            retry_after,
        );
    };
    if raw.get("error").is_none() {
        return llm_error::error_from_status_code(
            status,
            message,
            provider.to_string(),
            code,
            Some(raw),
            retry_after,
        );
    }
    let parsed = ParsedProviderError::from_raw(raw, Some(status));

    parsed.into_error(provider, retry_after)
}

pub(super) fn decode_stream(raw: serde_json::Value, provider: &str) -> Error {
    ParsedProviderError::from_raw(raw, None).into_error(provider, None)
}

fn kind_from_canonical_code(code: &str) -> Option<ProviderErrorKind> {
    let kind = match code {
        "authentication" => ProviderErrorKind::Authentication,
        "permission_denied" => ProviderErrorKind::AccessDenied,
        "not_found" => ProviderErrorKind::NotFound,
        "payment_required" => ProviderErrorKind::QuotaExceeded,
        "rate_limit_exceeded" => ProviderErrorKind::RateLimit,
        "provider_overloaded" | "provider_unavailable" | "server" | "timeout" | "unmapped" => {
            ProviderErrorKind::Server
        }
        "context_length_exceeded"
        | "max_tokens_exceeded"
        | "token_limit_exceeded"
        | "string_too_long" => ProviderErrorKind::ContextLength,
        "content_policy_violation" | "refusal" => ProviderErrorKind::ContentFilter,
        "invalid_request"
        | "invalid_prompt"
        | "precondition_failed"
        | "payload_too_large"
        | "unprocessable"
        | "invalid_image"
        | "image_too_large"
        | "image_too_small"
        | "unsupported_image_format"
        | "image_not_found"
        | "image_download_failed" => ProviderErrorKind::InvalidRequest,
        _ => return None,
    };
    Some(kind)
}

fn previous_attempt_statuses(value: &serde_json::Value) -> Option<Vec<u16>> {
    let attempts = value.as_array()?;
    if attempts.is_empty() {
        return None;
    }
    attempts
        .iter()
        .map(|attempt| attempt.get("code").and_then(status_from_value))
        .collect()
}

fn status_from_value(value: &serde_json::Value) -> Option<u16> {
    u16::try_from(value.as_u64()?).ok()
}

fn nonempty_string(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn scalar_string(value: &serde_json::Value) -> Option<String> {
    nonempty_string(value).or_else(|| {
        value
            .as_i64()
            .map(|value| value.to_string())
            .or_else(|| value.as_u64().map(|value| value.to_string()))
    })
}

const fn is_unambiguous_status_code(status: u16) -> bool {
    matches!(status, 401 | 403 | 404 | 408 | 413 | 429 | 500..=599)
}

const fn is_transient_status_code(status: u16) -> bool {
    matches!(status, 408 | 429 | 500..=599)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_transient_routes_are_retryable() {
        let body = r#"{
            "error": {
                "message": "Provider returned error",
                "code": 400,
                "metadata": {
                    "raw": "{\"error\":{\"message\":\"Failed to start generation: no model registered\",\"type\":\"invalid_request_error\"}}",
                    "provider_name": "Together",
                    "is_byok": false,
                    "previous_errors": [
                        {"code": 429, "provider_name": "Fireworks"},
                        {"code": 429, "provider_name": "DigitalOcean"},
                        {"code": 429, "provider_name": "BaseTen"}
                    ]
                }
            },
            "user_id": "org_test"
        }"#;

        let error = decode_http(400, body, "openrouter", None);
        assert!(error.retryable());
        assert!(error.failover_eligible());
        match error {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::Server);
                assert_eq!(detail.message, ROUTING_EXHAUSTED_MESSAGE);
                assert_eq!(detail.provider, "openrouter");
                assert_eq!(detail.status_code, Some(400));
                assert_eq!(detail.error_code, None);
                assert_eq!(detail.retry_after, None);
                assert_eq!(detail.raw, serde_json::from_str(body).ok());
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn canonical_provider_unavailable_overrides_ambiguous_status() {
        let body = r#"{
            "error": {
                "code": 400,
                "message": "Provider returned an invalid response",
                "metadata": {
                    "error_type": "provider_unavailable",
                    "provider_code": "no_endpoint"
                }
            }
        }"#;

        let error = decode_http(400, body, "openrouter", None);
        assert!(error.retryable());
        match error {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::Server);
                assert_eq!(detail.error_code.as_deref(), Some("provider_unavailable"));
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn canonical_rate_limit_preserves_provider_detail() {
        let body = r#"{
            "error": {
                "code": 400,
                "message": "Rate limit exceeded",
                "metadata": {"error_type": "rate_limit_exceeded"}
            }
        }"#;

        let error = decode_http(400, body, "openrouter", Some(2.5));
        assert!(error.retryable());
        match error {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::RateLimit);
                assert_eq!(detail.provider, "openrouter");
                assert_eq!(detail.status_code, Some(400));
                assert_eq!(detail.retry_after, Some(2.5));
                assert_eq!(detail.error_code.as_deref(), Some("rate_limit_exceeded"));
                assert_eq!(detail.raw, serde_json::from_str(body).ok());
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn plain_bad_request_remains_non_retryable() {
        let body = r#"{"error":{"message":"Bad request","type":"invalid_request_error"}}"#;

        let error = decode_http(400, body, "openrouter", None);
        assert!(!error.retryable());
        assert!(matches!(error, Error::Provider {
            kind: ProviderErrorKind::InvalidRequest,
            ..
        }));
    }

    #[test]
    fn unambiguous_status_precedes_legacy_route_history() {
        let body = serde_json::json!({
            "error": {
                "message": "Invalid API key",
                "code": 401,
                "metadata": {
                    "previous_errors": [{"code": 429}, {"code": 503}]
                }
            }
        })
        .to_string();

        let error = decode_http(401, &body, "openrouter", None);
        assert!(!error.retryable());
        assert!(matches!(error, Error::Provider {
            kind: ProviderErrorKind::Authentication,
            ..
        }));
    }

    #[test]
    fn malformed_or_deterministic_previous_attempts_do_not_change_bad_request() {
        for previous_errors in [
            serde_json::json!([{"code": 429}, {"message": "missing code"}]),
            serde_json::json!([{"code": 429}, {"code": 400}]),
        ] {
            let body = serde_json::json!({
                "error": {
                    "message": "Bad request",
                    "code": 400,
                    "metadata": {"previous_errors": previous_errors}
                }
            })
            .to_string();

            let error = decode_http(400, &body, "openrouter", None);
            assert!(!error.retryable());
            assert!(matches!(error, Error::Provider {
                kind: ProviderErrorKind::InvalidRequest,
                ..
            }));
        }
    }
}
