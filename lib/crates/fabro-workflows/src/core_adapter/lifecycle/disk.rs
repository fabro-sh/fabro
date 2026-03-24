use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use super::super::graph::WorkflowGraph;
use super::super::WorkflowNode;
use super::circuit_breaker::CircuitBreakerLifecycle;
use crate::checkpoint::Checkpoint;
use crate::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use crate::outcome::StageUsage;

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;

/// Sub-lifecycle responsible for writing run state to disk (node status, checkpoints).
pub struct DiskLifecycle {
    pub run_dir: PathBuf,
    pub run_id: String,
    pub emitter: Arc<EventEmitter>,
    pub circuit_breaker: Arc<CircuitBreakerLifecycle>,
    pub checkpoint_enabled: bool,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for DiskLifecycle {
    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        let gv = node.inner();
        let outcome = &result.outcome;
        let status_dir = self.run_dir.join("stages").join(&gv.id);
        let _ = std::fs::create_dir_all(&status_dir);
        let status_path = status_dir.join("status.json");
        let _ = crate::save_json(outcome, &status_path, "node_status");
        Ok(())
    }

    async fn on_checkpoint(
        &self,
        node: &WorkflowNode,
        result: &WfNodeResult,
        next_node_id: Option<&str>,
        state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        if !self.checkpoint_enabled {
            return Ok(());
        }

        let (loop_sigs, restart_sigs) = self.circuit_breaker.snapshot();

        // Build checkpoint from state
        let mut node_outcomes = state.node_outcomes.clone();
        node_outcomes.insert(node.id().to_string(), result.outcome.clone());

        let checkpoint = Checkpoint {
            timestamp: chrono::Utc::now(),
            current_node: node.id().to_string(),
            completed_nodes: state.completed_nodes.clone(),
            node_outcomes,
            node_retries: state.node_retries.clone(),
            context_values: state.context.snapshot(),
            next_node_id: next_node_id.map(String::from),
            git_commit_sha: None,
            node_visits: state.node_visits.clone(),
            loop_failure_signatures: loop_sigs,
            restart_failure_signatures: restart_sigs,
        };

        let checkpoint_path = self.run_dir.join("checkpoint.json");
        if let Err(e) = checkpoint.save(&checkpoint_path) {
            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "checkpoint_disk_save_failed".to_string(),
                message: format!("[node: {}] checkpoint save failed: {e}", node.id()),
            });
        }

        Ok(())
    }
}
