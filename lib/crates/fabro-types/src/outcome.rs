use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    TransientInfra,
    Deterministic,
    BudgetExhausted,
    CompilationLoop,
    Canceled,
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
            "transient_infra"
            | "transient"
            | "transient-infra"
            | "infra_transient"
            | "transient infra"
            | "infrastructure_transient"
            | "retryable"
            | "toolchain_workspace_io"
            | "toolchain-workspace-io"
            | "toolchain_or_dependency_registry_unavailable"
            | "toolchain-dependency-registry-unavailable" => Self::TransientInfra,
            "budget_exhausted" | "budget-exhausted" | "budget exhausted" | "budget" => {
                Self::BudgetExhausted
            }
            "compilation_loop" | "compilation-loop" | "compilation loop" | "compile_loop"
            | "compile-loop" => Self::CompilationLoop,
            "canceled" | "cancelled" => Self::Canceled,
            "structural" | "structure" | "scope_violation" | "write_scope_violation" => {
                Self::Structural
            }
            // "deterministic" and all unrecognized values
            _ => Self::Deterministic,
        })
    }
}

impl FailureCategory {
    pub fn is_signature_tracked(self) -> bool {
        matches!(self, Self::Deterministic | Self::Structural)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    pub fn from_skip(outcome: Outcome<M>) -> Self {
        Self {
            outcome,
            duration: Duration::ZERO,
            attempts: 0,
            max_attempts: 0,
        }
    }
}
