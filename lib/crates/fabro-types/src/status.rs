use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
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
        if self.is_terminal() {
            return false;
        }
        matches!(
            (self, to),
            (Self::Submitted, Self::Queued | Self::Starting)
                | (Self::Queued, Self::Starting)
                | (Self::Starting | Self::Paused, Self::Running)
                | (
                    Self::Running,
                    Self::Blocked | Self::Succeeded | Self::Paused | Self::Removing
                )
                | (Self::Blocked, Self::Running | Self::Paused | Self::Failed)
                | (
                    Self::Starting | Self::Running | Self::Paused | Self::Removing,
                    Self::Failed
                )
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

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Submitted => "submitted",
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
            Self::Removing => "removing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Dead => "dead",
        };
        f.write_str(s)
    }
}

impl FromStr for RunStatus {
    type Err = ParseRunStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "submitted" => Ok(Self::Submitted),
            "queued" => Ok(Self::Queued),
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
            "blocked" => Ok(Self::Blocked),
            "paused" => Ok(Self::Paused),
            "removing" => Ok(Self::Removing),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "dead" => Ok(Self::Dead),
            _ => Err(ParseRunStatusError(s.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParseRunStatusError(String);

impl fmt::Display for ParseRunStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid run status: {:?}", self.0)
    }
}

impl std::error::Error for ParseRunStatusError {}

#[derive(Debug, Clone, PartialEq)]
pub struct InvalidTransition {
    pub from: RunStatus,
    pub to: RunStatus,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid status transition: {} -> {}", self.from, self.to)
    }
}

impl std::error::Error for InvalidTransition {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<StatusReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<BlockedReason>,
    pub updated_at: DateTime<Utc>,
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
    use super::*;

    #[test]
    fn transition_submitted_to_queued() {
        assert!(RunStatus::Submitted.can_transition_to(RunStatus::Queued));
    }

    #[test]
    fn transition_queued_to_starting() {
        assert!(RunStatus::Queued.can_transition_to(RunStatus::Starting));
    }

    #[test]
    fn transition_running_to_blocked() {
        assert!(RunStatus::Running.can_transition_to(RunStatus::Blocked));
    }

    #[test]
    fn transition_blocked_to_running() {
        assert!(RunStatus::Blocked.can_transition_to(RunStatus::Running));
    }

    #[test]
    fn transition_blocked_to_paused() {
        assert!(RunStatus::Blocked.can_transition_to(RunStatus::Paused));
    }

    #[test]
    fn transition_blocked_to_failed() {
        assert!(RunStatus::Blocked.can_transition_to(RunStatus::Failed));
    }

    #[test]
    fn no_direct_paused_to_blocked() {
        assert!(!RunStatus::Paused.can_transition_to(RunStatus::Blocked));
    }

    #[test]
    fn display_and_from_str_queued() {
        let s = RunStatus::Queued.to_string();
        assert_eq!(s, "queued");
        assert_eq!(RunStatus::from_str(&s).unwrap(), RunStatus::Queued);
    }

    #[test]
    fn display_and_from_str_blocked() {
        let s = RunStatus::Blocked.to_string();
        assert_eq!(s, "blocked");
        assert_eq!(RunStatus::from_str(&s).unwrap(), RunStatus::Blocked);
    }

    #[test]
    fn blocked_reason_serde_round_trip() {
        let reason = BlockedReason::HumanInputRequired;
        let json = serde_json::to_string(&reason).unwrap();
        assert_eq!(json, r#""human_input_required""#);
        let parsed: BlockedReason = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, reason);
    }

    #[test]
    fn queued_and_blocked_are_active() {
        assert!(RunStatus::Queued.is_active());
        assert!(RunStatus::Blocked.is_active());
        assert!(!RunStatus::Queued.is_terminal());
        assert!(!RunStatus::Blocked.is_terminal());
    }
}
