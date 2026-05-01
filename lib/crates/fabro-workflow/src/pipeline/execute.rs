use std::sync::Arc;
use std::time::Instant;

use fabro_core::executor::ExecutorBuilder;
use fabro_core::handler::NodeHandler;
use fabro_core::state::ExecutionState;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::types::{Executed, Initialized};
use crate::artifact;
use crate::context::{self, Context};
use crate::error::Error;
use crate::event::Event;
use crate::graph::WorkflowGraph;
use crate::lifecycle::WorkflowLifecycle;
use crate::node_handler::WorkflowNodeHandler;
use crate::outcome::Outcome;
use crate::records::Checkpoint;
use crate::sandbox_git::GitState;

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
        checkpoint,
        seed_context,
        on_node,
        artifact_sink,
        run_control,
        engine,
        model,
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
            checkpoint_exclude_globs: run_options.checkpoint_exclude_globs(),
            git_author: run_options.git_author(),
        }))
    });
    engine.set_git_state(git_state);

    let handler = Arc::new(WorkflowNodeHandler {
        services: Arc::clone(&engine),
        run_dir:  run_options.run_dir.clone(),
        graph:    Arc::clone(&graph_arc),
    });

    let settings_arc = Arc::new(run_options.clone());
    let lifecycle = WorkflowLifecycle::new(
        &engine.run.emitter,
        engine.run.hook_runner.clone(),
        &engine.run.sandbox,
        graph_arc,
        &run_options.run_dir,
        &engine.run.run_store,
        artifact_sink,
        &settings_arc,
        Arc::clone(&engine.run.sandbox_git),
        Arc::clone(&engine.run.metadata_runtime),
        engine.run.metadata_writer.clone(),
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
        match ExecutionState::new(&wf_graph).map_err(|e| Error::engine(e.to_string())) {
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
                    duration_ms: crate::millis_u64(start.elapsed()),
                    final_context: seed_context_from_checkpoint(checkpoint.as_ref()),
                    engine,
                    model,
                };
            }
        }
    } else if let Some(seed) = seed_context {
        match ExecutionState::new(&wf_graph).map_err(|e| Error::engine(e.to_string())) {
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
                    duration_ms: crate::millis_u64(start.elapsed()),
                    final_context: seed,
                    engine,
                    model,
                };
            }
        }
    } else {
        match ExecutionState::new(&wf_graph).map_err(|e| Error::engine(e.to_string())) {
            Ok(s) => s,
            Err(err) => {
                return Executed {
                    graph,
                    outcome: Err(err),
                    run_options,
                    duration_ms: crate::millis_u64(start.elapsed()),
                    final_context: Context::new(),
                    engine,
                    model,
                };
            }
        }
    };

    let initial_context = state.context.clone();

    let graph_max = graph.max_node_visits();
    let max_node_visits = if graph_max > 0 {
        Some(usize::try_from(graph_max).expect("positive max_node_visits should fit in usize"))
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
            let emitter = Arc::clone(&engine.run.emitter);
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
                            .unwrap_or(i64::MAX);
                            let idle_ms = now.saturating_sub(last);
                            let stall_timeout_ms =
                                i64::try_from(stall_timeout.as_millis()).unwrap_or(i64::MAX);
                            if idle_ms >= stall_timeout_ms {
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
            let result = if core_outcome.status.is_failure() {
                core_outcome
            } else {
                let mut out = Outcome::success();
                out.notes = Some("Pipeline completed".to_string());
                out
            };
            (Ok(result), ctx)
        }
        Err(fabro_core::Error::StallTimeout { node_id }) => {
            let stall_timeout = graph.stall_timeout().unwrap_or_default();
            let idle_secs = stall_timeout.as_secs();
            engine.run.emitter.emit(&Event::StallWatchdogTimeout {
                node:         node_id.clone(),
                idle_seconds: idle_secs,
            });
            (
                Err(Error::engine(format!(
                    "stall watchdog: node \"{node_id}\" had no activity for {idle_secs}s"
                ))),
                initial_context,
            )
        }
        Err(fabro_core::Error::Cancelled) => (Err(Error::Cancelled), initial_context),
        Err(fabro_core::Error::Blocked { message }) => {
            (Err(Error::engine(message)), initial_context)
        }
        Err(e) => (Err(Error::engine(e.to_string())), initial_context),
    };

    let duration_ms = crate::millis_u64(start.elapsed());

    Executed {
        graph,
        outcome,
        run_options,
        duration_ms,
        final_context,
        engine,
        model,
    }
}

#[cfg(test)]
#[path = "execute/tests.rs"]
mod tests;
