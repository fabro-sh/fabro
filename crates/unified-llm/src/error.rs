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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SdkError {
    #[error("{kind} {}: {}", .detail.provider, .detail.message)]
    Provider {
        kind: ProviderErrorKind,
        detail: Box<ProviderErrorDetail>,
    },

    #[error("Request timed out: {message}")]
    RequestTimeout { message: String },

    #[error("Request aborted: {message}")]
    Abort { message: String },

    #[error("Network error: {message}")]
    Network { message: String },

    #[error("Stream error: {message}")]
    Stream { message: String },

    #[error("Invalid tool call: {message}")]
    InvalidToolCall { message: String },

    #[error("No object generated: {message}")]
    NoObjectGenerated { message: String },

    #[error("Configuration error: {message}")]
    Configuration { message: String },
}

impl SdkError {
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Provider {
                kind: ProviderErrorKind::RateLimit | ProviderErrorKind::Server,
                ..
            } | Self::RequestTimeout { .. }
                | Self::Network { .. }
                | Self::Stream { .. }
        )
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

    // First check message-based classification for ambiguous cases
    let lower_msg = detail.message.to_lowercase();
    if lower_msg.contains("not found") || lower_msg.contains("does not exist") {
        return SdkError::Provider {
            kind: ProviderErrorKind::NotFound,
            detail: Box::new(detail),
        };
    }
    if lower_msg.contains("unauthorized") || lower_msg.contains("invalid key") {
        return SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(detail),
        };
    }
    if lower_msg.contains("context length") || lower_msg.contains("too many tokens") {
        return SdkError::Provider {
            kind: ProviderErrorKind::ContextLength,
            detail: Box::new(detail),
        };
    }
    if lower_msg.contains("content filter") || lower_msg.contains("safety") {
        return SdkError::Provider {
            kind: ProviderErrorKind::ContentFilter,
            detail: Box::new(detail),
        };
    }

    let kind = match status_code {
        400 | 422 => ProviderErrorKind::InvalidRequest,
        401 => ProviderErrorKind::Authentication,
        403 => ProviderErrorKind::AccessDenied,
        404 => ProviderErrorKind::NotFound,
        408 => {
            return SdkError::RequestTimeout {
                message: detail.message,
            }
        }
        413 => ProviderErrorKind::ContextLength,
        429 => ProviderErrorKind::RateLimit,
        _ => ProviderErrorKind::Server,
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
            }
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
        };
        assert!(timeout.retryable());

        let network = SdkError::Network {
            message: "connection refused".into(),
        };
        assert!(network.retryable());

        let config = SdkError::Configuration {
            message: "missing provider".into(),
        };
        assert!(!config.retryable());
    }

    #[test]
    fn non_retryable_errors() {
        let kinds = [
            ProviderErrorKind::AccessDenied,
            ProviderErrorKind::NotFound,
            ProviderErrorKind::InvalidRequest,
            ProviderErrorKind::ContextLength,
            ProviderErrorKind::QuotaExceeded,
            ProviderErrorKind::ContentFilter,
        ];
        for kind in &kinds {
            let err = SdkError::Provider {
                kind: *kind,
                detail: Box::new(ProviderErrorDetail::new("error", "openai")),
            };
            assert!(!err.retryable(), "Expected non-retryable: {err}");
        }
    }

    #[test]
    fn error_from_status_code_mapping() {
        let err = error_from_status_code(401, "unauthorized".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::Authentication, .. }));
        assert!(!err.retryable());

        let err = error_from_status_code(403, "forbidden".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::AccessDenied, .. }));

        let err = error_from_status_code(404, "not found".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::NotFound, .. }));

        let err = error_from_status_code(400, "bad request".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::InvalidRequest, .. }));

        let err = error_from_status_code(422, "unprocessable".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::InvalidRequest, .. }));

        let err = error_from_status_code(408, "timeout".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::RequestTimeout { .. }));

        let err = error_from_status_code(413, "too large".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::ContextLength, .. }));

        let err = error_from_status_code(429, "rate limited".into(), "openai".into(), None, None, Some(5.0));
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::RateLimit, .. }));
        assert!(err.retryable());
        assert_eq!(err.retry_after(), Some(5.0));

        let err = error_from_status_code(500, "internal".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::Server, .. }));
        assert!(err.retryable());

        let err = error_from_status_code(502, "bad gateway".into(), "openai".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::Server, .. }));
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
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::ContextLength, .. }));
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
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::ContextLength, .. }));
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
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::ContentFilter, .. }));
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
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::ContentFilter, .. }));
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
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::NotFound, .. }));
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
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::NotFound, .. }));
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
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::Authentication, .. }));
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
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::Authentication, .. }));
    }

    #[test]
    fn grpc_status_mapping() {
        let err = error_from_grpc_status("NOT_FOUND", "model not found".into(), "gemini".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::NotFound, .. }));

        let err = error_from_grpc_status("RESOURCE_EXHAUSTED", "rate limited".into(), "gemini".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::RateLimit, .. }));
        assert!(err.retryable());

        let err = error_from_grpc_status("UNAUTHENTICATED", "bad key".into(), "gemini".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::Authentication, .. }));

        let err = error_from_grpc_status("DEADLINE_EXCEEDED", "timeout".into(), "gemini".into(), None, None, None);
        assert!(matches!(err, SdkError::RequestTimeout { .. }));

        let err = error_from_grpc_status("UNKNOWN_CODE", "something".into(), "gemini".into(), None, None, None);
        assert!(matches!(err, SdkError::Provider { kind: ProviderErrorKind::Server, .. }));
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
        };
        assert_eq!(err.status_code(), None);
    }
}
