use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_store::{NodeVisitRef, RunStore};
use fabro_types::NodeStatusRecord;

use fabro_core::error::Result as CoreResult;
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use super::circuit_breaker::CircuitBreakerLifecycle;
use crate::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::outcome::{OutcomeExt, StageUsage};
use crate::records::{Checkpoint, CheckpointExt};
use crate::run_dir::{write_node_status, write_start_record};
use crate::run_options::RunOptions;
use crate::run_status::{RunStatus, write_run_status};
use fabro_graphviz::graph::types::Graph as GvGraph;

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;

/// Sub-lifecycle responsible for writing run state to disk (node status, checkpoints).
pub(crate) struct DiskLifecycle {
    pub run_dir: PathBuf,
    pub run_id: String,
    pub run_store: Arc<dyn RunStore>,
    pub graph: Arc<GvGraph>,
    pub run_options: Arc<RunOptions>,
    pub emitter: Arc<EventEmitter>,
    pub circuit_breaker: Arc<CircuitBreakerLifecycle>,
    pub checkpoint_enabled: bool,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for DiskLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        // Write start.json
        let start_record = write_start_record(&self.run_dir, &self.run_options);
        // Write run status as Running
        write_run_status(&self.run_dir, RunStatus::Running, None);
        if let Err(err) = self.run_store.put_start(&start_record).await {
            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "start_store_save_failed".to_string(),
                message: format!("failed to save start record to store: {err}"),
            });
        }
        if let Err(err) = self
            .run_store
            .put_status(&fabro_types::RunStatusRecord::new(RunStatus::Running, None))
            .await
        {
            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "status_store_save_failed".to_string(),
                message: format!("failed to save running status to store: {err}"),
            });
        }
        Ok(())
    }

    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        state: &WfRunState,
    ) -> CoreResult<()> {
        let gv = node.inner();
        let visit = state.node_visits.get(gv.id.as_str()).copied().unwrap_or(1);
        write_node_status(&self.run_dir, &gv.id, visit, &result.outcome);
        let node_status = NodeStatusRecord {
            status: result.outcome.status.clone(),
            notes: result.outcome.notes.clone(),
            failure_reason: result.outcome.failure_reason().map(ToOwned::to_owned),
            timestamp: chrono::Utc::now(),
        };
        if let Err(err) = self
            .run_store
            .put_node_status(
                &NodeVisitRef {
                    node_id: &gv.id,
                    visit: u32::try_from(visit).unwrap_or(u32::MAX),
                },
                &node_status,
            )
            .await
        {
            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "node_status_store_save_failed".to_string(),
                message: format!("[node: {}] node status store save failed: {err}", node.id()),
            });
        }
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
        if let Err(err) = self.run_store.put_checkpoint(&checkpoint).await {
            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "checkpoint_store_save_failed".to_string(),
                message: format!("[node: {}] checkpoint store save failed: {err}", node.id()),
            });
        }
        if let Err(err) = self.run_store.append_checkpoint(&checkpoint).await {
            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "checkpoint_store_append_failed".to_string(),
                message: format!("[node: {}] checkpoint append failed: {err}", node.id()),
            });
        }

        Ok(())
    }
}
