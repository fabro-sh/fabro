pub mod artifact;
pub mod auto_status;
pub mod circuit_breaker;
pub mod disk;
pub mod event;
pub mod fidelity;
pub mod git;
pub mod hook;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use fabro_core::error::Result as CoreResult;
use fabro_core::lifecycle::{
    AttemptContext, AttemptResultContext, EdgeContext, EdgeDecision, NodeDecision, RunLifecycle,
};
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use super::graph::WorkflowGraph;
use super::WorkflowNode;
use crate::event::EventEmitter;
use crate::outcome::{Outcome, StageUsage};
use fabro_hooks::HookRunner;
use fabro_sandbox::Sandbox;

use self::artifact::ArtifactLifecycle;
use self::auto_status::AutoStatusLifecycle;
use self::circuit_breaker::CircuitBreakerLifecycle;
use self::disk::DiskLifecycle;
use self::event::EventLifecycle;
use self::fidelity::FidelityLifecycle;
use self::git::GitLifecycle;
use self::hook::HookLifecycle;

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;
type WfNodeDecision = NodeDecision<Option<StageUsage>>;

/// Orchestrates all sub-lifecycles with explicit per-callback ordering.
/// Implements `RunLifecycle<WorkflowGraph>` by delegating to focused structs.
pub struct WorkflowLifecycle {
    event: EventLifecycle,
    hook: HookLifecycle,
    fidelity: FidelityLifecycle,
    auto_status: AutoStatusLifecycle,
    circuit_breaker: Arc<CircuitBreakerLifecycle>,
    disk: DiskLifecycle,
    #[allow(dead_code)] // stub — will be wired when git operations move to core adapter
    git: GitLifecycle,
    #[allow(dead_code)] // stub — will be wired when artifact operations move to core adapter
    artifact: ArtifactLifecycle,
    /// Set in on_edge_selected when loop_restart approved; read+cleared by EventLifecycle::on_run_start
    restarted_from: Arc<Mutex<Option<(String, String)>>>,
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
        _dry_run: bool,
        _labels: HashMap<String, String>,
    ) -> Self {
        let restarted_from: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
        let loop_restart_signature_limit = graph.loop_restart_signature_limit();

        let circuit_breaker = Arc::new(CircuitBreakerLifecycle::new(loop_restart_signature_limit));

        let event = EventLifecycle {
            emitter: Arc::clone(&emitter),
            graph_name: graph.name.clone(),
            run_id: run_id.clone(),
            run_start: Mutex::new(Instant::now()),
            restarted_from: Arc::clone(&restarted_from),
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
            goal: None,
        };

        let hook = HookLifecycle {
            hook_runner,
            sandbox: Arc::clone(&sandbox),
            run_dir: run_dir.clone(),
            run_id: run_id.clone(),
            graph_name: graph.name.clone(),
        };

        let fidelity = FidelityLifecycle::new(Arc::clone(&graph));

        let disk = DiskLifecycle {
            run_dir: run_dir.clone(),
            run_id: run_id.clone(),
            emitter: Arc::clone(&emitter),
            circuit_breaker: Arc::clone(&circuit_breaker),
            checkpoint_enabled: true,
        };

        Self {
            event,
            hook,
            fidelity,
            auto_status: AutoStatusLifecycle,
            circuit_breaker,
            disk,
            git: GitLifecycle,
            artifact: ArtifactLifecycle,
            restarted_from,
        }
    }

    /// Restore circuit breaker state from a checkpoint (for resume).
    pub fn restore_circuit_breaker(
        &self,
        loop_sigs: HashMap<crate::error::FailureSignature, usize>,
        restart_sigs: HashMap<crate::error::FailureSignature, usize>,
    ) {
        self.circuit_breaker.restore(loop_sigs, restart_sigs);
    }

    /// Set the fidelity degradation flag for checkpoint resume.
    pub fn set_degrade_fidelity_on_resume(&self, flag: bool) {
        self.fidelity.set_degrade_fidelity_on_resume(flag);
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for WorkflowLifecycle {
    async fn on_run_start(&self, graph: &WorkflowGraph, state: &WfRunState) -> CoreResult<()> {
        // Reset restart-scoped state
        self.fidelity.on_run_start(graph, state).await?;
        // Observable callbacks
        self.event.on_run_start(graph, state).await?;
        self.hook.on_run_start(graph, state).await?;
        Ok(())
    }

    async fn on_terminal_reached(
        &self,
        node: &WorkflowNode,
        goal_gates_passed: bool,
        state: &WfRunState,
    ) {
        self.event
            .on_terminal_reached(node, goal_gates_passed, state)
            .await;
    }

    async fn before_node(
        &self,
        node: &WorkflowNode,
        state: &WfRunState,
    ) -> CoreResult<WfNodeDecision> {
        self.fidelity.before_node(node, state).await
    }

    async fn before_attempt(
        &self,
        ctx: &AttemptContext<'_, WorkflowGraph>,
        state: &WfRunState,
    ) -> CoreResult<WfNodeDecision> {
        // Hook first (can skip/block)
        match self.hook.before_attempt(ctx, state).await? {
            NodeDecision::Continue => {}
            decision => return Ok(decision),
        }
        // Event emission
        self.event.before_attempt(ctx, state).await?;
        Ok(NodeDecision::Continue)
    }

    async fn after_attempt(
        &self,
        ctx: &AttemptResultContext<'_, WorkflowGraph>,
        state: &WfRunState,
    ) -> CoreResult<()> {
        self.event.after_attempt(ctx, state).await?;
        Ok(())
    }

    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        state: &WfRunState,
    ) -> CoreResult<()> {
        self.auto_status.after_node(node, result, state).await?;
        self.circuit_breaker.after_node(node, result, state).await?;
        self.event.after_node(node, result, state).await?;
        self.hook.after_node(node, result, state).await?;
        self.disk.after_node(node, result, state).await?;
        Ok(())
    }

    async fn on_edge_selected(
        &self,
        ctx: &EdgeContext<'_, WorkflowGraph>,
        state: &WfRunState,
    ) -> CoreResult<EdgeDecision> {
        // Fidelity captures edge data
        self.fidelity.on_edge_selected(ctx, state).await?;
        // Event always fires first
        self.event.on_edge_selected(ctx, state).await?;
        // Hook can override/block
        match self.hook.on_edge_selected(ctx, state).await? {
            EdgeDecision::Continue => {
                // If loop_restart edge approved by hook, mark for LoopRestart emission
                if let Some(ref edge) = ctx.edge {
                    if edge.inner().loop_restart() {
                        *self.restarted_from.lock().unwrap() =
                            Some((ctx.from.to_string(), ctx.to.to_string()));
                    }
                }
                Ok(EdgeDecision::Continue)
            }
            decision => Ok(decision),
        }
    }

    async fn on_checkpoint(
        &self,
        node: &WorkflowNode,
        result: &WfNodeResult,
        next_node_id: Option<&str>,
        state: &WfRunState,
    ) -> CoreResult<()> {
        self.disk
            .on_checkpoint(node, result, next_node_id, state)
            .await?;
        self.event
            .on_checkpoint(node, result, next_node_id, state)
            .await?;
        Ok(())
    }

    async fn on_run_end(&self, outcome: &Outcome, state: &WfRunState) {
        if state.cancelled {
            return;
        }
        self.event.on_run_end(outcome, state).await;
        self.hook.on_run_end(outcome, state).await;
    }
}
