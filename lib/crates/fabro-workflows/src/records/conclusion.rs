use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::outcome::StageStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageSummary {
    pub stage_id: String,
    pub stage_label: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    pub retries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conclusion {
    pub timestamp: DateTime<Utc>,
    pub status: StageStatus,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<StageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost: Option<f64>,
    #[serde(default)]
    pub total_retries: u32,
    #[serde(default)]
    pub total_input_tokens: i64,
    #[serde(default)]
    pub total_output_tokens: i64,
    #[serde(default)]
    pub total_cache_read_tokens: i64,
    #[serde(default)]
    pub total_cache_write_tokens: i64,
    #[serde(default)]
    pub total_reasoning_tokens: i64,
    #[serde(default)]
    pub has_pricing: bool,
}

impl Conclusion {
    pub fn save(&self, path: &Path) -> Result<()> {
        crate::save_json(self, path, "conclusion")
    }

    pub fn load(path: &Path) -> Result<Self> {
        crate::load_json(path, "conclusion")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_conclusion() -> Conclusion {
        Conclusion {
            timestamp: Utc::now(),
            status: crate::outcome::StageStatus::Success,
            duration_ms: 12345,
            failure_reason: None,
            final_git_commit_sha: Some("deadbeef".to_string()),
            stages: vec![
                StageSummary {
                    stage_id: "plan".to_string(),
                    stage_label: "plan".to_string(),
                    duration_ms: 5000,
                    cost: Some(0.05),
                    retries: 0,
                },
                StageSummary {
                    stage_id: "code".to_string(),
                    stage_label: "code".to_string(),
                    duration_ms: 7345,
                    cost: Some(0.10),
                    retries: 1,
                },
            ],
            total_cost: Some(0.15),
            total_retries: 1,
            total_input_tokens: 5000,
            total_output_tokens: 1500,
            total_cache_read_tokens: 2000,
            total_cache_write_tokens: 500,
            total_reasoning_tokens: 300,
            has_pricing: true,
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conclusion.json");

        let conclusion = sample_conclusion();
        conclusion.save(&path).unwrap();
        let loaded = Conclusion::load(&path).unwrap();

        assert_eq!(loaded.status, crate::outcome::StageStatus::Success);
        assert_eq!(loaded.duration_ms, 12345);
        assert!(loaded.failure_reason.is_none());
        assert_eq!(loaded.final_git_commit_sha.as_deref(), Some("deadbeef"));
        assert_eq!(loaded.stages.len(), 2);
        assert_eq!(loaded.stages[0].stage_id, "plan");
        assert_eq!(loaded.stages[0].duration_ms, 5000);
        assert!((loaded.stages[0].cost.unwrap() - 0.05).abs() < f64::EPSILON);
        assert_eq!(loaded.stages[1].retries, 1);
        assert!((loaded.total_cost.unwrap() - 0.15).abs() < f64::EPSILON);
        assert_eq!(loaded.total_retries, 1);
    }

    #[test]
    fn load_nonexistent_file() {
        let result = Conclusion::load(Path::new("/nonexistent/conclusion.json"));
        assert!(result.is_err());
    }

    #[test]
    fn load_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();

        let result = Conclusion::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn optional_fields_omitted_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conclusion.json");

        let conclusion = Conclusion {
            timestamp: Utc::now(),
            status: crate::outcome::StageStatus::Fail,
            duration_ms: 500,
            failure_reason: None,
            final_git_commit_sha: None,
            stages: vec![],
            total_cost: None,
            total_retries: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            has_pricing: false,
        };
        conclusion.save(&path).unwrap();

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw.get("failure_reason").is_none());
        assert!(raw.get("final_git_commit_sha").is_none());
        assert!(raw.get("stages").is_none());
        assert!(raw.get("total_cost").is_none());
    }

    #[test]
    fn failure_reason_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conclusion.json");

        let conclusion = Conclusion {
            timestamp: Utc::now(),
            status: crate::outcome::StageStatus::Fail,
            duration_ms: 100,
            failure_reason: Some("timeout".to_string()),
            final_git_commit_sha: None,
            stages: vec![],
            total_cost: None,
            total_retries: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            has_pricing: false,
        };
        conclusion.save(&path).unwrap();
        let loaded = Conclusion::load(&path).unwrap();

        assert_eq!(loaded.failure_reason.as_deref(), Some("timeout"));
    }

    #[test]
    fn backward_compat_old_json_without_new_fields() {
        let json = r#"{
            "timestamp": "2025-01-01T00:00:00Z",
            "status": "success",
            "duration_ms": 5000,
            "final_git_commit_sha": "abc123"
        }"#;
        let loaded: Conclusion = serde_json::from_str(json).unwrap();
        assert_eq!(loaded.duration_ms, 5000);
        assert!(loaded.stages.is_empty());
        assert!(loaded.total_cost.is_none());
        assert_eq!(loaded.total_retries, 0);
    }
}
