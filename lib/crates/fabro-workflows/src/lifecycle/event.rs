use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use fabro_core::error::Result as CoreResult;
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{
    AttemptContext, AttemptResultContext, EdgeContext, EdgeDecision, NodeDecision, RunLifecycle,
};
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use super::git::GitCheckpointResult;
use crate::artifact::ArtifactStore;
use crate::error::FabroError;
use crate::event::{EventEmitter, WorkflowRunEvent};
use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::outcome::{
    FailureCategory, FailureDetail, Outcome, StageStatus, StageUsage, stage_usage_to_llm,
};
use fabro_graphviz::graph::types::Node as GvNode;

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;

fn node_script(node: &GvNode) -> Option<String> {
    node.attrs
        .get("script")
        .or_else(|| node.attrs.get("tool_command"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Sub-lifecycle responsible for emitting workflow run events.
pub struct EventLifecycle {
    pub emitter: Arc<EventEmitter>,
    pub graph_name: String,
    pub run_id: String,
    pub run_start: Mutex<Instant>,
    /// Set in on_edge_selected when loop_restart approved; emitted+cleared in on_run_start.
    pub restarted_from: Arc<Mutex<Option<(String, String)>>>,
    // Config for WorkflowRunStarted payload
    pub base_branch: Option<String>,
    pub base_sha: Option<String>,
    pub run_branch: Option<String>,
    pub worktree_dir: Option<String>,
    pub goal: Option<String>,
    // Shared swappable handle (same instance as orchestrator)
    pub artifact_store: Arc<Mutex<ArtifactStore>>,
    // Cross-lifecycle data
    pub checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    pub last_git_sha: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for EventLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
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
            base_branch: self.base_branch.clone(),
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
            script: node_script(gv),
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
    ) -> CoreResult<NodeDecision<Option<StageUsage>>> {
        let gv = ctx.node.inner();
        self.emitter.emit(&WorkflowRunEvent::StageStarted {
            node_id: gv.id.clone(),
            name: gv.label().to_string(),
            index: state.stage_index,
            handler_type: gv.handler_type().map(String::from),
            script: node_script(gv),
            attempt: ctx.attempt as usize,
            max_attempts: ctx.max_attempts as usize,
        });
        Ok(NodeDecision::Continue)
    }

    async fn after_attempt(
        &self,
        ctx: &AttemptResultContext<'_, WorkflowGraph>,
        state: &WfRunState,
    ) -> CoreResult<()> {
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
    ) -> CoreResult<()> {
        let outcome = &result.outcome;
        // Skipped nodes had no StageStarted, so skip completion events (engine.rs:2080)
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
                failure: outcome.failure.clone(),
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
    ) -> CoreResult<EdgeDecision> {
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
        Ok(EdgeDecision::Continue)
    }

    async fn on_checkpoint(
        &self,
        node: &WorkflowNode,
        result: &WfNodeResult,
        _next_node_id: Option<&str>,
        _state: &WfRunState,
    ) -> CoreResult<()> {
        let status = result.outcome.status.to_string();

        // Read git checkpoint result (set by GitLifecycle)
        let git_result = self.checkpoint_git_result.lock().unwrap().clone();

        let git_sha = git_result.as_ref().and_then(|r| r.commit_sha.clone());

        self.emitter.emit(&WorkflowRunEvent::CheckpointCompleted {
            node_id: node.id().to_string(),
            status,
            git_commit_sha: git_sha.clone(),
        });

        // Emit GitCommit + GitPush events if git produced results
        if let Some(ref result) = git_result {
            if let Some(ref sha) = result.commit_sha {
                self.emitter.emit(&WorkflowRunEvent::GitCommit {
                    node_id: Some(node.id().to_string()),
                    sha: sha.clone(),
                });
            }
            for (branch, success) in &result.push_results {
                self.emitter.emit(&WorkflowRunEvent::GitPush {
                    branch: branch.clone(),
                    success: *success,
                });
            }
        }

        Ok(())
    }

    async fn on_run_end(&self, outcome: &Outcome, state: &WfRunState) {
        if state.cancelled {
            return;
        }
        let duration_ms = self.run_start.lock().unwrap().elapsed().as_millis() as u64;
        let artifact_count = self.artifact_store.lock().unwrap().list().len();
        let last_sha = self.last_git_sha.lock().unwrap().clone();
        let total_cost = {
            let sum: f64 = state
                .node_outcomes
                .values()
                .filter_map(|o| o.usage.as_ref()?.cost)
                .sum();
            if sum > 0.0 { Some(sum) } else { None }
        };
        let run_usage = state
            .node_outcomes
            .values()
            .filter_map(|o| o.usage.as_ref().map(stage_usage_to_llm))
            .reduce(|a, b| a + b);

        if outcome.status == StageStatus::Success || outcome.status == StageStatus::PartialSuccess {
            self.emitter.emit(&WorkflowRunEvent::WorkflowRunCompleted {
                duration_ms,
                artifact_count,
                status: outcome.status.to_string(),
                total_cost,
                final_git_commit_sha: last_sha,
                usage: run_usage,
            });
        } else {
            let error_msg = outcome
                .failure
                .as_ref()
                .map(|f| f.message.clone())
                .unwrap_or_else(|| "run failed".to_string());
            self.emitter.emit(&WorkflowRunEvent::WorkflowRunFailed {
                error: FabroError::engine(error_msg),
                duration_ms,
                git_commit_sha: last_sha,
            });
        }
    }
}
