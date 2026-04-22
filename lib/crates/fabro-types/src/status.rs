use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum RunStatus {
    Submitted,
    Queued,
    Starting,
    Running,
    Blocked,
    Paused,
    Removing,
    Succeeded,
    Failed,
    Dead,
    Archived,
}

impl RunStatus {
    /// Whether the run has reached a terminal outcome and stops poll loops,
    /// finalization, and similar "done" handling. `Archived` is terminal
    /// because it is only reachable from another terminal status.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Dead | Self::Archived
        )
    }

    /// Whether the run's status is frozen and cannot transition outbound
    /// (except via the `* -> Dead` escape hatch). `Archived` is intentionally
    /// NOT immutable — it can transition back to its prior terminal status
    /// via `unarchive`.
    pub fn is_immutable(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Dead)
    }

    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Submitted
                | Self::Queued
                | Self::Starting
                | Self::Running
                | Self::Blocked
                | Self::Paused
                | Self::Removing
        )
    }

    pub fn can_transition_to(self, to: Self) -> bool {
        if to == Self::Dead {
            return true;
        }
        if self.is_immutable() {
            // Allow immutable terminal statuses to archive.
            return matches!(to, Self::Archived);
        }
        if self == Self::Archived {
            // Unarchive: restore to any prior terminal status.
            return matches!(to, Self::Succeeded | Self::Failed);
        }
        matches!(
            (self, to),
            (Self::Submitted, Self::Queued)
                | (Self::Queued, Self::Starting)
                | (Self::Starting | Self::Paused | Self::Blocked, Self::Running)
                | (
                    Self::Starting | Self::Running | Self::Blocked | Self::Paused | Self::Removing,
                    Self::Failed
                )
                | (
                    Self::Running,
                    Self::Succeeded | Self::Blocked | Self::Paused | Self::Removing
                )
                | (Self::Blocked, Self::Paused)
                | (Self::Paused, Self::Removing)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum StatusReason {
    Completed,
    PartialSuccess,
    WorkflowError,
    Cancelled,
    Terminated,
    TransientInfra,
    BudgetExhausted,
    LaunchFailed,
    BootstrapFailed,
    SandboxInitFailed,
    SandboxInitializing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStatusRecord {
    pub status:         RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason:  Option<StatusReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<BlockedReason>,
    pub updated_at:     DateTime<Utc>,
}

impl RunStatusRecord {
    pub fn new(status: RunStatus, status_reason: Option<StatusReason>) -> Self {
        Self {
            status,
            status_reason,
            blocked_reason: None,
            updated_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{InvalidTransition, RunStatus, StatusReason};

    #[test]
    fn queued_and_blocked_parse_and_format() {
        for status in ["queued", "blocked"] {
            let parsed = RunStatus::from_str(status)
                .unwrap_or_else(|_| panic!("expected {status} to parse"));
            assert_eq!(parsed.to_string(), status);
            assert!(parsed.is_active(), "{status} should be active");
            assert!(!parsed.is_terminal(), "{status} should not be terminal");
        }
    }

    #[test]
    fn canonical_blocked_transitions_are_allowed() {
        let submitted = RunStatus::from_str("submitted").unwrap();
        let queued =
            RunStatus::from_str("queued").unwrap_or_else(|_| panic!("expected queued to parse"));
        let running = RunStatus::from_str("running").unwrap();
        let blocked =
            RunStatus::from_str("blocked").unwrap_or_else(|_| panic!("expected blocked to parse"));
        let paused = RunStatus::from_str("paused").unwrap();

        assert!(submitted.can_transition_to(queued));
        assert!(queued.can_transition_to(RunStatus::from_str("starting").unwrap()));
        assert!(running.can_transition_to(blocked));
        assert!(blocked.can_transition_to(running));
        assert!(blocked.can_transition_to(paused));
        assert!(blocked.can_transition_to(RunStatus::from_str("failed").unwrap()));
    }

    #[test]
    fn archived_parses_and_round_trips() {
        let parsed = RunStatus::from_str("archived").expect("archived should parse");
        assert_eq!(parsed, RunStatus::Archived);
        assert_eq!(parsed.to_string(), "archived");
    }

    #[test]
    fn status_reason_parses_and_round_trips() {
        let parsed = StatusReason::from_str("cancelled").expect("cancelled should parse");
        assert_eq!(parsed, StatusReason::Cancelled);
        assert_eq!(parsed.to_string(), "cancelled");
    }

    #[test]
    fn terminal_statuses_can_transition_to_archived() {
        assert!(RunStatus::Succeeded.can_transition_to(RunStatus::Archived));
        assert!(RunStatus::Failed.can_transition_to(RunStatus::Archived));
        assert!(RunStatus::Dead.can_transition_to(RunStatus::Archived));
    }

    #[test]
    fn archived_can_transition_back_to_terminal() {
        assert!(RunStatus::Archived.can_transition_to(RunStatus::Succeeded));
        assert!(RunStatus::Archived.can_transition_to(RunStatus::Failed));
        // Dead is always reachable via the escape hatch.
        assert!(RunStatus::Archived.can_transition_to(RunStatus::Dead));
    }

    #[test]
    fn running_cannot_transition_to_archived() {
        assert!(!RunStatus::Running.can_transition_to(RunStatus::Archived));
        assert!(!RunStatus::Queued.can_transition_to(RunStatus::Archived));
        assert!(!RunStatus::Submitted.can_transition_to(RunStatus::Archived));
        assert!(!RunStatus::Paused.can_transition_to(RunStatus::Archived));
    }

    #[test]
    fn archived_to_archived_is_rejected() {
        // Idempotency of archive is handled at the operation layer, not the guard.
        assert!(!RunStatus::Archived.can_transition_to(RunStatus::Archived));
    }

    #[test]
    fn archived_is_terminal_but_not_immutable() {
        assert!(RunStatus::Archived.is_terminal());
        assert!(!RunStatus::Archived.is_immutable());
        assert!(!RunStatus::Archived.is_active());
    }

    #[test]
    fn immutable_terminal_statuses_are_also_terminal() {
        for status in [RunStatus::Succeeded, RunStatus::Failed, RunStatus::Dead] {
            assert!(status.is_terminal(), "{status} should be terminal");
            assert!(status.is_immutable(), "{status} should be immutable");
        }
    }

    #[test]
    fn invalid_transition_carries_from_and_to() {
        let from = RunStatus::Running;
        let to = RunStatus::Archived;
        let err = from.transition_to(to).expect_err("should reject");
        assert_eq!(err, InvalidTransition { from, to });
    }
}
