use std::fmt;

use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

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
pub enum RunStatusKind {
    Submitted,
    Pending,
    Runnable,
    Starting,
    Running,
    Blocked,
    Paused,
    Removing,
    Succeeded,
    Failed,
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunStatus {
    Submitted,
    Pending { reason: PendingReason },
    Runnable,
    Starting,
    Running,
    Blocked { blocked_reason: BlockedReason },
    Paused { prior_block: Option<BlockedReason> },
    Removing,
    Succeeded { reason: SuccessReason },
    Failed { reason: FailureReason },
    Dead,
}

impl RunStatus {
    pub fn kind(self) -> RunStatusKind {
        self.into()
    }

    /// Whether the run has reached a terminal outcome and stops poll loops,
    /// finalization, and similar "done" handling.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Failed { .. } | Self::Dead
        )
    }

    /// Whether the run's status is frozen and cannot transition outbound
    /// through normal lifecycle events. Deletion and the `* -> Dead` escape
    /// hatch are allowed separately.
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
                | Self::Pending { .. }
                | Self::Runnable
                | Self::Starting
                | Self::Running
                | Self::Blocked { .. }
                | Self::Paused { .. }
                | Self::Removing
        )
    }

    pub fn requires_force_to_delete(self) -> bool {
        self.is_active() && !matches!(self, Self::Removing)
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
        if matches!(to, Self::Removing) {
            return !matches!(self, Self::Removing);
        }
        if matches!((self, to), (Self::Failed { .. }, Self::Submitted)) {
            return true;
        }
        if self.is_immutable() {
            return false;
        }
        matches!(
            (self, to),
            (Self::Submitted, Self::Pending { .. } | Self::Runnable)
                | (
                    Self::Pending { .. }
                        | Self::Runnable
                        | Self::Starting
                        | Self::Running
                        | Self::Blocked { .. }
                        | Self::Paused { .. }
                        | Self::Removing
                        | Self::Failed { .. },
                    Self::Submitted
                )
                | (Self::Pending { .. }, Self::Runnable)
                | (Self::Runnable, Self::Starting)
                | (
                    Self::Submitted | Self::Pending { .. } | Self::Runnable,
                    Self::Failed {
                        reason: FailureReason::Cancelled,
                    }
                )
                | (Self::Pending { .. }, Self::Failed {
                    reason: FailureReason::ApprovalDenied,
                })
                | (
                    Self::Starting | Self::Paused { .. } | Self::Blocked { .. },
                    Self::Running
                )
                | (
                    Self::Starting
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

impl From<RunStatus> for RunStatusKind {
    fn from(status: RunStatus) -> Self {
        match status {
            RunStatus::Submitted => Self::Submitted,
            RunStatus::Pending { .. } => Self::Pending,
            RunStatus::Runnable => Self::Runnable,
            RunStatus::Starting => Self::Starting,
            RunStatus::Running => Self::Running,
            RunStatus::Blocked { .. } => Self::Blocked,
            RunStatus::Paused { .. } => Self::Paused,
            RunStatus::Removing => Self::Removing,
            RunStatus::Succeeded { .. } => Self::Succeeded,
            RunStatus::Failed { .. } => Self::Failed,
            RunStatus::Dead => Self::Dead,
        }
    }
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Submitted => f.write_str("submitted"),
            Self::Pending { reason } => write!(f, "pending({reason})"),
            Self::Runnable => f.write_str("runnable"),
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
pub enum PendingReason {
    ApprovalRequired,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SuccessReason {
    Completed,
    PartialSuccess,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum FailureReason {
    WorkflowError,
    Cancelled,
    ApprovalDenied,
    Terminated,
    TransientInfra,
    BudgetExhausted,
    LaunchFailed,
    BootstrapFailed,
    SandboxInitFailed,
}

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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum BlockedReason {
    HumanInputRequired,
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
        BlockedReason, FailureReason, InvalidTransition, PendingReason, RunStatus, SuccessReason,
    };

    #[test]
    fn pending_runnable_and_blocked_are_active() {
        let pending = RunStatus::Pending {
            reason: PendingReason::ApprovalRequired,
        };
        let runnable = RunStatus::Runnable;
        let blocked = RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        };

        assert_eq!(pending.to_string(), "pending(approval_required)");
        assert!(pending.is_active());
        assert!(!pending.is_terminal());

        assert_eq!(runnable.to_string(), "runnable");
        assert!(runnable.is_active());
        assert!(!runnable.is_terminal());

        assert_eq!(blocked.to_string(), "blocked(human_input_required)");
        assert!(blocked.is_active());
        assert!(!blocked.is_terminal());
    }

    #[test]
    fn canonical_blocked_transitions_are_allowed() {
        let submitted = RunStatus::Submitted;
        let pending = RunStatus::Pending {
            reason: PendingReason::ApprovalRequired,
        };
        let runnable = RunStatus::Runnable;
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

        assert!(submitted.can_transition_to(pending));
        assert!(submitted.can_transition_to(runnable));
        assert!(!submitted.can_transition_to(RunStatus::Starting));
        assert!(submitted.can_transition_to(RunStatus::Failed {
            reason: FailureReason::Cancelled,
        }));
        assert!(pending.can_transition_to(RunStatus::Submitted));
        assert!(runnable.can_transition_to(RunStatus::Submitted));
        assert!(failed.can_transition_to(RunStatus::Submitted));
        assert!(pending.can_transition_to(runnable));
        assert!(runnable.can_transition_to(RunStatus::Starting));
        assert!(pending.can_transition_to(RunStatus::Failed {
            reason: FailureReason::Cancelled,
        }));
        assert!(pending.can_transition_to(RunStatus::Failed {
            reason: FailureReason::ApprovalDenied,
        }));
        assert!(!pending.can_transition_to(RunStatus::Failed {
            reason: FailureReason::Terminated,
        }));
        assert!(runnable.can_transition_to(RunStatus::Failed {
            reason: FailureReason::Cancelled,
        }));
        assert!(!runnable.can_transition_to(RunStatus::Failed {
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

        let pending =
            PendingReason::from_str("approval_required").expect("approval_required should parse");
        assert_eq!(pending, PendingReason::ApprovalRequired);
        assert_eq!(pending.to_string(), "approval_required");
    }

    #[test]
    fn run_statuses_can_transition_to_removing_for_deletion() {
        let removing = RunStatus::Removing;
        for status in [
            RunStatus::Submitted,
            RunStatus::Pending {
                reason: PendingReason::ApprovalRequired,
            },
            RunStatus::Runnable,
            RunStatus::Starting,
            RunStatus::Running,
            RunStatus::Blocked {
                blocked_reason: BlockedReason::HumanInputRequired,
            },
            RunStatus::Paused { prior_block: None },
            RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            },
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            },
            RunStatus::Dead,
        ] {
            assert!(
                status.can_transition_to(removing),
                "{status} should transition to removing"
            );
        }
        assert!(!removing.can_transition_to(removing));
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
        let from = RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        };
        let to = RunStatus::Running;
        let err = from.transition_to(to).expect_err("should reject");
        assert_eq!(err, InvalidTransition { from, to });
    }
}
