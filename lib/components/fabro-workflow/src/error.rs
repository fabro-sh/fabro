use std::borrow::Cow;
use std::fmt;
use std::sync::{Arc, LazyLock};

use fabro_graphviz::Error as GraphvizError;
use fabro_llm::{Error as LlmError, ProviderErrorKind};
use fabro_model::ModelSelectionError;
use fabro_template::TemplateError;
pub use fabro_types::failure_signature::FailureSignature;
pub use fabro_types::outcome::FailureCategory;
use fabro_types::settings::{AmbiguousModelRef, ResolveError};
use fabro_types::{ExecOutputTail, FailureReason, RunFailure};
use fabro_util::error::{SharedError, collect_causes, collect_chain, render_with_causes};
use fabro_validate::Diagnostic;
use regex::Regex;
use thiserror::Error as ThisError;

use crate::event::RunEventPersistenceError;
use crate::outcome::{FailureDetail, Outcome, StageOutcome};

/// Classify an LLM error into a `FailureCategory` based on its structure.
#[must_use]
pub fn classify_sdk_error(err: &LlmError) -> FailureCategory {
    match err {
        LlmError::Provider { kind, .. } => match kind {
            ProviderErrorKind::RateLimit | ProviderErrorKind::Server => {
                FailureCategory::TransientInfra
            }
            ProviderErrorKind::ContextLength | ProviderErrorKind::QuotaExceeded => {
                FailureCategory::BudgetExhausted
            }
            ProviderErrorKind::Authentication
            | ProviderErrorKind::AccessDenied
            | ProviderErrorKind::NotFound
            | ProviderErrorKind::InvalidRequest
            | ProviderErrorKind::ContentFilter => FailureCategory::Deterministic,
        },
        LlmError::RequestTimeout { .. } | LlmError::Network { .. } | LlmError::Stream { .. } => {
            FailureCategory::TransientInfra
        }
        LlmError::Interrupt { .. } => FailureCategory::Canceled,
        LlmError::InvalidToolCall { .. }
        | LlmError::NoObjectGenerated { .. }
        | LlmError::InvalidRequest { .. }
        | LlmError::Configuration { .. }
        | LlmError::UnsupportedToolChoice { .. } => FailureCategory::Deterministic,
    }
}

/// Failures caused by a permanent provider spend/billing ceiling.
///
/// These are permanent and human-actionable: only a person raising the limit
/// clears them, so retrying is always wasted work. This list is checked before
/// `TRANSIENT_INFRA_HINTS` so an explicit permanent ceiling wins if the same
/// provider payload also contains a transient phrase such as `rate limited`.
const PERMANENT_BUDGET_EXHAUSTED_HINTS: &[&str] = &["spend limit"];

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
    "state change in progress",
    "sandbox stop still in progress",
];

const BUDGET_EXHAUSTED_HINTS: &[&str] = &[
    "turn limit",
    "token limit",
    "context length",
    "budget",
    "quota exceeded",
    "max_tokens",
    "max tokens",
    "context window exceeded",
    "budget exhausted",
    "token limit exceeded",
    "you've hit your limit",
];

const STRUCTURAL_HINTS: &[&str] = &[
    "write_scope_violation",
    "write scope violation",
    "scope violation",
];

#[derive(Debug, Clone)]
pub struct SharedTemplateError(Arc<TemplateError>);

impl SharedTemplateError {
    #[must_use]
    pub fn new(error: TemplateError) -> Self {
        Self(Arc::new(error))
    }

    #[must_use]
    pub fn inner(&self) -> &TemplateError {
        &self.0
    }
}

impl fmt::Display for SharedTemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::error::Error for SharedTemplateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl miette::Diagnostic for SharedTemplateError {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        miette::Diagnostic::code(self.inner())
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        miette::Diagnostic::help(self.inner())
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        miette::Diagnostic::source_code(self.inner())
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        miette::Diagnostic::labels(self.inner())
    }

    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        miette::Diagnostic::diagnostic_source(self.inner())
    }
}

/// Matches git SHAs and other long hex blobs.
static HEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[0-9a-f]{7,64}\b").expect("hardcoded regex should compile"));

/// Blank a Cargo registry source path in a structured `spawned_at` field.
///
/// ACP internal errors include this field as provenance. Its value identifies
/// where the provider-side process raised the error; it is not fault text.
/// Removing only this structured value prevents the `index.crates.io` path
/// component from triggering a transient hint without parsing arbitrary prose
/// or consuming fault text outside the matched field value.
fn discount_cargo_registry_source_paths(reason: &str) -> Cow<'_, str> {
    static JSON_STRING_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?P<field_prefix>"(?P<field_name>(?:\\.|[^"\\])*)"\s*:\s*")(?P<field_value>(?:\\.|[^"\\])*)(?P<field_end>")"#,
        )
        .expect("hardcoded regex should compile")
    });
    static REGISTRY_SRC_MARKER_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r#"(?:^|[/\\])registry[/\\]+src[/\\]+"#,
            r#"[a-z0-9._-]+-(?:[a-f0-9]{7,64}|<hex>)"#,
            r#"(?:[/\\]|$)"#,
        ))
        .expect("hardcoded regex should compile")
    });

    JSON_STRING_FIELD_RE.replace_all(reason, |captures: &regex::Captures<'_>| {
        let field_name = captures
            .name("field_name")
            .expect("hardcoded regex always captures the field name")
            .as_str();
        let decoded_field_name = decode_json_string_contents(field_name);
        if !decoded_field_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("spawned_at"))
        {
            return captures
                .get(0)
                .expect("capture zero always contains the full regex match")
                .as_str()
                .to_string();
        }

        let value = captures
            .name("field_value")
            .expect("hardcoded regex always captures the field value")
            .as_str();
        let Some(decoded_value) = decode_json_string_contents(value) else {
            return captures
                .get(0)
                .expect("capture zero always contains the full regex match")
                .as_str()
                .to_string();
        };
        let decoded_value = decoded_value.to_lowercase();
        let is_url = spawned_at_value_is_url(&decoded_value);
        if is_url || !REGISTRY_SRC_MARKER_RE.is_match(&decoded_value) {
            return captures
                .get(0)
                .expect("capture zero always contains the full regex match")
                .as_str()
                .to_string();
        }

        format!(
            "{} {}",
            captures
                .name("field_prefix")
                .expect("hardcoded regex always captures the field prefix")
                .as_str(),
            captures
                .name("field_end")
                .expect("hardcoded regex always captures the field terminator")
                .as_str(),
        )
    })
}

fn decode_json_string_contents(serialized_value: &str) -> Option<String> {
    serde_json::from_str(&format!(r#""{serialized_value}""#)).ok()
}

/// Whether decoded `spawned_at` text describes a URL.
fn spawned_at_value_is_url(value: &str) -> bool {
    if value.starts_with("//") {
        return true;
    }

    value.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            && scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    })
}

/// Classify a failure reason string using heuristics.
///
/// This is the fallback when structured error information is not available
/// (e.g. for `Handler(String)` or `Engine(String)` errors).
///
/// Hint lists are consulted in order and the first match wins, so the order is
/// load-bearing: permanent provider limits are tested before transient infra
/// precisely because their payloads also contain transient-looking text.
#[must_use]
pub fn classify_failure_reason(reason: &str) -> FailureCategory {
    // Discount structured provenance before normalizing case or masking hex.
    // JSON escapes are case-sensitive, so lowercasing first could turn an
    // invalid string into a valid one and cross the structured boundary.
    // Masking first could rewrite a registry identifier or URI scheme and make
    // its semantics impossible to recognize. Commit SHAs are then masked
    // because one contains "500" or "503" often enough to read as a transient
    // hint. The bare status codes those hints look for are too short to be
    // masked.
    let reason = discount_cargo_registry_source_paths(reason);
    let lowered = reason.to_lowercase();
    let lower = HEX_RE.replace_all(&lowered, "<hex>");

    if lower.contains("interrupt")
        || (lower.contains("cancel")
            && !lower.contains("cancelling due to test failure")
            && !lower.contains("canceling due to test failure"))
    {
        return FailureCategory::Canceled;
    }

    if PERMANENT_BUDGET_EXHAUSTED_HINTS
        .iter()
        .any(|hint| lower.contains(hint))
    {
        return FailureCategory::BudgetExhausted;
    }

    if TRANSIENT_INFRA_HINTS
        .iter()
        .any(|hint| lower.contains(hint))
    {
        return FailureCategory::TransientInfra;
    }

    if BUDGET_EXHAUSTED_HINTS
        .iter()
        .any(|hint| lower.contains(hint))
    {
        return FailureCategory::BudgetExhausted;
    }

    if STRUCTURAL_HINTS.iter().any(|hint| lower.contains(hint)) {
        return FailureCategory::Structural;
    }

    FailureCategory::Deterministic
}

/// Normalize a failure reason for stable signature grouping.
///
/// Replaces variable data (hex strings, digits) with placeholders so that
/// semantically identical errors produce the same signature regardless of
/// line numbers, commit hashes, or timestamps.
pub fn normalize_failure_reason(reason: &str) -> String {
    static DIGITS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b\d+\b").expect("hardcoded regex should compile"));
    static COMMA_SPACE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r",\s+").expect("hardcoded regex should compile"));
    static WHITESPACE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s+").expect("hardcoded regex should compile"));

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

pub trait FailureSignatureExt {
    fn new(
        node_id: &str,
        failure_class: FailureCategory,
        signature_hint: Option<&str>,
        failure_reason: Option<&str>,
    ) -> Self;
}

impl FailureSignatureExt for FailureSignature {
    fn new(
        node_id: &str,
        failure_class: FailureCategory,
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

/// Pipeline stage that produced an [`Error::Stage`].
///
/// The three stages share a failure shape — a message, an eagerly classified
/// [`FailureCategory`], an optional command output tail, and an optional
/// source — and differ only in where they run and whether a retry is possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub enum ErrorStage {
    /// A node handler failed. Retryable: the engine can re-run the node.
    Handler,
    /// The engine itself failed while driving the graph. Retryable.
    Engine,
    /// The publish stage failed. Terminal: publish runs once, after execution.
    Publish,
}

#[derive(ThisError, Debug, Clone)]
pub enum Error {
    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Validation failed")]
    ValidationFailed { diagnostics: Vec<Diagnostic> },

    #[error("Validation error: script interpolation failed in {owner}: {source} ({fix})")]
    ScriptInterpolation {
        owner:  String,
        fix:    String,
        #[source]
        source: ResolveError,
    },

    #[error("Model selection failed: {0}")]
    ModelSelection(#[from] ModelSelectionError),

    #[error("Model reference failed: {0}")]
    ModelReference(#[from] AmbiguousModelRef),

    #[error("{message}")]
    Template {
        message: String,
        #[source]
        source:  SharedTemplateError,
    },

    #[error("{stage} error: {message}")]
    Stage {
        stage:            ErrorStage,
        message:          String,
        failure_class:    FailureCategory,
        exec_output_tail: Option<ExecOutputTail>,
        /// Structured context lines appended after the source chain in
        /// `causes()` — e.g. one line per push attempt on a publish push
        /// failure.
        extra_causes:     Vec<String>,
        #[source]
        source:           Option<SharedError>,
    },

    #[error("LLM error: {0}")]
    Llm(LlmError),

    #[error("Checkpoint error: {0}")]
    Checkpoint(String),

    #[error("Stylesheet error: {0}")]
    Stylesheet(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("Precondition failed: {0}")]
    Precondition(String),

    #[error("Run not found: {0}")]
    RunNotFound(String),

    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("{0}")]
    OutputSchemaValidation(String),

    #[error("Pipeline cancelled")]
    Cancelled,
}

impl Error {
    /// Smart constructor for Handler errors. Classifies the failure reason
    /// eagerly.
    /// Build a stage error, classifying the message eagerly.
    fn stage(
        stage: ErrorStage,
        message: impl Into<String>,
        exec_output_tail: Option<ExecOutputTail>,
    ) -> Self {
        let message = message.into();
        let failure_class = classify_failure_reason(&message);
        Self::Stage {
            stage,
            message,
            failure_class,
            exec_output_tail,
            extra_causes: Vec::new(),
            source: None,
        }
    }

    /// Build a stage error from a source, classifying the rendered chain so
    /// hints buried in the causes still reach [`Self::failure_category`].
    fn stage_with_source(
        stage: ErrorStage,
        message: impl Into<String>,
        source: impl Into<anyhow::Error>,
        exec_output_tail: Option<ExecOutputTail>,
    ) -> Self {
        Self::stage_with_source_details(stage, message, source, None, exec_output_tail, Vec::new())
    }

    fn stage_with_source_details(
        stage: ErrorStage,
        message: impl Into<String>,
        source: impl Into<anyhow::Error>,
        failure_class: Option<FailureCategory>,
        exec_output_tail: Option<ExecOutputTail>,
        extra_causes: Vec<String>,
    ) -> Self {
        let message = message.into();
        let source = SharedError::new(source.into());
        let failure_class = failure_class.unwrap_or_else(|| {
            classify_failure_reason(&render_with_causes(&message, &collect_chain(&source)))
        });
        Self::Stage {
            stage,
            message,
            failure_class,
            exec_output_tail,
            extra_causes,
            source: Some(source),
        }
    }

    pub fn handler(message: impl Into<String>) -> Self {
        Self::stage(ErrorStage::Handler, message, None)
    }

    pub fn template(message: impl Into<String>, source: TemplateError) -> Self {
        Self::Template {
            message: message.into(),
            source:  SharedTemplateError::new(source),
        }
    }

    pub fn handler_with_exec_output_tail(
        message: impl Into<String>,
        exec_output_tail: Option<ExecOutputTail>,
    ) -> Self {
        Self::stage(ErrorStage::Handler, message, exec_output_tail)
    }

    pub fn handler_with_source(
        message: impl Into<String>,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self::handler_with_source_and_exec_output_tail(message, source, None)
    }

    pub fn handler_with_source_and_exec_output_tail(
        message: impl Into<String>,
        source: impl Into<anyhow::Error>,
        exec_output_tail: Option<ExecOutputTail>,
    ) -> Self {
        Self::stage_with_source(ErrorStage::Handler, message, source, exec_output_tail)
    }

    pub fn handler_with_anyhow(message: impl Into<String>, source: anyhow::Error) -> Self {
        Self::handler_with_source(message, source)
    }

    pub fn engine(message: impl Into<String>) -> Self {
        Self::stage(ErrorStage::Engine, message, None)
    }

    pub fn engine_with_source(
        message: impl Into<String>,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self::stage_with_source(ErrorStage::Engine, message, source, None)
    }

    pub fn engine_with_anyhow(message: impl Into<String>, source: anyhow::Error) -> Self {
        Self::engine_with_source(message, source)
    }

    /// Build an error for the required publish stage.
    pub fn publish(message: impl Into<String>) -> Self {
        Self::stage(ErrorStage::Publish, message, None)
    }

    pub fn publish_with_source(
        message: impl Into<String>,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self::publish_with_source_and_exec_output_tail(message, source, None)
    }

    pub fn publish_with_source_and_exec_output_tail(
        message: impl Into<String>,
        source: impl Into<anyhow::Error>,
        exec_output_tail: Option<ExecOutputTail>,
    ) -> Self {
        Self::stage_with_source(ErrorStage::Publish, message, source, exec_output_tail)
    }

    /// Build a publish error with an explicitly determined failure category,
    /// for callers that know more than message sniffing can recover — e.g.
    /// exhausted push retries whose attempts all classified as transient.
    /// `extra_causes` lines land after the source chain in the failure
    /// detail (one line per push attempt).
    pub fn publish_with_source_and_class(
        message: impl Into<String>,
        source: impl Into<anyhow::Error>,
        failure_class: FailureCategory,
        exec_output_tail: Option<ExecOutputTail>,
        extra_causes: Vec<String>,
    ) -> Self {
        Self::stage_with_source_details(
            ErrorStage::Publish,
            message,
            source,
            Some(failure_class),
            exec_output_tail,
            extra_causes,
        )
    }

    #[must_use]
    pub fn causes(&self) -> Vec<String> {
        match self {
            Self::Stage {
                source,
                extra_causes,
                ..
            } => {
                let mut causes = source
                    .as_ref()
                    .map_or_else(Vec::new, |source| collect_chain(source));
                causes.extend(extra_causes.iter().cloned());
                causes
            }
            Self::Template { source, .. } => collect_chain(source),
            Self::ScriptInterpolation { source, .. } => collect_chain(source),
            Self::Llm(err) => collect_causes(err),
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub fn display_with_causes(&self) -> String {
        render_with_causes(&self.to_string(), &self.causes())
    }

    /// Whether this error category is retryable (transient) or terminal.
    ///
    /// Retryable: Handler and Engine stages (the engine can re-run the node),
    /// I/O, and LLM errors the SDK marks retryable. Terminal: the Publish
    /// stage (it runs once, after execution), Parse, Validation,
    /// OutputSchemaValidation, Stylesheet, Checkpoint, and Cancelled.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Io(_) => true,
            Self::Stage { stage, .. } => {
                matches!(stage, ErrorStage::Handler | ErrorStage::Engine)
            }
            Self::Llm(sdk_err) => sdk_err.retryable(),
            Self::Parse(_)
            | Self::Validation(_)
            | Self::ValidationFailed { .. }
            | Self::ScriptInterpolation { .. }
            | Self::ModelSelection(_)
            | Self::ModelReference(_)
            | Self::Template { .. }
            | Self::Stylesheet(_)
            | Self::Checkpoint(_)
            | Self::Precondition(_)
            | Self::RunNotFound(_)
            | Self::Unsupported(_)
            | Self::OutputSchemaValidation(_)
            | Self::Cancelled => false,
        }
    }

    /// Classify this error into a `FailureCategory`.
    #[must_use]
    pub fn failure_category(&self) -> FailureCategory {
        match self {
            Self::Cancelled => FailureCategory::Canceled,
            Self::Llm(sdk_err) => classify_sdk_error(sdk_err),
            Self::Io(_) => FailureCategory::TransientInfra,
            Self::Parse(_)
            | Self::Validation(_)
            | Self::ValidationFailed { .. }
            | Self::ScriptInterpolation { .. }
            | Self::ModelSelection(_)
            | Self::ModelReference(_)
            | Self::Template { .. }
            | Self::Stylesheet(_)
            | Self::Checkpoint(_)
            | Self::Unsupported(_)
            | Self::OutputSchemaValidation(_) => FailureCategory::Deterministic,
            Self::Precondition(_) | Self::RunNotFound(_) => FailureCategory::Structural,
            Self::Stage { failure_class, .. } => *failure_class,
        }
    }

    /// The terminal [`FailureReason`] this error maps to on a run.
    #[must_use]
    pub fn failure_reason(&self) -> FailureReason {
        match self {
            Self::Cancelled => FailureReason::Cancelled,
            Self::Stage {
                stage: ErrorStage::Publish,
                ..
            } => FailureReason::PublishFailed,
            _ => FailureReason::WorkflowError,
        }
    }

    /// Return a stable failure signature hint when structured error info is
    /// available.
    #[must_use]
    pub fn failure_signature_hint(&self) -> Option<FailureSignature> {
        match self {
            Self::Llm(sdk_err) => Some(FailureSignature(sdk_err.failure_signature_hint())),
            _ => None,
        }
    }

    #[must_use]
    pub fn to_failure_detail(&self) -> FailureDetail {
        let (message, explicit_exec_output_tail) = match self {
            Self::Stage {
                message,
                exec_output_tail,
                ..
            } => (message.clone(), exec_output_tail.clone()),
            _ => (self.to_string(), None),
        };
        FailureDetail {
            message,
            causes: self.causes(),
            category: self.failure_category(),
            system_actor: None,
            signature: self.failure_signature_hint(),
            exec_output_tail: explicit_exec_output_tail
                .or_else(|| fabro_sandbox::default_redacted_output_tail(self)),
        }
    }

    /// Build a fail `Outcome` with structured `FailureDetail`.
    pub fn to_fail_outcome(&self) -> Outcome {
        let failure = self.to_failure_detail();
        Outcome {
            status: StageOutcome::Failed {
                retry_requested: false,
            },
            failure: Some(failure),
            ..Outcome::success()
        }
    }
}

impl miette::Diagnostic for Error {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        match self {
            Self::Template { source, .. } => miette::Diagnostic::code(source),
            _ => None,
        }
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        match self {
            Self::Template { source, .. } => miette::Diagnostic::help(source),
            _ => None,
        }
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        match self {
            Self::Template { source, .. } => miette::Diagnostic::source_code(source),
            _ => None,
        }
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        match self {
            Self::Template { source, .. } => miette::Diagnostic::labels(source),
            _ => None,
        }
    }

    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        match self {
            Self::Template { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[must_use]
pub fn run_failure_from_error(error: &Error, reason: FailureReason) -> RunFailure {
    RunFailure {
        reason,
        detail: error.to_failure_detail(),
    }
}

#[must_use]
pub fn run_failure_from_outcome_failure(
    failure: &FailureDetail,
    reason: FailureReason,
) -> RunFailure {
    RunFailure {
        reason,
        detail: failure.clone(),
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<LlmError> for Error {
    fn from(err: LlmError) -> Self {
        Self::Llm(err)
    }
}

impl From<GraphvizError> for Error {
    fn from(e: GraphvizError) -> Self {
        match e {
            GraphvizError::Parse(msg) => Self::Parse(msg),
            GraphvizError::Stylesheet(msg) => Self::Stylesheet(msg),
        }
    }
}

impl From<fabro_template::TemplateError> for Error {
    fn from(err: fabro_template::TemplateError) -> Self {
        let rendered = collect_chain(&err).join(": ");
        Self::template(format!("template expansion failed: {rendered}"), err)
    }
}

impl From<fabro_validate::ValidationError> for Error {
    fn from(e: fabro_validate::ValidationError) -> Self {
        Self::Validation(e.0)
    }
}

impl From<RunEventPersistenceError> for Error {
    fn from(err: RunEventPersistenceError) -> Self {
        Self::engine_with_source("run event persistence failed", err)
    }
}

impl From<fabro_checkpoint::MetadataError> for Error {
    fn from(err: fabro_checkpoint::MetadataError) -> Self {
        match err {
            err @ fabro_checkpoint::MetadataError::Deserialize {
                entity: "checkpoint",
                ..
            } => Self::Checkpoint(err.to_string()),
            err => {
                let message = err.to_string();
                Self::engine_with_source(message, err)
            }
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use fabro_checkpoint::MetadataError;
    use fabro_llm::{Error as SdkError, ProviderErrorDetail};

    use super::*;
    use crate::outcome::OutcomeExt;

    #[derive(Debug)]
    struct TestCause(&'static str);

    impl std::fmt::Display for TestCause {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for TestCause {}

    #[derive(Debug)]
    struct TestOuterError {
        message: &'static str,
        source:  TestCause,
    }

    impl std::fmt::Display for TestOuterError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.message)
        }
    }

    impl std::error::Error for TestOuterError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.source)
        }
    }

    #[test]
    fn parse_error_display() {
        let err = Error::Parse("unexpected token".to_string());
        assert_eq!(err.to_string(), "Parse error: unexpected token");
    }

    #[test]
    fn validation_error_display() {
        let err = Error::Validation("missing start node".to_string());
        assert_eq!(err.to_string(), "Validation error: missing start node");
    }

    #[test]
    fn validation_failed_display() {
        let err = Error::ValidationFailed {
            diagnostics: vec![Diagnostic {
                rule: "test".to_string(),
                severity: fabro_validate::Severity::Error,
                message: "missing start node".to_string(),
                node_id: None,
                edge: None,
                fix: None,

                ..Diagnostic::default()
            }],
        };
        assert_eq!(err.to_string(), "Validation failed");
    }

    #[test]
    fn template_error_variant_preserves_source_chain() {
        let template_err = fabro_template::render_named(
            "workflow.fabro",
            "{{ inputs.missing }}",
            &fabro_template::TemplateContext::new(),
        )
        .unwrap_err();

        let err = Error::template("template expansion failed", template_err);
        let chain = collect_chain(&err);

        assert!(
            chain
                .iter()
                .any(|part| part.contains("template expansion failed"))
        );
        assert!(
            chain
                .iter()
                .any(|part| part.contains("undefined template variable"))
        );
    }

    #[test]
    fn engine_error_display() {
        let err = Error::engine("no outgoing edge");
        assert_eq!(err.to_string(), "Engine error: no outgoing edge");
    }

    #[test]
    fn engine_error_with_source_preserves_cause_chain() {
        let source = TestOuterError {
            message: "Failed to pull Docker image buildpack-deps:noble",
            source:  TestCause("connection refused"),
        };
        let err = Error::engine_with_source("Failed to initialize sandbox", source);

        assert_eq!(
            err.to_string(),
            "Engine error: Failed to initialize sandbox"
        );
        assert_eq!(err.causes(), vec![
            "Failed to pull Docker image buildpack-deps:noble".to_string(),
            "connection refused".to_string(),
        ]);
        assert_eq!(
            err.display_with_causes(),
            "Engine error: Failed to initialize sandbox\n  caused by: Failed to pull Docker image buildpack-deps:noble\n  caused by: connection refused"
        );
        assert_eq!(err.failure_category(), FailureCategory::TransientInfra);
    }

    #[test]
    fn engine_error_with_sandbox_state_change_cause_classifies_transient() {
        let source = TestOuterError {
            message: "Failed to start Daytona sandbox",
            source:  TestCause("Sandbox state change in progress"),
        };
        let err = Error::engine_with_source("Pipeline lifecycle operation failed", source);

        assert_eq!(err.failure_category(), FailureCategory::TransientInfra);
        assert!(err.is_retryable());
    }

    #[test]
    fn handler_error_display() {
        let err = Error::handler("LLM call failed");
        assert_eq!(err.to_string(), "Handler error: LLM call failed");
    }

    #[test]
    fn checkpoint_error_display() {
        let err = Error::Checkpoint("file not found".to_string());
        assert_eq!(err.to_string(), "Checkpoint error: file not found");
    }

    #[test]
    fn io_error_display() {
        let err = Error::Io("permission denied".to_string());
        assert_eq!(err.to_string(), "I/O error: permission denied");
    }

    #[test]
    fn io_error_from_std() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err = Error::from(io_err);
        assert!(matches!(err, Error::Io(_)));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn result_type_alias_works() {
        let ok: Result<i32> = Ok(42);
        assert!(ok.is_ok());

        let err: Result<i32> = Err(Error::Parse("bad".to_string()));
        assert!(err.is_err());
    }

    #[test]
    fn metadata_checkpoint_deserialize_error_preserves_source_detail() {
        let source = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let source_message = source.to_string();
        let fabro_error = Error::from(MetadataError::Deserialize {
            entity: "checkpoint",
            branch: "fabro/meta/run-1".to_string(),
            source,
        });

        assert!(matches!(fabro_error, Error::Checkpoint(_)));
        let message = fabro_error.to_string();
        assert!(message.contains("deserialize checkpoint on branch fabro/meta/run-1"));
        assert!(message.contains(&source_message));
    }

    #[test]
    fn metadata_non_checkpoint_deserialize_error_maps_to_engine_with_source_detail() {
        let source = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let source_message = source.to_string();
        let fabro_error = Error::from(MetadataError::Deserialize {
            entity: "run spec",
            branch: "fabro/meta/run-1".to_string(),
            source,
        });

        assert!(matches!(fabro_error, Error::Stage {
            stage: ErrorStage::Engine,
            ..
        }));
        let message = fabro_error.to_string();
        assert!(message.contains("deserialize run spec on branch fabro/meta/run-1"));
        assert!(message.contains(&source_message));
    }

    #[test]
    fn cancelled_error_display() {
        let err = Error::Cancelled;
        assert_eq!(err.to_string(), "Pipeline cancelled");
    }

    #[test]
    fn cancelled_is_not_retryable() {
        assert!(!Error::Cancelled.is_retryable());
    }

    #[test]
    fn is_retryable_terminal_errors() {
        assert!(!Error::Parse("bad".to_string()).is_retryable());
        assert!(!Error::Validation("bad".to_string()).is_retryable());
        assert!(
            !Error::ValidationFailed {
                diagnostics: vec![],
            }
            .is_retryable()
        );
        assert!(!Error::Stylesheet("bad".to_string()).is_retryable());
        assert!(!Error::Checkpoint("bad".to_string()).is_retryable());
    }

    #[test]
    fn is_retryable_transient_errors() {
        assert!(Error::handler("timeout").is_retryable());
        assert!(Error::engine("transient").is_retryable());
        assert!(Error::Io("connection reset".to_string()).is_retryable());
    }

    // --- FailureCategory Display/FromStr/serde tests ---

    #[test]
    fn failure_class_display_all_values() {
        assert_eq!(
            FailureCategory::TransientInfra.to_string(),
            "transient_infra"
        );
        assert_eq!(FailureCategory::Deterministic.to_string(), "deterministic");
        assert_eq!(
            FailureCategory::BudgetExhausted.to_string(),
            "budget_exhausted"
        );
        assert_eq!(
            FailureCategory::CompilationLoop.to_string(),
            "compilation_loop"
        );
        assert_eq!(FailureCategory::Canceled.to_string(), "canceled");
        assert_eq!(FailureCategory::Structural.to_string(), "structural");
    }

    #[test]
    fn failure_class_from_str_all_values() {
        assert_eq!(
            "transient_infra".parse::<FailureCategory>().unwrap(),
            FailureCategory::TransientInfra
        );
        assert_eq!(
            "deterministic".parse::<FailureCategory>().unwrap(),
            FailureCategory::Deterministic
        );
        assert_eq!(
            "budget_exhausted".parse::<FailureCategory>().unwrap(),
            FailureCategory::BudgetExhausted
        );
        assert_eq!(
            "compilation_loop".parse::<FailureCategory>().unwrap(),
            FailureCategory::CompilationLoop
        );
        assert_eq!(
            "canceled".parse::<FailureCategory>().unwrap(),
            FailureCategory::Canceled
        );
        assert_eq!(
            "structural".parse::<FailureCategory>().unwrap(),
            FailureCategory::Structural
        );
    }

    #[test]
    fn failure_class_from_str_invalid() {
        assert_eq!(
            "unknown".parse::<FailureCategory>().unwrap(),
            FailureCategory::Deterministic
        );
    }

    #[test]
    fn failure_class_from_str_alias_retryable() {
        assert_eq!(
            "retryable".parse::<FailureCategory>().unwrap(),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn failure_class_from_str_alias_transient() {
        assert_eq!(
            "transient".parse::<FailureCategory>().unwrap(),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn failure_class_from_str_alias_permanent() {
        assert_eq!(
            "permanent".parse::<FailureCategory>().unwrap(),
            FailureCategory::Deterministic
        );
    }

    #[test]
    fn failure_class_from_str_alias_cancelled_british() {
        assert_eq!(
            "cancelled".parse::<FailureCategory>().unwrap(),
            FailureCategory::Canceled
        );
    }

    #[test]
    fn failure_class_from_str_alias_budget() {
        assert_eq!(
            "budget".parse::<FailureCategory>().unwrap(),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn failure_class_from_str_alias_compile_loop() {
        assert_eq!(
            "compile_loop".parse::<FailureCategory>().unwrap(),
            FailureCategory::CompilationLoop
        );
    }

    #[test]
    fn failure_class_from_str_alias_scope_violation() {
        assert_eq!(
            "scope_violation".parse::<FailureCategory>().unwrap(),
            FailureCategory::Structural
        );
    }

    #[test]
    fn failure_class_from_str_unknown_defaults_deterministic() {
        assert_eq!(
            "garbage_xyz".parse::<FailureCategory>().unwrap(),
            FailureCategory::Deterministic
        );
    }

    #[test]
    fn failure_class_from_str_case_insensitive() {
        assert_eq!(
            "TRANSIENT_INFRA".parse::<FailureCategory>().unwrap(),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn failure_class_from_str_trims_whitespace() {
        assert_eq!(
            " transient_infra ".parse::<FailureCategory>().unwrap(),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn failure_class_from_str_empty_defaults_deterministic() {
        assert_eq!(
            "".parse::<FailureCategory>().unwrap(),
            FailureCategory::Deterministic
        );
    }

    #[test]
    fn failure_class_serde_roundtrip() {
        let values = [
            FailureCategory::TransientInfra,
            FailureCategory::Deterministic,
            FailureCategory::BudgetExhausted,
            FailureCategory::CompilationLoop,
            FailureCategory::Canceled,
            FailureCategory::Structural,
        ];
        for fc in values {
            let json = serde_json::to_string(&fc).unwrap();
            let parsed: FailureCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, fc);
        }
    }

    // --- Llm variant tests ---

    #[test]
    fn llm_error_display() {
        let sdk_err = SdkError::Network {
            message: "connection refused".into(),
            source:  None,
        };
        let err = Error::Llm(sdk_err);
        assert_eq!(
            err.to_string(),
            "LLM error: Network error: connection refused"
        );
    }

    #[test]
    fn llm_error_retryable_delegates_to_sdk() {
        let retryable = Error::Llm(SdkError::Network {
            message: "timeout".into(),
            source:  None,
        });
        assert!(retryable.is_retryable());

        let non_retryable = Error::Llm(SdkError::Configuration {
            message: "bad config".into(),
            source:  None,
        });
        assert!(!non_retryable.is_retryable());
    }

    #[test]
    fn llm_error_from_sdk_error() {
        let sdk_err = SdkError::Stream {
            message: "broken pipe".into(),
            source:  None,
        };
        let err = Error::from(sdk_err);
        assert!(matches!(err, Error::Llm(_)));
    }

    // --- failure_class() method tests ---

    #[test]
    fn failure_class_cancelled() {
        assert_eq!(
            Error::Cancelled.failure_category(),
            FailureCategory::Canceled
        );
    }

    #[test]
    fn failure_class_io() {
        assert_eq!(
            Error::Io("disk full".into()).failure_category(),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn failure_class_parse() {
        assert_eq!(
            Error::Parse("bad syntax".into()).failure_category(),
            FailureCategory::Deterministic
        );
    }

    #[test]
    fn failure_class_handler_with_timeout() {
        assert_eq!(
            Error::handler("request timed out").failure_category(),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn failure_class_handler_deterministic() {
        assert_eq!(
            Error::handler("invalid configuration").failure_category(),
            FailureCategory::Deterministic
        );
    }

    #[test]
    fn failure_class_llm_rate_limit() {
        let err = Error::Llm(SdkError::Provider {
            kind:   ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail::new("too fast", "openai")),
        });
        assert_eq!(err.failure_category(), FailureCategory::TransientInfra);
    }

    #[test]
    fn failure_class_llm_context_length() {
        let err = Error::Llm(SdkError::Provider {
            kind:   ProviderErrorKind::ContextLength,
            detail: Box::new(ProviderErrorDetail::new("too long", "openai")),
        });
        assert_eq!(err.failure_category(), FailureCategory::BudgetExhausted);
    }

    #[test]
    fn failure_class_llm_auth() {
        let err = Error::Llm(SdkError::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        });
        assert_eq!(err.failure_category(), FailureCategory::Deterministic);
    }

    #[test]
    fn failure_class_llm_abort() {
        let err = Error::Llm(SdkError::Interrupt {
            message: "user cancelled".into(),
        });
        assert_eq!(err.failure_category(), FailureCategory::Canceled);
    }

    #[test]
    fn failure_class_llm_timeout() {
        let err = Error::Llm(SdkError::RequestTimeout {
            message: "timed out".into(),
            source:  None,
        });
        assert_eq!(err.failure_category(), FailureCategory::TransientInfra);
    }

    // --- classify_sdk_error tests ---

    #[test]
    fn classify_sdk_rate_limit() {
        let err = SdkError::Provider {
            kind:   ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail::new("too fast", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureCategory::TransientInfra);
    }

    #[test]
    fn classify_sdk_server() {
        let err = SdkError::Provider {
            kind:   ProviderErrorKind::Server,
            detail: Box::new(ProviderErrorDetail::new("500", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureCategory::TransientInfra);
    }

    #[test]
    fn classify_sdk_context_length() {
        let err = SdkError::Provider {
            kind:   ProviderErrorKind::ContextLength,
            detail: Box::new(ProviderErrorDetail::new("too long", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureCategory::BudgetExhausted);
    }

    #[test]
    fn classify_sdk_quota_exceeded() {
        let err = SdkError::Provider {
            kind:   ProviderErrorKind::QuotaExceeded,
            detail: Box::new(ProviderErrorDetail::new("out of quota", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureCategory::BudgetExhausted);
    }

    #[test]
    fn classify_sdk_auth() {
        let err = SdkError::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        };
        assert_eq!(classify_sdk_error(&err), FailureCategory::Deterministic);
    }

    #[test]
    fn classify_sdk_request_timeout() {
        let err = SdkError::RequestTimeout {
            message: "timed out".into(),
            source:  None,
        };
        assert_eq!(classify_sdk_error(&err), FailureCategory::TransientInfra);
    }

    #[test]
    fn classify_sdk_abort() {
        let err = SdkError::Interrupt {
            message: "cancelled".into(),
        };
        assert_eq!(classify_sdk_error(&err), FailureCategory::Canceled);
    }

    #[test]
    fn classify_sdk_invalid_tool_call() {
        let err = SdkError::InvalidToolCall {
            message: "bad tool".into(),
        };
        assert_eq!(classify_sdk_error(&err), FailureCategory::Deterministic);
    }

    #[test]
    fn classify_sdk_invalid_request() {
        let err = SdkError::InvalidRequest {
            message: "unsupported reasoning effort".into(),
        };
        assert_eq!(classify_sdk_error(&err), FailureCategory::Deterministic);
    }

    // --- hints count guards ---

    #[test]
    fn permanent_budget_exhausted_hints_count() {
        assert_eq!(PERMANENT_BUDGET_EXHAUSTED_HINTS.len(), 1);
    }

    #[test]
    fn transient_infra_hints_count() {
        assert_eq!(TRANSIENT_INFRA_HINTS.len(), 40);
    }

    #[test]
    fn budget_exhausted_hints_count() {
        assert_eq!(BUDGET_EXHAUSTED_HINTS.len(), 11);
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
            FailureCategory::Canceled
        );
    }

    #[test]
    fn classify_reason_nextest_canceling_due_to_test_failure_is_deterministic() {
        assert_eq!(
            classify_failure_reason(
                "Script failed with exit code: 100\n\nCancelling due to test failure: 7 tests still running"
            ),
            FailureCategory::Deterministic
        );
    }

    #[test]
    fn classify_reason_abort() {
        assert_eq!(
            classify_failure_reason("interrupted by signal"),
            FailureCategory::Canceled
        );
    }

    // Budget exhausted

    #[test]
    fn classify_reason_turn_limit() {
        assert_eq!(
            classify_failure_reason("exceeded turn limit of 10"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_token_limit() {
        assert_eq!(
            classify_failure_reason("token limit reached"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_context_length() {
        assert_eq!(
            classify_failure_reason("context length exceeded"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_budget() {
        assert_eq!(
            classify_failure_reason("budget exceeded for run"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_quota_exceeded() {
        assert_eq!(
            classify_failure_reason("quota exceeded"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_max_tokens() {
        assert_eq!(
            classify_failure_reason("max_tokens exceeded"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_max_tokens_space() {
        assert_eq!(
            classify_failure_reason("max tokens reached"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_context_window_exceeded() {
        assert_eq!(
            classify_failure_reason("context window exceeded"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_budget_exhausted() {
        assert_eq!(
            classify_failure_reason("budget exhausted for this session"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_token_limit_exceeded() {
        assert_eq!(
            classify_failure_reason("token limit exceeded"),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn classify_reason_resetting_provider_limit_is_budget_exhausted() {
        assert_eq!(
            classify_failure_reason(
                "Internal error: You've hit your limit · resets Jul 31, 5am (UTC)"
            ),
            FailureCategory::BudgetExhausted
        );
    }

    // Structural

    #[test]
    fn classify_reason_scope_violation() {
        assert_eq!(
            classify_failure_reason("scope violation detected"),
            FailureCategory::Structural
        );
    }

    // Transient infra

    #[test]
    fn classify_reason_timeout() {
        assert_eq!(
            classify_failure_reason("request timed out after 30s"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_rate_limit() {
        assert_eq!(
            classify_failure_reason("rate limited by provider"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_connection_refused() {
        assert_eq!(
            classify_failure_reason("connection refused"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_connection_reset() {
        assert_eq!(
            classify_failure_reason("connection reset by peer"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_sandbox_state_change_in_progress() {
        assert_eq!(
            classify_failure_reason(
                "Pipeline lifecycle operation failed: failed to activate sandbox after node \
                 attempt survey: Failed to start Daytona sandbox: Sandbox state change in progress"
            ),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_sandbox_stop_still_in_progress() {
        assert_eq!(
            classify_failure_reason("Daytona sandbox stop still in progress after 120s"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_500() {
        assert_eq!(
            classify_failure_reason("HTTP 500 Internal Server Error"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_502() {
        assert_eq!(
            classify_failure_reason("HTTP 502 Bad Gateway"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_503() {
        assert_eq!(
            classify_failure_reason("HTTP 503 Service Unavailable"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_504() {
        assert_eq!(
            classify_failure_reason("HTTP 504 Gateway Timeout"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_context_deadline_exceeded() {
        assert_eq!(
            classify_failure_reason("context deadline exceeded"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_could_not_resolve_host() {
        assert_eq!(
            classify_failure_reason("could not resolve host api.example.com"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_could_not_resolve_hostname() {
        assert_eq!(
            classify_failure_reason("could not resolve hostname"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_temporary_failure() {
        assert_eq!(
            classify_failure_reason("temporary failure"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_temporary_failure_in_name_resolution() {
        assert_eq!(
            classify_failure_reason("temporary failure in name resolution"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_network_is_unreachable() {
        assert_eq!(
            classify_failure_reason("network is unreachable"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_broken_pipe() {
        assert_eq!(
            classify_failure_reason("broken pipe"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_tls_handshake_timeout() {
        assert_eq!(
            classify_failure_reason("tls handshake timeout"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_io_timeout() {
        assert_eq!(
            classify_failure_reason("i/o timeout"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_no_route_to_host() {
        assert_eq!(
            classify_failure_reason("no route to host"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_temporarily_unavailable() {
        assert_eq!(
            classify_failure_reason("resource temporarily unavailable"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_try_again() {
        assert_eq!(
            classify_failure_reason("try again later"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_too_many_requests() {
        assert_eq!(
            classify_failure_reason("too many requests"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_service_unavailable() {
        assert_eq!(
            classify_failure_reason("service unavailable"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_gateway_timeout() {
        assert_eq!(
            classify_failure_reason("gateway timeout"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_econnrefused() {
        assert_eq!(
            classify_failure_reason("ECONNREFUSED"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_econnreset() {
        assert_eq!(
            classify_failure_reason("ECONNRESET"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_dial_tcp() {
        assert_eq!(
            classify_failure_reason("dial tcp 10.0.0.1:443: connect: connection refused"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_transport_is_closing() {
        assert_eq!(
            classify_failure_reason("transport is closing"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_stream_disconnected() {
        assert_eq!(
            classify_failure_reason("stream disconnected"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_stream_closed_before() {
        assert_eq!(
            classify_failure_reason("stream closed before completion"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_index_crates_io() {
        assert_eq!(
            classify_failure_reason("failed to fetch index.crates.io"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_download_config_json_failed() {
        assert_eq!(
            classify_failure_reason("download of config.json failed"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_toolchain_registry_unavailable() {
        assert_eq!(
            classify_failure_reason("toolchain_or_dependency_registry_unavailable"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_toolchain_dependency_network() {
        assert_eq!(
            classify_failure_reason("toolchain dependency resolution blocked by network"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_toolchain_workspace_io() {
        assert_eq!(
            classify_failure_reason("toolchain_workspace_io"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_cross_device_link() {
        assert_eq!(
            classify_failure_reason("cross-device link"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_invalid_cross_device_link() {
        assert_eq!(
            classify_failure_reason("invalid cross-device link"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_os_error_18() {
        assert_eq!(
            classify_failure_reason("os error 18"),
            FailureCategory::TransientInfra
        );
    }

    // Structural

    #[test]
    fn classify_reason_write_scope_violation_underscore() {
        assert_eq!(
            classify_failure_reason("write_scope_violation detected"),
            FailureCategory::Structural
        );
    }

    #[test]
    fn classify_reason_write_scope_violation_space() {
        assert_eq!(
            classify_failure_reason("write scope violation detected"),
            FailureCategory::Structural
        );
    }

    // Default deterministic

    #[test]
    fn classify_reason_default_deterministic() {
        assert_eq!(
            classify_failure_reason("invalid configuration parameter"),
            FailureCategory::Deterministic
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
            FailureCategory::Deterministic,
            None,
            Some("test failed"),
        );
        assert_eq!(sig.to_string(), "verify|deterministic|test failed");
    }

    #[test]
    fn failure_signature_display() {
        let sig = FailureSignature::new(
            "build",
            FailureCategory::Structural,
            None,
            Some("scope violation"),
        );
        assert_eq!(format!("{sig}"), "build|structural|scope violation");
    }

    #[test]
    fn failure_signature_hint_takes_priority() {
        let sig = FailureSignature::new(
            "verify",
            FailureCategory::Deterministic,
            Some("custom hint"),
            Some("raw reason"),
        );
        assert_eq!(sig.to_string(), "verify|deterministic|custom hint");
    }

    #[test]
    fn failure_signature_missing_reason_falls_back_to_unknown() {
        let sig = FailureSignature::new("node", FailureCategory::Deterministic, None, None);
        assert_eq!(sig.to_string(), "node|deterministic|unknown");
    }

    #[test]
    fn failure_signature_equality_and_hash() {
        let sig1 = FailureSignature::new(
            "verify",
            FailureCategory::Deterministic,
            None,
            Some("test failed"),
        );
        let sig2 = FailureSignature::new(
            "verify",
            FailureCategory::Deterministic,
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
        assert!(FailureCategory::Deterministic.is_signature_tracked());
        assert!(FailureCategory::Structural.is_signature_tracked());
    }

    #[test]
    fn is_signature_tracked_false_for_others() {
        assert!(!FailureCategory::TransientInfra.is_signature_tracked());
        assert!(!FailureCategory::BudgetExhausted.is_signature_tracked());
        assert!(!FailureCategory::Canceled.is_signature_tracked());
        assert!(!FailureCategory::CompilationLoop.is_signature_tracked());
    }

    // --- failure_signature_hint tests ---

    #[test]
    fn failure_signature_hint_llm_returns_some() {
        let err = Error::Llm(SdkError::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        });
        assert_eq!(
            err.failure_signature_hint(),
            Some(FailureSignature(
                "api_deterministic|openai|authentication".to_string()
            ))
        );
    }

    #[test]
    fn failure_signature_hint_handler_returns_none() {
        let err = Error::handler("something failed");
        assert_eq!(err.failure_signature_hint(), None);
    }

    #[test]
    fn failure_signature_hint_engine_returns_none() {
        let err = Error::engine("engine error");
        assert_eq!(err.failure_signature_hint(), None);
    }

    // --- to_fail_outcome tests ---

    #[test]
    fn to_fail_outcome_llm_has_class_and_signature() {
        let err = Error::Llm(SdkError::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail::new("bad key", "openai")),
        });
        let outcome = err.to_fail_outcome();
        assert_eq!(outcome.status, crate::outcome::StageOutcome::Failed {
            retry_requested: false,
        });
        let failure = outcome.failure.as_ref().unwrap();
        assert_eq!(failure.category, FailureCategory::Deterministic);
        assert_eq!(
            failure.signature.as_deref(),
            Some("api_deterministic|openai|authentication")
        );
    }

    #[test]
    fn to_fail_outcome_handler_has_class_but_no_signature() {
        let err = Error::handler("connection refused");
        let outcome = err.to_fail_outcome();
        assert_eq!(outcome.status, crate::outcome::StageOutcome::Failed {
            retry_requested: false,
        });
        let failure = outcome.failure.as_ref().unwrap();
        assert_eq!(failure.category, FailureCategory::TransientInfra);
        assert!(failure.signature.is_none());
    }

    #[test]
    fn to_fail_outcome_includes_error_message_as_reason() {
        let err = Error::Llm(SdkError::Network {
            message: "connection refused".into(),
            source:  None,
        });
        let outcome = err.to_fail_outcome();
        assert!(
            outcome
                .failure_reason()
                .unwrap()
                .contains("connection refused")
        );
    }

    #[test]
    fn to_fail_outcome_no_context_updates() {
        let err = Error::Llm(SdkError::Network {
            message: "refused".into(),
            source:  None,
        });
        let outcome = err.to_fail_outcome();
        assert!(outcome.context_updates.is_empty());
    }

    // --- Phase 2: Eager classification tests ---

    #[test]
    fn handler_eager_classification() {
        let err = Error::handler("connection refused");
        assert_eq!(err.failure_category(), FailureCategory::TransientInfra);
    }

    #[test]
    fn handler_eager_classification_survives_clone() {
        let err = Error::handler("connection refused");
        let cloned = err.clone();
        assert_eq!(cloned.failure_category(), FailureCategory::TransientInfra);
    }

    #[test]
    fn handler_smart_constructor_preserves_message() {
        let err = Error::handler("some error");
        assert!(err.to_string().contains("some error"));
    }

    #[test]
    fn engine_eager_classification() {
        let err = Error::engine("rate limit exceeded");
        assert_eq!(err.failure_category(), FailureCategory::TransientInfra);
    }

    #[test]
    fn error_clone_preserves_display_for_all_variants() {
        let errors: Vec<Error> = vec![
            Error::Parse("bad".into()),
            Error::Validation("bad".into()),
            Error::ValidationFailed {
                diagnostics: vec![Diagnostic {
                    rule: "test".into(),
                    severity: fabro_validate::Severity::Error,
                    message: "bad".into(),
                    node_id: None,
                    edge: None,
                    fix: None,

                    ..Diagnostic::default()
                }],
            },
            Error::engine("engine err"),
            Error::publish("publish err"),
            Error::handler("handler err"),
            Error::Llm(SdkError::Network {
                message: "refused".into(),
                source:  None,
            }),
            Error::Checkpoint("cp err".into()),
            Error::Stylesheet("style err".into()),
            Error::Io("io err".into()),
            Error::Cancelled,
        ];
        for err in errors {
            assert_eq!(err.to_string(), err.clone().to_string());
        }
    }

    #[test]
    fn handler_display_unchanged() {
        assert_eq!(
            Error::handler("LLM call failed").to_string(),
            "Handler error: LLM call failed"
        );
    }

    #[test]
    fn engine_display_unchanged() {
        assert_eq!(
            Error::engine("no outgoing edge").to_string(),
            "Engine error: no outgoing edge"
        );
    }

    /// Publish runs once, after execution, so no caller can retry it — even
    /// when the message looks transient. The failure category is still
    /// classified for reporting.
    #[test]
    fn publish_errors_are_terminal() {
        assert!(!Error::publish("connection timed out").is_retryable());
        assert!(!Error::publish("permission denied").is_retryable());
        assert_eq!(
            Error::publish("connection timed out").failure_category(),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn failure_reason_distinguishes_publish_and_cancelled() {
        assert_eq!(
            Error::publish("nope").failure_reason(),
            FailureReason::PublishFailed
        );
        assert_eq!(Error::Cancelled.failure_reason(), FailureReason::Cancelled);
        assert_eq!(
            Error::engine("boom").failure_reason(),
            FailureReason::WorkflowError
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
                Error::handler(msg).failure_category(),
                classify_failure_reason(msg),
                "mismatch for message: {msg}"
            );
        }
    }

    /// Commit SHAs are hex, so they contain digit runs like "503" often enough
    /// to matter. Masking them keeps a deterministic failure from being
    /// reported as transient just because of the SHA it names.
    #[test]
    fn commit_shas_do_not_trip_transient_infra_hints() {
        let sha = "a503b1c9d4e2f7a8b6c3d0e1f2a3b4c5d6e7f8a9";
        assert_eq!(
            classify_failure_reason(&format!("failed to push final commit {sha} to branch 'x'")),
            FailureCategory::Deterministic
        );
        // A real status code is still a transient hint.
        assert_eq!(
            classify_failure_reason("push rejected with 503"),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn to_fail_outcome_preserves_class() {
        let err = Error::handler("timeout");
        let outcome = err.to_fail_outcome();
        assert_eq!(
            outcome.failure_category(),
            Some(FailureCategory::TransientInfra)
        );
    }

    // --- E2E error pipeline tests ---

    #[test]
    fn e2e_llm_error_to_outcome_to_event_preserves_classification() {
        use crate::event::Event;

        // 1. Create SdkError → Error
        let sdk_err = SdkError::Provider {
            kind:   ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail::new("too fast", "openai")),
        };
        let arc_err = Error::Llm(sdk_err);
        assert_eq!(arc_err.failure_category(), FailureCategory::TransientInfra);

        // 2. Error → Outcome
        let outcome = arc_err.to_fail_outcome();
        assert_eq!(
            outcome.failure_category(),
            Some(FailureCategory::TransientInfra)
        );

        // 3. Outcome → StageFailed event
        let failure = outcome.failure.clone().unwrap();
        let event = Event::StageFailed {
            node_id:    "code".into(),
            name:       "code".into(),
            index:      0,
            failure:    failure.clone(),
            will_retry: false,
            timing:     fabro_types::StageTiming::wall_only(0),
            billing:    None,
            actor:      None,
        };

        // 4. Verify classification survived all the way through
        match &event {
            Event::StageFailed { failure, .. } => {
                assert_eq!(failure.category, FailureCategory::TransientInfra);
            }
            _ => panic!("expected StageFailed"),
        }
    }

    #[test]
    fn e2e_handler_error_classified_at_edge() {
        // handler smart constructor classifies eagerly
        let err = Error::handler("connection refused");
        assert_eq!(err.failure_category(), FailureCategory::TransientInfra);

        // to_fail_outcome preserves
        let outcome = err.to_fail_outcome();
        assert_eq!(
            outcome.failure_category(),
            Some(FailureCategory::TransientInfra)
        );

        // event preserves
        let failure = outcome.failure.unwrap();
        assert_eq!(failure.category, FailureCategory::TransientInfra);
    }

    #[test]
    fn e2e_handler_retryable_checks() {
        assert!(Error::handler("timeout").is_retryable());
        assert!(Error::handler("auth error").is_retryable());
    }

    #[test]
    fn e2e_run_failure_projection_uses_handler_error_shape() {
        let err = Error::handler("connection refused");
        let failure = run_failure_from_error(&err, FailureReason::WorkflowError);

        assert_eq!(failure.detail.message, "connection refused");
        assert_eq!(failure.detail.causes, Vec::<String>::new());
        assert_eq!(failure.reason, FailureReason::WorkflowError);
        assert_eq!(failure.detail.category, FailureCategory::TransientInfra);
    }

    #[test]
    fn e2e_serde_stability_agent_error() {
        use fabro_agent::Error as AgentError;

        let err = AgentError::Llm(SdkError::Provider {
            kind:   ProviderErrorKind::RateLimit,
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
        assert_eq!(failure.category, FailureCategory::TransientInfra);
        assert_eq!(
            failure.signature.as_deref(),
            Some("api_transient|openai|rate_limited")
        );
    }

    // --- provider limits: resource exhaustion, not provenance or code faults ---

    /// Representative ACP failure payload. The only
    /// `TRANSIENT_INFRA_HINTS` entry this payload matches is `index.crates.io`,
    /// and it matches inside a `spawned_at` cargo registry *source* path.
    const SPEND_LIMIT_MESSAGE: &str = "ACP turn failed";
    const SPEND_LIMIT_PROTOCOL_CAUSE: &str = "ACP protocol error";
    const SPEND_LIMIT_INTERNAL_CAUSE: &str = r#"Internal error: You've hit your org's monthly spend limit · ask your admin to raise it at claude.ai/settings/usage: {
  "spawned_at": "/home/ubuntu/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/agent-client-protocol-0.11.1/src/session.rs:567:14",
  "data": {
    "errorKind": "rate_limit"
  }
}"#;
    const RESETTING_LIMIT_INTERNAL_CAUSE: &str = r#"Internal error: You've hit your limit · resets Jul 31, 5am (UTC): {
  "spawned_at": "/home/ubuntu/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/agent-client-protocol-0.11.1/src/session.rs:567:14",
  "data": {
    "errorKind": "rate_limit"
  }
}"#;

    #[test]
    fn classify_reason_provider_spend_limit_is_budget_exhausted() {
        let rendered = render_with_causes(SPEND_LIMIT_MESSAGE, &[
            SPEND_LIMIT_PROTOCOL_CAUSE.to_string(),
            SPEND_LIMIT_INTERNAL_CAUSE.to_string(),
        ]);

        assert_eq!(
            classify_failure_reason(&rendered),
            FailureCategory::BudgetExhausted,
            "an org spend limit is permanent and human-actionable: retrying never clears it"
        );
    }

    #[test]
    fn classify_reason_spend_limit_wins_over_transient_hint() {
        assert_eq!(
            classify_failure_reason("provider rate limited: monthly spend limit reached"),
            FailureCategory::BudgetExhausted,
            "an explicit permanent provider ceiling must override a transient phrase"
        );
    }

    #[test]
    fn acp_spend_limit_failure_detail_is_not_transient_infra() {
        let err = Error::handler_with_source(SPEND_LIMIT_MESSAGE, TestOuterError {
            message: SPEND_LIMIT_PROTOCOL_CAUSE,
            source:  TestCause(SPEND_LIMIT_INTERNAL_CAUSE),
        });

        let detail = err.to_failure_detail();
        assert_eq!(detail.message, "ACP turn failed");
        assert_eq!(detail.causes, vec![
            SPEND_LIMIT_PROTOCOL_CAUSE.to_string(),
            SPEND_LIMIT_INTERNAL_CAUSE.to_string(),
        ]);
        assert_eq!(detail.category, FailureCategory::BudgetExhausted);
    }

    #[test]
    fn acp_resetting_limit_failure_detail_is_budget_exhausted() {
        let err = Error::handler_with_source(SPEND_LIMIT_MESSAGE, TestOuterError {
            message: SPEND_LIMIT_PROTOCOL_CAUSE,
            source:  TestCause(RESETTING_LIMIT_INTERNAL_CAUSE),
        });

        let detail = err.to_failure_detail();
        assert_eq!(detail.message, SPEND_LIMIT_MESSAGE);
        assert_eq!(detail.causes, vec![
            SPEND_LIMIT_PROTOCOL_CAUSE.to_string(),
            RESETTING_LIMIT_INTERNAL_CAUSE.to_string(),
        ]);
        assert_eq!(detail.category, FailureCategory::BudgetExhausted);
    }

    #[test]
    fn resetting_limit_does_not_override_a_genuine_transient_fault() {
        let rendered =
            format!("{RESETTING_LIMIT_INTERNAL_CAUSE}\n  caused by: connection reset by peer");

        assert_eq!(
            classify_failure_reason(&rendered),
            FailureCategory::TransientInfra,
            "ordinary budget hints remain below genuine transient faults"
        );
    }

    #[test]
    fn classify_reason_cargo_registry_source_path_is_not_a_network_fault() {
        // `registry/src/index.crates.io-<hash>` names already-extracted crate
        // source. It rides along as `spawned_at` provenance on ACP internal
        // errors and describes no network fault whatsoever.
        let rendered = concat!(
            "ACP turn failed\n  caused by: ACP protocol error\n  caused by: ",
            r#"Internal error: agent refused the request: {"spawned_at": "#,
            r#""/home/ubuntu/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f"#,
            r#"/agent-client-protocol-0.11.1/src/session.rs:567:14"}"#,
        );

        assert_eq!(
            classify_failure_reason(rendered),
            FailureCategory::Deterministic,
            "a cargo registry source path is provenance, not a transient fault"
        );
    }

    #[test]
    fn classify_reason_hex_masked_registry_source_path_is_not_a_network_fault() {
        // Guards the port to a caller that masks hex blobs before classifying:
        // the registry hash then reads `index.crates.io-<hex>`, and a strict
        // `[0-9a-f]+` hash pattern would stop matching without failing loudly.
        let rendered = concat!(
            "acp turn failed\n  caused by: acp protocol error\n  caused by: ",
            r#"internal error: agent refused the request: {"spawned_at": "#,
            r#""/home/ubuntu/.cargo/registry/src/index.crates.io-<hex>"#,
            r#"/agent-client-protocol-0.11.1/src/session.rs:567:14"}"#,
        );

        assert_eq!(
            classify_failure_reason(rendered),
            FailureCategory::Deterministic,
            "a hex-masked registry source path is still provenance, not a fault"
        );
    }

    #[test]
    fn classify_reason_unicode_escaped_registry_source_path_is_not_a_network_fault() {
        let rendered = r#"internal error: {"spawned_at":"\u002fhome\u002fu\u002f.cargo\u002fregistry\u002fsrc\u002findex.crates.io-1949cf8c6b5b557f\u002fserde\u002fsrc\u002flib.rs"}"#;

        assert_eq!(
            discount_cargo_registry_source_paths(rendered),
            r#"internal error: {"spawned_at":" "}"#,
        );
        assert_eq!(
            classify_failure_reason(rendered),
            FailureCategory::Deterministic,
            "JSON escapes must not hide structured Cargo source provenance"
        );
    }

    #[test]
    fn classify_reason_partially_escaped_registry_hash_is_not_a_network_fault() {
        let rendered = r#"{"spawned_at":"/home/u/.cargo/registry/src/index.crates.io-1949cf8\u00636b5b557f/serde/src/lib.rs"}"#;

        assert!(
            !discount_cargo_registry_source_paths(rendered).contains("index.crates.io"),
            "the semantic hash must be checked before global hex normalization"
        );
        assert_eq!(
            classify_failure_reason(rendered),
            FailureCategory::Deterministic
        );
    }

    #[test]
    fn classify_reason_json_escaped_spawned_at_key_is_not_a_network_fault() {
        for key in [r"spawned\u005fat", r"\u0053pawned_at"] {
            let rendered = format!(
                r#"{{"{key}":"/home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde/src/lib.rs"}}"#
            );

            assert!(
                !discount_cargo_registry_source_paths(&rendered).contains("index.crates.io"),
                "JSON-equivalent spellings of the structured key must be recognized"
            );
            assert_eq!(
                classify_failure_reason(&rendered),
                FailureCategory::Deterministic
            );
        }
    }

    #[test]
    fn classify_reason_malformed_spawned_at_value_is_not_discounted() {
        let path = "/home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde/src/lib.rs";
        let invalid_escape = format!(r#"{{"spawned_at":"connection reset\q {path}"}}"#);
        let literal_newline = format!("{{\"spawned_at\":\"connection reset\n{path}\"}}");
        let uppercase_unicode_escapes = r#"{"spawned_at":"connection reset \U002fhome\U002fu\U002f.cargo\U002fregistry\U002fsrc\U002findex.crates.io-1949cf8c6b5b557f\U002fserde"}"#.to_string();
        let uppercase_newline_escape = format!(r#"{{"spawned_at":"connection reset\N{path}"}}"#);
        let malformed_key = format!(r#"{{"spawned\U005fat":"connection reset {path}"}}"#);

        for rendered in [
            invalid_escape,
            literal_newline,
            uppercase_unicode_escapes,
            uppercase_newline_escape,
            malformed_key,
        ] {
            assert_eq!(
                discount_cargo_registry_source_paths(&rendered),
                rendered,
                "malformed JSON is outside the structured-field boundary"
            );
            assert_eq!(
                classify_failure_reason(&rendered),
                FailureCategory::TransientInfra,
                "fault text in a malformed value must remain available to the classifier"
            );
        }
    }

    // --- non-weakening guards: genuine registry faults stay transient ---

    #[test]
    fn classify_reason_registry_download_url_still_transient() {
        assert_eq!(
            classify_failure_reason(
                "error: failed to download from https://index.crates.io/api/v1/crates/serde/1.0.0/download"
            ),
            FailureCategory::TransientInfra
        );
    }

    #[test]
    fn classify_reason_registry_source_shaped_url_still_transient() {
        for value in [
            "https://index.crates.io/registry/src/index.crates.io-1949cf8c6b5b557f/config.json",
            "//index.crates.io/registry/src/index.crates.io-1949cf8c6b5b557f/config.json",
            r"https:\/\/index.crates.io\/registry\/src\/index.crates.io-1949cf8c6b5b557f\/config.json",
            r"\/\/index.crates.io\/registry\/src\/index.crates.io-1949cf8c6b5b557f\/config.json",
            r"file:\/srv\/cargo\/registry\/src\/index.crates.io-1949cf8c6b5b557f\/config.json",
            r"https:\u002f\u002findex.crates.io\u002fregistry\u002fsrc\u002findex.crates.io-1949cf8c6b5b557f\u002fconfig.json",
            "abcdef0:/srv/registry/src/index.crates.io-1949cf8c6b5b557f/config.json",
            "deadbeef+pkg:/srv/registry/src/index.crates.io-1949cf8c6b5b557f/config.json",
        ] {
            let rendered = format!(r#"{{"spawned_at":"{value}"}}"#);
            assert_eq!(
                discount_cargo_registry_source_paths(&rendered),
                rendered,
                "a URL-valued spawned_at field must not be mistaken for a local source path"
            );
            assert_eq!(
                classify_failure_reason(&rendered),
                FailureCategory::TransientInfra
            );
        }
    }

    #[test]
    fn discount_spawned_at_preserves_field_structure_and_flexible_spacing() {
        let rendered = r#"internal error: { "spawned_at" : "/home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.0/src/lib.rs:1:1" }"#;

        assert_eq!(
            discount_cargo_registry_source_paths(rendered),
            r#"internal error: { "spawned_at" : " " }"#,
            "only the provenance value should be blanked"
        );
        assert_eq!(
            classify_failure_reason(rendered),
            FailureCategory::Deterministic,
            "spacing around the structured field must not affect classification"
        );
    }

    #[test]
    fn classify_reason_registry_source_path_beside_real_fault_still_transient() {
        let rendered = concat!(
            r#"internal error: {"error":"connection reset by peer","spawned_at":"#,
            r#""/home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/"#,
            r#"serde-1.0.0/src/lib.rs"}"#,
        );

        assert_eq!(
            classify_failure_reason(rendered),
            FailureCategory::TransientInfra,
            "discounting spawned_at must not hide a genuine fault elsewhere in the payload"
        );
    }

    #[test]
    fn classify_reason_registry_path_in_an_unrelated_field_is_not_discounted() {
        let rendered = r#"{"log":"panic at /home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.0/src/lib.rs"}"#;

        assert_eq!(discount_cargo_registry_source_paths(rendered), rendered);
        assert_eq!(
            classify_failure_reason(rendered),
            FailureCategory::TransientInfra,
            "only the structured spawned_at provenance field is discounted"
        );
    }

    #[test]
    fn classify_reason_mixed_separator_windows_source_path_is_not_a_network_fault() {
        for rendered in [
            r#"{"spawned_at":"c:/cargo/registry/src/github.com-1ecc6299db9ec823/serde-1.0.0/src\\timeout.rs"}"#,
            r#"{"spawned_at":"/cargo/registry/src/github.com-1ecc6299db9ec823/serde-1.0.0/src\\timeout.rs"}"#,
            r#"{"spawned_at":"cargo/registry/src/github.com-1ecc6299db9ec823/serde-1.0.0/src\\timeout.rs"}"#,
            r#"{"spawned_at":"/cargo/econnreset\\n/home/registry/src/github.com-1ecc6299db9ec823/serde-1.0.0/src/lib.rs"}"#,
        ] {
            let discounted = discount_cargo_registry_source_paths(rendered);
            assert!(
                !discounted.contains("timeout") && !discounted.contains("econnreset"),
                "source-path provenance must not retain fault hints: {rendered}"
            );
            assert_eq!(
                classify_failure_reason(rendered),
                FailureCategory::Deterministic,
                "a mixed-separator source path is provenance, not escaped fault text: {rendered}"
            );
        }
    }

    #[test]
    fn classify_reason_invalid_registry_hash_is_not_discounted() {
        let hashes = [
            "2",
            "abcdef0z",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ];

        for hash in hashes {
            let rendered = format!(
                r#"{{"spawned_at":"/srv/app/registry/src/index.crates.io-{hash}/src/main.rs"}}"#
            );
            assert_eq!(
                discount_cargo_registry_source_paths(&rendered),
                rendered,
                "an invalid registry hash must leave spawned_at untouched"
            );
            assert_eq!(
                classify_failure_reason(&rendered),
                FailureCategory::TransientInfra,
                "a short, non-hex, or overlong suffix is not a Cargo registry source hash: {hash}"
            );
        }
    }

    #[test]
    fn classify_reason_relative_registry_source_path_is_not_a_network_fault() {
        assert_eq!(
            classify_failure_reason(
                r#"internal error: {"spawned_at":"registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.0/src/lib.rs:1:1"}"#
            ),
            FailureCategory::Deterministic,
            "a relative cargo registry source path in spawned_at is still provenance"
        );
    }

    #[test]
    fn discount_relative_registry_source_path_preserves_spawned_at_quotes() {
        let rendered = r#"error: {"spawned_at":"registry/src/index.crates.io-1949cf8c6b5b557f/foo/src/lib.rs:1:1"}"#;

        assert_eq!(
            discount_cargo_registry_source_paths(rendered),
            r#"error: {"spawned_at":" "}"#,
            "discounting a relative path must preserve the structured field and its quotes"
        );
        assert_eq!(
            classify_failure_reason(rendered),
            FailureCategory::Deterministic
        );
    }

    #[test]
    fn classify_reason_non_default_registry_source_path_is_not_a_network_fault() {
        for registry in ["github.com", "abcdef0", "abcdef0.index.crates.io"] {
            let rendered = format!(
                r#"internal error: {{"spawned_at":"/home/u/.cargo/registry/src/{registry}-1ecc6299db9ec823/hyper-timeout-0.5.2/src/lib.rs:1:1"}}"#
            );

            assert_eq!(
                classify_failure_reason(&rendered),
                FailureCategory::Deterministic,
                "a crate name inside non-default registry {registry:?} is still provenance"
            );
        }
    }

    #[test]
    fn classify_reason_hint_bearing_registry_source_path_parents_are_not_faults() {
        for rendered in [
            r#"{"spawned_at":"/tmp/build-500/econnreset/.cargo/registry/src/github.com-1ecc6299db9ec823/serde-1.0.0/src/lib.rs:1:1"}"#,
            r#"{"spawned_at":"tmp/build-500/econnreset/.cargo/registry/src/github.com-1ecc6299db9ec823/serde-1.0.0/src/lib.rs:1:1"}"#,
            r#"{"spawned_at":"/tmp/build 500/econnreset/.cargo/registry/src/github.com-1ecc6299db9ec823/serde-1.0.0/src/lib.rs:1:1"}"#,
            r#"internal error: {"spawned_at":"C:\\build-500\\econnreset\\.cargo\\registry\\src\\github.com-1ecc6299db9ec823\\serde-1.0.0\\src\\lib.rs:1:1"}"#,
        ] {
            assert_eq!(
                classify_failure_reason(rendered),
                FailureCategory::Deterministic,
                "parent components of a cargo registry source path are also provenance: {rendered}"
            );
        }
    }
}
