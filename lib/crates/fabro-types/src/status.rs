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

    use super::RunStatus;

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
}
