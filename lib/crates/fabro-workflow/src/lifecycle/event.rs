use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use fabro_core::error::Result as CoreResult;
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{
    AttemptContext, AttemptResultContext, EdgeContext, EdgeDecision, NodeDecision, RunLifecycle,
};
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;

use super::circuit_breaker::CircuitBreakerLifecycle;
use super::git::GitCheckpointResult;
use crate::context;
use crate::error::FabroError;
use crate::event::{Emitter, Event};
use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::outcome::{
    FailureCategory, FailureDetail, Outcome, StageStatus, StageUsage, stage_usage_to_llm,
};
use fabro_types::{RunId, StatusReason};

type WfRunState = ExecutionState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;
type FailureSignatureSnapshot = (
    Option<BTreeMap<String, usize>>,
    Option<BTreeMap<String, usize>>,
);

/// Sub-lifecycle responsible for emitting workflow run events.
pub(crate) struct EventLifecycle {
    pub emitter: Arc<Emitter>,
    pub graph_name: String,
    pub run_id: RunId,
    pub run_start: Mutex<Instant>,
    /// Set in on_edge_selected when loop_restart approved; emitted+cleared in on_run_start.
    pub restarted_from: Arc<Mutex<Option<(String, String)>>>,
    // Config for WorkflowRunStarted payload
    pub base_branch: Option<String>,
    pub base_sha: Option<String>,
    pub run_branch: Option<String>,
    pub worktree_dir: Option<String>,
    pub goal: Option<String>,
    pub captured_artifact_count: Arc<AtomicUsize>,
    // Cross-lifecycle data
    pub checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    pub last_git_sha: Arc<Mutex<Option<String>>>,
    pub final_patch: Arc<Mutex<Option<String>>>,
    pub circuit_breaker: Arc<CircuitBreakerLifecycle>,
}

fn snapshot_failure_signatures(
    circuit_breaker: &CircuitBreakerLifecycle,
) -> FailureSignatureSnapshot {
    let (loop_sigs, restart_sigs) = circuit_breaker.snapshot();
    let loop_sigs = (!loop_sigs.is_empty()).then(|| {
        loop_sigs
            .into_iter()
            .map(|(sig, count)| (sig.to_string(), count))
            .collect::<BTreeMap<_, _>>()
    });
    let restart_sigs = (!restart_sigs.is_empty()).then(|| {
        restart_sigs
            .into_iter()
            .map(|(sig, count)| (sig.to_string(), count))
            .collect::<BTreeMap<_, _>>()
    });
    (loop_sigs, restart_sigs)
}

fn response_from_outcome(node_id: &str, outcome: &Outcome) -> Option<String> {
    outcome
        .context_updates
        .get(&context::keys::response_key(node_id))
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for EventLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        // If restarted_from is Some, emit LoopRestart and clear it
        {
            let mut restarted = self.restarted_from.lock().unwrap();
            if let Some((from_node, to_node)) = restarted.take() {
                self.emitter
                    .emit(&Event::LoopRestart { from_node, to_node });
            }
        }

        // Reset run_start for duration measurement
        *self.run_start.lock().unwrap() = Instant::now();

        // Emit RunStarted
        self.emitter.emit(&Event::WorkflowRunStarted {
            name: self.graph_name.clone(),
            run_id: self.run_id,
            base_branch: self.base_branch.clone(),
            base_sha: self.base_sha.clone(),
            run_branch: self.run_branch.clone(),
            worktree_dir: self.worktree_dir.clone(),
            goal: self.goal.clone(),
        });
        self.emitter.emit(&Event::RunRunning { reason: None });

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
        let (loop_failure_signatures, restart_failure_signatures) =
            snapshot_failure_signatures(&self.circuit_breaker);
        self.emitter.emit(&Event::StageStarted {
            node_id: gv.id.clone(),
            name: gv.label().to_string(),
            index: stage_index,
            handler_type: gv.handler_type().unwrap_or_default().to_string(),
            attempt: 1,
            max_attempts: 1,
        });
        self.emitter.emit(&Event::StageCompleted {
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
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: None,
            loop_failure_signatures,
            restart_failure_signatures,
            response: state
                .context
                .get(&context::keys::response_key(&gv.id))
                .and_then(|value| value.as_str().map(ToOwned::to_owned)),
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
        self.emitter.emit(&Event::StageStarted {
            node_id: gv.id.clone(),
            name: gv.label().to_string(),
            index: state.stage_index,
            handler_type: gv.handler_type().unwrap_or_default().to_string(),
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

            self.emitter.emit(&Event::StageFailed {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                failure: outcome.failure.clone().unwrap_or_else(|| {
                    FailureDetail::new("handler failed", FailureCategory::TransientInfra)
                }),
                will_retry: true,
            });

            self.emitter.emit(&Event::StageRetrying {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                attempt: ctx.attempt as usize,
                max_attempts: ctx.result.max_attempts as usize,
                delay_ms: ctx
                    .backoff_delay
                    .map_or(0, |d| u64::try_from(d.as_millis()).unwrap()),
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
        let duration_ms = u64::try_from(result.duration.as_millis()).unwrap();
        let (loop_failure_signatures, restart_failure_signatures) =
            snapshot_failure_signatures(&self.circuit_breaker);

        if outcome.status == StageStatus::Fail {
            self.emitter.emit(&Event::StageFailed {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                failure: outcome.failure.clone().unwrap_or_else(|| {
                    FailureDetail::new("handler failed", FailureCategory::Deterministic)
                }),
                will_retry: false,
            });
        } else {
            self.emitter.emit(&Event::StageCompleted {
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
                context_updates: (!outcome.context_updates.is_empty()).then(|| {
                    outcome
                        .context_updates
                        .clone()
                        .into_iter()
                        .collect::<BTreeMap<_, _>>()
                }),
                jump_to_node: outcome.jump_to_node.clone(),
                context_values: {
                    let snapshot = state.context.snapshot();
                    (!snapshot.is_empty()).then(|| snapshot.into_iter().collect::<BTreeMap<_, _>>())
                },
                node_visits: (!state.node_visits.is_empty()).then(|| {
                    state
                        .node_visits
                        .clone()
                        .into_iter()
                        .collect::<BTreeMap<_, _>>()
                }),
                loop_failure_signatures,
                restart_failure_signatures,
                response: response_from_outcome(&gv.id, outcome),
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
        self.emitter.emit(&Event::EdgeSelected {
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
        next_node_id: Option<&str>,
        state: &WfRunState,
    ) -> CoreResult<()> {
        let status = result.outcome.status.to_string();

        // Read git checkpoint result (set by GitLifecycle)
        let git_result = self.checkpoint_git_result.lock().unwrap().clone();

        let git_sha = git_result.as_ref().and_then(|r| r.commit_sha.clone());
        let diff = git_result.as_ref().and_then(|r| r.diff.clone());
        let (loop_failure_signatures, restart_failure_signatures) =
            snapshot_failure_signatures(&self.circuit_breaker);

        self.emitter.emit(&Event::CheckpointCompleted {
            node_id: node.id().to_string(),
            status,
            current_node: node.id().to_string(),
            completed_nodes: state.completed_nodes.clone(),
            node_retries: state
                .node_retries
                .clone()
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
            context_values: state
                .context
                .snapshot()
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
            node_outcomes: state
                .node_outcomes
                .clone()
                .into_iter()
                .chain(std::iter::once((
                    node.id().to_string(),
                    result.outcome.clone(),
                )))
                .collect::<BTreeMap<_, _>>(),
            next_node_id: next_node_id.map(ToOwned::to_owned),
            git_commit_sha: git_sha.clone(),
            loop_failure_signatures: loop_failure_signatures.unwrap_or_default(),
            restart_failure_signatures: restart_failure_signatures.unwrap_or_default(),
            node_visits: state
                .node_visits
                .clone()
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
            diff,
        });

        // Emit GitCommit + GitPush events if git produced results
        if let Some(ref result) = git_result {
            if let Some(ref sha) = result.commit_sha {
                self.emitter.emit(&Event::GitCommit {
                    node_id: Some(node.id().to_string()),
                    sha: sha.clone(),
                });
            }
            for (branch, success) in &result.push_results {
                self.emitter.emit(&Event::GitPush {
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
        let duration_ms =
            u64::try_from(self.run_start.lock().unwrap().elapsed().as_millis()).unwrap();
        let artifact_count = self.captured_artifact_count.load(Ordering::Relaxed);
        let last_sha = self.last_git_sha.lock().unwrap().clone();
        let final_patch = self.final_patch.lock().unwrap().clone();
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
            self.emitter.emit(&Event::WorkflowRunCompleted {
                duration_ms,
                artifact_count,
                status: outcome.status.to_string(),
                reason: Some(match outcome.status {
                    StageStatus::PartialSuccess => StatusReason::PartialSuccess,
                    _ => StatusReason::Completed,
                }),
                total_cost,
                final_git_commit_sha: last_sha,
                final_patch,
                usage: run_usage,
            });
        } else {
            let error_msg = outcome
                .failure
                .as_ref()
                .map_or_else(|| "run failed".to_string(), |f| f.message.clone());
            self.emitter.emit(&Event::WorkflowRunFailed {
                error: FabroError::engine(error_msg),
                duration_ms,
                reason: Some(StatusReason::WorkflowError),
                git_commit_sha: last_sha,
            });
        }
    }
}
