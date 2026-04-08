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
    pub message: String,
    pub provider: String,
    pub status_code: Option<u16>,
    pub error_code: Option<String>,
    pub retry_after: Option<f64>,
    pub raw: Option<serde_json::Value>,
}

impl ProviderErrorDetail {
    pub fn new(message: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            provider: provider.into(),
            status_code: None,
            error_code: None,
            retry_after: None,
            raw: None,
        }
    }
}

use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SdkError {
    #[error("{kind} {}: {}", .detail.provider, .detail.message)]
    Provider {
        kind: ProviderErrorKind,
        detail: Box<ProviderErrorDetail>,
    },

    #[error("Request timed out: {message}")]
    RequestTimeout {
        message: String,
        #[source]
        #[serde(skip)]
        source: Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Request interrupted: {message}")]
    Interrupt { message: String },

    #[error("Network error: {message}")]
    Network {
        message: String,
        #[source]
        #[serde(skip)]
        source: Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Stream error: {message}")]
    Stream {
        message: String,
        #[source]
        #[serde(skip)]
        source: Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Invalid tool call: {message}")]
    InvalidToolCall { message: String },

    #[error("No object generated: {message}")]
    NoObjectGenerated { message: String },

    #[error("Configuration error: {message}")]
    Configuration {
        message: String,
        #[source]
        #[serde(skip)]
        source: Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Unsupported tool choice: {message}")]
    UnsupportedToolChoice { message: String },
}

impl SdkError {
    pub fn network(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Network {
            message: message.into(),
            source: Some(Arc::new(source)),
        }
    }

    pub fn request_timeout(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::RequestTimeout {
            message: message.into(),
            source: Some(Arc::new(source)),
        }
    }

    pub fn stream_error(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Stream {
            message: message.into(),
            source: Some(Arc::new(source)),
        }
    }

    pub fn configuration_error(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Configuration {
            message: message.into(),
            source: Some(Arc::new(source)),
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
    /// same-provider retry) plus `QuotaExceeded` — a different provider won't
    /// share the same quota.
    #[must_use]
    pub const fn failover_eligible(&self) -> bool {
        if self.retryable() {
            return true;
        }
        matches!(
            self,
            Self::Provider {
                kind: ProviderErrorKind::QuotaExceeded,
                ..
            } | Self::RequestTimeout { .. }
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
            Self::UnsupportedToolChoice { .. } => {
                format!("api_deterministic|{provider}|unsupported_tool_choice")
            }
        }
    }
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
) -> SdkError {
    let detail = ProviderErrorDetail {
        message,
        provider,
        status_code: Some(status_code),
        error_code,
        retry_after,
        raw,
    };

    // Check specific status codes first -- these always map to their designated error types
    let kind = match status_code {
        401 => ProviderErrorKind::Authentication,
        403 => ProviderErrorKind::AccessDenied,
        404 => ProviderErrorKind::NotFound,
        408 => {
            return SdkError::RequestTimeout {
                message: detail.message,
                source: None,
            };
        }
        413 => ProviderErrorKind::ContextLength,
        429 => ProviderErrorKind::RateLimit,
        500..=599 => ProviderErrorKind::Server,
        // For ambiguous status codes (400, 422, etc.), use message-based classification
        _ => {
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
        }
    };

    SdkError::Provider {
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
) -> SdkError {
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
            return SdkError::RequestTimeout {
                message: detail.message,
                source: None,
            };
        }
        _ => ProviderErrorKind::Server,
    };

    SdkError::Provider {
        kind,
        detail: Box::new(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn retryable_classification() {
        let auth_err = SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(401),
                ..ProviderErrorDetail::new("bad key", "openai")
            }),
        };
        assert!(!auth_err.retryable());

        let rate_err = SdkError::Provider {
            kind: ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(429),
                retry_after: Some(2.0),
                ..ProviderErrorDetail::new("too fast", "openai")
            }),
        };
        assert!(rate_err.retryable());
        assert_eq!(rate_err.retry_after(), Some(2.0));

        let server_err = SdkError::Provider {
            kind: ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(500),
                ..ProviderErrorDetail::new("internal error", "anthropic")
            }),
        };
        assert!(server_err.retryable());

        let timeout = SdkError::RequestTimeout {
            message: "timed out".into(),
            source: None,
        };
        assert!(!timeout.retryable());

        let network = SdkError::Network {
            message: "connection refused".into(),
            source: None,
        };
        assert!(network.retryable());

        let config = SdkError::Configuration {
            message: "missing provider".into(),
            source: None,
        };
        assert!(!config.retryable());
    }

    #[test]
    fn non_retryable_provider_errors() {
        let detail = || Box::new(ProviderErrorDetail::new("error", "openai"));

        let access_denied = SdkError::Provider {
            kind: ProviderErrorKind::AccessDenied,
            detail: detail(),
        };
        assert!(!access_denied.retryable());

        let not_found = SdkError::Provider {
            kind: ProviderErrorKind::NotFound,
            detail: detail(),
        };
        assert!(!not_found.retryable());

        let invalid_req = SdkError::Provider {
            kind: ProviderErrorKind::InvalidRequest,
            detail: detail(),
        };
        assert!(!invalid_req.retryable());

        let ctx_length = SdkError::Provider {
            kind: ProviderErrorKind::ContextLength,
            detail: detail(),
        };
        assert!(!ctx_length.retryable());

        let quota = SdkError::Provider {
            kind: ProviderErrorKind::QuotaExceeded,
            detail: detail(),
        };
        assert!(!quota.retryable());

        let content_filter = SdkError::Provider {
            kind: ProviderErrorKind::ContentFilter,
            detail: detail(),
        };
        assert!(!content_filter.retryable());
    }

    #[test]
    fn non_retryable_sdk_errors() {
        let invalid_tool = SdkError::InvalidToolCall {
            message: "bad tool".into(),
        };
        assert!(!invalid_tool.retryable());

        let no_object = SdkError::NoObjectGenerated {
            message: "no output".into(),
        };
        assert!(!no_object.retryable());

        let interrupt = SdkError::Interrupt {
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::Authentication,
                ..
            }
        ));
        assert!(!err.retryable());

        let err =
            error_from_status_code(403, "forbidden".into(), "openai".into(), None, None, None);
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::AccessDenied,
                ..
            }
        ));

        let err =
            error_from_status_code(404, "not found".into(), "openai".into(), None, None, None);
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::NotFound,
                ..
            }
        ));

        let err =
            error_from_status_code(400, "bad request".into(), "openai".into(), None, None, None);
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::InvalidRequest,
                ..
            }
        ));

        let err = error_from_status_code(
            422,
            "unprocessable".into(),
            "openai".into(),
            None,
            None,
            None,
        );
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::InvalidRequest,
                ..
            }
        ));

        let err = error_from_status_code(408, "timeout".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::RequestTimeout { .. }));

        let err =
            error_from_status_code(413, "too large".into(), "openai".into(), None, None, None);
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::ContextLength,
                ..
            }
        ));

        let err = error_from_status_code(
            429,
            "rate limited".into(),
            "openai".into(),
            None,
            None,
            Some(5.0),
        );
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::RateLimit,
                ..
            }
        ));
        assert!(err.retryable());
        assert_eq!(err.retry_after(), Some(5.0));

        let err = error_from_status_code(500, "internal".into(), "openai".into(), None, None, None);
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::Server,
                ..
            }
        ));
        assert!(err.retryable());

        let err =
            error_from_status_code(502, "bad gateway".into(), "openai".into(), None, None, None);
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::Server,
                ..
            }
        ));

        let err = error_from_status_code(
            529,
            "Overloaded".into(),
            "anthropic".into(),
            None,
            None,
            None,
        );
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::Server,
                ..
            }
        ));
        assert!(err.retryable());
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::ContextLength,
                ..
            }
        ));
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::ContextLength,
                ..
            }
        ));
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::ContentFilter,
                ..
            }
        ));
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::ContentFilter,
                ..
            }
        ));
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::NotFound,
                ..
            }
        ));
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::NotFound,
                ..
            }
        ));
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::Authentication,
                ..
            }
        ));
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::Authentication,
                ..
            }
        ));
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
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::NotFound,
                ..
            }
        ));

        let err = error_from_grpc_status(
            "RESOURCE_EXHAUSTED",
            "rate limited".into(),
            "gemini".into(),
            None,
            None,
            None,
        );
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::RateLimit,
                ..
            }
        ));
        assert!(err.retryable());

        let err = error_from_grpc_status(
            "UNAUTHENTICATED",
            "bad key".into(),
            "gemini".into(),
            None,
            None,
            None,
        );
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::Authentication,
                ..
            }
        ));

        let err = error_from_grpc_status(
            "DEADLINE_EXCEEDED",
            "timeout".into(),
            "gemini".into(),
            None,
            None,
            None,
        );
        assert!(matches!(err, SdkError::RequestTimeout { .. }));

        let err = error_from_grpc_status(
            "UNKNOWN_CODE",
            "something".into(),
            "gemini".into(),
            None,
            None,
            None,
        );
        assert!(matches!(
            err,
            SdkError::Provider {
                kind: ProviderErrorKind::Server,
                ..
            }
        ));
    }

    #[test]
    fn error_display_messages() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(401),
                ..ProviderErrorDetail::new("invalid api key", "openai")
            }),
        };
        assert_eq!(
            err.to_string(),
            "Authentication error for openai: invalid api key"
        );

        let err = SdkError::Configuration {
            message: "no provider".into(),
            source: None,
        };
        assert_eq!(err.to_string(), "Configuration error: no provider");
    }

    #[test]
    fn status_code_accessor() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(503),
                ..ProviderErrorDetail::new("error", "openai")
            }),
        };
        assert_eq!(err.status_code(), Some(503));

        let err = SdkError::Network {
            message: "refused".into(),
            source: None,
        };
        assert_eq!(err.status_code(), None);
    }

    #[test]
    fn provider_name_from_provider_variant() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        };
        assert_eq!(err.provider_name(), "openai");
    }

    #[test]
    fn provider_name_defaults_to_unknown() {
        let err = SdkError::Network {
            message: "refused".into(),
            source: None,
        };
        assert_eq!(err.provider_name(), "unknown");
    }

    #[test]
    fn failover_eligible_transient_provider_errors() {
        let detail = || Box::new(ProviderErrorDetail::new("error", "openai"));

        assert!(
            SdkError::Provider {
                kind: ProviderErrorKind::RateLimit,
                detail: detail(),
            }
            .failover_eligible()
        );

        assert!(
            SdkError::Provider {
                kind: ProviderErrorKind::Server,
                detail: detail(),
            }
            .failover_eligible()
        );

        assert!(
            SdkError::Provider {
                kind: ProviderErrorKind::QuotaExceeded,
                detail: detail(),
            }
            .failover_eligible()
        );
    }

    #[test]
    fn failover_eligible_transient_non_provider_errors() {
        assert!(
            SdkError::RequestTimeout {
                message: "timed out".into(),
                source: None,
            }
            .failover_eligible()
        );

        assert!(
            SdkError::Network {
                message: "refused".into(),
                source: None,
            }
            .failover_eligible()
        );

        assert!(
            SdkError::Stream {
                message: "broken".into(),
                source: None,
            }
            .failover_eligible()
        );
    }

    #[test]
    fn failover_not_eligible_deterministic_errors() {
        let detail = || Box::new(ProviderErrorDetail::new("error", "openai"));

        assert!(
            !SdkError::Provider {
                kind: ProviderErrorKind::Authentication,
                detail: detail(),
            }
            .failover_eligible()
        );

        assert!(
            !SdkError::Provider {
                kind: ProviderErrorKind::InvalidRequest,
                detail: detail(),
            }
            .failover_eligible()
        );

        assert!(
            !SdkError::Provider {
                kind: ProviderErrorKind::ContextLength,
                detail: detail(),
            }
            .failover_eligible()
        );

        assert!(
            !SdkError::Provider {
                kind: ProviderErrorKind::ContentFilter,
                detail: detail(),
            }
            .failover_eligible()
        );
    }

    #[test]
    fn failover_not_eligible_non_provider_errors() {
        assert!(
            !SdkError::Configuration {
                message: "bad".into(),
                source: None,
            }
            .failover_eligible()
        );

        assert!(
            !SdkError::Interrupt {
                message: "cancelled".into()
            }
            .failover_eligible()
        );

        assert!(
            !SdkError::InvalidToolCall {
                message: "bad".into()
            }
            .failover_eligible()
        );

        assert!(
            !SdkError::NoObjectGenerated {
                message: "none".into()
            }
            .failover_eligible()
        );

        assert!(
            !SdkError::UnsupportedToolChoice {
                message: "nope".into()
            }
            .failover_eligible()
        );
    }

    #[test]
    fn failure_signature_hint_provider_transient() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail::new("too fast", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_transient|openai|rate_limited"
        );

        let err = SdkError::Provider {
            kind: ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail::new("500", "anthropic")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_transient|anthropic|server_error"
        );
    }

    #[test]
    fn failure_signature_hint_provider_deterministic() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|authentication"
        );

        let err = SdkError::Provider {
            kind: ProviderErrorKind::AccessDenied,
            detail: Box::new(ProviderErrorDetail::new("denied", "anthropic")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|anthropic|access_denied"
        );

        let err = SdkError::Provider {
            kind: ProviderErrorKind::NotFound,
            detail: Box::new(ProviderErrorDetail::new("missing", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|not_found"
        );

        let err = SdkError::Provider {
            kind: ProviderErrorKind::InvalidRequest,
            detail: Box::new(ProviderErrorDetail::new("bad", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|invalid_request"
        );

        let err = SdkError::Provider {
            kind: ProviderErrorKind::ContentFilter,
            detail: Box::new(ProviderErrorDetail::new("blocked", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|content_filter"
        );

        let err = SdkError::Provider {
            kind: ProviderErrorKind::ContextLength,
            detail: Box::new(ProviderErrorDetail::new("too long", "openai")),
        };
        assert_eq!(
            err.failure_signature_hint(),
            "api_deterministic|openai|context_length"
        );

        let err = SdkError::Provider {
            kind: ProviderErrorKind::QuotaExceeded,
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
            SdkError::RequestTimeout {
                message: "timed out".into(),
                source: None,
            }
            .failure_signature_hint(),
            "api_transient|unknown|timeout"
        );
        assert_eq!(
            SdkError::Network {
                message: "refused".into(),
                source: None,
            }
            .failure_signature_hint(),
            "api_transient|unknown|network"
        );
        assert_eq!(
            SdkError::Stream {
                message: "broken".into(),
                source: None,
            }
            .failure_signature_hint(),
            "api_transient|unknown|stream"
        );
        assert_eq!(
            SdkError::Interrupt {
                message: "cancelled".into()
            }
            .failure_signature_hint(),
            "api_canceled|unknown|interrupt"
        );
        assert_eq!(
            SdkError::Configuration {
                message: "bad".into(),
                source: None,
            }
            .failure_signature_hint(),
            "api_deterministic|unknown|configuration"
        );
        assert_eq!(
            SdkError::InvalidToolCall {
                message: "bad".into()
            }
            .failure_signature_hint(),
            "api_deterministic|unknown|invalid_tool_call"
        );
        assert_eq!(
            SdkError::NoObjectGenerated {
                message: "none".into()
            }
            .failure_signature_hint(),
            "api_deterministic|unknown|no_object"
        );
        assert_eq!(
            SdkError::UnsupportedToolChoice {
                message: "nope".into()
            }
            .failure_signature_hint(),
            "api_deterministic|unknown|unsupported_tool_choice"
        );
    }

    #[test]
    fn sdk_error_source_chaining() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let err = SdkError::network("connection failed", io_err);
        assert!(err.source().is_some());
    }

    #[test]
    fn sdk_error_source_chain_walkable() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let err = SdkError::network("connection failed", io_err);
        // The source chain is walkable — the Arc wrapper preserves the inner error's display
        let source = err.source().unwrap();
        assert!(source.to_string().contains("refused"));
    }

    #[test]
    fn sdk_error_serde_roundtrip_without_source() {
        let io_err = std::io::Error::other("boom");
        let err = SdkError::network("network failed", io_err);
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: SdkError = serde_json::from_str(&json).unwrap();
        // source is lost through serde, message is preserved
        assert!(deserialized.source().is_none());
        assert_eq!(deserialized.to_string(), "Network error: network failed");
    }
}
