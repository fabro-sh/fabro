#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    Authentication,
    AccessDenied,
    NotFound,
    InvalidRequest,
    RateLimit,
    Server,
    ContentFilter,
    ContextLength,
    QuotaExceeded,
}

impl std::fmt::Display for ProviderErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authentication => write!(f, "Authentication error for"),
            Self::AccessDenied => write!(f, "Access denied by"),
            Self::NotFound => write!(f, "Not found on"),
            Self::InvalidRequest => write!(f, "Invalid request to"),
            Self::RateLimit => write!(f, "Rate limited by"),
            Self::Server => write!(f, "Server error from"),
            Self::ContentFilter => write!(f, "Content filtered by"),
            Self::ContextLength => write!(f, "Context length exceeded for"),
            Self::QuotaExceeded => write!(f, "Quota exceeded for"),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderErrorDetail {
    pub message:     String,
    pub provider:    String,
    pub status_code: Option<u16>,
    pub error_code:  Option<String>,
    pub retry_after: Option<f64>,
    pub raw:         Option<serde_json::Value>,
}

impl ProviderErrorDetail {
    pub fn new(message: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            message:     message.into(),
            provider:    provider.into(),
            status_code: None,
            error_code:  None,
            retry_after: None,
            raw:         None,
        }
    }
}

use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Error {
    #[error("{kind} {}: {}", .detail.provider, .detail.message)]
    Provider {
        kind:   ProviderErrorKind,
        detail: Box<ProviderErrorDetail>,
    },

    #[error("Request timed out: {message}")]
    RequestTimeout {
        message: String,
        #[source]
        #[serde(skip)]
        source:  Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Request interrupted: {message}")]
    Interrupt { message: String },

    #[error("Network error: {message}")]
    Network {
        message: String,
        #[source]
        #[serde(skip)]
        source:  Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Stream error: {message}")]
    Stream {
        message: String,
        #[source]
        #[serde(skip)]
        source:  Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Invalid tool call: {message}")]
    InvalidToolCall { message: String },

    #[error("No object generated: {message}")]
    NoObjectGenerated { message: String },

    #[error("Invalid request: {message}")]
    InvalidRequest { message: String },

    #[error("Configuration error: {message}")]
    Configuration {
        message: String,
        #[source]
        #[serde(skip)]
        source:  Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Unsupported tool choice: {message}")]
    UnsupportedToolChoice { message: String },
}

impl Error {
    pub fn network(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Network {
            message: message.into(),
            source:  Some(Arc::new(source)),
        }
    }

    pub fn request_timeout(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::RequestTimeout {
            message: message.into(),
            source:  Some(Arc::new(source)),
        }
    }

    pub fn stream_error(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Stream {
            message: message.into(),
            source:  Some(Arc::new(source)),
        }
    }

    pub fn configuration_error(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Configuration {
            message: message.into(),
            source:  Some(Arc::new(source)),
        }
    }

    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Provider { kind, .. } => !matches!(
                kind,
                ProviderErrorKind::Authentication
                    | ProviderErrorKind::AccessDenied
                    | ProviderErrorKind::NotFound
                    | ProviderErrorKind::InvalidRequest
                    | ProviderErrorKind::ContextLength
                    | ProviderErrorKind::QuotaExceeded
                    | ProviderErrorKind::ContentFilter
            ),
            Self::InvalidToolCall { .. }
            | Self::NoObjectGenerated { .. }
            | Self::Interrupt { .. }
            | Self::InvalidRequest { .. }
            | Self::Configuration { .. }
            | Self::UnsupportedToolChoice { .. }
            | Self::RequestTimeout { .. } => false,
            _ => true,
        }
    }

    #[must_use]
    pub const fn retry_after(&self) -> Option<f64> {
        match self {
            Self::Provider { detail, .. } => detail.retry_after,
            _ => None,
        }
    }

    #[must_use]
    pub const fn status_code(&self) -> Option<u16> {
        match self {
            Self::Provider { detail, .. } => detail.status_code,
            _ => None,
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> Option<ProviderErrorKind> {
        match self {
            Self::Provider { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    #[must_use]
    pub fn provider_name(&self) -> &str {
        match self {
            Self::Provider { detail, .. } => &detail.provider,
            _ => "unknown",
        }
    }

    /// Whether this error is eligible for provider-level failover.
    ///
    /// Includes everything that is `retryable()` (transient errors good for
    /// same-provider retry), provider-local availability failures, and
    /// `QuotaExceeded`. A different provider has independent credentials,
    /// access policy, model inventory, and quota.
    #[must_use]
    pub fn failover_eligible(&self) -> bool {
        if self.retryable() {
            return true;
        }
        matches!(
            self,
            Self::Provider {
                kind: ProviderErrorKind::Authentication
                    | ProviderErrorKind::AccessDenied
                    | ProviderErrorKind::NotFound
                    | ProviderErrorKind::QuotaExceeded,
                ..
            } | Self::RequestTimeout { .. }
        ) || self.refusal_content_filter()
    }

    fn refusal_content_filter(&self) -> bool {
        matches!(
            self,
            Self::Provider {
                kind: ProviderErrorKind::ContentFilter,
                detail,
            } if detail.error_code.as_deref() == Some("refusal")
        )
    }

    #[must_use]
    pub fn failure_signature_hint(&self) -> String {
        let provider = self.provider_name();
        match self {
            Self::Provider { kind, .. } => {
                let category = if self.retryable() {
                    "api_transient"
                } else {
                    "api_deterministic"
                };
                let detail = match kind {
                    ProviderErrorKind::RateLimit => "rate_limited",
                    ProviderErrorKind::Server => "server_error",
                    ProviderErrorKind::ContextLength => "context_length",
                    ProviderErrorKind::QuotaExceeded => "quota_exceeded",
                    ProviderErrorKind::Authentication => "authentication",
                    ProviderErrorKind::AccessDenied => "access_denied",
                    ProviderErrorKind::NotFound => "not_found",
                    ProviderErrorKind::InvalidRequest => "invalid_request",
                    ProviderErrorKind::ContentFilter => "content_filter",
                };
                format!("{category}|{provider}|{detail}")
            }
            Self::RequestTimeout { .. } => format!("api_transient|{provider}|timeout"),
            Self::Network { .. } => format!("api_transient|{provider}|network"),
            Self::Stream { .. } => format!("api_transient|{provider}|stream"),
            Self::Interrupt { .. } => format!("api_canceled|{provider}|interrupt"),
            Self::Configuration { .. } => format!("api_deterministic|{provider}|configuration"),
            Self::InvalidToolCall { .. } => {
                format!("api_deterministic|{provider}|invalid_tool_call")
            }
            Self::NoObjectGenerated { .. } => {
                format!("api_deterministic|{provider}|no_object")
            }
            Self::InvalidRequest { .. } => {
                format!("api_deterministic|{provider}|invalid_request")
            }
            Self::UnsupportedToolChoice { .. } => {
                format!("api_deterministic|{provider}|unsupported_tool_choice")
            }
        }
    }
}

/// Provider error code to error kind mapping, for the codes that say more
/// than the transport-level status or stream event type does.
///
/// Returns `None` when the code adds nothing, so each caller keeps its own
/// default: the stream decoders treat an unrecognized code as transient,
/// while [`error_from_status_code`] falls back to the HTTP status.
///
/// Every dialect classifies through this one table so a code such as
/// `insufficient_quota` means the same thing whether it arrives in an HTTP
/// error body or in a mid-stream error event.
#[must_use]
pub(crate) fn kind_from_error_code(code: &str) -> Option<ProviderErrorKind> {
    Some(match code {
        // Out of credit, or over a billing cap. Distinct from RateLimit:
        // backoff never clears it, but another provider has its own quota.
        "insufficient_quota" | "billing_hard_limit_reached" | "exceeded_current_quota_error" => {
            ProviderErrorKind::QuotaExceeded
        }
        "rate_limit_error" | "rate_limit_exceeded" | "too_many_requests" => {
            ProviderErrorKind::RateLimit
        }
        "authentication_error" | "invalid_api_key" | "invalid_authentication" => {
            ProviderErrorKind::Authentication
        }
        "access_denied" | "account_deactivated" | "permission_denied" | "permission_error" => {
            ProviderErrorKind::AccessDenied
        }
        "content_filter" | "content_policy_violation" => ProviderErrorKind::ContentFilter,
        // `request_too_large` is anthropic's oversized-input code, so it has
        // to precede the `_too_large` suffix rule below.
        "context_length_exceeded" | "request_too_large" => ProviderErrorKind::ContextLength,
        "server_error" | "internal_error" | "service_unavailable" | "engine_overloaded" => {
            ProviderErrorKind::Server
        }
        c if c == "not_found_error" || c.ends_with("_not_found") => ProviderErrorKind::NotFound,
        c if c.starts_with("invalid_")
            || c.starts_with("unsupported_")
            || c.ends_with("_too_large")
            || c.ends_with("_too_long") =>
        {
            ProviderErrorKind::InvalidRequest
        }
        _ => return None,
    })
}

/// HTTP status code to error type mapping (Section 6.4).
#[must_use]
pub fn error_from_status_code(
    status_code: u16,
    message: String,
    provider: String,
    error_code: Option<String>,
    raw: Option<serde_json::Value>,
    retry_after: Option<f64>,
) -> Error {
    let detail = ProviderErrorDetail {
        message,
        provider,
        status_code: Some(status_code),
        error_code,
        retry_after,
        raw,
    };

    let code_kind = detail.error_code.as_deref().and_then(kind_from_error_code);

    // Check specific status codes first -- these always map to their designated
    // error types
    let kind = match status_code {
        401 => ProviderErrorKind::Authentication,
        // A 412 is never about the request: no LLM request carries
        // conditional-request preconditions. Fireworks documents it as
        // "Account is suspended or there's an issue with account status",
        // also emitted for a LoRA model that failed to load
        // (https://docs.fireworks.ai/guides/inference-error-codes). The same
        // family as `account_deactivated`: deterministic here, but another
        // provider has independent billing and model inventory.
        403 | 412 => ProviderErrorKind::AccessDenied,
        404 => ProviderErrorKind::NotFound,
        408 => {
            return Error::RequestTimeout {
                message: detail.message,
                source:  None,
            };
        }
        413 => ProviderErrorKind::ContextLength,
        // A 429 means rate limited unless the body reports a spent quota,
        // which retrying will never clear.
        429 if code_kind == Some(ProviderErrorKind::QuotaExceeded) => {
            ProviderErrorKind::QuotaExceeded
        }
        429 => ProviderErrorKind::RateLimit,
        500..=599 => ProviderErrorKind::Server,
        // For ambiguous status codes (400, 422, etc.), the provider's error
        // code is the better signal; fall back to the message only without one
        _ => code_kind.unwrap_or_else(|| {
            let lower_msg = detail.message.to_lowercase();
            if lower_msg.contains("not found") || lower_msg.contains("does not exist") {
                ProviderErrorKind::NotFound
            } else if lower_msg.contains("unauthorized") || lower_msg.contains("invalid key") {
                ProviderErrorKind::Authentication
            } else if lower_msg.contains("context length") || lower_msg.contains("too many tokens")
            {
                ProviderErrorKind::ContextLength
            } else if lower_msg.contains("content filter") || lower_msg.contains("safety") {
                ProviderErrorKind::ContentFilter
            } else {
                ProviderErrorKind::InvalidRequest
            }
        }),
    };

    Error::Provider {
        kind,
        detail: Box::new(detail),
    }
}

/// gRPC status code to error type mapping (Section 6.4, for Gemini).
#[must_use]
pub fn error_from_grpc_status(
    grpc_code: &str,
    message: String,
    provider: String,
    error_code: Option<String>,
    raw: Option<serde_json::Value>,
    retry_after: Option<f64>,
) -> Error {
    let detail = ProviderErrorDetail {
        message,
        provider,
        status_code: None,
        error_code,
        retry_after,
        raw,
    };

    let kind = match grpc_code {
        "NOT_FOUND" => ProviderErrorKind::NotFound,
        "INVALID_ARGUMENT" => ProviderErrorKind::InvalidRequest,
        "UNAUTHENTICATED" => ProviderErrorKind::Authentication,
        "PERMISSION_DENIED" => ProviderErrorKind::AccessDenied,
        "RESOURCE_EXHAUSTED" => ProviderErrorKind::RateLimit,
        "DEADLINE_EXCEEDED" => {
            return Error::RequestTimeout {
                message: detail.message,
                source:  None,
            };
        }
        _ => ProviderErrorKind::Server,
    };

    Error::Provider {
        kind,
        detail: Box::new(detail),
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[test]
    fn retryable_classification() {
        let auth_err = Error::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(401),
                ..ProviderErrorDetail::new("bad key", "openai")
            }),
        };
        assert!(!auth_err.retryable());

        let rate_err = Error::Provider {
            kind:   ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(429),
                retry_after: Some(2.0),
                ..ProviderErrorDetail::new("too fast", "openai")
            }),
        };
        assert!(rate_err.retryable());
        assert_eq!(rate_err.retry_after(), Some(2.0));

        let server_err = Error::Provider {
            kind:   ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(500),
                ..ProviderErrorDetail::new("internal error", "anthropic")
            }),
        };
        assert!(server_err.retryable());

        let timeout = Error::RequestTimeout {
            message: "timed out".into(),
            source:  None,
        };
        assert!(!timeout.retryable());

        let network = Error::Network {
            message: "connection refused".into(),
            source:  None,
        };
        assert!(network.retryable());

        let config = Error::Configuration {
            message: "missing provider".into(),
            source:  None,
        };
        assert!(!config.retryable());
    }

    #[test]
    fn non_retryable_provider_errors() {
        let detail = || Box::new(ProviderErrorDetail::new("error", "openai"));

        let access_denied = Error::Provider {
            kind:   ProviderErrorKind::AccessDenied,
            detail: detail(),
        };
        assert!(!access_denied.retryable());

        let not_found = Error::Provider {
            kind:   ProviderErrorKind::NotFound,
            detail: detail(),
        };
        assert!(!not_found.retryable());

        let invalid_req = Error::Provider {
            kind:   ProviderErrorKind::InvalidRequest,
            detail: detail(),
        };
        assert!(!invalid_req.retryable());

        let ctx_length = Error::Provider {
            kind:   ProviderErrorKind::ContextLength,
            detail: detail(),
        };
        assert!(!ctx_length.retryable());

        let quota = Error::Provider {
            kind:   ProviderErrorKind::QuotaExceeded,
            detail: detail(),
        };
        assert!(!quota.retryable());

        let content_filter = Error::Provider {
            kind:   ProviderErrorKind::ContentFilter,
            detail: detail(),
        };
        assert!(!content_filter.retryable());
    }

    #[test]
    fn non_retryable_sdk_errors() {
        let invalid_tool = Error::InvalidToolCall {
            message: "bad tool".into(),
        };
        assert!(!invalid_tool.retryable());

        let no_object = Error::NoObjectGenerated {
            message: "no output".into(),
        };
        assert!(!no_object.retryable());

        let interrupt = Error::Interrupt {
            message: "interrupted".into(),
        };
        assert!(!interrupt.retryable());
    }

    #[test]
    fn error_from_status_code_mapping() {
        let err = error_from_status_code(
            401,
            "unauthorized".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Authentication,
            ..
        }));
        assert!(!err.retryable());

        let err =
            error_from_status_code(403, "forbidden".into(), "openai".into(), None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::AccessDenied,
            ..
        }));

        let err =
            error_from_status_code(404, "not found".into(), "openai".into(), None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::NotFound,
            ..
        }));

        let err =
            error_from_status_code(400, "bad request".into(), "openai".into(), None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::InvalidRequest,
            ..
        }));

        let err = error_from_status_code(
            422,
            "unprocessable".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::InvalidRequest,
            ..
        }));

        let err = error_from_status_code(408, "timeout".into(), "openai".into(), None, None, None);
        assert!(matches!(err, Error::RequestTimeout { .. }));

        let err =
            error_from_status_code(413, "too large".into(), "openai".into(), None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::ContextLength,
            ..
        }));

        let err = error_from_status_code(
            429,
            "rate limited".into(),
            "openai".into(),
            None,
            None,
            Some(5.0),
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::RateLimit,
            ..
        }));
        assert!(err.retryable());
        assert_eq!(err.retry_after(), Some(5.0));

        let err = error_from_status_code(500, "internal".into(), "openai".into(), None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Server,
            ..
        }));
        assert!(err.retryable());

        let err =
            error_from_status_code(502, "bad gateway".into(), "openai".into(), None, None, None);
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Server,
            ..
        }));

        let err = error_from_status_code(
            529,
            "Overloaded".into(),
            "anthropic".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Server,
            ..
        }));
        assert!(err.retryable());
    }

    /// Every vendor spelling of "you are out of credit" arrives as a 429 and
    /// has to classify as a spent quota, not as a rate limit.
    #[test]
    fn quota_codes_on_429_are_non_retryable_quota_failures() {
        for (provider, code) in [
            ("kimi", "exceeded_current_quota_error"),
            ("openai", "insufficient_quota"),
            ("openai", "billing_hard_limit_reached"),
        ] {
            let err = error_from_status_code(
                429,
                "Your account has insufficient balance".into(),
                provider.into(),
                Some(code.into()),
                None,
                None,
            );

            assert_eq!(
                err.provider_kind(),
                Some(ProviderErrorKind::QuotaExceeded),
                "{code}"
            );
            assert!(!err.retryable(), "{code}");
            assert!(err.failover_eligible(), "{code}");
        }
    }

    /// A 429 that is a genuine rate limit stays retryable, whether the body
    /// names it, names something unrecognized, or carries no code at all.
    #[test]
    fn non_quota_429_stays_a_retryable_rate_limit() {
        for code in [
            Some("rate_limit_error"),
            Some("rate_limit_reached_error"),
            Some("invalid_request_error"),
            None,
        ] {
            let err = error_from_status_code(
                429,
                "slow down".into(),
                "openai".into(),
                code.map(String::from),
                None,
                None,
            );

            assert_eq!(
                err.provider_kind(),
                Some(ProviderErrorKind::RateLimit),
                "{code:?}"
            );
            assert!(err.retryable(), "{code:?}");
        }
    }

    /// For a status with no fixed meaning, the structured code beats guessing
    /// from the message text.
    #[test]
    fn ambiguous_status_prefers_error_code_over_message() {
        let err = error_from_status_code(
            402,
            "Payment required".into(),
            "openai".into(),
            Some("insufficient_quota".into()),
            None,
            None,
        );
        assert_eq!(err.provider_kind(), Some(ProviderErrorKind::QuotaExceeded));
    }

    /// Fireworks reports an account suspension (spending cap reached or
    /// unpaid invoices) as HTTP 412 with `code: "PRECONDITION_FAILED"` in
    /// the body. A chat completion carries no conditional-request
    /// preconditions, so a 412 is always an account-level lockout, never a
    /// defect in the request: it must not classify as `InvalidRequest`, and
    /// a fallback provider with independent billing must stay eligible.
    #[test]
    fn account_suspension_412_is_failover_eligible() {
        let err = error_from_status_code(
            412,
            "Account lithoscomputer is suspended, possibly due to reaching \
             the monthly spending limit or failure to pay past invoices."
                .into(),
            "fireworks".into(),
            // The openai_compatible dialect reads `error.type` as the code,
            // so the discriminating `PRECONDITION_FAILED` only reaches this
            // mapping through the status code.
            Some("error".into()),
            Some(serde_json::json!({
                "error": {
                    "message": "Account lithoscomputer is suspended, possibly due to reaching the monthly spending limit or failure to pay past invoices. Please go to https://fireworks.ai/account/billing for more information.",
                    "param": null,
                    "code": "PRECONDITION_FAILED",
                    "type": "error"
                },
                "request_id": "chatcmpl-d9652b89a6604931ac27dddd5ef5bdc0"
            })),
            None,
        );

        assert_eq!(err.provider_kind(), Some(ProviderErrorKind::AccessDenied));
        assert!(!err.retryable());
        assert!(err.failover_eligible());

        // A bare 412 with no parseable body classifies the same way.
        let err = error_from_status_code(
            412,
            "Precondition Failed".into(),
            "fireworks".into(),
            None,
            None,
            None,
        );
        assert_eq!(err.provider_kind(), Some(ProviderErrorKind::AccessDenied));
        assert!(err.failover_eligible());
    }

    #[test]
    fn kind_from_error_code_covers_every_dialect() {
        for (code, expected) in [
            ("insufficient_quota", ProviderErrorKind::QuotaExceeded),
            ("rate_limit_error", ProviderErrorKind::RateLimit),
            ("authentication_error", ProviderErrorKind::Authentication),
            ("permission_error", ProviderErrorKind::AccessDenied),
            ("content_policy_violation", ProviderErrorKind::ContentFilter),
            ("context_length_exceeded", ProviderErrorKind::ContextLength),
            ("engine_overloaded", ProviderErrorKind::Server),
            // anthropic's oversized-input code beats the `_too_large` rule
            ("request_too_large", ProviderErrorKind::ContextLength),
            ("prompt_too_long", ProviderErrorKind::InvalidRequest),
            ("invalid_request_error", ProviderErrorKind::InvalidRequest),
            ("unsupported_parameter", ProviderErrorKind::InvalidRequest),
            // both the anthropic and openai not-found spellings
            ("not_found_error", ProviderErrorKind::NotFound),
            ("model_not_found", ProviderErrorKind::NotFound),
        ] {
            assert_eq!(kind_from_error_code(code), Some(expected), "{code}");
        }

        // No opinion, so the caller keeps its own default.
        assert_eq!(kind_from_error_code("overloaded_error"), None);
        assert_eq!(kind_from_error_code("api_error"), None);
        assert_eq!(kind_from_error_code(""), None);
    }

    #[test]
    fn error_message_classification_context_length() {
        let err = error_from_status_code(
            400,
            "This model's maximum context length is 4096 tokens".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::ContextLength,
            ..
        }));
    }

    #[test]
    fn error_message_classification_too_many_tokens() {
        let err = error_from_status_code(
            400,
            "too many tokens in the request".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::ContextLength,
            ..
        }));
    }

    #[test]
    fn error_message_classification_content_filter() {
        let err = error_from_status_code(
            400,
            "Output blocked by content filter".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::ContentFilter,
            ..
        }));
    }

    #[test]
    fn error_message_classification_safety() {
        let err = error_from_status_code(
            400,
            "Response blocked due to safety concerns".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::ContentFilter,
            ..
        }));
    }

    #[test]
    fn error_message_classification_not_found() {
        let err = error_from_status_code(
            400,
            "The model gpt-5 was not found".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::NotFound,
            ..
        }));
    }

    #[test]
    fn error_message_classification_does_not_exist() {
        let err = error_from_status_code(
            400,
            "The resource does not exist".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::NotFound,
            ..
        }));
    }

    #[test]
    fn error_message_classification_unauthorized() {
        let err = error_from_status_code(
            400,
            "Request unauthorized for this resource".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Authentication,
            ..
        }));
    }

    #[test]
    fn error_message_classification_invalid_key() {
        let err = error_from_status_code(
            400,
            "Provided invalid key for authentication".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Authentication,
            ..
        }));
    }

    #[test]
    fn grpc_status_mapping() {
        let err = error_from_grpc_status(
            "NOT_FOUND",
            "model not found".into(),
            "gemini".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::NotFound,
            ..
        }));

        let err = error_from_grpc_status(
            "RESOURCE_EXHAUSTED",
            "rate limited".into(),
            "gemini".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::RateLimit,
            ..
        }));
        assert!(err.retryable());

        let err = error_from_grpc_status(
            "UNAUTHENTICATED",
            "bad key".into(),
            "gemini".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Authentication,
            ..
        }));

        let err = error_from_grpc_status(
            "DEADLINE_EXCEEDED",
            "timeout".into(),
            "gemini".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::RequestTimeout { .. }));

        let err = error_from_grpc_status(
            "UNKNOWN_CODE",
            "something".into(),
            "gemini".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::Server,
            ..
        }));
    }

    #[test]
    fn error_display_messages() {
        let err = Error::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(401),
                ..ProviderErrorDetail::new("invalid api key", "openai")
            }),
        };
        assert_eq!(
            err.to_string(),
            "Authentication error for openai: invalid api key"
        );

        let err = Error::Configuration {
            message: "no provider".into(),
            source:  None,
        };
        assert_eq!(err.to_string(), "Configuration error: no provider");

        let err = Error::InvalidRequest {
            message: "unsupported reasoning effort".into(),
        };
        assert_eq!(
            err.to_string(),
            "Invalid request: unsupported reasoning effort"
        );
    }

    #[test]
    fn status_code_accessor() {
        let err = Error::Provider {
            kind:   ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(503),
                ..ProviderErrorDetail::new("error", "openai")
            }),
        };
        assert_eq!(err.status_code(), Some(503));

        let err = Error::Network {
            message: "refused".into(),
            source:  None,
        };
        assert_eq!(err.status_code(), None);
    }

    #[test]
    fn provider_name_from_provider_variant() {
        let err = Error::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        };
        assert_eq!(err.provider_name(), "openai");
    }

    #[test]
    fn provider_name_defaults_to_unknown() {
        let err = Error::Network {
            message: "refused".into(),
            source:  None,
        };
        assert_eq!(err.provider_name(), "unknown");
    }

    #[test]
    fn failover_eligible_transient_provider_errors() {
        let detail = || Box::new(ProviderErrorDetail::new("error", "openai"));

        assert!(
            Error::Provider {
                kind:   ProviderErrorKind::RateLimit,
                detail: detail(),
            }
            .failover_eligible()
        );

        assert!(
            Error::Provider {
                kind:   ProviderErrorKind::Server,
                detail: detail(),
            }
            .failover_eligible()
        );

        assert!(
            Error::Provider {
                kind:   ProviderErrorKind::QuotaExceeded,
                detail: detail(),
            }
            .failover_eligible()
        );
    }

    #[test]
    fn failover_eligible_provider_local_availability_errors() {
        let detail = || Box::new(ProviderErrorDetail::new("error", "openai"));

        for kind in [
            ProviderErrorKind::Authentication,
            ProviderErrorKind::AccessDenied,
            ProviderErrorKind::NotFound,
        ] {
            assert!(
                Error::Provider {
                    kind,
                    detail: detail(),
                }
                .failover_eligible(),
                "{kind:?} should permit another provider"
            );
        }
    }

    #[test]
    fn failover_eligible_transient_non_provider_errors() {
        assert!(
            Error::RequestTimeout {
                message: "timed out".into(),
                source:  None,
            }
            .failover_eligible()
        );

        assert!(
            Error::Network {
                message: "refused".into(),
                source:  None,
            }
            .failover_eligible()
        );

        assert!(
            Error::Stream {
                message: "broken".into(),
                source:  None,
            }
            .failover_eligible()
        );
    }

    #[test]
    fn failover_not_eligible_deterministic_errors() {
        let detail = || Box::new(ProviderErrorDetail::new("error", "openai"));

        assert!(
            !Error::Provider {
                kind:   ProviderErrorKind::InvalidRequest,
                detail: detail(),
            }
            .failover_eligible()
        );

        assert!(
            !Error::Provider {
                kind:   ProviderErrorKind::ContextLength,
                detail: detail(),
            }
            .failover_eligible()
        );

        assert!(
            !Error::Provider {
                kind:   ProviderErrorKind::ContentFilter,
                detail: detail(),
            }
            .failover_eligible()
        );
    }

    #[test]
    fn failover_eligible_for_refusal_content_filter_only() {
        assert!(
            Error::Provider {
                kind:   ProviderErrorKind::ContentFilter,
                detail: Box::new(ProviderErrorDetail {
                    error_code: Some("refusal".to_string()),
                    raw: Some(serde_json::json!({
                        "stop_reason": "refusal",
                        "stop_details": {"type": "refusal", "category": "cyber"}
                    })),
                    ..ProviderErrorDetail::new("declined", "anthropic")
                }),
            }
            .failover_eligible()
        );

        assert!(
            !Error::Provider {
                kind:   ProviderErrorKind::ContentFilter,
                detail: Box::new(ProviderErrorDetail {
                    error_code: Some("safety".to_string()),
                    ..ProviderErrorDetail::new("blocked", "anthropic")
                }),
            }
            .failover_eligible()
        );
    }

    #[test]
    fn failover_not_eligible_non_provider_errors() {
        assert!(
            !Error::Configuration {
                message: "bad".into(),
                source:  None,
            }
            .failover_eligible()
        );

        assert!(
            !Error::Interrupt {
                message: "cancelled".into(),
            }
            .failover_eligible()
        );

        assert!(
            !Error::InvalidToolCall {
                message: "bad".into(),
            }
            .failover_eligible()
        );

        assert!(
            !Error::NoObjectGenerated {
                message: "none".into(),
            }
            .failover_eligible()
        );

        assert!(
            !Error::InvalidRequest {
                message: "bad".into(),
            }
            .failover_eligible()
        );

        assert!(
            !Error::UnsupportedToolChoice {
                message: "nope".into(),
            }
            .failover_eligible()
        );
    }

    #[test]
    fn failure_signature_hint_provider_transient() {
        let err = Error::Provider {
            kind:   ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail::new("too fast", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_transient|openai|rate_limited"
        );

        let err = Error::Provider {
            kind:   ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail::new("500", "anthropic")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_transient|anthropic|server_error"
        );
    }

    #[test]
    fn failure_signature_hint_provider_deterministic() {
        let err = Error::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|authentication"
        );

        let err = Error::Provider {
            kind:   ProviderErrorKind::AccessDenied,
            detail: Box::new(ProviderErrorDetail::new("denied", "anthropic")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|anthropic|access_denied"
        );

        let err = Error::Provider {
            kind:   ProviderErrorKind::NotFound,
            detail: Box::new(ProviderErrorDetail::new("missing", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|not_found"
        );

        let err = Error::Provider {
            kind:   ProviderErrorKind::InvalidRequest,
            detail: Box::new(ProviderErrorDetail::new("bad", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|invalid_request"
        );

        let err = Error::Provider {
            kind:   ProviderErrorKind::ContentFilter,
            detail: Box::new(ProviderErrorDetail::new("blocked", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|content_filter"
        );

        let err = Error::Provider {
            kind:   ProviderErrorKind::ContextLength,
            detail: Box::new(ProviderErrorDetail::new("too long", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|context_length"
        );

        let err = Error::Provider {
            kind:   ProviderErrorKind::QuotaExceeded,
            detail: Box::new(ProviderErrorDetail::new("out of quota", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|quota_exceeded"
        );
    }

    #[test]
    fn failure_signature_hint_non_provider_variants() {
        assert_eq!(
            Error::RequestTimeout {
                message: "timed out".into(),
                source:  None,
            }
            .failure_signature_hint(),
            "api_transient|unknown|timeout"
        );
        assert_eq!(
            Error::Network {
                message: "refused".into(),
                source:  None,
            }
            .failure_signature_hint(),
            "api_transient|unknown|network"
        );
        assert_eq!(
            Error::Stream {
                message: "broken".into(),
                source:  None,
            }
            .failure_signature_hint(),
            "api_transient|unknown|stream"
        );
        assert_eq!(
            Error::Interrupt {
                message: "cancelled".into(),
            }
            .failure_signature_hint(),
            "api_canceled|unknown|interrupt"
        );
        assert_eq!(
            Error::Configuration {
                message: "bad".into(),
                source:  None,
            }
            .failure_signature_hint(),
            "api_deterministic|unknown|configuration"
        );
        assert_eq!(
            Error::InvalidToolCall {
                message: "bad".into(),
            }
            .failure_signature_hint(),
            "api_deterministic|unknown|invalid_tool_call"
        );
        assert_eq!(
            Error::NoObjectGenerated {
                message: "none".into(),
            }
            .failure_signature_hint(),
            "api_deterministic|unknown|no_object"
        );
        assert_eq!(
            Error::InvalidRequest {
                message: "bad".into(),
            }
            .failure_signature_hint(),
            "api_deterministic|unknown|invalid_request"
        );
        assert_eq!(
            Error::UnsupportedToolChoice {
                message: "nope".into(),
            }
            .failure_signature_hint(),
            "api_deterministic|unknown|unsupported_tool_choice"
        );
    }

    #[test]
    fn sdk_error_source_chaining() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let err = Error::network("connection failed", io_err);
        assert!(err.source().is_some());
    }

    #[test]
    fn sdk_error_source_chain_walkable() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let err = Error::network("connection failed", io_err);
        // The source chain is walkable — the Arc wrapper preserves the inner error's
        // display
        let source = err.source().unwrap();
        assert!(source.to_string().contains("refused"));
    }

    #[test]
    fn sdk_error_serde_roundtrip_without_source() {
        let io_err = std::io::Error::other("boom");
        let err = Error::network("network failed", io_err);
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: Error = serde_json::from_str(&json).unwrap();
        // source is lost through serde, message is preserved
        assert!(deserialized.source().is_none());
        assert_eq!(deserialized.to_string(), "Network error: network failed");
    }
}
