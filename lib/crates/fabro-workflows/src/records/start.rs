use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "start.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartRecord {
    pub run_id: String,
    pub start_time: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
}

impl StartRecord {
    pub fn file_name() -> &'static str {
        FILE_NAME
    }

    pub fn save(&self, run_dir: &Path) -> crate::error::Result<()> {
        crate::save_json(self, &run_dir.join(FILE_NAME), "start record")
    }

    pub fn load(run_dir: &Path) -> crate::error::Result<Self> {
        crate::load_json(&run_dir.join(FILE_NAME), "start record")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_start_record() -> StartRecord {
        StartRecord {
            run_id: "run-1".to_string(),
            start_time: Utc::now(),
            run_branch: Some("fabro/run/run-1".to_string()),
            base_sha: Some("abc123".to_string()),
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let record = sample_start_record();

        record.save(dir.path()).unwrap();
        let loaded = StartRecord::load(dir.path()).unwrap();

        assert_eq!(loaded.run_id, "run-1");
        assert_eq!(loaded.run_branch.as_deref(), Some("fabro/run/run-1"));
        assert_eq!(loaded.base_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn load_nonexistent() {
        let result = StartRecord::load(Path::new("/nonexistent/dir"));
        assert!(result.is_err());
    }

    #[test]
    fn optional_fields_omitted_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = sample_start_record();
        record.run_branch = None;
        record.base_sha = None;
        record.save(dir.path()).unwrap();

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("start.json")).unwrap())
                .unwrap();
        assert!(raw.get("run_branch").is_none());
        assert!(raw.get("base_sha").is_none());
    }
}
