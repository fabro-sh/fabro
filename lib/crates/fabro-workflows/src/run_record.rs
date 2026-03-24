use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fabro_config::config::FabroConfig;
use fabro_graphviz::graph::Graph;
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "run.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub created_at: DateTime<Utc>,
    pub config: FabroConfig,
    pub graph: Graph,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug: Option<String>,
    pub working_directory: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_repo_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
}

impl RunRecord {
    pub fn file_name() -> &'static str {
        FILE_NAME
    }

    pub fn save(&self, run_dir: &Path) -> crate::error::Result<()> {
        crate::save_json(self, &run_dir.join(FILE_NAME), "run record")
    }

    pub fn load(run_dir: &Path) -> crate::error::Result<Self> {
        crate::load_json(&run_dir.join(FILE_NAME), "run record")
    }

    /// Workflow name derived from the graph.
    pub fn workflow_name(&self) -> &str {
        if self.graph.name.is_empty() {
            "unnamed"
        } else {
            &self.graph.name
        }
    }

    /// Goal derived from the graph.
    pub fn goal(&self) -> &str {
        self.graph.goal()
    }

    pub fn node_count(&self) -> usize {
        self.graph.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edges.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> RunRecord {
        let graph = Graph {
            name: "test_pipeline".to_string(),
            ..Default::default()
        };
        RunRecord {
            run_id: "run-abc123".to_string(),
            created_at: Utc::now(),
            config: FabroConfig::default(),
            graph,
            workflow_slug: Some("smoke".to_string()),
            working_directory: PathBuf::from("/home/user/project"),
            host_repo_path: Some("/home/user/project".to_string()),
            base_branch: Some("main".to_string()),
            labels: HashMap::from([("env".into(), "test".into())]),
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let record = sample_record();

        record.save(dir.path()).unwrap();
        let loaded = RunRecord::load(dir.path()).unwrap();

        assert_eq!(loaded.run_id, "run-abc123");
        assert_eq!(loaded.workflow_name(), "test_pipeline");
        assert_eq!(loaded.workflow_slug.as_deref(), Some("smoke"));
        assert_eq!(loaded.labels.get("env").map(String::as_str), Some("test"));
    }

    #[test]
    fn load_nonexistent() {
        let dir = PathBuf::from("/tmp/nonexistent-run-record-dir-that-does-not-exist");
        assert!(RunRecord::load(&dir).is_err());
    }

    #[test]
    fn labels_omitted_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = sample_record();
        record.labels = HashMap::new();
        record.host_repo_path = None;
        record.base_branch = None;
        record.workflow_slug = None;
        record.save(dir.path()).unwrap();

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("run.json")).unwrap())
                .unwrap();
        assert!(raw.get("labels").is_none());
        assert!(raw.get("host_repo_path").is_none());
        assert!(raw.get("base_branch").is_none());
        assert!(raw.get("workflow_slug").is_none());
    }
}
