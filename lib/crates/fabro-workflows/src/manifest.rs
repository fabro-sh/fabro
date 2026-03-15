use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub run_id: String,
    pub workflow_name: String,
    pub goal: String,
    pub start_time: DateTime<Utc>,
    pub node_count: usize,
    pub edge_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_repo_path: Option<String>,
}

impl Manifest {
    pub fn save(&self, path: &Path) -> Result<()> {
        crate::save_json(self, path, "manifest")
    }

    pub fn load(path: &Path) -> Result<Self> {
        crate::load_json(path, "manifest")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        Manifest {
            run_id: "run-1".to_string(),
            workflow_name: "test_pipeline".to_string(),
            goal: "Fix the bug".to_string(),
            start_time: Utc::now(),
            node_count: 3,
            edge_count: 2,
            run_branch: Some("feature/test".to_string()),
            base_sha: Some("abc123".to_string()),
            labels: HashMap::from([("env".into(), "test".into())]),
            base_branch: None,
            workflow_slug: None,
            host_repo_path: None,
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");

        let manifest = sample_manifest();
        manifest.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();

        assert_eq!(loaded.run_id, "run-1");
        assert_eq!(loaded.workflow_name, "test_pipeline");
        assert_eq!(loaded.goal, "Fix the bug");
        assert_eq!(loaded.node_count, 3);
        assert_eq!(loaded.edge_count, 2);
        assert_eq!(loaded.run_branch.as_deref(), Some("feature/test"));
        assert_eq!(loaded.base_sha.as_deref(), Some("abc123"));
        assert_eq!(loaded.labels.get("env").map(String::as_str), Some("test"));
    }

    #[test]
    fn load_nonexistent_file() {
        let result = Manifest::load(Path::new("/nonexistent/manifest.json"));
        assert!(result.is_err());
    }

    #[test]
    fn load_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();

        let result = Manifest::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn save_and_load_roundtrip_with_slug() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");

        let mut manifest = sample_manifest();
        manifest.workflow_slug = Some("smoke".to_string());
        manifest.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();

        assert_eq!(loaded.workflow_slug.as_deref(), Some("smoke"));
    }

    #[test]
    fn labels_omitted_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");

        let mut manifest = sample_manifest();
        manifest.labels = HashMap::new();
        manifest.run_branch = None;
        manifest.base_sha = None;
        manifest.save(&path).unwrap();

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw.get("labels").is_none());
        assert!(raw.get("run_branch").is_none());
        assert!(raw.get("base_sha").is_none());
        assert!(raw.get("workflow_slug").is_none());
        assert!(raw.get("host_repo_path").is_none());
    }
}
