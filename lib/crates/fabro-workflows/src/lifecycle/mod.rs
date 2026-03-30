pub(crate) mod artifact;
pub(crate) mod auto_status;
pub(crate) mod circuit_breaker;
pub(crate) mod disk;
pub(crate) mod event;
pub(crate) mod fidelity;
pub(crate) mod git;
pub(crate) mod hook;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use fabro_store::RunStore;
use fabro_store::RuntimeState;
use fabro_types::RunId;

use fabro_core::error::Result as CoreResult;
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{
    AttemptContext, AttemptResultContext, EdgeContext, EdgeDecision, NodeDecision, RunLifecycle,
};
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use crate::artifact::ArtifactStore;
use crate::context;
use crate::error::{FailureSignature, FailureSignatureExt};
use crate::event::EventEmitter;
use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::outcome::{Outcome, StageUsage};
use crate::run_options::RunOptions;
use fabro_graphviz::graph::types::Graph as GvGraph;
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
use crate::outcome::OutcomeExt;

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;
type WfNodeDecision = NodeDecision<Option<StageUsage>>;

/// Orchestrates all sub-lifecycles with explicit per-callback ordering.
/// Implements `RunLifecycle<WorkflowGraph>` by delegating to focused structs.
pub(crate) struct WorkflowLifecycle {
    event: EventLifecycle,
    hook: HookLifecycle,
    fidelity: FidelityLifecycle,
    auto_status: AutoStatusLifecycle,
    circuit_breaker: Arc<CircuitBreakerLifecycle>,
    disk: DiskLifecycle,
    git: GitLifecycle,
    artifact: ArtifactLifecycle,
    on_node: crate::OnNodeCallback,
    /// Set in on_edge_selected when loop_restart approved; read+cleared by EventLifecycle::on_run_start
    restarted_from: Arc<Mutex<Option<(String, String)>>>,
    /// Shared git checkpoint result (written by git, read by event)
    checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    /// True when constructed with a checkpoint; cleared after first on_run_start.
    /// Gates context seeding on initial resume.
    is_initial_resume: AtomicBool,
    // Config needed for context seeding
    graph: Arc<GvGraph>,
    run_id: RunId,
    working_directory: Option<String>,
}

impl WorkflowLifecycle {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        emitter: &Arc<EventEmitter>,
        hook_runner: Option<Arc<HookRunner>>,
        sandbox: &Arc<dyn Sandbox>,
        graph: Arc<GvGraph>,
        run_dir: &PathBuf,
        run_store: Arc<dyn RunStore>,
        run_options: &Arc<RunOptions>,
        is_resume: bool,
        on_node: crate::OnNodeCallback,
    ) -> Self {
        let runtime_state = RuntimeState::new(run_dir);
        let restarted_from: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
        let loop_restart_signature_limit = graph.loop_restart_signature_limit();
        let checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>> =
            Arc::new(Mutex::new(None));
        let last_git_sha: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let artifact_store = Arc::new(Mutex::new(ArtifactStore::new(Some(
            runtime_state.artifact_values_dir(),
        ))));

        let circuit_breaker = Arc::new(CircuitBreakerLifecycle::new(loop_restart_signature_limit));

        let has_run_branch = run_options
            .git
            .as_ref()
            .and_then(|g| g.run_branch.as_ref())
            .is_some();
        let local_git_checkpoint = has_run_branch && sandbox.host_git_dir().is_some();
        let working_directory = if local_git_checkpoint {
            Some(sandbox.working_directory().to_string())
        } else {
            None
        };

        let event = EventLifecycle {
            emitter: Arc::clone(emitter),
            graph_name: graph.name.clone(),
            run_id: run_options.run_id.clone(),
            run_start: Mutex::new(Instant::now()),
            restarted_from: Arc::clone(&restarted_from),
            base_branch: run_options.base_branch.clone(),
            base_sha: run_options.display_base_sha.clone(),
            run_branch: run_options.git.as_ref().and_then(|g| g.run_branch.clone()),
            worktree_dir: working_directory.clone(),
            goal: (!graph.goal().is_empty()).then(|| graph.goal().to_string()),
            artifact_store: Arc::clone(&artifact_store),
            last_git_sha: Arc::clone(&last_git_sha),
            checkpoint_git_result: Arc::clone(&checkpoint_git_result),
        };

        let hook = HookLifecycle {
            hook_runner,
            sandbox: Arc::clone(sandbox),
            hook_work_dir: working_directory.clone().map(PathBuf::from),
            run_id: run_options.run_id.clone(),
            graph_name: graph.name.clone(),
        };

        let fidelity = FidelityLifecycle::new(Arc::clone(&graph));

        let disk = DiskLifecycle {
            run_dir: run_dir.clone(),
            run_id: run_options.run_id.clone(),
            run_store: Arc::clone(&run_store),
            graph: Arc::clone(&graph),
            run_options: Arc::clone(run_options),
            emitter: Arc::clone(emitter),
            circuit_breaker: Arc::clone(&circuit_breaker),
            checkpoint_enabled: true,
        };

        let start_node_id = graph.find_start_node().map(|n| n.id.clone());

        let git = GitLifecycle {
            sandbox: Arc::clone(sandbox),
            artifact_store: Arc::clone(&artifact_store),
            emitter: Arc::clone(emitter),
            run_dir: run_dir.clone(),
            run_id: run_options.run_id.clone(),
            run_store,
            run_options: Arc::clone(run_options),
            start_node_id,
            checkpoint_git_result: Arc::clone(&checkpoint_git_result),
            last_git_sha: Arc::clone(&last_git_sha),
        };

        let artifact = ArtifactLifecycle::new(
            Arc::clone(sandbox),
            Arc::clone(&artifact_store),
            Some(runtime_state.artifact_values_dir()),
            Arc::clone(emitter),
            runtime_state.assets_dir(),
            run_options.asset_globs().to_vec(),
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
            on_node,
            restarted_from,
            checkpoint_git_result,
            is_initial_resume: AtomicBool::new(is_resume),
            graph,
            run_id: run_options.run_id.clone(),
            working_directory,
        }
    }

    /// Restore circuit breaker state from a checkpoint (for resume).
    pub(crate) fn restore_circuit_breaker(
        &self,
        loop_sigs: HashMap<FailureSignature, usize>,
        restart_sigs: HashMap<FailureSignature, usize>,
    ) {
        self.circuit_breaker.restore(loop_sigs, restart_sigs);
    }

    /// Set the fidelity degradation flag for checkpoint resume.
    pub(crate) fn set_degrade_fidelity_on_resume(&self, flag: bool) {
        self.fidelity.set_degrade_fidelity_on_resume(flag);
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for WorkflowLifecycle {
    async fn on_run_start(&self, graph: &WorkflowGraph, state: &WfRunState) -> CoreResult<()> {
        // Re-seed context keys (fires on initial start AND after every loop restart).
        // Skip on initial checkpoint resume (context already has them).
        if self.is_initial_resume.swap(false, Ordering::Relaxed) {
            // First on_run_start after checkpoint resume — skip context seeding
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
        if let Some(on_node) = &self.on_node {
            on_node(node.id());
        }
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

    async fn after_record(
        &self,
        node: &WorkflowNode,
        result: &WfNodeResult,
        state: &WfRunState,
    ) -> CoreResult<()> {
        let outcome = &result.outcome;
        let retry_count = state.node_retries.get(node.id()).copied().unwrap_or(0);
        let failure_class = outcome.classified_failure_category();
        let failure_signature = failure_class
            .map(|category| {
                let signature_hint = outcome
                    .failure
                    .as_ref()
                    .and_then(|f| f.signature.as_deref());
                FailureSignature::new(
                    node.id(),
                    category,
                    signature_hint,
                    outcome.failure_reason(),
                )
                .to_string()
            })
            .unwrap_or_default();

        state.context.set(
            context::keys::retry_count_key(node.id()),
            serde_json::json!(retry_count),
        );
        state.context.set(
            context::keys::OUTCOME,
            serde_json::json!(outcome.status.to_string()),
        );
        state.context.set(
            context::keys::FAILURE_CLASS,
            serde_json::json!(failure_class.map_or(String::new(), |fc| fc.to_string())),
        );
        state.context.set(
            context::keys::FAILURE_SIGNATURE,
            serde_json::json!(failure_signature),
        );
        if let Some(ref preferred_label) = outcome.preferred_label {
            state.context.set(
                context::keys::PREFERRED_LABEL,
                serde_json::json!(preferred_label),
            );
        }
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
