use std::sync::Arc;
use std::time::{Duration, Instant};

use fabro_core::executor::ExecutorBuilder;
use fabro_core::handler::NodeHandler;
use fabro_core::state::ExecutionState;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant as TokioInstant, sleep_until};
use tokio_util::sync::CancellationToken;

use super::types::{Executed, Initialized};
use crate::artifact;
use crate::context::{self, Context};
use crate::error::Error;
use crate::event::{Emitter, Event, RunNoticeCode, RunNoticeLevel};
use crate::graph::WorkflowGraph;
use crate::interview_runtime::InterviewBlockState;
use crate::lifecycle::WorkflowLifecycle;
use crate::node_handler::WorkflowNodeHandler;
use crate::outcome::Outcome;
use crate::records::Checkpoint;

fn seed_context_from_checkpoint(checkpoint: Option<&Checkpoint>) -> Context {
    let context = Context::new();
    if let Some(cp) = checkpoint {
        for (k, v) in &cp.context_values {
            context.set(k.clone(), v.clone());
        }
    }
    context
}

/// Background watchdog that cancels a run which stops emitting events.
struct StallWatchdog {
    /// Cancelled by the monitor once the run stalls. Handed to the executor.
    stall_token: CancellationToken,
    /// Cancelled by us to stop the monitor once the run finishes.
    shutdown:    CancellationToken,
    task:        JoinHandle<()>,
}

impl StallWatchdog {
    fn spawn(
        stall_timeout: Duration,
        emitter: Arc<Emitter>,
        interview_blocks: watch::Receiver<InterviewBlockState>,
    ) -> Self {
        let stall_token = CancellationToken::new();
        let shutdown = CancellationToken::new();
        emitter.touch();
        let task = tokio::spawn(monitor_for_stall(
            stall_timeout,
            stall_token.clone(),
            shutdown.clone(),
            emitter,
            interview_blocks,
        ));
        Self {
            stall_token,
            shutdown,
            task,
        }
    }

    fn stall_token(&self) -> CancellationToken {
        self.stall_token.clone()
    }

    async fn stop(self) {
        self.shutdown.cancel();
        if let Err(error) = self.task.await {
            tracing::error!(error = ?error, "stall watchdog task failed");
        }
    }
}

/// Cancels `stall_token` once the run goes `stall_timeout` without emitting an
/// event. Waiting on human input suspends the timer, and the first unblock
/// starts a fresh full deadline.
///
/// Ordinary activity does not wake this task — a busy run emits an event per
/// agent stream delta. The deadline instead re-reads `Emitter::last_activity()`
/// when it fires and re-arms if the run was active in the meantime.
async fn monitor_for_stall(
    stall_timeout: Duration,
    stall_token: CancellationToken,
    shutdown: CancellationToken,
    emitter: Arc<Emitter>,
    mut interview_blocks: watch::Receiver<InterviewBlockState>,
) {
    let mut deadline = emitter.last_activity() + stall_timeout;

    loop {
        let blocked = interview_blocks.borrow_and_update().is_run_blocked();
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            changed = interview_blocks.changed() => {
                if changed.is_err() {
                    return;
                }
                // Blocking parks the timer; unblocking restarts the full budget.
                deadline = TokioInstant::now() + stall_timeout;
            }
            () = sleep_until(deadline), if !blocked => {
                let extended = emitter.last_activity() + stall_timeout;
                if extended > deadline {
                    deadline = extended;
                    continue;
                }
                stall_token.cancel();
                return;
            }
        }
    }
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

    // Budget gate: a run whose seeded spend (a resume after a budget failure)
    // already exceeds `[run.limits]` fails before the first node. Seeded 80%
    // warnings drain here so they surface once at start.
    if let Some(budget) = engine.run.run_budget() {
        for warning in budget.take_pending_warnings() {
            engine.run.emitter.notice(
                RunNoticeLevel::Warn,
                RunNoticeCode::BudgetNearlyExhausted,
                warning.warning_message(),
            );
        }
        if let Some(exceeded) = budget.exceeded() {
            return Executed {
                graph,
                outcome: Err(Error::BudgetExhausted(format!(
                    "{} — raise [run.limits] to resume",
                    exceeded.exceeded_message()
                ))),
                run_options,
                wall_time_ms: crate::millis_u64(start.elapsed()),
                final_context: seed_context_from_checkpoint(checkpoint.as_ref()),
                engine,
                model,
            };
        }
    }

    let graph_arc = Arc::new(graph.clone());
    let wf_graph = WorkflowGraph(Arc::clone(&graph_arc));

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
        &engine.run.locations,
        &settings_arc,
        Arc::clone(&engine.run.sandbox_git),
        Arc::clone(&engine.run.metadata_runtime),
        engine.run.metadata_writer.clone(),
        checkpoint.is_some(),
        on_node,
        run_control,
        engine.run.stage_executions.clone(),
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
                    wall_time_ms: crate::millis_u64(start.elapsed()),
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
                    wall_time_ms: crate::millis_u64(start.elapsed()),
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
                    wall_time_ms: crate::millis_u64(start.elapsed()),
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

    let stall_watchdog = graph.stall_timeout().map(|stall_timeout| {
        StallWatchdog::spawn(
            stall_timeout,
            Arc::clone(&engine.run.emitter),
            engine.run.interview_blocker.subscribe(),
        )
    });

    let mut builder = ExecutorBuilder::new(handler as Arc<dyn NodeHandler<WorkflowGraph>>)
        .lifecycle(Box::new(lifecycle));

    builder = builder.cancel_token(run_options.cancel_token.clone());
    if let Some(token) = stall_watchdog.as_ref().map(StallWatchdog::stall_token) {
        builder = builder.stall_token(token);
    }
    if let Some(limit) = max_node_visits {
        builder = builder.max_node_visits(limit);
    }

    let executor = builder.build();
    let result = executor.run(&wf_graph, state).await;

    if let Some(watchdog) = stall_watchdog {
        watchdog.stop().await;
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
        Err(fabro_core::Error::BudgetExhausted { message }) => {
            (Err(Error::BudgetExhausted(message)), initial_context)
        }
        Err(fabro_core::Error::Blocked { message }) => {
            (Err(Error::engine(message)), initial_context)
        }
        Err(error @ fabro_core::Error::Context { .. }) => (
            Err(Error::engine_with_source(
                "Pipeline lifecycle operation failed",
                error,
            )),
            initial_context,
        ),
        Err(e) => (Err(Error::engine(e.to_string())), initial_context),
    };

    engine.registry.shutdown_all(&engine.run.emitter).await;

    let wall_time_ms = crate::millis_u64(start.elapsed());

    Executed {
        graph,
        outcome,
        run_options,
        wall_time_ms,
        final_context,
        engine,
        model,
    }
}

#[cfg(test)]
#[path = "execute/tests.rs"]
mod tests;
