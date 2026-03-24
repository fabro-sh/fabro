use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Supertrait for the generic usage/metadata type parameter on `Outcome`.
pub trait OutcomeMeta:
    Default + Clone + Send + Sync + fmt::Debug + Serialize + DeserializeOwned + 'static
{
}

impl<T> OutcomeMeta for T where
    T: Default + Clone + Send + Sync + fmt::Debug + Serialize + DeserializeOwned + 'static
{
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Success,
    Fail,
    Skipped,
    PartialSuccess,
    Retry,
}

impl fmt::Display for StageStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Fail => write!(f, "fail"),
            Self::Skipped => write!(f, "skipped"),
            Self::PartialSuccess => write!(f, "partial_success"),
            Self::Retry => write!(f, "retry"),
        }
    }
}

impl FromStr for StageStatus {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "success" => Ok(Self::Success),
            "fail" => Ok(Self::Fail),
            "skipped" => Ok(Self::Skipped),
            "partial_success" => Ok(Self::PartialSuccess),
            "retry" => Ok(Self::Retry),
            other => Err(format!("unknown stage status: {other}")),
        }
    }
}

/// Classification of failure modes.
///
/// Pipeline authors can write edge conditions like `context.failure_class=budget_exhausted`
/// to route execution based on the nature of the failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
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

impl fmt::Display for FailureCategory {
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

impl FromStr for FailureCategory {
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

impl FailureCategory {
    /// Whether this failure category should be tracked by the cycle breaker.
    pub fn is_signature_tracked(self) -> bool {
        matches!(self, Self::Deterministic | Self::Structural)
    }
}

/// Structured failure information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureDetail {
    pub message: String,
    #[serde(rename = "failure_class")]
    pub category: FailureCategory,
    #[serde(
        rename = "failure_signature",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub signature: Option<String>,
}

impl FailureDetail {
    pub fn new(message: impl Into<String>, category: FailureCategory) -> Self {
        Self {
            message: message.into(),
            category,
            signature: None,
        }
    }
}

/// The result of executing a node handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound = "M: OutcomeMeta")]
pub struct Outcome<M: OutcomeMeta = ()> {
    pub status: StageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_next_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub context_updates: HashMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_to_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureDetail>,
    #[serde(default)]
    pub usage: M,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl<M: OutcomeMeta> Default for Outcome<M> {
    fn default() -> Self {
        Self {
            status: StageStatus::Success,
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            context_updates: HashMap::new(),
            jump_to_node: None,
            notes: None,
            failure: None,
            usage: M::default(),
            files_touched: Vec::new(),
            duration_ms: None,
        }
    }
}

impl<M: OutcomeMeta> Outcome<M> {
    pub fn success() -> Self {
        Self::default()
    }

    pub fn fail(message: &str) -> Self {
        Self {
            status: StageStatus::Fail,
            failure: Some(FailureDetail {
                message: message.to_string(),
                category: FailureCategory::Deterministic,
                signature: None,
            }),
            ..Self::default()
        }
    }

    pub fn skipped(reason: &str) -> Self {
        Self {
            status: StageStatus::Skipped,
            notes: Some(reason.to_string()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeResult<M: OutcomeMeta = ()> {
    pub outcome: Outcome<M>,
    pub duration: Duration,
    pub attempts: u32,
    pub max_attempts: u32,
}

impl<M: OutcomeMeta> NodeResult<M> {
    pub fn new(outcome: Outcome<M>, duration: Duration, attempts: u32, max_attempts: u32) -> Self {
        Self {
            outcome,
            duration,
            attempts,
            max_attempts,
        }
    }

    pub fn from_error(
        error: &crate::error::CoreError,
        duration: Duration,
        attempts: u32,
        max_attempts: u32,
    ) -> Self {
        Self {
            outcome: error.to_fail_outcome(),
            duration,
            attempts,
            max_attempts,
        }
    }

    pub fn from_skip(outcome: Outcome<M>) -> Self {
        Self {
            outcome,
            duration: Duration::ZERO,
            attempts: 0,
            max_attempts: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_status_display_roundtrip() {
        let statuses = [
            StageStatus::Success,
            StageStatus::Fail,
            StageStatus::Skipped,
            StageStatus::PartialSuccess,
            StageStatus::Retry,
        ];
        for status in &statuses {
            let s = status.to_string();
            let parsed: StageStatus = s.parse().unwrap();
            assert_eq!(&parsed, status);
        }
    }

    #[test]
    fn stage_status_serde_roundtrip() {
        let statuses = [
            StageStatus::Success,
            StageStatus::Fail,
            StageStatus::Skipped,
            StageStatus::PartialSuccess,
            StageStatus::Retry,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).unwrap();
            let parsed: StageStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, status);
        }
    }

    #[test]
    fn failure_category_display_roundtrip() {
        let categories = [
            FailureCategory::TransientInfra,
            FailureCategory::Deterministic,
            FailureCategory::BudgetExhausted,
            FailureCategory::CompilationLoop,
            FailureCategory::Canceled,
            FailureCategory::Structural,
        ];
        for cat in &categories {
            let s = cat.to_string();
            let parsed: FailureCategory = s.parse().unwrap();
            assert_eq!(&parsed, cat);
        }
    }

    #[test]
    fn failure_category_aliases() {
        assert_eq!(
            "transient".parse::<FailureCategory>().unwrap(),
            FailureCategory::TransientInfra
        );
        assert_eq!(
            "cancelled".parse::<FailureCategory>().unwrap(),
            FailureCategory::Canceled
        );
        assert_eq!(
            "permanent".parse::<FailureCategory>().unwrap(),
            FailureCategory::Deterministic
        );
        assert_eq!(
            "budget".parse::<FailureCategory>().unwrap(),
            FailureCategory::BudgetExhausted
        );
    }

    #[test]
    fn failure_category_is_signature_tracked() {
        assert!(FailureCategory::Deterministic.is_signature_tracked());
        assert!(FailureCategory::Structural.is_signature_tracked());
        assert!(!FailureCategory::TransientInfra.is_signature_tracked());
        assert!(!FailureCategory::BudgetExhausted.is_signature_tracked());
        assert!(!FailureCategory::Canceled.is_signature_tracked());
    }

    #[test]
    fn outcome_success_factory() {
        let o: Outcome = Outcome::success();
        assert_eq!(o.status, StageStatus::Success);
        assert!(o.failure.is_none());
        assert!(o.notes.is_none());
    }

    #[test]
    fn outcome_fail_factory() {
        let o: Outcome = Outcome::fail("broken");
        assert_eq!(o.status, StageStatus::Fail);
        let f = o.failure.unwrap();
        assert_eq!(f.message, "broken");
        assert_eq!(f.category, FailureCategory::Deterministic);
        assert!(f.signature.is_none());
    }

    #[test]
    fn outcome_skipped_factory() {
        let o: Outcome = Outcome::skipped("not needed");
        assert_eq!(o.status, StageStatus::Skipped);
        assert_eq!(o.notes.as_deref(), Some("not needed"));
    }

    #[test]
    fn outcome_with_context_updates() {
        let mut o: Outcome = Outcome::success();
        o.context_updates
            .insert("key".into(), serde_json::json!("value"));
        assert_eq!(o.context_updates["key"], serde_json::json!("value"));
    }

    #[test]
    fn outcome_with_jump() {
        let mut o: Outcome = Outcome::success();
        o.jump_to_node = Some("target".into());
        assert_eq!(o.jump_to_node.as_deref(), Some("target"));
    }

    #[test]
    fn outcome_serde_roundtrip() {
        let mut o: Outcome = Outcome::success();
        o.notes = Some("done".to_string());
        o.context_updates
            .insert("key".into(), serde_json::json!("val"));
        let json = serde_json::to_string(&o).unwrap();
        let parsed: Outcome = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, StageStatus::Success);
        assert_eq!(parsed.notes.as_deref(), Some("done"));
        assert_eq!(
            parsed.context_updates.get("key"),
            Some(&serde_json::json!("val"))
        );
    }

    #[test]
    fn outcome_deserialize_without_usage_key() {
        // Old checkpoints may not have "usage" key — serde(default) handles this
        let json = r#"{"status":"success"}"#;
        let o: Outcome = serde_json::from_str(json).unwrap();
        assert_eq!(o.status, StageStatus::Success);
    }

    #[test]
    fn failure_detail_construction() {
        let f = FailureDetail::new("timeout", FailureCategory::TransientInfra);
        assert_eq!(f.message, "timeout");
        assert_eq!(f.category, FailureCategory::TransientInfra);
        assert!(f.signature.is_none());
    }

    #[test]
    fn failure_detail_serde_uses_renamed_keys() {
        let f = FailureDetail::new("timeout", FailureCategory::TransientInfra);
        let json = serde_json::to_string(&f).unwrap();
        // category serializes as "failure_class"
        assert!(json.contains("\"failure_class\""));
        assert!(!json.contains("\"category\""));
        // signature omitted when None
        assert!(!json.contains("failure_signature"));
    }

    #[test]
    fn failure_detail_serde_roundtrip() {
        let f = FailureDetail {
            message: "api down".into(),
            category: FailureCategory::TransientInfra,
            signature: Some("sig123".into()),
        };
        let json = serde_json::to_string(&f).unwrap();
        let parsed: FailureDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.message, "api down");
        assert_eq!(parsed.category, FailureCategory::TransientInfra);
        assert_eq!(parsed.signature.as_deref(), Some("sig123"));
    }

    #[test]
    fn node_result_from_outcome() {
        let o: Outcome = Outcome::success();
        let r = NodeResult::new(o, Duration::from_millis(100), 1, 3);
        assert_eq!(r.outcome.status, StageStatus::Success);
        assert_eq!(r.duration, Duration::from_millis(100));
        assert_eq!(r.attempts, 1);
        assert_eq!(r.max_attempts, 3);
    }
}
