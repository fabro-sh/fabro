use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{AttemptContext, AttemptResultContext, EdgeContext, RunLifecycle};
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use super::super::graph::WorkflowGraph;
use super::super::WorkflowNode;
use crate::event::{EventEmitter, WorkflowRunEvent};
use crate::outcome::{FailureCategory, FailureDetail, Outcome, StageStatus, StageUsage};

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;

/// Sub-lifecycle responsible for emitting workflow run events.
pub struct EventLifecycle {
    pub emitter: Arc<EventEmitter>,
    pub graph_name: String,
    pub run_id: String,
    pub run_start: Mutex<Instant>,
    /// Set in on_edge_selected when loop_restart approved; emitted+cleared in on_run_start.
    pub restarted_from: Arc<Mutex<Option<(String, String)>>>,
    // Config for WorkflowRunStarted payload
    pub base_sha: Option<String>,
    pub run_branch: Option<String>,
    pub worktree_dir: Option<String>,
    pub goal: Option<String>,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for EventLifecycle {
    async fn on_run_start(
        &self,
        _graph: &WorkflowGraph,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        // If restarted_from is Some, emit LoopRestart and clear it
        {
            let mut restarted = self.restarted_from.lock().unwrap();
            if let Some((from_node, to_node)) = restarted.take() {
                self.emitter
                    .emit(&WorkflowRunEvent::LoopRestart { from_node, to_node });
            }
        }

        // Reset run_start for duration measurement
        *self.run_start.lock().unwrap() = Instant::now();

        // Emit WorkflowRunStarted
        self.emitter.emit(&WorkflowRunEvent::WorkflowRunStarted {
            name: self.graph_name.clone(),
            run_id: self.run_id.clone(),
            base_sha: self.base_sha.clone(),
            run_branch: self.run_branch.clone(),
            worktree_dir: self.worktree_dir.clone(),
            goal: self.goal.clone(),
        });

        Ok(())
    }

    async fn on_terminal_reached(
        &self,
        node: &WorkflowNode,
        goal_gates_passed: bool,
        state: &WfRunState,
    ) {
        if !goal_gates_passed {
            return;
        }
        let gv = node.inner();
        let stage_index = state.stage_index;
        self.emitter.emit(&WorkflowRunEvent::StageStarted {
            node_id: gv.id.clone(),
            name: gv.label().to_string(),
            index: stage_index,
            handler_type: gv.handler_type().map(String::from),
            script: None,
            attempt: 1,
            max_attempts: 1,
        });
        self.emitter.emit(&WorkflowRunEvent::StageCompleted {
            node_id: gv.id.clone(),
            name: gv.label().to_string(),
            index: stage_index,
            duration_ms: 0,
            status: StageStatus::Success.to_string(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            usage: None,
            failure: None,
            notes: None,
            files_touched: Vec::new(),
            attempt: 1,
            max_attempts: 1,
        });
    }

    async fn before_attempt(
        &self,
        ctx: &AttemptContext<'_, WorkflowGraph>,
        state: &WfRunState,
    ) -> fabro_core::error::Result<fabro_core::lifecycle::NodeDecision<Option<StageUsage>>> {
        let gv = ctx.node.inner();
        self.emitter.emit(&WorkflowRunEvent::StageStarted {
            node_id: gv.id.clone(),
            name: gv.label().to_string(),
            index: state.stage_index,
            handler_type: gv.handler_type().map(String::from),
            script: None,
            attempt: ctx.attempt as usize,
            max_attempts: ctx.max_attempts as usize,
        });
        Ok(fabro_core::lifecycle::NodeDecision::Continue)
    }

    async fn after_attempt(
        &self,
        ctx: &AttemptResultContext<'_, WorkflowGraph>,
        state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        if ctx.will_retry {
            let gv = ctx.node.inner();
            let outcome = &ctx.result.outcome;
            let stage_index = state.stage_index;

            self.emitter.emit(&WorkflowRunEvent::StageFailed {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                failure: outcome.failure.clone().unwrap_or_else(|| {
                    FailureDetail::new("handler failed", FailureCategory::TransientInfra)
                }),
                will_retry: true,
            });

            self.emitter.emit(&WorkflowRunEvent::StageRetrying {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                attempt: ctx.attempt as usize,
                max_attempts: ctx.result.max_attempts as usize,
                delay_ms: ctx.backoff_delay.map(|d| d.as_millis() as u64).unwrap_or(0),
            });
        }
        Ok(())
    }

    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        let outcome = &result.outcome;
        // Skip events for Skipped nodes
        if outcome.status == StageStatus::Skipped {
            return Ok(());
        }
        let gv = node.inner();
        let stage_index = state.stage_index;
        let duration_ms = result.duration.as_millis() as u64;

        if outcome.status == StageStatus::Fail {
            self.emitter.emit(&WorkflowRunEvent::StageFailed {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                failure: outcome.failure.clone().unwrap_or_else(|| {
                    FailureDetail::new("handler failed", FailureCategory::Deterministic)
                }),
                will_retry: false,
            });
        } else {
            self.emitter.emit(&WorkflowRunEvent::StageCompleted {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                duration_ms,
                status: outcome.status.to_string(),
                preferred_label: outcome.preferred_label.clone(),
                suggested_next_ids: outcome.suggested_next_ids.clone(),
                usage: outcome.usage.clone(),
                failure: None,
                notes: outcome.notes.clone(),
                files_touched: outcome.files_touched.clone(),
                attempt: result.attempts as usize,
                max_attempts: result.max_attempts as usize,
            });
        }
        Ok(())
    }

    async fn on_edge_selected(
        &self,
        ctx: &EdgeContext<'_, WorkflowGraph>,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<fabro_core::lifecycle::EdgeDecision> {
        let outcome = ctx.outcome;
        let label = ctx
            .edge
            .as_ref()
            .and_then(|e| e.inner().label().map(String::from));
        let condition = ctx
            .edge
            .as_ref()
            .and_then(|e| e.inner().condition().map(String::from));
        self.emitter.emit(&WorkflowRunEvent::EdgeSelected {
            from_node: ctx.from.to_string(),
            to_node: ctx.to.to_string(),
            label,
            condition,
            reason: ctx.reason.to_string(),
            preferred_label: outcome.preferred_label.clone(),
            suggested_next_ids: outcome.suggested_next_ids.clone(),
            stage_status: outcome.status.to_string(),
            is_jump: ctx.is_jump,
        });
        Ok(fabro_core::lifecycle::EdgeDecision::Continue)
    }

    async fn on_checkpoint(
        &self,
        node: &WorkflowNode,
        result: &WfNodeResult,
        _next_node_id: Option<&str>,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        let status = result.outcome.status.to_string();
        self.emitter.emit(&WorkflowRunEvent::CheckpointCompleted {
            node_id: node.id().to_string(),
            status,
            git_commit_sha: None,
        });
        Ok(())
    }

    async fn on_run_end(&self, outcome: &Outcome, state: &WfRunState) {
        if state.cancelled {
            return;
        }
        let duration_ms = self.run_start.lock().unwrap().elapsed().as_millis() as u64;

        if outcome.status == StageStatus::Success || outcome.status == StageStatus::PartialSuccess {
            self.emitter.emit(&WorkflowRunEvent::WorkflowRunCompleted {
                duration_ms,
                artifact_count: 0,
                status: outcome.status.to_string(),
                total_cost: None,
                final_git_commit_sha: None,
                usage: None,
            });
        } else {
            let error_msg = outcome
                .failure
                .as_ref()
                .map(|f| f.message.clone())
                .unwrap_or_else(|| "run failed".to_string());
            self.emitter.emit(&WorkflowRunEvent::WorkflowRunFailed {
                error: crate::error::FabroError::engine(error_msg),
                duration_ms,
                git_commit_sha: None,
            });
        }
    }
}
