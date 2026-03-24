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
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::artifact::ArtifactStore;
use crate::context;
use crate::engine::RunConfig;
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
use self::git::{GitCheckpointResult, GitLifecycle};
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
    git: GitLifecycle,
    artifact: ArtifactLifecycle,
    /// Set in on_edge_selected when loop_restart approved; read+cleared by EventLifecycle::on_run_start
    restarted_from: Arc<Mutex<Option<(String, String)>>>,
    /// Shared git checkpoint result (written by git, read by event)
    checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    /// True when constructed with a checkpoint; cleared after first on_run_start.
    /// Gates mirror_graph_attributes on initial resume.
    is_initial_resume: AtomicBool,
    // Config needed for context seeding
    graph: Arc<fabro_graphviz::graph::types::Graph>,
    run_id: String,
    working_directory: Option<String>,
}

impl WorkflowLifecycle {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        emitter: Arc<EventEmitter>,
        hook_runner: Option<Arc<HookRunner>>,
        sandbox: Arc<dyn Sandbox>,
        graph: Arc<fabro_graphviz::graph::types::Graph>,
        run_dir: PathBuf,
        config: Arc<RunConfig>,
        is_resume: bool,
    ) -> Self {
        let restarted_from: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
        let loop_restart_signature_limit = graph.loop_restart_signature_limit();
        let checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>> =
            Arc::new(Mutex::new(None));
        let last_git_sha: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let artifact_store = Arc::new(Mutex::new(ArtifactStore::new(Some(run_dir.clone()))));

        let circuit_breaker = Arc::new(CircuitBreakerLifecycle::new(loop_restart_signature_limit));

        let local_git_checkpoint =
            config.git_checkpoint_enabled && sandbox.host_git_dir().is_some();
        let working_directory = if local_git_checkpoint {
            Some(sandbox.working_directory().to_string())
        } else {
            None
        };

        let event = EventLifecycle {
            emitter: Arc::clone(&emitter),
            graph_name: graph.name.clone(),
            run_id: config.run_id.clone(),
            run_start: Mutex::new(Instant::now()),
            restarted_from: Arc::clone(&restarted_from),
            base_sha: config.base_sha.clone(),
            run_branch: config.run_branch.clone(),
            worktree_dir: working_directory.clone(),
            goal: (!graph.goal().is_empty()).then(|| graph.goal().to_string()),
            artifact_store: Arc::clone(&artifact_store),
            last_git_sha: Arc::clone(&last_git_sha),
            checkpoint_git_result: Arc::clone(&checkpoint_git_result),
        };

        let hook = HookLifecycle {
            hook_runner,
            sandbox: Arc::clone(&sandbox),
            run_dir: run_dir.clone(),
            run_id: config.run_id.clone(),
            graph_name: graph.name.clone(),
        };

        let fidelity = FidelityLifecycle::new(Arc::clone(&graph));

        let disk = DiskLifecycle {
            run_dir: run_dir.clone(),
            run_id: config.run_id.clone(),
            graph: Arc::clone(&graph),
            config: Arc::clone(&config),
            emitter: Arc::clone(&emitter),
            circuit_breaker: Arc::clone(&circuit_breaker),
            checkpoint_enabled: true,
        };

        let start_node_id = graph.find_start_node().map(|n| n.id.clone());

        let git = GitLifecycle {
            sandbox: Arc::clone(&sandbox),
            artifact_store: Arc::clone(&artifact_store),
            emitter: Arc::clone(&emitter),
            run_dir: run_dir.clone(),
            run_id: config.run_id.clone(),
            config: Arc::clone(&config),
            start_node_id,
            checkpoint_git_result: Arc::clone(&checkpoint_git_result),
            last_git_sha: Arc::clone(&last_git_sha),
        };

        let artifact = ArtifactLifecycle::new(
            Arc::clone(&sandbox),
            Arc::clone(&artifact_store),
            Some(run_dir.clone()),
            Arc::clone(&emitter),
            run_dir,
            config.asset_globs.clone(),
        );

        Self {
            event,
            hook,
            fidelity,
            auto_status: AutoStatusLifecycle,
            circuit_breaker,
            disk,
            git,
            artifact,
            restarted_from,
            checkpoint_git_result,
            is_initial_resume: AtomicBool::new(is_resume),
            graph,
            run_id: config.run_id.clone(),
            working_directory,
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
        // Re-seed context keys (fires on initial start AND after every loop restart).
        // mirror_graph_attributes: skip on initial checkpoint resume (context already has them)
        if self.is_initial_resume.swap(false, Ordering::Relaxed) {
            // First on_run_start after checkpoint resume — skip mirror_graph_attributes
        } else {
            // Mirror graph-level attributes into the core context
            if !self.graph.goal().is_empty() {
                state.context.set(
                    context::keys::GRAPH_GOAL,
                    serde_json::json!(self.graph.goal()),
                );
            }
            for (key, val) in &self.graph.attrs {
                state.context.set(
                    context::keys::graph_attr_key(key),
                    serde_json::json!(val.to_string_value()),
                );
            }
        }
        // Always set run_id and work_dir (idempotent)
        state.context.set(
            context::keys::INTERNAL_RUN_ID,
            serde_json::json!(self.run_id),
        );
        if let Some(ref wd) = self.working_directory {
            state
                .context
                .set(context::keys::INTERNAL_WORK_DIR, serde_json::json!(wd));
        }

        // Reset restart-scoped state
        self.fidelity.on_run_start(graph, state).await?;
        self.artifact.on_run_start(graph, state).await?;
        // Observable callbacks
        self.event.on_run_start(graph, state).await?;
        self.hook.on_run_start(graph, state).await?;
        self.disk.on_run_start(graph, state).await?;
        self.git.on_run_start(graph, state).await?;
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
        // Record epoch AFTER hook+event (engine.rs:968→1006)
        self.artifact.before_attempt(ctx, state).await?;
        Ok(NodeDecision::Continue)
    }

    async fn after_attempt(
        &self,
        ctx: &AttemptResultContext<'_, WorkflowGraph>,
        state: &WfRunState,
    ) -> CoreResult<()> {
        self.artifact.after_attempt(ctx, state).await?;
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
        self.artifact.after_node(node, result, state).await?;
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
                // Edge unchanged — check circuit breaker for loop_restart
                let decision = self.circuit_breaker.on_edge_selected(ctx, state).await?;
                // If loop_restart edge approved by both hook and circuit breaker, mark for LoopRestart emission
                if matches!(decision, EdgeDecision::Continue) {
                    if let Some(ref edge) = ctx.edge {
                        if edge.inner().loop_restart() {
                            *self.restarted_from.lock().unwrap() =
                                Some((ctx.from.to_string(), ctx.to.to_string()));
                        }
                    }
                }
                Ok(decision)
            }
            decision => Ok(decision), // Override/Block — skip circuit breaker
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
        self.git
            .on_checkpoint(node, result, next_node_id, state)
            .await?;
        self.event
            .on_checkpoint(node, result, next_node_id, state)
            .await?;
        self.hook
            .on_checkpoint(node, result, next_node_id, state)
            .await?;
        // Clear checkpoint result for next checkpoint
        *self.checkpoint_git_result.lock().unwrap() = None;
        Ok(())
    }

    async fn on_run_end(&self, outcome: &Outcome, state: &WfRunState) {
        if state.cancelled {
            return;
        }
        self.event.on_run_end(outcome, state).await;
        self.hook.on_run_end(outcome, state).await;
        self.git.on_run_end(outcome, state).await;
    }
}
