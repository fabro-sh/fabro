use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunStatus {
    Submitted,
    Queued,
    Starting,
    Running,
    Blocked { blocked_reason: BlockedReason },
    Paused { prior_block: Option<BlockedReason> },
    Removing,
    Succeeded { reason: SuccessReason },
    Failed { reason: FailureReason },
    Dead,
    Archived { prior: TerminalStatus },
}

impl RunStatus {
    /// Whether the run has reached a terminal outcome and stops poll loops,
    /// finalization, and similar "done" handling. `Archived` is terminal
    /// because it is only reachable from another terminal status.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Failed { .. } | Self::Dead | Self::Archived { .. }
        )
    }

    /// Whether the run's status is frozen and cannot transition outbound
    /// (except via the `* -> Dead` escape hatch). `Archived` is intentionally
    /// NOT immutable — it can transition back to its prior terminal status
    /// via `unarchive`.
    pub fn is_immutable(self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Failed { .. } | Self::Dead
        )
    }

    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Submitted
                | Self::Queued
                | Self::Starting
                | Self::Running
                | Self::Blocked { .. }
                | Self::Paused { .. }
                | Self::Removing
        )
    }

    pub fn blocked_reason(self) -> Option<BlockedReason> {
        match self {
            Self::Blocked { blocked_reason } => Some(blocked_reason),
            Self::Paused { prior_block } => prior_block,
            _ => None,
        }
    }

    pub fn terminal_status(self) -> Option<TerminalStatus> {
        match self {
            Self::Succeeded { reason } => Some(TerminalStatus::Succeeded { reason }),
            Self::Failed { reason } => Some(TerminalStatus::Failed { reason }),
            Self::Dead => Some(TerminalStatus::Dead),
            _ => None,
        }
    }

    pub fn can_transition_to(self, to: Self) -> bool {
        if matches!(to, Self::Dead) {
            return true;
        }
        if matches!((self, to), (Self::Failed { .. }, Self::Submitted)) {
            return true;
        }
        if self.is_immutable() {
            return matches!(to, Self::Archived { .. });
        }
        if matches!(self, Self::Archived { .. }) {
            return matches!(
                to,
                Self::Succeeded { .. } | Self::Failed { .. } | Self::Dead
            );
        }
        matches!(
            (self, to),
            (Self::Submitted, Self::Queued | Self::Starting)
                | (
                    Self::Queued
                        | Self::Starting
                        | Self::Running
                        | Self::Blocked { .. }
                        | Self::Paused { .. }
                        | Self::Removing
                        | Self::Failed { .. },
                    Self::Submitted
                )
                | (Self::Queued, Self::Starting)
                | (Self::Submitted | Self::Queued, Self::Failed {
                    reason: FailureReason::Cancelled,
                })
                | (
                    Self::Starting | Self::Paused { .. } | Self::Blocked { .. },
                    Self::Running
                )
                | (
                    Self::Starting
                        | Self::Queued
                        | Self::Running
                        | Self::Blocked { .. }
                        | Self::Paused { .. }
                        | Self::Removing,
                    Self::Failed { .. }
                )
                | (
                    Self::Running,
                    Self::Succeeded { .. }
                        | Self::Blocked { .. }
                        | Self::Paused { .. }
                        | Self::Removing
                )
                | (Self::Blocked { .. }, Self::Paused { .. })
                | (Self::Paused { .. }, Self::Paused { .. })
                | (Self::Paused { .. }, Self::Blocked { .. })
                | (Self::Paused { .. }, Self::Removing)
        )
    }

    pub fn transition_to(self, to: Self) -> Result<Self, InvalidTransition> {
        if self.can_transition_to(to) {
            Ok(to)
        } else {
            Err(InvalidTransition { from: self, to })
        }
    }
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Submitted => f.write_str("submitted"),
            Self::Queued => f.write_str("queued"),
            Self::Starting => f.write_str("starting"),
            Self::Running => f.write_str("running"),
            Self::Blocked { blocked_reason } => write!(f, "blocked({blocked_reason})"),
            Self::Paused {
                prior_block: Some(blocked_reason),
            } => write!(f, "paused({blocked_reason})"),
            Self::Paused { prior_block: None } => f.write_str("paused"),
            Self::Removing => f.write_str("removing"),
            Self::Succeeded { reason } => write!(f, "succeeded({reason})"),
            Self::Failed { reason } => write!(f, "failed({reason})"),
            Self::Dead => f.write_str("dead"),
            Self::Archived { prior } => write!(f, "archived({prior})"),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct InvalidTransition {
    pub from: RunStatus,
    pub to:   RunStatus,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid status transition: {} -> {}", self.from, self.to)
    }
}

impl std::error::Error for InvalidTransition {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessReason {
    Completed,
    PartialSuccess,
}

impl fmt::Display for SuccessReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Completed => "completed",
            Self::PartialSuccess => "partial_success",
        })
    }
}

impl FromStr for SuccessReason {
    type Err = ParseSuccessReasonError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "completed" => Ok(Self::Completed),
            "partial_success" => Ok(Self::PartialSuccess),
            _ => Err(ParseSuccessReasonError(s.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParseSuccessReasonError(String);

impl fmt::Display for ParseSuccessReasonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid success reason: {:?}", self.0)
    }
}

impl std::error::Error for ParseSuccessReasonError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    WorkflowError,
    Cancelled,
    Terminated,
    TransientInfra,
    BudgetExhausted,
    LaunchFailed,
    BootstrapFailed,
    SandboxInitFailed,
}

impl fmt::Display for FailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WorkflowError => "workflow_error",
            Self::Cancelled => "cancelled",
            Self::Terminated => "terminated",
            Self::TransientInfra => "transient_infra",
            Self::BudgetExhausted => "budget_exhausted",
            Self::LaunchFailed => "launch_failed",
            Self::BootstrapFailed => "bootstrap_failed",
            Self::SandboxInitFailed => "sandbox_init_failed",
        })
    }
}

impl FromStr for FailureReason {
    type Err = ParseFailureReasonError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "workflow_error" => Ok(Self::WorkflowError),
            "cancelled" => Ok(Self::Cancelled),
            "terminated" => Ok(Self::Terminated),
            "transient_infra" => Ok(Self::TransientInfra),
            "budget_exhausted" => Ok(Self::BudgetExhausted),
            "launch_failed" => Ok(Self::LaunchFailed),
            "bootstrap_failed" => Ok(Self::BootstrapFailed),
            "sandbox_init_failed" => Ok(Self::SandboxInitFailed),
            _ => Err(ParseFailureReasonError(s.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParseFailureReasonError(String);

impl fmt::Display for ParseFailureReasonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid failure reason: {:?}", self.0)
    }
}

impl std::error::Error for ParseFailureReasonError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalStatus {
    Succeeded { reason: SuccessReason },
    Failed { reason: FailureReason },
    Dead,
}

impl fmt::Display for TerminalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Succeeded { reason } => write!(f, "succeeded({reason})"),
            Self::Failed { reason } => write!(f, "failed({reason})"),
            Self::Dead => f.write_str("dead"),
        }
    }
}

impl From<TerminalStatus> for RunStatus {
    fn from(value: TerminalStatus) -> Self {
        match value {
            TerminalStatus::Succeeded { reason } => Self::Succeeded { reason },
            TerminalStatus::Failed { reason } => Self::Failed { reason },
            TerminalStatus::Dead => Self::Dead,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    HumanInputRequired,
}

impl fmt::Display for BlockedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::HumanInputRequired => "human_input_required",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunControlAction {
    Cancel,
    Pause,
    Unpause,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        BlockedReason, FailureReason, InvalidTransition, RunStatus, SuccessReason, TerminalStatus,
    };

    #[test]
    fn queued_and_blocked_are_active() {
        let queued = RunStatus::Queued;
        let blocked = RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        };

        assert_eq!(queued.to_string(), "queued");
        assert!(queued.is_active());
        assert!(!queued.is_terminal());

        assert_eq!(blocked.to_string(), "blocked(human_input_required)");
        assert!(blocked.is_active());
        assert!(!blocked.is_terminal());
    }

    #[test]
    fn canonical_blocked_transitions_are_allowed() {
        let submitted = RunStatus::Submitted;
        let queued = RunStatus::Queued;
        let running = RunStatus::Running;
        let blocked = RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        };
        let paused = RunStatus::Paused {
            prior_block: Some(BlockedReason::HumanInputRequired),
        };
        let failed = RunStatus::Failed {
            reason: FailureReason::WorkflowError,
        };

        assert!(submitted.can_transition_to(queued));
        assert!(submitted.can_transition_to(RunStatus::Starting));
        assert!(submitted.can_transition_to(RunStatus::Failed {
            reason: FailureReason::Cancelled,
        }));
        assert!(queued.can_transition_to(RunStatus::Submitted));
        assert!(failed.can_transition_to(RunStatus::Submitted));
        assert!(queued.can_transition_to(RunStatus::Starting));
        assert!(queued.can_transition_to(RunStatus::Failed {
            reason: FailureReason::Cancelled,
        }));
        assert!(queued.can_transition_to(RunStatus::Failed {
            reason: FailureReason::Terminated,
        }));
        assert!(running.can_transition_to(blocked));
        assert!(blocked.can_transition_to(running));
        assert!(blocked.can_transition_to(paused));
        assert!(blocked.can_transition_to(RunStatus::Failed {
            reason: FailureReason::WorkflowError,
        }));
    }

    #[test]
    fn success_and_failure_reasons_parse_and_round_trip() {
        let success = SuccessReason::from_str("completed").expect("completed should parse");
        assert_eq!(success, SuccessReason::Completed);
        assert_eq!(success.to_string(), "completed");

        let failure = FailureReason::from_str("cancelled").expect("cancelled should parse");
        assert_eq!(failure, FailureReason::Cancelled);
        assert_eq!(failure.to_string(), "cancelled");
    }

    #[test]
    fn archived_display_includes_prior_terminal_status() {
        let archived = RunStatus::Archived {
            prior: TerminalStatus::Succeeded {
                reason: SuccessReason::Completed,
            },
        };
        assert_eq!(archived.to_string(), "archived(succeeded(completed))");
    }

    #[test]
    fn terminal_statuses_can_transition_to_archived() {
        let archived = RunStatus::Archived {
            prior: TerminalStatus::Dead,
        };
        assert!(
            RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            }
            .can_transition_to(archived)
        );
        assert!(
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            }
            .can_transition_to(archived)
        );
        assert!(RunStatus::Dead.can_transition_to(archived));
    }

    #[test]
    fn archived_can_transition_back_to_terminal() {
        let archived = RunStatus::Archived {
            prior: TerminalStatus::Succeeded {
                reason: SuccessReason::Completed,
            },
        };
        assert!(archived.can_transition_to(RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        }));
        assert!(archived.can_transition_to(RunStatus::Failed {
            reason: FailureReason::Cancelled,
        }));
        assert!(archived.can_transition_to(RunStatus::Dead));
    }

    #[test]
    fn running_cannot_transition_to_archived() {
        let archived = RunStatus::Archived {
            prior: TerminalStatus::Succeeded {
                reason: SuccessReason::Completed,
            },
        };
        assert!(!RunStatus::Running.can_transition_to(archived));
        assert!(!RunStatus::Queued.can_transition_to(archived));
        assert!(!RunStatus::Submitted.can_transition_to(archived));
        assert!(!RunStatus::Paused { prior_block: None }.can_transition_to(archived));
    }

    #[test]
    fn archived_to_archived_is_rejected() {
        let archived = RunStatus::Archived {
            prior: TerminalStatus::Dead,
        };
        assert!(!archived.can_transition_to(archived));
    }

    #[test]
    fn archived_is_terminal_but_not_immutable() {
        let archived = RunStatus::Archived {
            prior: TerminalStatus::Dead,
        };
        assert!(archived.is_terminal());
        assert!(!archived.is_immutable());
        assert!(!archived.is_active());
    }

    #[test]
    fn immutable_terminal_statuses_are_also_terminal() {
        for status in [
            RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            },
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            },
            RunStatus::Dead,
        ] {
            assert!(status.is_terminal(), "{status} should be terminal");
            assert!(status.is_immutable(), "{status} should be immutable");
        }
    }

    #[test]
    fn invalid_transition_carries_from_and_to() {
        let from = RunStatus::Running;
        let to = RunStatus::Archived {
            prior: TerminalStatus::Dead,
        };
        let err = from.transition_to(to).expect_err("should reject");
        assert_eq!(err, InvalidTransition { from, to });
    }
}
