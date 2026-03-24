use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use fabro_core::error::{CoreError, Result as CoreResult};
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{
    AttemptContext, AttemptResultContext, EdgeContext, EdgeDecision, NodeDecision, RunLifecycle,
};
use fabro_core::outcome::{NodeResult, Outcome as CoreOutcome};
use fabro_core::state::RunState;

use super::graph::WorkflowGraph;
use super::outcome::{core_to_wf_outcome, core_to_wf_status};
use super::WorkflowNode;
use crate::checkpoint::Checkpoint;
use crate::context::keys;
use crate::error::{FailureClass, FailureSignature};
use crate::event::{EventEmitter, WorkflowRunEvent};
use crate::outcome::StageStatus as WfStatus;
use fabro_hooks::{HookContext, HookDecision, HookEvent, HookRunner};
use fabro_sandbox::Sandbox;

/// Data captured from an edge selection to pass to the next node's before_node.
#[derive(Debug, Clone)]
struct IncomingEdgeData {
    fidelity: Option<String>,
    thread_id: Option<String>,
}

/// Implements the full RunLifecycle for fabro-workflows, mapping all domain
/// concerns (events, hooks, git, disk I/O, fidelity, circuit breaker, etc.)
/// into fabro-core lifecycle callbacks.
pub struct WorkflowLifecycle {
    pub emitter: Arc<EventEmitter>,
    pub hook_runner: Option<Arc<HookRunner>>,
    pub sandbox: Arc<dyn Sandbox>,
    pub graph: Arc<fabro_graphviz::graph::types::Graph>,
    pub run_dir: PathBuf,
    pub run_id: String,
    pub run_start: Instant,
    pub labels: HashMap<String, String>,
    // Circuit breaker state
    loop_failure_signatures: Mutex<HashMap<FailureSignature, usize>>,
    restart_failure_signatures: Mutex<HashMap<FailureSignature, usize>>,
    // Edge data for next node
    incoming_edge_data: Mutex<Option<IncomingEdgeData>>,
    // Config flags
    pub dry_run: bool,
    pub checkpoint_enabled: bool,
}

impl WorkflowLifecycle {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        emitter: Arc<EventEmitter>,
        hook_runner: Option<Arc<HookRunner>>,
        sandbox: Arc<dyn Sandbox>,
        graph: Arc<fabro_graphviz::graph::types::Graph>,
        run_dir: PathBuf,
        run_id: String,
        dry_run: bool,
        labels: HashMap<String, String>,
    ) -> Self {
        Self {
            emitter,
            hook_runner,
            sandbox,
            graph,
            run_dir,
            run_id,
            run_start: Instant::now(),
            labels,
            loop_failure_signatures: Mutex::new(HashMap::new()),
            restart_failure_signatures: Mutex::new(HashMap::new()),
            incoming_edge_data: Mutex::new(None),
            dry_run,
            checkpoint_enabled: true,
        }
    }

    /// Restore circuit breaker state from a checkpoint (for resume).
    pub fn restore_circuit_breaker(
        &self,
        loop_sigs: HashMap<FailureSignature, usize>,
        restart_sigs: HashMap<FailureSignature, usize>,
    ) {
        *self.loop_failure_signatures.lock().unwrap() = loop_sigs;
        *self.restart_failure_signatures.lock().unwrap() = restart_sigs;
    }

    async fn run_hook(&self, hook_ctx: &HookContext) -> HookDecision {
        let Some(ref runner) = self.hook_runner else {
            return HookDecision::Proceed;
        };
        runner
            .run(hook_ctx, self.sandbox.clone(), Some(&self.run_dir))
            .await
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for WorkflowLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &RunState) -> CoreResult<()> {
        // Clear incoming edge data (reset stale fidelity/thread from prior iteration)
        *self.incoming_edge_data.lock().unwrap() = None;

        // Emit WorkflowRunStarted event
        self.emitter.emit(&WorkflowRunEvent::WorkflowRunStarted {
            name: self.graph.name.clone(),
            run_id: self.run_id.clone(),
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
            goal: None,
        });

        // RunStart hook (blocking)
        let hook_ctx = HookContext::new(
            HookEvent::RunStart,
            self.run_id.clone(),
            self.graph.name.clone(),
        );
        let decision = self.run_hook(&hook_ctx).await;
        if let HookDecision::Block { reason } = decision {
            let msg = reason.unwrap_or_else(|| "blocked by RunStart hook".into());
            return Err(CoreError::blocked(msg));
        }

        Ok(())
    }

    async fn on_terminal_reached(
        &self,
        node: &WorkflowNode,
        goal_gates_passed: bool,
        state: &RunState,
    ) {
        if !goal_gates_passed {
            return;
        }
        let gv = node.inner();
        let stage_index = state.stage_index;
        // Emit StageStarted + StageCompleted for the terminal node
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
            status: "success".to_string(),
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

    async fn before_node(&self, node: &WorkflowNode, state: &RunState) -> CoreResult<NodeDecision> {
        // Resolve fidelity from incoming edge data
        let incoming = self.incoming_edge_data.lock().unwrap().take();
        let gv_node = node.inner();

        // Set context keys for the current node
        // Note: This operates on state.context which is the core context bridged to wf context
        let visits = state.node_visits.get(node.id()).copied().unwrap_or(0);
        state
            .context
            .set(keys::CURRENT_NODE, serde_json::json!(node.id()));
        state
            .context
            .set(keys::INTERNAL_NODE_VISIT_COUNT, serde_json::json!(visits));

        // Fidelity resolution
        let fidelity = if let Some(ref edge_data) = incoming {
            edge_data
                .fidelity
                .as_deref()
                .or(gv_node.fidelity())
                .unwrap_or("compact")
                .to_string()
        } else {
            gv_node.fidelity().unwrap_or("compact").to_string()
        };
        state
            .context
            .set(keys::INTERNAL_FIDELITY, serde_json::json!(fidelity));

        // Thread ID resolution
        if let Some(ref edge_data) = incoming {
            if let Some(ref tid) = edge_data.thread_id {
                state
                    .context
                    .set(keys::INTERNAL_THREAD_ID, serde_json::json!(tid));
            }
        } else if let Some(tid) = gv_node.thread_id() {
            state
                .context
                .set(keys::INTERNAL_THREAD_ID, serde_json::json!(tid));
        }

        Ok(NodeDecision::Continue)
    }

    async fn before_attempt(
        &self,
        ctx: &AttemptContext<'_, WorkflowGraph>,
        state: &RunState,
    ) -> CoreResult<NodeDecision> {
        let gv = ctx.node.inner();
        let stage_index = state.stage_index;

        // StageStart hook (blocking)
        let hook_ctx = HookContext::new(
            HookEvent::StageStart,
            self.run_id.clone(),
            self.graph.name.clone(),
        );
        let decision = self.run_hook(&hook_ctx).await;
        match decision {
            HookDecision::Skip { reason } => {
                let msg = reason.unwrap_or_else(|| "skipped by hook".into());
                return Ok(NodeDecision::Skip(Box::new(CoreOutcome::skipped(&msg))));
            }
            HookDecision::Block { reason } => {
                let msg = reason.unwrap_or_else(|| "blocked by StageStart hook".into());
                return Err(CoreError::blocked(msg));
            }
            _ => {}
        }

        // Emit StageStarted event
        self.emitter.emit(&WorkflowRunEvent::StageStarted {
            node_id: gv.id.clone(),
            name: gv.label().to_string(),
            index: stage_index,
            handler_type: gv.handler_type().map(String::from),
            script: None,
            attempt: ctx.attempt as usize,
            max_attempts: ctx.max_attempts as usize,
        });

        Ok(NodeDecision::Continue)
    }

    async fn after_attempt(
        &self,
        ctx: &AttemptResultContext<'_, WorkflowGraph>,
        state: &RunState,
    ) -> CoreResult<()> {
        if ctx.will_retry {
            let gv = ctx.node.inner();
            let wf_outcome = core_to_wf_outcome(&ctx.result.outcome);
            let stage_index = state.stage_index;

            // Emit StageFailed event
            self.emitter.emit(&WorkflowRunEvent::StageFailed {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                failure: wf_outcome.failure.unwrap_or_else(|| {
                    crate::outcome::FailureDetail::new(
                        "handler failed",
                        FailureClass::TransientInfra,
                    )
                }),
                will_retry: true,
            });

            // Emit StageRetrying event
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
        result: &mut NodeResult,
        state: &RunState,
    ) -> CoreResult<()> {
        let gv = node.inner();
        let stage_index = state.stage_index;
        let mut wf_outcome = core_to_wf_outcome(&result.outcome);

        // Auto-status override
        if gv.auto_status()
            && wf_outcome.status != WfStatus::Success
            && wf_outcome.status != WfStatus::Skipped
        {
            wf_outcome.status = WfStatus::Success;
            wf_outcome.notes =
                Some("auto-status: handler completed without writing status".to_string());
            result.outcome.status = fabro_core::outcome::StageStatus::Success;
            result.outcome.notes = wf_outcome.notes.clone();
        }

        // Circuit breaker: classify + track failure signatures
        let outcome_failure_class = if wf_outcome.status == WfStatus::Fail {
            wf_outcome.failure.as_ref().map(|f| f.failure_class)
        } else {
            None
        };

        if let Some(fc) = outcome_failure_class {
            let sig_hint = wf_outcome
                .failure
                .as_ref()
                .and_then(|f| f.failure_signature.as_deref());
            let sig = FailureSignature::new(
                &gv.id,
                fc,
                sig_hint,
                wf_outcome.failure.as_ref().map(|f| f.message.as_str()),
            );
            if fc.is_signature_tracked() {
                let mut sigs = self.loop_failure_signatures.lock().unwrap();
                let count = sigs.entry(sig.clone()).or_insert(0);
                *count += 1;
                let limit = self.graph.loop_restart_signature_limit();
                if *count >= limit {
                    return Err(CoreError::Other(format!(
                        "deterministic failure cycle detected: signature {sig} repeated {count} times (limit {limit})"
                    )));
                }
            }
        }

        // Emit StageCompleted or StageFailed event
        let duration_ms = result.duration.as_millis() as u64;
        if wf_outcome.status == WfStatus::Fail {
            self.emitter.emit(&WorkflowRunEvent::StageFailed {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                failure: wf_outcome.failure.clone().unwrap_or_else(|| {
                    crate::outcome::FailureDetail::new(
                        "handler failed",
                        FailureClass::Deterministic,
                    )
                }),
                will_retry: false,
            });
        } else {
            self.emitter.emit(&WorkflowRunEvent::StageCompleted {
                node_id: gv.id.clone(),
                name: gv.label().to_string(),
                index: stage_index,
                duration_ms,
                status: wf_outcome.status.to_string(),
                preferred_label: wf_outcome.preferred_label.clone(),
                suggested_next_ids: wf_outcome.suggested_next_ids.clone(),
                usage: wf_outcome.usage.clone(),
                failure: None,
                notes: wf_outcome.notes.clone(),
                files_touched: wf_outcome.files_touched.clone(),
                attempt: result.attempts as usize,
                max_attempts: result.max_attempts as usize,
            });
        }

        // StageComplete/StageFailed hook (non-blocking)
        let hook_event = if wf_outcome.status == WfStatus::Fail {
            HookEvent::StageFailed
        } else {
            HookEvent::StageComplete
        };
        let mut hook_ctx =
            HookContext::new(hook_event, self.run_id.clone(), self.graph.name.clone());
        hook_ctx.status = Some(wf_outcome.status.to_string());
        let _ = self.run_hook(&hook_ctx).await;

        // Write node status
        let status_dir = self.run_dir.join("stages").join(&gv.id);
        let _ = std::fs::create_dir_all(&status_dir);
        let status_path = status_dir.join("status.json");
        let _ = crate::save_json(&wf_outcome, &status_path, "node_status");

        Ok(())
    }

    async fn on_edge_selected(
        &self,
        ctx: &EdgeContext<'_, WorkflowGraph>,
        _state: &RunState,
    ) -> CoreResult<EdgeDecision> {
        // Capture fidelity/thread from edge for next node
        if let Some(ref edge) = ctx.edge {
            let gv_edge = edge.inner();
            let edge_data = IncomingEdgeData {
                fidelity: gv_edge.fidelity().map(String::from),
                thread_id: gv_edge.thread_id().map(String::from),
            };
            *self.incoming_edge_data.lock().unwrap() = Some(edge_data);
        }

        // Compute outcome-derived fields for EdgeSelected event
        let wf_outcome = core_to_wf_outcome(ctx.outcome);

        // Emit EdgeSelected event
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
            preferred_label: wf_outcome.preferred_label.clone(),
            suggested_next_ids: wf_outcome.suggested_next_ids.clone(),
            stage_status: wf_outcome.status.to_string(),
            is_jump: ctx.is_jump,
        });

        // EdgeSelected hook (blocking, can override)
        let mut hook_ctx = HookContext::new(
            HookEvent::EdgeSelected,
            self.run_id.clone(),
            self.graph.name.clone(),
        );
        hook_ctx.edge_from = Some(ctx.from.to_string());
        hook_ctx.edge_to = Some(ctx.to.to_string());
        let decision = self.run_hook(&hook_ctx).await;
        match decision {
            HookDecision::Override { edge_to } => {
                return Ok(EdgeDecision::Override(edge_to));
            }
            HookDecision::Block { reason } => {
                let msg = reason.unwrap_or_else(|| "blocked by EdgeSelected hook".into());
                return Err(CoreError::blocked(msg));
            }
            _ => {}
        }

        // Circuit breaker for loop_restart edges
        if let Some(ref edge) = ctx.edge {
            if edge.inner().loop_restart() {
                // Check restart_failure_signatures limit
                // (implemented in Phase 5 when full checkpoint resume is wired)
            }
        }

        Ok(EdgeDecision::Continue)
    }

    async fn on_checkpoint(
        &self,
        node: &WorkflowNode,
        result: &NodeResult,
        next_node_id: Option<&str>,
        state: &RunState,
    ) -> CoreResult<()> {
        if !self.checkpoint_enabled {
            return Ok(());
        }

        // Build checkpoint from state
        let wf_outcome = core_to_wf_outcome(&result.outcome);
        let mut node_outcomes: HashMap<String, crate::outcome::Outcome> = state
            .node_outcomes
            .iter()
            .map(|(k, v)| (k.clone(), core_to_wf_outcome(v)))
            .collect();
        // Include current node's outcome
        node_outcomes.insert(node.id().to_string(), wf_outcome);

        let checkpoint = Checkpoint {
            timestamp: chrono::Utc::now(),
            current_node: node.id().to_string(),
            completed_nodes: state.completed_nodes.clone(),
            node_outcomes,
            node_retries: state.node_retries.clone(),
            context_values: state.context.snapshot(),
            logs: state.context.logs_snapshot(),
            next_node_id: next_node_id.map(String::from),
            git_commit_sha: None,
            node_visits: state.node_visits.clone(),
            loop_failure_signatures: self.loop_failure_signatures.lock().unwrap().clone(),
            restart_failure_signatures: self.restart_failure_signatures.lock().unwrap().clone(),
        };

        // Write checkpoint.json
        let checkpoint_path = self.run_dir.join("checkpoint.json");
        if let Err(e) = checkpoint.save(&checkpoint_path) {
            state
                .context
                .append_log(format!("checkpoint save failed: {e}"));
        }

        // Emit CheckpointCompleted event
        let status = core_to_wf_status(&result.outcome.status).to_string();
        self.emitter.emit(&WorkflowRunEvent::CheckpointCompleted {
            node_id: node.id().to_string(),
            status,
            git_commit_sha: None,
        });

        Ok(())
    }

    async fn on_run_end(&self, outcome: &CoreOutcome, state: &RunState) {
        // If cancelled, skip all events/hooks
        if state.cancelled {
            return;
        }

        let duration_ms = self.run_start.elapsed().as_millis() as u64;
        let wf_outcome = core_to_wf_outcome(outcome);

        if wf_outcome.status == WfStatus::Success || wf_outcome.status == WfStatus::PartialSuccess {
            // Success path
            self.emitter.emit(&WorkflowRunEvent::WorkflowRunCompleted {
                duration_ms,
                artifact_count: 0,
                status: wf_outcome.status.to_string(),
                total_cost: None,
                final_git_commit_sha: None,
                usage: None,
            });

            // RunComplete hook
            let hook_ctx = HookContext::new(
                HookEvent::RunComplete,
                self.run_id.clone(),
                self.graph.name.clone(),
            );
            let _ = self.run_hook(&hook_ctx).await;
        } else {
            // Failure path
            let error_msg = wf_outcome
                .failure
                .as_ref()
                .map(|f| f.message.clone())
                .unwrap_or_else(|| "run failed".to_string());
            self.emitter.emit(&WorkflowRunEvent::WorkflowRunFailed {
                error: crate::error::FabroError::engine(error_msg.clone()),
                duration_ms,
                git_commit_sha: None,
            });

            // RunFailed hook
            let mut hook_ctx = HookContext::new(
                HookEvent::RunFailed,
                self.run_id.clone(),
                self.graph.name.clone(),
            );
            hook_ctx.failure_reason = Some(error_msg);
            let _ = self.run_hook(&hook_ctx).await;
        }
    }
}
