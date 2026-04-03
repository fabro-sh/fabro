use std::collections::HashMap;
use std::path::Path;

pub use fabro_types::checkpoint::Checkpoint;

use crate::context::Context;
use crate::error::{FabroError, FailureSignature, Result as CrateResult};
use crate::outcome::Outcome;

pub trait CheckpointExt {
    #[allow(clippy::too_many_arguments)]
    fn from_context(
        context: &Context,
        current_node: impl Into<String>,
        completed_nodes: Vec<String>,
        node_retries: HashMap<String, u32>,
        node_outcomes: HashMap<String, Outcome>,
        next_node_id: Option<String>,
        loop_failure_signatures: HashMap<FailureSignature, usize>,
        restart_failure_signatures: HashMap<FailureSignature, usize>,
        node_visits: HashMap<String, usize>,
    ) -> Self
    where
        Self: Sized;
    fn save(&self, path: &Path) -> CrateResult<()>;
    fn load(path: &Path) -> CrateResult<Self>
    where
        Self: Sized;
}

impl CheckpointExt for Checkpoint {
    fn from_context(
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
            timestamp: chrono::Utc::now(),
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

    fn save(&self, path: &Path) -> CrateResult<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| FabroError::Checkpoint(format!("checkpoint serialize failed: {e}")))?;
        std::fs::write(path, json)?;
        Ok(())
    }

    fn load(path: &Path) -> CrateResult<Self> {
        crate::load_json(path, "checkpoint")
    }
}
