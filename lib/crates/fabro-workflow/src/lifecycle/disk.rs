use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::fs;

use fabro_core::error::CoreError;
use fabro_core::error::Result as CoreResult;
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;

use super::circuit_breaker::CircuitBreakerLifecycle;
use super::git::GitCheckpointResult;
use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::outcome::StageUsage;

type WfRunState = ExecutionState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;

/// Sub-lifecycle responsible for emitting store-backed run lifecycle events.
pub(crate) struct DiskLifecycle {
    pub run_dir: PathBuf,
    pub checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    pub circuit_breaker: Arc<CircuitBreakerLifecycle>,
    pub checkpoint_enabled: bool,
}

pub(super) fn build_checkpoint(
    node: &WorkflowNode,
    result: &WfNodeResult,
    next_node_id: Option<&str>,
    state: &WfRunState,
    loop_failure_signatures: std::collections::HashMap<fabro_types::FailureSignature, usize>,
    restart_failure_signatures: std::collections::HashMap<fabro_types::FailureSignature, usize>,
    git_commit_sha: Option<String>,
) -> fabro_types::Checkpoint {
    let mut node_outcomes = state.node_outcomes.clone();
    node_outcomes.insert(node.id().to_string(), result.outcome.clone());

    fabro_types::Checkpoint {
        timestamp: chrono::Utc::now(),
        current_node: node.id().to_string(),
        completed_nodes: state.completed_nodes.clone(),
        node_outcomes,
        node_retries: state.node_retries.clone(),
        context_values: state.context.snapshot(),
        next_node_id: next_node_id.map(String::from),
        git_commit_sha,
        node_visits: state.node_visits.clone(),
        loop_failure_signatures,
        restart_failure_signatures,
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for DiskLifecycle {
    async fn after_node(
        &self,
        _node: &WorkflowNode,
        _result: &mut WfNodeResult,
        _state: &WfRunState,
    ) -> CoreResult<()> {
        Ok(())
    }

    async fn on_checkpoint(
        &self,
        node: &WorkflowNode,
        result: &WfNodeResult,
        next_node_id: Option<&str>,
        state: &WfRunState,
    ) -> CoreResult<()> {
        if !self.checkpoint_enabled {
            return Ok(());
        }

        let git_commit_sha = self
            .checkpoint_git_result
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|result| result.commit_sha.clone());
        let (loop_sigs, restart_sigs) = self.circuit_breaker.snapshot();
        let checkpoint = build_checkpoint(
            node,
            result,
            next_node_id,
            state,
            loop_sigs,
            restart_sigs,
            git_commit_sha,
        );
        let checkpoint_bytes = serde_json::to_vec_pretty(&checkpoint)
            .map_err(|err| CoreError::Other(format!("failed to serialize checkpoint: {err}")))?;
        fs::write(self.run_dir.join("checkpoint.json"), checkpoint_bytes)
            .await
            .map_err(|err| CoreError::Other(format!("failed to write checkpoint.json: {err}")))?;
        Ok(())
    }
}
