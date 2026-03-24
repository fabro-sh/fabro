use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::Context;
use crate::error::{FailureSignature, Result};
use crate::outcome::Outcome;

/// Serializable snapshot of execution state for crash recovery and resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub timestamp: DateTime<Utc>,
    pub current_node: String,
    pub completed_nodes: Vec<String>,
    pub node_retries: HashMap<String, u32>,
    pub context_values: HashMap<String, Value>,
    /// Persisted node outcomes for goal gate checks after resume.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub node_outcomes: HashMap<String, Outcome>,
    /// The node to resume execution at (the next node after the checkpoint's `current_node`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_node_id: Option<String>,
    /// SHA of the git commit created at this checkpoint (when running in a worktree).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
    /// Failure signature counts within the main loop (deterministic/structural failures).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub loop_failure_signatures: HashMap<FailureSignature, usize>,
    /// Failure signature counts across loop_restart edges.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub restart_failure_signatures: HashMap<FailureSignature, usize>,
    /// Per-node visit counts persisted for accurate resume.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub node_visits: HashMap<String, usize>,
}

impl Checkpoint {
    /// Create a checkpoint from the current execution state.
    #[allow(clippy::too_many_arguments)]
    pub fn from_context(
        context: &Context,
        current_node: impl Into<String>,
        completed_nodes: Vec<String>,
        node_retries: HashMap<String, u32>,
        node_outcomes: HashMap<String, Outcome>,
        next_node_id: Option<String>,
        loop_failure_signatures: HashMap<FailureSignature, usize>,
        restart_failure_signatures: HashMap<FailureSignature, usize>,
        node_visits: HashMap<String, usize>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            current_node: current_node.into(),
            completed_nodes,
            node_retries,
            context_values: context.snapshot(),
            node_outcomes,
            next_node_id,
            git_commit_sha: None,
            loop_failure_signatures,
            restart_failure_signatures,
            node_visits,
        }
    }

    /// Save the checkpoint as JSON to a file.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or file writing fails.
    pub fn save(&self, path: &Path) -> Result<()> {
        tracing::debug!(path = %path.display(), node = %self.current_node, "Saving checkpoint");
        crate::save_json(self, path, "checkpoint")
    }

    /// Load a checkpoint from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or deserialization fails.
    pub fn load(path: &Path) -> Result<Self> {
        tracing::debug!(path = %path.display(), "Loading checkpoint");
        crate::load_json(path, "checkpoint")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_context_captures_state() {
        let ctx = Context::new();
        ctx.set("key", serde_json::json!("value"));

        let cp = Checkpoint::from_context(
            &ctx,
            "node_a",
            vec!["start".to_string(), "node_a".to_string()],
            HashMap::new(),
            HashMap::new(),
            None,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );

        assert_eq!(cp.current_node, "node_a");
        assert_eq!(cp.completed_nodes.len(), 2);
        assert_eq!(cp.completed_nodes[0], "start");
        assert_eq!(cp.completed_nodes[1], "node_a");
        assert_eq!(
            cp.context_values.get("key"),
            Some(&serde_json::json!("value"))
        );
        assert!(cp.node_retries.is_empty());
        assert!(cp.node_outcomes.is_empty());
        assert!(cp.next_node_id.is_none());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoint.json");

        let ctx = Context::new();
        ctx.set("goal", serde_json::json!("test"));

        let mut retries = HashMap::new();
        retries.insert("work".to_string(), 2u32);
        let mut outcomes = HashMap::new();
        outcomes.insert("start".to_string(), Outcome::success());
        let cp = Checkpoint::from_context(
            &ctx,
            "work",
            vec!["start".to_string()],
            retries,
            outcomes,
            Some("next_step".to_string()),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );

        cp.save(&path).unwrap();
        let loaded = Checkpoint::load(&path).unwrap();

        assert_eq!(loaded.current_node, "work");
        assert_eq!(loaded.completed_nodes, vec!["start"]);
        assert_eq!(loaded.node_retries.get("work"), Some(&2));
        assert_eq!(
            loaded.context_values.get("goal"),
            Some(&serde_json::json!("test"))
        );
        assert_eq!(
            loaded.node_outcomes.get("start").map(|o| &o.status),
            Some(&crate::outcome::StageStatus::Success)
        );
        assert_eq!(loaded.next_node_id.as_deref(), Some("next_step"));
    }

    #[test]
    fn load_nonexistent_file() {
        let result = Checkpoint::load(Path::new("/nonexistent/checkpoint.json"));
        assert!(result.is_err());
    }

    #[test]
    fn load_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();

        let result = Checkpoint::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn serialization_roundtrip() {
        let ctx = Context::new();
        let cp = Checkpoint::from_context(
            &ctx,
            "n1",
            vec![],
            HashMap::new(),
            HashMap::new(),
            None,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );

        let json = serde_json::to_string(&cp).unwrap();
        let deserialized: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.current_node, "n1");
    }

    #[test]
    fn signature_maps_roundtrip() {
        use crate::error::{FailureCategory, FailureSignature};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoint.json");

        let ctx = Context::new();
        let mut loop_sigs = HashMap::new();
        loop_sigs.insert(
            FailureSignature::new(
                "verify",
                FailureCategory::Deterministic,
                None,
                Some("test failed"),
            ),
            2,
        );
        let mut restart_sigs = HashMap::new();
        restart_sigs.insert(
            FailureSignature::new(
                "build",
                FailureCategory::Structural,
                None,
                Some("scope error"),
            ),
            1,
        );

        let cp = Checkpoint::from_context(
            &ctx,
            "verify",
            vec![],
            HashMap::new(),
            HashMap::new(),
            None,
            loop_sigs,
            restart_sigs,
            HashMap::new(),
        );
        cp.save(&path).unwrap();

        let loaded = Checkpoint::load(&path).unwrap();
        assert_eq!(loaded.loop_failure_signatures.len(), 1);
        assert_eq!(loaded.restart_failure_signatures.len(), 1);
        let sig = FailureSignature::new(
            "verify",
            FailureCategory::Deterministic,
            None,
            Some("test failed"),
        );
        assert_eq!(loaded.loop_failure_signatures.get(&sig), Some(&2));
    }

    #[test]
    fn backward_compat_missing_signature_fields() {
        // A checkpoint saved before signatures were added should deserialize with empty maps
        let json = r#"{
            "timestamp": "2025-01-01T00:00:00Z",
            "current_node": "work",
            "completed_nodes": ["start"],
            "node_retries": {},
            "context_values": {},
            "logs": []
        }"#;
        let cp: Checkpoint = serde_json::from_str(json).unwrap();
        assert!(cp.loop_failure_signatures.is_empty());
        assert!(cp.restart_failure_signatures.is_empty());
    }

    #[test]
    fn backward_compat_old_checkpoint_with_logs_ignored() {
        // Old checkpoints that contain a `logs` field should deserialize fine (field is ignored)
        let json = r#"{
            "timestamp": "2025-01-01T00:00:00Z",
            "current_node": "work",
            "completed_nodes": ["start"],
            "node_retries": {},
            "context_values": {},
            "logs": ["old entry 1", "old entry 2"]
        }"#;
        let cp: Checkpoint = serde_json::from_str(json).unwrap();
        assert_eq!(cp.current_node, "work");
    }

    #[test]
    fn new_checkpoint_does_not_serialize_logs() {
        let ctx = Context::new();
        let cp = Checkpoint::from_context(
            &ctx,
            "n1",
            vec![],
            HashMap::new(),
            HashMap::new(),
            None,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let json = serde_json::to_string(&cp).unwrap();
        assert!(!json.contains("\"logs\""));
    }
}
