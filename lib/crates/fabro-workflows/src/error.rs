use std::fmt;
use std::str::FromStr;

use fabro_llm::error::{ProviderErrorKind, SdkError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Classification of failure modes for pipeline edge conditions.
///
/// Pipeline authors can write edge conditions like `context.failure_class=budget_exhausted`
/// to route execution based on the nature of the failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Temporary infrastructure failure (rate limit, timeout, network, 5xx).
    TransientInfra,
    /// Permanent failure (auth, bad config, code bug).
    Deterministic,
    /// Context length, token/turn limit, quota exceeded.
    BudgetExhausted,
    /// Reserved for future loop detection.
    CompilationLoop,
    /// User/system cancellation.
    Canceled,
    /// Reserved for future scope enforcement.
    Structural,
}

impl fmt::Display for FailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::TransientInfra => "transient_infra",
            Self::Deterministic => "deterministic",
            Self::BudgetExhausted => "budget_exhausted",
            Self::CompilationLoop => "compilation_loop",
            Self::Canceled => "canceled",
            Self::Structural => "structural",
        };
        write!(f, "{s}")
    }
}

impl FromStr for FailureClass {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let normalized = s.trim().to_lowercase();
        Ok(match normalized.as_str() {
            // Canonical names
            "transient_infra" => Self::TransientInfra,
            "deterministic" => Self::Deterministic,
            "budget_exhausted" => Self::BudgetExhausted,
            "compilation_loop" => Self::CompilationLoop,
            "canceled" => Self::Canceled,
            "structural" => Self::Structural,

            // Aliases: transient_infra
            "transient"
            | "transient-infra"
            | "infra_transient"
            | "transient infra"
            | "infrastructure_transient"
            | "retryable"
            | "toolchain_workspace_io"
            | "toolchain-workspace-io"
            | "toolchain_or_dependency_registry_unavailable"
            | "toolchain-dependency-registry-unavailable" => Self::TransientInfra,

            // Aliases: deterministic
            "non_transient" | "non-transient" | "permanent" | "logic" | "product" => {
                Self::Deterministic
            }

            // Aliases: canceled
            "cancelled" => Self::Canceled,

            // Aliases: budget_exhausted
            "budget-exhausted" | "budget exhausted" | "budget" => Self::BudgetExhausted,

            // Aliases: compilation_loop
            "compilation-loop" | "compilation loop" | "compile_loop" | "compile-loop" => {
                Self::CompilationLoop
            }

            // Aliases: structural
            "structure" | "scope_violation" | "write_scope_violation" => Self::Structural,

            // Unknown → fail-closed to Deterministic
            _ => Self::Deterministic,
        })
    }
}

/// Classify an `SdkError` into a `FailureClass` based on its structure.
#[must_use]
pub fn classify_sdk_error(err: &SdkError) -> FailureClass {
    match err {
        SdkError::Provider { kind, .. } => match kind {
            ProviderErrorKind::RateLimit | ProviderErrorKind::Server => {
                FailureClass::TransientInfra
            }
            ProviderErrorKind::ContextLength | ProviderErrorKind::QuotaExceeded => {
                FailureClass::BudgetExhausted
            }
            ProviderErrorKind::Authentication
            | ProviderErrorKind::AccessDenied
            | ProviderErrorKind::NotFound
            | ProviderErrorKind::InvalidRequest
            | ProviderErrorKind::ContentFilter => FailureClass::Deterministic,
        },
        SdkError::RequestTimeout { .. } | SdkError::Network { .. } | SdkError::Stream { .. } => {
            FailureClass::TransientInfra
        }
        SdkError::Abort { .. } => FailureClass::Canceled,
        SdkError::InvalidToolCall { .. }
        | SdkError::NoObjectGenerated { .. }
        | SdkError::Configuration { .. }
        | SdkError::UnsupportedToolChoice { .. } => FailureClass::Deterministic,
    }
}

const TRANSIENT_INFRA_HINTS: &[&str] = &[
    "timeout",
    "timed out",
    "rate limit",
    "rate limited",
    "connection refused",
    "connection reset",
    "500",
    "502",
    "503",
    "504",
    "context deadline exceeded",
    "could not resolve host",
    "could not resolve hostname",
    "temporary failure",
    "network is unreachable",
    "broken pipe",
    "tls handshake timeout",
    "i/o timeout",
    "no route to host",
    "temporarily unavailable",
    "try again",
    "too many requests",
    "service unavailable",
    "gateway timeout",
    "econnrefused",
    "econnreset",
    "dial tcp",
    "transport is closing",
    "stream disconnected",
    "stream closed before",
    "index.crates.io",
    "download of config.json failed",
    "toolchain_or_dependency_registry_unavailable",
    "toolchain dependency resolution blocked by network",
    "toolchain_workspace_io",
    "cross-device link",
    "invalid cross-device link",
    "os error 18",
];

const BUDGET_EXHAUSTED_HINTS: &[&str] = &[
    "turn limit",
    "token limit",
    "context length",
    "budget",
    "quota exceeded",
    "max_turns",
    "max turns",
    "max_tokens",
    "max tokens",
    "context window exceeded",
    "budget exhausted",
    "token limit exceeded",
];

const STRUCTURAL_HINTS: &[&str] = &[
    "write_scope_violation",
    "write scope violation",
    "scope violation",
];

/// Classify a failure reason string using heuristics.
///
/// This is the fallback when structured error information is not available
/// (e.g. for `Handler(String)` or `Engine(String)` errors).
#[must_use]
pub fn classify_failure_reason(reason: &str) -> FailureClass {
    let lower = reason.to_lowercase();

    if lower.contains("cancel") || lower.contains("abort") {
        return FailureClass::Canceled;
    }

    if TRANSIENT_INFRA_HINTS
        .iter()
        .any(|hint| lower.contains(hint))
    {
        return FailureClass::TransientInfra;
    }

    if BUDGET_EXHAUSTED_HINTS
        .iter()
        .any(|hint| lower.contains(hint))
    {
        return FailureClass::BudgetExhausted;
    }

    if STRUCTURAL_HINTS.iter().any(|hint| lower.contains(hint)) {
        return FailureClass::Structural;
    }

    FailureClass::Deterministic
}

/// Normalize a failure reason for stable signature grouping.
///
/// Replaces variable data (hex strings, digits) with placeholders so that
/// semantically identical errors produce the same signature regardless of
/// line numbers, commit hashes, or timestamps.
pub fn normalize_failure_reason(reason: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;

    static HEX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-f]{7,64}\b").unwrap());
    static DIGITS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d+\b").unwrap());
    static COMMA_SPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s+").unwrap());
    static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

    let s = reason.trim().to_lowercase();
    if s.is_empty() {
        return String::new();
    }
    let s = HEX_RE.replace_all(&s, "<hex>");
    let s = DIGITS_RE.replace_all(&s, "<n>");
    let s = COMMA_SPACE_RE.replace_all(&s, ",");
    let s = WHITESPACE_RE.replace_all(&s, " ");
    let s = s.trim();
    if s.len() > 240 {
        s[..s.floor_char_boundary(240)].to_string()
    } else {
        s.to_string()
    }
}

/// Composite key that uniquely identifies a specific recurring failure.
///
/// Format: `node_id|failure_class|normalized_reason`
///
/// Used by circuit breakers to detect when the same failure keeps repeating,
/// e.g. "verify|deterministic|assertion failed in foo_test".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FailureSignature(String);

impl FailureSignature {
    /// Build a signature from failure context.
    ///
    /// The signature hint from `outcome.context_updates["failure_signature"]` takes
    /// priority over the raw `failure_reason`, allowing handlers to provide explicit
    /// grouping keys.
    pub fn new(
        node_id: &str,
        failure_class: FailureClass,
        signature_hint: Option<&str>,
        failure_reason: Option<&str>,
    ) -> Self {
        let reason = signature_hint
            .map(normalize_failure_reason)
            .filter(|s| !s.is_empty())
            .or_else(|| failure_reason.map(normalize_failure_reason))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        Self(format!("{}|{}|{}", node_id.trim(), failure_class, reason))
    }
}

impl fmt::Display for FailureSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FailureClass {
    /// Whether this failure class should be tracked by the cycle breaker.
    pub fn is_signature_tracked(self) -> bool {
        matches!(self, Self::Deterministic | Self::Structural)
    }
}

#[derive(Error, Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum FabroError {
    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Engine error: {message}")]
    Engine {
        message: String,
        failure_class: FailureClass,
    },

    #[error("Handler error: {message}")]
    Handler {
        message: String,
        failure_class: FailureClass,
    },

    #[error("LLM error: {0}")]
    Llm(SdkError),

    #[error("Checkpoint error: {0}")]
    Checkpoint(String),

    #[error("Stylesheet error: {0}")]
    Stylesheet(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("Pipeline cancelled")]
    Cancelled,
}

impl FabroError {
    /// Smart constructor for Handler errors. Classifies the failure reason eagerly.
    pub fn handler(message: impl Into<String>) -> Self {
        let message = message.into();
        let failure_class = classify_failure_reason(&message);
        Self::Handler {
            message,
            failure_class,
        }
    }

    /// Smart constructor for Engine errors. Classifies the failure reason eagerly.
    pub fn engine(message: impl Into<String>) -> Self {
        let message = message.into();
        let failure_class = classify_failure_reason(&message);
        Self::Engine {
            message,
            failure_class,
        }
    }

    /// Whether this error category is retryable (transient) or terminal.
    ///
    /// Retryable: Handler (transient handler failures), Engine (could be transient),
    ///            Io (network/disk issues are often transient), Llm (delegates to SdkError).
    /// Terminal:  Parse, Validation, Stylesheet (configuration errors),
    ///            Checkpoint (storage integrity), Cancelled (explicit cancellation).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Handler { .. } | Self::Engine { .. } | Self::Io(_) => true,
            Self::Llm(sdk_err) => sdk_err.retryable(),
            Self::Parse(_)
            | Self::Validation(_)
            | Self::Stylesheet(_)
            | Self::Checkpoint(_)
            | Self::Cancelled => false,
        }
    }

    /// Classify this error into a `FailureClass`.
    #[must_use]
    pub fn failure_class(&self) -> FailureClass {
        match self {
            Self::Cancelled => FailureClass::Canceled,
            Self::Llm(sdk_err) => classify_sdk_error(sdk_err),
            Self::Io(_) => FailureClass::TransientInfra,
            Self::Parse(_) | Self::Validation(_) | Self::Stylesheet(_) | Self::Checkpoint(_) => {
                FailureClass::Deterministic
            }
            Self::Handler { failure_class, .. } | Self::Engine { failure_class, .. } => {
                *failure_class
            }
        }
    }

    /// Return a stable failure signature hint when structured error info is available.
    #[must_use]
    pub fn failure_signature_hint(&self) -> Option<String> {
        match self {
            Self::Llm(sdk_err) => Some(sdk_err.failure_signature_hint()),
            _ => None,
        }
    }

    /// Build a fail `Outcome` with structured `FailureDetail`.
    pub fn to_fail_outcome(&self) -> crate::outcome::Outcome {
        let failure = crate::outcome::FailureDetail {
            message: self.to_string(),
            failure_class: self.failure_class(),
            failure_signature: self.failure_signature_hint(),
        };
        crate::outcome::Outcome {
            status: crate::outcome::StageStatus::Fail,
            failure: Some(failure),
            ..crate::outcome::Outcome::success()
        }
    }
}

impl From<std::io::Error> for FabroError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<SdkError> for FabroError {
    fn from(err: SdkError) -> Self {
        Self::Llm(err)
    }
}

impl From<fabro_graphviz::error::GraphvizError> for FabroError {
    fn from(e: fabro_graphviz::error::GraphvizError) -> Self {
        let fabro_graphviz::error::GraphvizError::Parse(msg) = e;
        Self::Parse(msg)
    }
}

pub type Result<T> = std::result::Result<T, FabroError>;

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_llm::error::ProviderErrorDetail;

    #[test]
    fn parse_error_display() {
        let err = FabroError::Parse("unexpected token".to_string());
        assert_eq!(err.to_string(), "Parse error: unexpected token");
    }

    #[test]
    fn validation_error_display() {
        let err = FabroError::Validation("missing start node".to_string());
        assert_eq!(err.to_string(), "Validation error: missing start node");
    }

    #[test]
    fn engine_error_display() {
        let err = FabroError::engine("no outgoing edge");
        assert_eq!(err.to_string(), "Engine error: no outgoing edge");
    }

    #[test]
    fn handler_error_display() {
        let err = FabroError::handler("LLM call failed");
        assert_eq!(err.to_string(), "Handler error: LLM call failed");
    }

    #[test]
    fn checkpoint_error_display() {
        let err = FabroError::Checkpoint("file not found".to_string());
        assert_eq!(err.to_string(), "Checkpoint error: file not found");
    }

    #[test]
    fn io_error_display() {
        let err = FabroError::Io("permission denied".to_string());
        assert_eq!(err.to_string(), "I/O error: permission denied");
    }

    #[test]
    fn io_error_from_std() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err = FabroError::from(io_err);
        assert!(matches!(err, FabroError::Io(_)));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn result_type_alias_works() {
        let ok: Result<i32> = Ok(42);
        assert!(ok.is_ok());

        let err: Result<i32> = Err(FabroError::Parse("bad".to_string()));
        assert!(err.is_err());
    }

    #[test]
    fn cancelled_error_display() {
        let err = FabroError::Cancelled;
        assert_eq!(err.to_string(), "Pipeline cancelled");
    }

    #[test]
    fn cancelled_is_not_retryable() {
        assert!(!FabroError::Cancelled.is_retryable());
    }

    #[test]
    fn is_retryable_terminal_errors() {
        assert!(!FabroError::Parse("bad".to_string()).is_retryable());
        assert!(!FabroError::Validation("bad".to_string()).is_retryable());
        assert!(!FabroError::Stylesheet("bad".to_string()).is_retryable());
        assert!(!FabroError::Checkpoint("bad".to_string()).is_retryable());
    }

    #[test]
    fn is_retryable_transient_errors() {
        assert!(FabroError::handler("timeout").is_retryable());
        assert!(FabroError::engine("transient").is_retryable());
        assert!(FabroError::Io("connection reset".to_string()).is_retryable());
    }

    // --- FailureClass Display/FromStr/serde tests ---

    #[test]
    fn failure_class_display_all_values() {
        assert_eq!(FailureClass::TransientInfra.to_string(), "transient_infra");
        assert_eq!(FailureClass::Deterministic.to_string(), "deterministic");
        assert_eq!(
            FailureClass::BudgetExhausted.to_string(),
            "budget_exhausted"
        );
        assert_eq!(
            FailureClass::CompilationLoop.to_string(),
            "compilation_loop"
        );
        assert_eq!(FailureClass::Canceled.to_string(), "canceled");
        assert_eq!(FailureClass::Structural.to_string(), "structural");
    }

    #[test]
    fn failure_class_from_str_all_values() {
        assert_eq!(
            "transient_infra".parse::<FailureClass>().unwrap(),
            FailureClass::TransientInfra
        );
        assert_eq!(
            "deterministic".parse::<FailureClass>().unwrap(),
            FailureClass::Deterministic
        );
        assert_eq!(
            "budget_exhausted".parse::<FailureClass>().unwrap(),
            FailureClass::BudgetExhausted
        );
        assert_eq!(
            "compilation_loop".parse::<FailureClass>().unwrap(),
            FailureClass::CompilationLoop
        );
        assert_eq!(
            "canceled".parse::<FailureClass>().unwrap(),
            FailureClass::Canceled
        );
        assert_eq!(
            "structural".parse::<FailureClass>().unwrap(),
            FailureClass::Structural
        );
    }

    #[test]
    fn failure_class_from_str_invalid() {
        assert_eq!(
            "unknown".parse::<FailureClass>().unwrap(),
            FailureClass::Deterministic
        );
    }

    #[test]
    fn failure_class_from_str_alias_retryable() {
        assert_eq!(
            "retryable".parse::<FailureClass>().unwrap(),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn failure_class_from_str_alias_transient() {
        assert_eq!(
            "transient".parse::<FailureClass>().unwrap(),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn failure_class_from_str_alias_permanent() {
        assert_eq!(
            "permanent".parse::<FailureClass>().unwrap(),
            FailureClass::Deterministic
        );
    }

    #[test]
    fn failure_class_from_str_alias_cancelled_british() {
        assert_eq!(
            "cancelled".parse::<FailureClass>().unwrap(),
            FailureClass::Canceled
        );
    }

    #[test]
    fn failure_class_from_str_alias_budget() {
        assert_eq!(
            "budget".parse::<FailureClass>().unwrap(),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn failure_class_from_str_alias_compile_loop() {
        assert_eq!(
            "compile_loop".parse::<FailureClass>().unwrap(),
            FailureClass::CompilationLoop
        );
    }

    #[test]
    fn failure_class_from_str_alias_scope_violation() {
        assert_eq!(
            "scope_violation".parse::<FailureClass>().unwrap(),
            FailureClass::Structural
        );
    }

    #[test]
    fn failure_class_from_str_unknown_defaults_deterministic() {
        assert_eq!(
            "garbage_xyz".parse::<FailureClass>().unwrap(),
            FailureClass::Deterministic
        );
    }

    #[test]
    fn failure_class_from_str_case_insensitive() {
        assert_eq!(
            "TRANSIENT_INFRA".parse::<FailureClass>().unwrap(),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn failure_class_from_str_trims_whitespace() {
        assert_eq!(
            " transient_infra ".parse::<FailureClass>().unwrap(),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn failure_class_from_str_empty_defaults_deterministic() {
        assert_eq!(
            "".parse::<FailureClass>().unwrap(),
            FailureClass::Deterministic
        );
    }

    #[test]
    fn failure_class_serde_roundtrip() {
        let values = [
            FailureClass::TransientInfra,
            FailureClass::Deterministic,
            FailureClass::BudgetExhausted,
            FailureClass::CompilationLoop,
            FailureClass::Canceled,
            FailureClass::Structural,
        ];
        for fc in values {
            let json = serde_json::to_string(&fc).unwrap();
            let parsed: FailureClass = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, fc);
        }
    }

    // --- Llm variant tests ---

    #[test]
    fn llm_error_display() {
        let sdk_err = SdkError::Network {
            message: "connection refused".into(),
        };
        let err = FabroError::Llm(sdk_err);
        assert_eq!(
            err.to_string(),
            "LLM error: Network error: connection refused"
        );
    }

    #[test]
    fn llm_error_retryable_delegates_to_sdk() {
        let retryable = FabroError::Llm(SdkError::Network {
            message: "timeout".into(),
        });
        assert!(retryable.is_retryable());

        let non_retryable = FabroError::Llm(SdkError::Configuration {
            message: "bad config".into(),
        });
        assert!(!non_retryable.is_retryable());
    }

    #[test]
    fn llm_error_from_sdk_error() {
        let sdk_err = SdkError::Stream {
            message: "broken pipe".into(),
        };
        let err = FabroError::from(sdk_err);
        assert!(matches!(err, FabroError::Llm(_)));
    }

    // --- failure_class() method tests ---

    #[test]
    fn failure_class_cancelled() {
        assert_eq!(
            FabroError::Cancelled.failure_class(),
            FailureClass::Canceled
        );
    }

    #[test]
    fn failure_class_io() {
        assert_eq!(
            FabroError::Io("disk full".into()).failure_class(),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn failure_class_parse() {
        assert_eq!(
            FabroError::Parse("bad syntax".into()).failure_class(),
            FailureClass::Deterministic
        );
    }

    #[test]
    fn failure_class_handler_with_timeout() {
        assert_eq!(
            FabroError::handler("request timed out").failure_class(),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn failure_class_handler_deterministic() {
        assert_eq!(
            FabroError::handler("invalid configuration").failure_class(),
            FailureClass::Deterministic
        );
    }

    #[test]
    fn failure_class_llm_rate_limit() {
        let err = FabroError::Llm(SdkError::Provider {
            kind: ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail::new("too fast", "openai")),
        });
        assert_eq!(err.failure_class(), FailureClass::TransientInfra);
    }

    #[test]
    fn failure_class_llm_context_length() {
        let err = FabroError::Llm(SdkError::Provider {
            kind: ProviderErrorKind::ContextLength,
            detail: Box::new(ProviderErrorDetail::new("too long", "openai")),
        });
        assert_eq!(err.failure_class(), FailureClass::BudgetExhausted);
    }

    #[test]
    fn failure_class_llm_auth() {
        let err = FabroError::Llm(SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        });
        assert_eq!(err.failure_class(), FailureClass::Deterministic);
    }

    #[test]
    fn failure_class_llm_abort() {
        let err = FabroError::Llm(SdkError::Abort {
            message: "user cancelled".into(),
        });
        assert_eq!(err.failure_class(), FailureClass::Canceled);
    }

    #[test]
    fn failure_class_llm_timeout() {
        let err = FabroError::Llm(SdkError::RequestTimeout {
            message: "timed out".into(),
        });
        assert_eq!(err.failure_class(), FailureClass::TransientInfra);
    }

    // --- classify_sdk_error tests ---

    #[test]
    fn classify_sdk_rate_limit() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail::new("too fast", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureClass::TransientInfra);
    }

    #[test]
    fn classify_sdk_server() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail::new("500", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureClass::TransientInfra);
    }

    #[test]
    fn classify_sdk_context_length() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::ContextLength,
            detail: Box::new(ProviderErrorDetail::new("too long", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureClass::BudgetExhausted);
    }

    #[test]
    fn classify_sdk_quota_exceeded() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::QuotaExceeded,
            detail: Box::new(ProviderErrorDetail::new("out of quota", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureClass::BudgetExhausted);
    }

    #[test]
    fn classify_sdk_auth() {
        let err = SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureClass::Deterministic);
    }

    #[test]
    fn classify_sdk_request_timeout() {
        let err = SdkError::RequestTimeout {
            message: "timed out".into(),
        };
        assert_eq!(classify_sdk_error(&err), FailureClass::TransientInfra);
    }

    #[test]
    fn classify_sdk_abort() {
        let err = SdkError::Abort {
            message: "cancelled".into(),
        };
        assert_eq!(classify_sdk_error(&err), FailureClass::Canceled);
    }

    #[test]
    fn classify_sdk_invalid_tool_call() {
        let err = SdkError::InvalidToolCall {
            message: "bad tool".into(),
        };
        assert_eq!(classify_sdk_error(&err), FailureClass::Deterministic);
    }

    // --- hints count guards ---

    #[test]
    fn transient_infra_hints_count() {
        assert_eq!(TRANSIENT_INFRA_HINTS.len(), 38);
    }

    #[test]
    fn budget_exhausted_hints_count() {
        assert_eq!(BUDGET_EXHAUSTED_HINTS.len(), 12);
    }

    #[test]
    fn structural_hints_count() {
        assert_eq!(STRUCTURAL_HINTS.len(), 3);
    }

    // --- classify_failure_reason regression tests ---

    // Canceled

    #[test]
    fn classify_reason_cancel() {
        assert_eq!(
            classify_failure_reason("operation cancelled by user"),
            FailureClass::Canceled
        );
    }

    #[test]
    fn classify_reason_abort() {
        assert_eq!(
            classify_failure_reason("aborted by signal"),
            FailureClass::Canceled
        );
    }

    // Budget exhausted

    #[test]
    fn classify_reason_turn_limit() {
        assert_eq!(
            classify_failure_reason("exceeded turn limit of 10"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_token_limit() {
        assert_eq!(
            classify_failure_reason("token limit reached"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_context_length() {
        assert_eq!(
            classify_failure_reason("context length exceeded"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_budget() {
        assert_eq!(
            classify_failure_reason("budget exceeded for run"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_quota_exceeded() {
        assert_eq!(
            classify_failure_reason("quota exceeded"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_max_turns() {
        assert_eq!(
            classify_failure_reason("hit max_turns limit"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_max_turns_space() {
        assert_eq!(
            classify_failure_reason("max turns reached"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_max_tokens() {
        assert_eq!(
            classify_failure_reason("max_tokens exceeded"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_max_tokens_space() {
        assert_eq!(
            classify_failure_reason("max tokens reached"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_context_window_exceeded() {
        assert_eq!(
            classify_failure_reason("context window exceeded"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_budget_exhausted() {
        assert_eq!(
            classify_failure_reason("budget exhausted for this session"),
            FailureClass::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_token_limit_exceeded() {
        assert_eq!(
            classify_failure_reason("token limit exceeded"),
            FailureClass::BudgetExhausted
        );
    }

    // Structural

    #[test]
    fn classify_reason_scope_violation() {
        assert_eq!(
            classify_failure_reason("scope violation detected"),
            FailureClass::Structural
        );
    }

    // Transient infra

    #[test]
    fn classify_reason_timeout() {
        assert_eq!(
            classify_failure_reason("request timed out after 30s"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_rate_limit() {
        assert_eq!(
            classify_failure_reason("rate limited by provider"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_connection_refused() {
        assert_eq!(
            classify_failure_reason("connection refused"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_connection_reset() {
        assert_eq!(
            classify_failure_reason("connection reset by peer"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_500() {
        assert_eq!(
            classify_failure_reason("HTTP 500 Internal Server Error"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_502() {
        assert_eq!(
            classify_failure_reason("HTTP 502 Bad Gateway"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_503() {
        assert_eq!(
            classify_failure_reason("HTTP 503 Service Unavailable"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_504() {
        assert_eq!(
            classify_failure_reason("HTTP 504 Gateway Timeout"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_context_deadline_exceeded() {
        assert_eq!(
            classify_failure_reason("context deadline exceeded"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_could_not_resolve_host() {
        assert_eq!(
            classify_failure_reason("could not resolve host api.example.com"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_could_not_resolve_hostname() {
        assert_eq!(
            classify_failure_reason("could not resolve hostname"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_temporary_failure() {
        assert_eq!(
            classify_failure_reason("temporary failure"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_temporary_failure_in_name_resolution() {
        assert_eq!(
            classify_failure_reason("temporary failure in name resolution"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_network_is_unreachable() {
        assert_eq!(
            classify_failure_reason("network is unreachable"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_broken_pipe() {
        assert_eq!(
            classify_failure_reason("broken pipe"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_tls_handshake_timeout() {
        assert_eq!(
            classify_failure_reason("tls handshake timeout"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_io_timeout() {
        assert_eq!(
            classify_failure_reason("i/o timeout"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_no_route_to_host() {
        assert_eq!(
            classify_failure_reason("no route to host"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_temporarily_unavailable() {
        assert_eq!(
            classify_failure_reason("resource temporarily unavailable"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_try_again() {
        assert_eq!(
            classify_failure_reason("try again later"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_too_many_requests() {
        assert_eq!(
            classify_failure_reason("too many requests"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_service_unavailable() {
        assert_eq!(
            classify_failure_reason("service unavailable"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_gateway_timeout() {
        assert_eq!(
            classify_failure_reason("gateway timeout"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_econnrefused() {
        assert_eq!(
            classify_failure_reason("ECONNREFUSED"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_econnreset() {
        assert_eq!(
            classify_failure_reason("ECONNRESET"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_dial_tcp() {
        assert_eq!(
            classify_failure_reason("dial tcp 10.0.0.1:443: connect: connection refused"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_transport_is_closing() {
        assert_eq!(
            classify_failure_reason("transport is closing"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_stream_disconnected() {
        assert_eq!(
            classify_failure_reason("stream disconnected"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_stream_closed_before() {
        assert_eq!(
            classify_failure_reason("stream closed before completion"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_index_crates_io() {
        assert_eq!(
            classify_failure_reason("failed to fetch index.crates.io"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_download_config_json_failed() {
        assert_eq!(
            classify_failure_reason("download of config.json failed"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_toolchain_registry_unavailable() {
        assert_eq!(
            classify_failure_reason("toolchain_or_dependency_registry_unavailable"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_toolchain_dependency_network() {
        assert_eq!(
            classify_failure_reason("toolchain dependency resolution blocked by network"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_toolchain_workspace_io() {
        assert_eq!(
            classify_failure_reason("toolchain_workspace_io"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_cross_device_link() {
        assert_eq!(
            classify_failure_reason("cross-device link"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_invalid_cross_device_link() {
        assert_eq!(
            classify_failure_reason("invalid cross-device link"),
            FailureClass::TransientInfra
        );
    }

    #[test]
    fn classify_reason_os_error_18() {
        assert_eq!(
            classify_failure_reason("os error 18"),
            FailureClass::TransientInfra
        );
    }

    // Structural

    #[test]
    fn classify_reason_write_scope_violation_underscore() {
        assert_eq!(
            classify_failure_reason("write_scope_violation detected"),
            FailureClass::Structural
        );
    }

    #[test]
    fn classify_reason_write_scope_violation_space() {
        assert_eq!(
            classify_failure_reason("write scope violation detected"),
            FailureClass::Structural
        );
    }

    // Default deterministic

    #[test]
    fn classify_reason_default_deterministic() {
        assert_eq!(
            classify_failure_reason("invalid configuration parameter"),
            FailureClass::Deterministic
        );
    }

    // --- normalize_failure_reason tests ---

    #[test]
    fn normalize_empty_and_whitespace_returns_empty() {
        assert_eq!(normalize_failure_reason(""), "");
        assert_eq!(normalize_failure_reason("   "), "");
        assert_eq!(normalize_failure_reason("\n\t"), "");
    }

    #[test]
    fn normalize_lowercases_and_trims() {
        assert_eq!(normalize_failure_reason("  Hello World  "), "hello world");
    }

    #[test]
    fn normalize_replaces_hex_strings() {
        assert_eq!(
            normalize_failure_reason("commit abc123def0"),
            "commit <hex>"
        );
        // Short hex (< 7 chars) not replaced
        assert_eq!(normalize_failure_reason("value abcdef"), "value abcdef");
    }

    #[test]
    fn normalize_replaces_digit_sequences() {
        assert_eq!(normalize_failure_reason("line 42"), "line <n>");
        assert_eq!(normalize_failure_reason("error 0"), "error <n>");
    }

    #[test]
    fn normalize_collapses_comma_space_and_whitespace() {
        assert_eq!(normalize_failure_reason("a,  b,   c"), "a,b,c");
        assert_eq!(normalize_failure_reason("a   b"), "a b");
    }

    #[test]
    fn normalize_truncates_to_240_chars() {
        let long = "a".repeat(300);
        let result = normalize_failure_reason(&long);
        assert_eq!(result.len(), 240);
    }

    #[test]
    fn normalize_truncation_respects_utf8_boundaries() {
        // Build a string of 2-byte chars ("é" is 2 bytes in UTF-8) that crosses
        // the 240 byte boundary mid-character.
        let input = "é".repeat(200); // 400 bytes, each char is 2 bytes
        let result = normalize_failure_reason(&input);
        assert!(result.len() <= 240);
        // Must be valid UTF-8 (String guarantees this, but verify length is even
        // since every char is 2 bytes)
        assert_eq!(result.len() % 2, 0);

        // Also test with a mix: 239 ASCII bytes + a 2-byte char
        let input2 = format!("{}{}", "a".repeat(239), "é");
        let result2 = normalize_failure_reason(&input2);
        assert!(result2.len() <= 240);
        // Should truncate to 239 (dropping the 2-byte char that would push to 241)
        assert_eq!(result2.len(), 239);
    }

    #[test]
    fn normalize_combined_example() {
        assert_eq!(
            normalize_failure_reason("Error at line 42 in abc123def"),
            "error at line <n> in <hex>"
        );
    }

    // --- FailureSignature tests ---

    #[test]
    fn failure_signature_format() {
        let sig = FailureSignature::new(
            "verify",
            FailureClass::Deterministic,
            None,
            Some("test failed"),
        );
        assert_eq!(sig.to_string(), "verify|deterministic|test failed");
    }

    #[test]
    fn failure_signature_display() {
        let sig = FailureSignature::new(
            "build",
            FailureClass::Structural,
            None,
            Some("scope violation"),
        );
        assert_eq!(format!("{sig}"), "build|structural|scope violation");
    }

    #[test]
    fn failure_signature_hint_takes_priority() {
        let sig = FailureSignature::new(
            "verify",
            FailureClass::Deterministic,
            Some("custom hint"),
            Some("raw reason"),
        );
        assert_eq!(sig.to_string(), "verify|deterministic|custom hint");
    }

    #[test]
    fn failure_signature_missing_reason_falls_back_to_unknown() {
        let sig = FailureSignature::new("node", FailureClass::Deterministic, None, None);
        assert_eq!(sig.to_string(), "node|deterministic|unknown");
    }

    #[test]
    fn failure_signature_equality_and_hash() {
        let sig1 = FailureSignature::new(
            "verify",
            FailureClass::Deterministic,
            None,
            Some("test failed"),
        );
        let sig2 = FailureSignature::new(
            "verify",
            FailureClass::Deterministic,
            None,
            Some("test failed"),
        );
        assert_eq!(sig1, sig2);

        let mut map = std::collections::HashMap::new();
        map.insert(sig1.clone(), 1);
        assert_eq!(map.get(&sig2), Some(&1));
    }

    // --- is_signature_tracked tests ---

    #[test]
    fn is_signature_tracked_deterministic_and_structural() {
        assert!(FailureClass::Deterministic.is_signature_tracked());
        assert!(FailureClass::Structural.is_signature_tracked());
    }

    #[test]
    fn is_signature_tracked_false_for_others() {
        assert!(!FailureClass::TransientInfra.is_signature_tracked());
        assert!(!FailureClass::BudgetExhausted.is_signature_tracked());
        assert!(!FailureClass::Canceled.is_signature_tracked());
        assert!(!FailureClass::CompilationLoop.is_signature_tracked());
    }

    // --- failure_signature_hint tests ---

    #[test]
    fn failure_signature_hint_llm_returns_some() {
        let err = FabroError::Llm(SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        });
        assert_eq!(
            err.failure_signature_hint(),
            Some("api_deterministic|openai|authentication".to_string())
        );
    }

    #[test]
    fn failure_signature_hint_handler_returns_none() {
        let err = FabroError::handler("something failed");
        assert_eq!(err.failure_signature_hint(), None);
    }

    #[test]
    fn failure_signature_hint_engine_returns_none() {
        let err = FabroError::engine("engine error");
        assert_eq!(err.failure_signature_hint(), None);
    }

    // --- to_fail_outcome tests ---

    #[test]
    fn to_fail_outcome_llm_has_class_and_signature() {
        let err = FabroError::Llm(SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        });
        let outcome = err.to_fail_outcome();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
        let failure = outcome.failure.as_ref().unwrap();
        assert_eq!(failure.failure_class, FailureClass::Deterministic);
        assert_eq!(
            failure.failure_signature.as_deref(),
            Some("api_deterministic|openai|authentication")
        );
    }

    #[test]
    fn to_fail_outcome_handler_has_class_but_no_signature() {
        let err = FabroError::handler("connection refused");
        let outcome = err.to_fail_outcome();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
        let failure = outcome.failure.as_ref().unwrap();
        assert_eq!(failure.failure_class, FailureClass::TransientInfra);
        assert!(failure.failure_signature.is_none());
    }

    #[test]
    fn to_fail_outcome_includes_error_message_as_reason() {
        let err = FabroError::Llm(SdkError::Network {
            message: "connection refused".into(),
        });
        let outcome = err.to_fail_outcome();
        assert!(outcome
            .failure_reason()
            .unwrap()
            .contains("connection refused"));
    }

    #[test]
    fn to_fail_outcome_no_context_updates() {
        let err = FabroError::Llm(SdkError::Network {
            message: "refused".into(),
        });
        let outcome = err.to_fail_outcome();
        assert!(outcome.context_updates.is_empty());
    }

    // --- Phase 2: Eager classification tests ---

    #[test]
    fn handler_eager_classification() {
        let err = FabroError::handler("connection refused");
        assert_eq!(err.failure_class(), FailureClass::TransientInfra);
    }

    #[test]
    fn handler_eager_classification_roundtrip() {
        let err = FabroError::handler("connection refused");
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: FabroError = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.failure_class(), FailureClass::TransientInfra);
    }

    #[test]
    fn handler_smart_constructor_preserves_message() {
        let err = FabroError::handler("some error");
        assert!(err.to_string().contains("some error"));
    }

    #[test]
    fn engine_eager_classification() {
        let err = FabroError::engine("rate limit exceeded");
        assert_eq!(err.failure_class(), FailureClass::TransientInfra);
    }

    #[test]
    fn arc_error_serde_roundtrip_all_variants() {
        let errors: Vec<FabroError> = vec![
            FabroError::Parse("bad".into()),
            FabroError::Validation("bad".into()),
            FabroError::engine("engine err"),
            FabroError::handler("handler err"),
            FabroError::Llm(SdkError::Network {
                message: "refused".into(),
            }),
            FabroError::Checkpoint("cp err".into()),
            FabroError::Stylesheet("style err".into()),
            FabroError::Io("io err".into()),
            FabroError::Cancelled,
        ];
        for err in &errors {
            let json = serde_json::to_string(err).unwrap();
            let deserialized: FabroError = serde_json::from_str(&json).unwrap();
            assert_eq!(err.to_string(), deserialized.to_string());
        }
    }

    #[test]
    fn handler_display_unchanged() {
        assert_eq!(
            FabroError::handler("LLM call failed").to_string(),
            "Handler error: LLM call failed"
        );
    }

    #[test]
    fn engine_display_unchanged() {
        assert_eq!(
            FabroError::engine("no outgoing edge").to_string(),
            "Engine error: no outgoing edge"
        );
    }

    #[test]
    fn failure_class_stability() {
        let messages = [
            "connection refused",
            "timeout",
            "rate limit",
            "context length exceeded",
            "cancel",
            "invalid configuration",
            "write_scope_violation",
        ];
        for msg in messages {
            assert_eq!(
                FabroError::handler(msg).failure_class(),
                classify_failure_reason(msg),
                "mismatch for message: {msg}"
            );
        }
    }

    #[test]
    fn to_fail_outcome_preserves_class() {
        let err = FabroError::handler("timeout");
        let outcome = err.to_fail_outcome();
        assert_eq!(outcome.failure_class(), Some(FailureClass::TransientInfra));
    }

    // --- E2E error pipeline tests ---

    #[test]
    fn e2e_llm_error_to_outcome_to_event_preserves_classification() {
        use crate::event::WorkflowRunEvent;

        // 1. Create SdkError → FabroError
        let sdk_err = SdkError::Provider {
            kind: ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail::new("too fast", "openai")),
        };
        let arc_err = FabroError::Llm(sdk_err);
        assert_eq!(arc_err.failure_class(), FailureClass::TransientInfra);

        // 2. FabroError → Outcome
        let outcome = arc_err.to_fail_outcome();
        assert_eq!(outcome.failure_class(), Some(FailureClass::TransientInfra));

        // 3. Outcome → StageFailed event
        let failure = outcome.failure.clone().unwrap();
        let event = WorkflowRunEvent::StageFailed {
            node_id: "code".into(),
            name: "code".into(),
            index: 0,
            failure: failure.clone(),
            will_retry: false,
        };

        // 4. Verify classification survived all the way through
        match &event {
            WorkflowRunEvent::StageFailed { failure, .. } => {
                assert_eq!(failure.failure_class, FailureClass::TransientInfra);
            }
            _ => panic!("expected StageFailed"),
        }
    }

    #[test]
    fn e2e_handler_error_classified_at_edge() {
        // handler smart constructor classifies eagerly
        let err = FabroError::handler("connection refused");
        assert_eq!(err.failure_class(), FailureClass::TransientInfra);

        // to_fail_outcome preserves
        let outcome = err.to_fail_outcome();
        assert_eq!(outcome.failure_class(), Some(FailureClass::TransientInfra));

        // event preserves
        let failure = outcome.failure.unwrap();
        assert_eq!(failure.failure_class, FailureClass::TransientInfra);
    }

    #[test]
    fn e2e_handler_retryable_checks() {
        assert!(FabroError::handler("timeout").is_retryable());
        assert!(FabroError::handler("auth error").is_retryable());
    }

    #[test]
    fn e2e_serde_stability_arc_error() {
        let err = FabroError::handler("connection refused");
        let json = serde_json::to_string(&err).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Verify wire format
        assert_eq!(v["type"], "handler");
        assert!(v["data"]["message"]
            .as_str()
            .unwrap()
            .contains("connection refused"));
        assert_eq!(v["data"]["failure_class"], "transient_infra");

        // Round-trip
        let deserialized: FabroError = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.failure_class(), FailureClass::TransientInfra);
    }

    #[test]
    fn e2e_serde_stability_agent_error() {
        use fabro_agent::error::AgentError;

        let err = AgentError::Llm(SdkError::Provider {
            kind: ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail::new("too fast", "openai")),
        });
        let json = serde_json::to_string(&err).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "llm");

        let deserialized: AgentError = serde_json::from_str(&json).unwrap();
        assert_eq!(err.to_string(), deserialized.to_string());
    }

    #[test]
    fn e2e_failure_detail_in_outcome_serde_roundtrip() {
        use crate::outcome::Outcome;

        let outcome = Outcome::fail_classify("rate limit exceeded")
            .with_signature(Some("api_transient|openai|rate_limited"));

        let json = serde_json::to_string(&outcome).unwrap();
        let deserialized: Outcome = serde_json::from_str(&json).unwrap();

        let failure = deserialized.failure.unwrap();
        assert_eq!(failure.message, "rate limit exceeded");
        assert_eq!(failure.failure_class, FailureClass::TransientInfra);
        assert_eq!(
            failure.failure_signature.as_deref(),
            Some("api_transient|openai|rate_limited")
        );
    }
}
