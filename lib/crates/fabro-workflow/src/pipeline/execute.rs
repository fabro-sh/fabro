use std::sync::Arc;
use std::time::Instant;

use fabro_core::executor::ExecutorBuilder;
use fabro_core::handler::NodeHandler;
use fabro_core::state::ExecutionState;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::artifact;
use crate::context::{self, Context};
use crate::error::FabroError;
use crate::event::Event;
use crate::graph::WorkflowGraph;
use crate::handler::EngineServices;
use crate::lifecycle::WorkflowLifecycle;
use crate::node_handler::WorkflowNodeHandler;
use crate::outcome::{Outcome, StageStatus};
use crate::records::Checkpoint;
use crate::sandbox_git::GitState;

use super::types::{Executed, Initialized};

fn seed_context_from_checkpoint(checkpoint: Option<&Checkpoint>) -> Context {
    let context = Context::new();
    if let Some(cp) = checkpoint {
        for (k, v) in &cp.context_values {
            context.set(k.clone(), v.clone());
        }
    }
    context
}

/// EXECUTE phase: run the workflow graph.
///
/// Infallible at the function level — engine errors are captured in `outcome`.
pub async fn execute(init: Initialized) -> Executed {
    let Initialized {
        graph,
        source: _,
        run_options,
        workflow_path,
        workflow_bundle,
        run_store,
        checkpoint,
        seed_context,
        emitter,
        sandbox,
        registry,
        on_node,
        artifact_sink,
        run_control,
        hook_runner,
        env,
        dry_run,
        llm_client,
        model,
        provider,
    } = init;

    let mut checkpoint = checkpoint;
    if let Some(cp) = checkpoint.as_mut() {
        artifact::normalize_checkpoint_for_resume(cp);
    }

    let start = Instant::now();
    let graph_arc = Arc::new(graph.clone());
    let wf_graph = WorkflowGraph(Arc::clone(&graph_arc));

    let git_state = run_options.git.as_ref().and_then(|git| {
        let base_sha = git.base_sha.clone()?;
        Some(Arc::new(GitState {
            run_id: run_options.run_id,
            base_sha,
            run_branch: git.run_branch.clone(),
            meta_branch: git.meta_branch.clone(),
            checkpoint_exclude_globs: run_options.checkpoint_exclude_globs().to_vec(),
            git_author: run_options.git_author(),
        }))
    });

    let shared_services = Arc::new(EngineServices {
        registry,
        emitter: Arc::clone(&emitter),
        sandbox: Arc::clone(&sandbox),
        run_store: run_store.clone(),
        git_state: std::sync::RwLock::new(git_state),
        hook_runner: hook_runner.clone(),
        env,
        dry_run,
        cancel_requested: run_options.cancel_token.clone(),
        workflow_path,
        workflow_bundle,
    });

    let handler = Arc::new(WorkflowNodeHandler {
        services: shared_services,
        run_dir: run_options.run_dir.clone(),
        graph: Arc::clone(&graph_arc),
    });

    let settings_arc = Arc::new(run_options.clone());
    let lifecycle = WorkflowLifecycle::new(
        &emitter,
        hook_runner.clone(),
        &sandbox,
        graph_arc,
        &run_options.run_dir,
        &run_store,
        artifact_sink,
        &settings_arc,
        checkpoint.is_some(),
        on_node,
        run_control,
    );

    if let Some(ref cp) = checkpoint {
        lifecycle.restore_circuit_breaker(
            cp.loop_failure_signatures.clone(),
            cp.restart_failure_signatures.clone(),
        );
        if cp.context_values.get(context::keys::INTERNAL_FIDELITY)
            == Some(&serde_json::json!(
                context::keys::Fidelity::Full.to_string()
            ))
        {
            lifecycle.set_degrade_fidelity_on_resume(true);
        }
    }

    let state = if let Some(ref cp) = checkpoint {
        match ExecutionState::new(&wf_graph).map_err(|e| FabroError::engine(e.to_string())) {
            Ok(mut s) => {
                for (k, v) in &cp.context_values {
                    s.context.set(k.clone(), v.clone());
                }
                s.completed_nodes.clone_from(&cp.completed_nodes);
                s.node_retries.clone_from(&cp.node_retries);
                if cp.node_visits.is_empty() {
                    for id in &cp.completed_nodes {
                        *s.node_visits.entry(id.clone()).or_insert(0) += 1;
                    }
                } else {
                    s.node_visits.clone_from(&cp.node_visits);
                }
                for (k, v) in &cp.node_outcomes {
                    s.node_outcomes.insert(k.clone(), v.clone());
                }
                s.stage_index = cp.completed_nodes.len();
                if let Some(ref next) = cp.next_node_id {
                    s.current_node_id.clone_from(next);
                } else {
                    let edges = graph.outgoing_edges(&cp.current_node);
                    if let Some(edge) = edges.first() {
                        s.current_node_id.clone_from(&edge.to);
                    } else {
                        s.current_node_id.clone_from(&cp.current_node);
                    }
                }
                s
            }
            Err(err) => {
                return Executed {
                    graph,
                    outcome: Err(err),
                    run_options,
                    run_store,
                    hook_runner,
                    emitter,
                    sandbox,
                    duration_ms: crate::millis_u64(start.elapsed()),
                    final_context: seed_context_from_checkpoint(checkpoint.as_ref()),
                    llm_client,
                    model,
                    provider,
                };
            }
        }
    } else if let Some(seed) = seed_context {
        match ExecutionState::new(&wf_graph).map_err(|e| FabroError::engine(e.to_string())) {
            Ok(s) => {
                for (k, v) in seed.snapshot() {
                    s.context.set(k, v);
                }
                s
            }
            Err(err) => {
                return Executed {
                    graph,
                    outcome: Err(err),
                    run_options,
                    run_store,
                    hook_runner,
                    emitter,
                    sandbox,
                    duration_ms: crate::millis_u64(start.elapsed()),
                    final_context: seed,
                    llm_client,
                    model,
                    provider,
                };
            }
        }
    } else {
        match ExecutionState::new(&wf_graph).map_err(|e| FabroError::engine(e.to_string())) {
            Ok(s) => s,
            Err(err) => {
                return Executed {
                    graph,
                    outcome: Err(err),
                    run_options,
                    run_store,
                    hook_runner,
                    emitter,
                    sandbox,
                    duration_ms: crate::millis_u64(start.elapsed()),
                    final_context: Context::new(),
                    llm_client,
                    model,
                    provider,
                };
            }
        }
    };

    let initial_context = state.context.clone();

    let graph_max = graph.max_node_visits();
    let max_node_visits = if graph_max > 0 {
        Some(usize::try_from(graph_max).unwrap())
    } else if run_options.dry_run_enabled() {
        Some(10)
    } else {
        None
    };

    let stall_timeout_opt = graph.stall_timeout();
    let stall_token = stall_timeout_opt.map(|_| CancellationToken::new());
    let stall_shutdown =
        if let (Some(stall_timeout), Some(ref token)) = (stall_timeout_opt, &stall_token) {
            let shutdown = CancellationToken::new();
            let emitter = Arc::clone(&emitter);
            let token_clone = token.clone();
            let shutdown_clone = shutdown.clone();
            emitter.touch();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = sleep(stall_timeout) => {
                            if shutdown_clone.is_cancelled() {
                                return;
                            }
                            let last = emitter.last_event_at();
                            let now = i64::try_from(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis(),
                            )
                            .unwrap();
                            let idle_ms = now.saturating_sub(last);
                            if idle_ms >= i64::try_from(stall_timeout.as_millis()).unwrap() {
                                token_clone.cancel();
                                return;
                            }
                        }
                        () = shutdown_clone.cancelled() => {
                            return;
                        }
                    }
                }
            });
            Some(shutdown)
        } else {
            None
        };

    let mut builder = ExecutorBuilder::new(handler as Arc<dyn NodeHandler<WorkflowGraph>>)
        .lifecycle(Box::new(lifecycle));

    if let Some(ref cancel) = run_options.cancel_token {
        builder = builder.cancel_token(cancel.clone());
    }
    if let Some(token) = stall_token.clone() {
        builder = builder.stall_token(token);
    }
    if let Some(limit) = max_node_visits {
        builder = builder.max_node_visits(limit);
    }

    let executor = builder.build();
    let result = executor.run(&wf_graph, state).await;

    if let Some(shutdown) = stall_shutdown {
        shutdown.cancel();
    }

    let (outcome, final_context) = match result {
        Ok((core_outcome, final_state)) => {
            let ctx = final_state.context.clone();
            let result = if core_outcome.status == StageStatus::Fail {
                core_outcome
            } else {
                let mut out = Outcome::success();
                out.notes = Some("Pipeline completed".to_string());
                out
            };
            (Ok(result), ctx)
        }
        Err(fabro_core::CoreError::StallTimeout { node_id }) => {
            let stall_timeout = graph.stall_timeout().unwrap_or_default();
            let idle_secs = stall_timeout.as_secs();
            emitter.emit(&Event::StallWatchdogTimeout {
                node: node_id.clone(),
                idle_seconds: idle_secs,
            });
            (
                Err(FabroError::engine(format!(
                    "stall watchdog: node \"{node_id}\" had no activity for {idle_secs}s"
                ))),
                initial_context,
            )
        }
        Err(fabro_core::CoreError::Cancelled) => (Err(FabroError::Cancelled), initial_context),
        Err(fabro_core::CoreError::Blocked { message }) => {
            (Err(FabroError::engine(message)), initial_context)
        }
        Err(e) => (Err(FabroError::engine(e.to_string())), initial_context),
    };

    let duration_ms = crate::millis_u64(start.elapsed());

    Executed {
        graph,
        outcome,
        run_options,
        run_store,
        hook_runner,
        emitter,
        sandbox,
        duration_ms,
        final_context,
        llm_client,
        model,
        provider,
    }
}

#[cfg(test)]
#[path = "execute/tests.rs"]
mod tests;
