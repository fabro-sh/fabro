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
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
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
        if self.is_terminal() {
            return false;
        }
        matches!(
            (self, to),
            (Self::Submitted, Self::Queued | Self::Starting)
                | (
                    Self::Queued,
                    Self::Starting | Self::Failed | Self::Cancelled
                )
                | (Self::Starting | Self::Paused | Self::Blocked, Self::Running)
                | (
                    Self::Starting | Self::Running | Self::Blocked | Self::Paused | Self::Removing,
                    Self::Failed | Self::Cancelled
                )
                | (
                    Self::Running,
                    Self::Completed | Self::Blocked | Self::Paused | Self::Removing
                )
                | (
                    Self::Blocked,
                    Self::Completed | Self::Paused | Self::Removing
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
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
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
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
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
    pub reason: Option<StatusReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<BlockedReason>,
    pub updated_at: DateTime<Utc>,
}

impl RunStatusRecord {
    pub fn new(status: RunStatus, reason: Option<StatusReason>) -> Self {
        Self {
            status,
            reason,
            blocked_reason: None,
            updated_at: Utc::now(),
        }
    }
}
