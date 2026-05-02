use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::de::{DeserializeOwned, Error as DeError};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use strum::{Display, EnumString, IntoStaticStr};

pub trait OutcomeMeta:
    Default + Clone + Send + Sync + fmt::Debug + Serialize + DeserializeOwned + 'static
{
}

impl<T> OutcomeMeta for T where
    T: Default + Clone + Send + Sync + fmt::Debug + Serialize + DeserializeOwned + 'static
{
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOutcome {
    Succeeded,
    PartiallySucceeded,
    Failed { retry_requested: bool },
    Skipped,
}

impl StageOutcome {
    #[must_use]
    pub fn is_successful(self) -> bool {
        matches!(self, Self::Succeeded | Self::PartiallySucceeded)
    }

    #[must_use]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    #[must_use]
    pub fn retry_requested(self) -> bool {
        matches!(self, Self::Failed {
            retry_requested: true,
        })
    }
}

impl fmt::Display for StageOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Succeeded => write!(f, "succeeded"),
            Self::PartiallySucceeded => write!(f, "partially_succeeded"),
            Self::Failed { .. } => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

impl FromStr for StageOutcome {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "succeeded" => Ok(Self::Succeeded),
            "partially_succeeded" => Ok(Self::PartiallySucceeded),
            "failed" => Ok(Self::Failed {
                retry_requested: false,
            }),
            "skipped" => Ok(Self::Skipped),
            other => Err(format!("unknown stage outcome: {other}")),
        }
    }
}

impl Serialize for StageOutcome {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for StageOutcome {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(DeError::custom)
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum StageState {
    Pending,
    Running,
    Retrying,
    Succeeded,
    PartiallySucceeded,
    Failed,
    Skipped,
    Cancelled,
}

impl StageState {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::PartiallySucceeded
                | Self::Failed
                | Self::Skipped
                | Self::Cancelled
        )
    }
}

impl From<StageOutcome> for StageState {
    fn from(outcome: StageOutcome) -> Self {
        match outcome {
            StageOutcome::Succeeded => Self::Succeeded,
            StageOutcome::PartiallySucceeded => Self::PartiallySucceeded,
            StageOutcome::Failed { .. } => Self::Failed,
            StageOutcome::Skipped => Self::Skipped,
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
    pub message:   String,
    #[serde(rename = "failure_class")]
    pub category:  FailureCategory,
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
    pub status:             StageOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_label:    Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_next_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub context_updates:    HashMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_to_node:       Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes:              Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure:            Option<FailureDetail>,
    #[serde(default)]
    pub usage:              M,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched:      Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms:        Option<u64>,
}

impl<M: OutcomeMeta> Default for Outcome<M> {
    fn default() -> Self {
        Self {
            status:             StageOutcome::Succeeded,
            preferred_label:    None,
            suggested_next_ids: Vec::new(),
            context_updates:    HashMap::new(),
            jump_to_node:       None,
            notes:              None,
            failure:            None,
            usage:              M::default(),
            files_touched:      Vec::new(),
            duration_ms:        None,
        }
    }
}

impl<M: OutcomeMeta> Outcome<M> {
    pub fn success() -> Self {
        Self::default()
    }

    pub fn fail(message: &str) -> Self {
        Self {
            status: StageOutcome::Failed {
                retry_requested: false,
            },
            failure: Some(FailureDetail {
                message:   message.to_string(),
                category:  FailureCategory::Deterministic,
                signature: None,
            }),
            ..Self::default()
        }
    }

    pub fn skipped(reason: &str) -> Self {
        Self {
            status: StageOutcome::Skipped,
            notes: Some(reason.to_string()),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{StageOutcome, StageState};

    #[test]
    fn stage_outcome_failed_serde_is_lossy_for_retry_intent() {
        assert_eq!(
            serde_json::to_value(StageOutcome::Failed {
                retry_requested: true,
            })
            .unwrap(),
            json!("failed")
        );
        assert_eq!(
            serde_json::from_value::<StageOutcome>(json!("failed")).unwrap(),
            StageOutcome::Failed {
                retry_requested: false,
            }
        );
    }

    #[test]
    fn stage_state_projects_terminal_outcomes() {
        assert_eq!(
            StageState::from(StageOutcome::Succeeded),
            StageState::Succeeded
        );
        assert_eq!(
            StageState::from(StageOutcome::PartiallySucceeded),
            StageState::PartiallySucceeded
        );
        assert_eq!(
            StageState::from(StageOutcome::Failed {
                retry_requested: true,
            }),
            StageState::Failed
        );
        assert!(StageState::Cancelled.is_terminal());
        assert!(!StageState::Running.is_terminal());
    }
}

#[derive(Debug, Clone)]
pub struct NodeResult<M: OutcomeMeta = ()> {
    pub outcome:      Outcome<M>,
    pub duration:     Duration,
    pub attempts:     u32,
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
