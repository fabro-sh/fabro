use std::fmt;
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Status of a workflow run in its lifecycle.
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
    /// Terminal statuses cannot transition to anything (except Dead via `can_transition_to`).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Dead)
    }

    /// Active statuses represent runs that are in-progress or about to run.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Submitted | Self::Starting | Self::Running | Self::Paused | Self::Removing
        )
    }

    /// Check whether a transition from `self` to `to` is valid.
    pub fn can_transition_to(self, to: Self) -> bool {
        // Any state can transition to Dead
        if to == Self::Dead {
            return true;
        }
        // Terminal states cannot transition to anything else
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

    /// Attempt to transition from `self` to `to`. Returns an error if the transition is invalid.
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

/// Reason for the current status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusReason {
    // Succeeded reasons
    Completed,
    PartialSuccess,
    // Failed reasons
    WorkflowError,
    Cancelled,
    Terminated,
    TransientInfra,
    BudgetExhausted,
    LaunchFailed,
    BootstrapFailed,
    SandboxInitFailed,
    // Non-terminal reasons
    SandboxInitializing,
}

/// Persisted record of a run's status, written to `status.json`.
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

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// Write the run status to `status.json` (best-effort).
pub fn write_run_status(run_dir: &Path, status: RunStatus, reason: Option<StatusReason>) {
    let record = RunStatusRecord::new(status, reason);
    let _ = record.save(&run_dir.join("status.json"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states() {
        assert!(RunStatus::Succeeded.is_terminal());
        assert!(RunStatus::Failed.is_terminal());
        assert!(RunStatus::Dead.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
        assert!(!RunStatus::Submitted.is_terminal());
        assert!(!RunStatus::Starting.is_terminal());
        assert!(!RunStatus::Paused.is_terminal());
        assert!(!RunStatus::Removing.is_terminal());
    }

    #[test]
    fn active_states() {
        assert!(RunStatus::Submitted.is_active());
        assert!(RunStatus::Starting.is_active());
        assert!(RunStatus::Running.is_active());
        assert!(RunStatus::Paused.is_active());
        assert!(RunStatus::Removing.is_active());
        assert!(!RunStatus::Succeeded.is_active());
        assert!(!RunStatus::Failed.is_active());
        assert!(!RunStatus::Dead.is_active());
    }

    #[test]
    fn valid_transitions() {
        assert!(RunStatus::Submitted.can_transition_to(RunStatus::Starting));
        assert!(RunStatus::Starting.can_transition_to(RunStatus::Running));
        assert!(RunStatus::Starting.can_transition_to(RunStatus::Failed));
        assert!(RunStatus::Running.can_transition_to(RunStatus::Succeeded));
        assert!(RunStatus::Running.can_transition_to(RunStatus::Failed));
        assert!(RunStatus::Running.can_transition_to(RunStatus::Paused));
        assert!(RunStatus::Running.can_transition_to(RunStatus::Removing));
        assert!(RunStatus::Paused.can_transition_to(RunStatus::Running));
        assert!(RunStatus::Paused.can_transition_to(RunStatus::Failed));
        assert!(RunStatus::Paused.can_transition_to(RunStatus::Removing));
        assert!(RunStatus::Removing.can_transition_to(RunStatus::Failed));
    }

    #[test]
    fn dead_reachable_from_any() {
        let all = [
            RunStatus::Submitted,
            RunStatus::Starting,
            RunStatus::Running,
            RunStatus::Paused,
            RunStatus::Removing,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Dead,
        ];
        for s in all {
            assert!(
                s.can_transition_to(RunStatus::Dead),
                "{s} should be able to transition to Dead"
            );
        }
    }

    #[test]
    fn invalid_transitions() {
        assert!(!RunStatus::Submitted.can_transition_to(RunStatus::Running));
        assert!(!RunStatus::Submitted.can_transition_to(RunStatus::Succeeded));
        assert!(!RunStatus::Starting.can_transition_to(RunStatus::Succeeded));
        assert!(!RunStatus::Running.can_transition_to(RunStatus::Starting));
        assert!(!RunStatus::Succeeded.can_transition_to(RunStatus::Running));
        assert!(!RunStatus::Failed.can_transition_to(RunStatus::Running));
    }

    #[test]
    fn transition_to_returns_result() {
        assert_eq!(
            RunStatus::Submitted.transition_to(RunStatus::Starting),
            Ok(RunStatus::Starting)
        );
        assert!(RunStatus::Failed.transition_to(RunStatus::Running).is_err());
    }

    #[test]
    fn display_and_from_str_roundtrip() {
        let all = [
            RunStatus::Submitted,
            RunStatus::Starting,
            RunStatus::Running,
            RunStatus::Paused,
            RunStatus::Removing,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Dead,
        ];
        for s in all {
            let text = s.to_string();
            let parsed: RunStatus = text.parse().unwrap();
            assert_eq!(s, parsed);
        }
    }

    #[test]
    fn from_str_invalid() {
        assert!("bogus".parse::<RunStatus>().is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let record =
            RunStatusRecord::new(RunStatus::Running, Some(StatusReason::SandboxInitializing));
        let json = serde_json::to_string(&record).unwrap();
        let parsed: RunStatusRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, RunStatus::Running);
        assert_eq!(parsed.reason, Some(StatusReason::SandboxInitializing));
    }

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        let record = RunStatusRecord::new(RunStatus::Succeeded, Some(StatusReason::Completed));
        record.save(&path).unwrap();
        let loaded = RunStatusRecord::load(&path).unwrap();
        assert_eq!(loaded.status, RunStatus::Succeeded);
        assert_eq!(loaded.reason, Some(StatusReason::Completed));
    }

    #[test]
    fn launch_failed_reason_roundtrip() {
        let record = RunStatusRecord::new(RunStatus::Failed, Some(StatusReason::LaunchFailed));
        let json = serde_json::to_string(&record).unwrap();
        let parsed: RunStatusRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reason, Some(StatusReason::LaunchFailed));
    }

    #[test]
    fn bootstrap_failed_reason_roundtrip() {
        let record = RunStatusRecord::new(RunStatus::Failed, Some(StatusReason::BootstrapFailed));
        let json = serde_json::to_string(&record).unwrap();
        let parsed: RunStatusRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reason, Some(StatusReason::BootstrapFailed));
    }

    #[test]
    fn load_missing_file() {
        let result = RunStatusRecord::load(Path::new("/nonexistent/status.json"));
        assert!(result.is_err());
    }
}
