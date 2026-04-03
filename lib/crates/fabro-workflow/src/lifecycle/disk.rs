use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_store::SlateRunStore;
use fabro_types::RunId;

use fabro_core::error::Result as CoreResult;
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;

use super::circuit_breaker::CircuitBreakerLifecycle;
use crate::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent, append_workflow_event};
use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::outcome::StageUsage;
use crate::run_options::RunOptions;
use fabro_graphviz::graph::types::Graph as GvGraph;

type WfRunState = ExecutionState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;

/// Sub-lifecycle responsible for emitting store-backed run lifecycle events.
pub(crate) struct DiskLifecycle {
    pub run_dir: PathBuf,
    pub run_id: RunId,
    pub run_store: SlateRunStore,
    pub graph: Arc<GvGraph>,
    pub run_options: Arc<RunOptions>,
    pub emitter: Arc<EventEmitter>,
    pub circuit_breaker: Arc<CircuitBreakerLifecycle>,
    pub checkpoint_enabled: bool,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for DiskLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        if let Err(err) = append_workflow_event(
            &self.run_store,
            &self.run_id,
            &WorkflowRunEvent::RunRunning { reason: None },
        )
        .await
        {
            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "status_event_append_failed".to_string(),
                message: format!("failed to append running status event: {err}"),
            });
        }
        Ok(())
    }

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

        let (loop_sigs, restart_sigs) = self.circuit_breaker.snapshot();

        // Build checkpoint from state
        let mut node_outcomes = state.node_outcomes.clone();
        node_outcomes.insert(node.id().to_string(), result.outcome.clone());

        let _checkpoint = fabro_types::Checkpoint {
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
        Ok(())
    }
}
