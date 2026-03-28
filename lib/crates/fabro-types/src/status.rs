use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Submitted,
    Starting,
    Running,
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
            Self::Submitted | Self::Starting | Self::Running | Self::Paused | Self::Removing
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
            (Self::Submitted, Self::Starting)
                | (Self::Starting, Self::Running)
                | (Self::Starting, Self::Failed)
                | (Self::Running, Self::Succeeded)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Paused)
                | (Self::Running, Self::Removing)
                | (Self::Paused, Self::Running)
                | (Self::Paused, Self::Failed)
                | (Self::Paused, Self::Removing)
                | (Self::Removing, Self::Failed)
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
            Self::Starting => "starting",
            Self::Running => "running",
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
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStatusRecord {
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StatusReason>,
    pub updated_at: DateTime<Utc>,
}

impl RunStatusRecord {
    pub fn new(status: RunStatus, reason: Option<StatusReason>) -> Self {
        Self {
            status,
            reason,
            updated_at: Utc::now(),
        }
    }
}
