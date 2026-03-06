use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{ArcError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunFinal {
    pub timestamp: DateTime<Utc>,
    pub status: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
}

impl RunFinal {
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| ArcError::Checkpoint(format!("run_final serialize failed: {e}")))?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read_to_string(path)?;
        let run_final: Self = serde_json::from_str(&data)
            .map_err(|e| ArcError::Checkpoint(format!("run_final deserialize failed: {e}")))?;
        Ok(run_final)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_run_final() -> RunFinal {
        RunFinal {
            timestamp: Utc::now(),
            status: "success".to_string(),
            duration_ms: 12345,
            failure_reason: None,
            final_git_commit_sha: Some("deadbeef".to_string()),
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("final.json");

        let run_final = sample_run_final();
        run_final.save(&path).unwrap();
        let loaded = RunFinal::load(&path).unwrap();

        assert_eq!(loaded.status, "success");
        assert_eq!(loaded.duration_ms, 12345);
        assert!(loaded.failure_reason.is_none());
        assert_eq!(
            loaded.final_git_commit_sha.as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn load_nonexistent_file() {
        let result = RunFinal::load(Path::new("/nonexistent/final.json"));
        assert!(result.is_err());
    }

    #[test]
    fn load_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();

        let result = RunFinal::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn optional_fields_omitted_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("final.json");

        let run_final = RunFinal {
            timestamp: Utc::now(),
            status: "fail".to_string(),
            duration_ms: 500,
            failure_reason: None,
            final_git_commit_sha: None,
        };
        run_final.save(&path).unwrap();

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw.get("failure_reason").is_none());
        assert!(raw.get("final_git_commit_sha").is_none());
    }

    #[test]
    fn failure_reason_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("final.json");

        let run_final = RunFinal {
            timestamp: Utc::now(),
            status: "fail".to_string(),
            duration_ms: 100,
            failure_reason: Some("timeout".to_string()),
            final_git_commit_sha: None,
        };
        run_final.save(&path).unwrap();
        let loaded = RunFinal::load(&path).unwrap();

        assert_eq!(loaded.failure_reason.as_deref(), Some("timeout"));
    }
}
