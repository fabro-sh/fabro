use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fabro_core::error::{Error as CoreError, HandlerErrorDetail, Result as CoreResult};
use fabro_core::handler::NodeHandler;
use fabro_core::outcome::FailureCategory;
use fabro_core::retry::RetryPolicy as CoreRetryPolicy;
use fabro_graphviz::graph::types::{Graph as GvGraph, Node as GvNode};
use fabro_types::{StageId, SystemActorKind};
use futures::FutureExt;
use tokio::sync::watch;
use tokio::time::{Instant, sleep, timeout};

use crate::artifact;
use crate::context::Context;
use crate::error::Error;
use crate::event::StageScope;
use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::handler::{EngineServices, NodeTimeoutPolicy, dispatch_handler, format_panic_message};
use crate::interview_runtime::InterviewBlockState;
use crate::outcome::{FailureDetail, Outcome, StageOutcome};
use crate::retry::build_retry_policy;

/// Runs `future` under a `duration` budget that only counts time when this
/// stage is not waiting on human input. A sibling stage's interview does not
/// pause this budget — the wait is keyed by `stage_id`.
///
/// Returns `None` if the budget runs out first.
async fn timeout_excluding_interview_wait<F>(
    duration: Duration,
    stage_id: &StageId,
    mut interview_blocks: watch::Receiver<InterviewBlockState>,
    future: F,
) -> Option<F::Output>
where
    F: Future,
{
    tokio::pin!(future);
    let mut remaining = duration;

    loop {
        let blocked = interview_blocks
            .borrow_and_update()
            .is_stage_blocked(stage_id);
        let active_started = Instant::now();
        tokio::select! {
            biased;
            output = &mut future => return Some(output),
            changed = interview_blocks.changed() => {
                if changed.is_err() {
                    // The blocker outlives every handler. If it ever goes away,
                    // fall back to a plain deadline rather than spinning.
                    return timeout(remaining, future).await.ok();
                }
                if !blocked {
                    remaining = remaining.saturating_sub(active_started.elapsed());
                }
            }
            () = sleep(remaining), if !blocked => return None,
        }
    }
}

/// Production node handler that bridges fabro-core's NodeHandler to the
/// existing fabro-workflow Handler trait via EngineServices.
///
/// On each `execute()` call, forks the context, runs the handler,
/// then diffs and applies changes back.
pub(crate) struct WorkflowNodeHandler {
    pub services: Arc<EngineServices>,
    pub run_dir:  PathBuf,
    pub graph:    Arc<GvGraph>,
}

/// Execute one handler attempt through the workflow-owned artifact, panic, and
/// timeout envelope.
///
/// The core executor and direct parallel branch runner deliberately own their
/// retry loops separately, but both attempts must receive identical handler
/// semantics.
pub(crate) async fn execute_single_attempt(
    node: &GvNode,
    context: &Context,
    graph: &GvGraph,
    run_dir: &Path,
    services: &EngineServices,
) -> CoreResult<Outcome> {
    // Between-stage budget gate. LLM stages also charge and check per
    // response; this stops every stage type (command, human, parallel) from
    // starting once the run's `[run.limits]` budget is spent.
    if let Some(exceeded) = services
        .run
        .run_budget()
        .and_then(|budget| budget.exceeded())
    {
        return Err(CoreError::BudgetExhausted {
            message: exceeded.exceeded_message(),
        });
    }

    let handler = services.registry.resolve(node);

    let wf_context = artifact::resolve_context_for_execution(
        context,
        &services.run.run_store,
        &*services.run.sandbox,
        run_dir,
    )
    .await
    .map_err(|err| {
        CoreError::handler(HandlerErrorDetail {
            retryable: true,
            failure:   err.to_failure_detail(),
        })
    })?;
    let execution_snapshot = wf_context.snapshot();

    let node_timeout = match handler.node_timeout_policy(node) {
        NodeTimeoutPolicy::ExecutorEnforced => node.timeout(),
        NodeTimeoutPolicy::HandlerManaged => None,
    };

    let future = dispatch_handler(handler, node, &wf_context, graph, run_dir, services);
    let panic_safe = AssertUnwindSafe(future).catch_unwind();
    let timed_result = if let Some(duration) = node_timeout {
        let stage_id = StageScope::for_handler(&wf_context, &node.id).stage_id();
        let Some(inner) = timeout_excluding_interview_wait(
            duration,
            &stage_id,
            services.run.interview_blocker.subscribe(),
            panic_safe,
        )
        .await
        else {
            let mut failure = FailureDetail::new(
                format!("handler timed out after {}ms", duration.as_millis()),
                FailureCategory::TransientInfra,
            );
            failure.system_actor = Some(SystemActorKind::Timeout);
            return Err(CoreError::handler(HandlerErrorDetail {
                retryable: true,
                failure,
            }));
        };
        inner
    } else {
        panic_safe.await
    };

    let mut new_values = wf_context.snapshot();
    artifact::normalize_durable_updates(&mut new_values);
    for (key, value) in &new_values {
        if execution_snapshot.get(key) != Some(value) {
            context.set(key.clone(), value.clone());
        }
    }

    match timed_result {
        Ok(Ok(wf_outcome)) => Ok(wf_outcome),
        Ok(Err(Error::Cancelled)) => Err(CoreError::Cancelled),
        // Budget exhaustion terminates the run: it must not become a fail
        // outcome, which would follow fail edges and keep executing stages.
        Ok(Err(Error::BudgetExhausted(message))) => Err(CoreError::BudgetExhausted { message }),
        Ok(Err(fabro_err)) => {
            let retryable = handler.should_retry(&fabro_err);
            Err(CoreError::handler(HandlerErrorDetail {
                retryable,
                failure: fabro_err.to_failure_detail(),
            }))
        }
        Err(panic_payload) => {
            let msg = format_panic_message(&panic_payload);
            Err(CoreError::handler(HandlerErrorDetail {
                retryable: false,
                failure:   FailureDetail::new(msg, FailureCategory::Deterministic),
            }))
        }
    }
}

pub(crate) fn finalize_retries_exhausted(node: &GvNode, last_outcome: Outcome) -> Outcome {
    if node.allow_partial() {
        Outcome {
            status: StageOutcome::PartiallySucceeded,
            ..last_outcome
        }
    } else {
        Outcome {
            status: StageOutcome::Failed {
                retry_requested: false,
            },
            ..last_outcome
        }
    }
}

#[async_trait]
impl NodeHandler<WorkflowGraph> for WorkflowNodeHandler {
    async fn execute(
        &self,
        node: &WorkflowNode,
        context: &Context,
        _graph: &WorkflowGraph,
    ) -> CoreResult<Outcome> {
        execute_single_attempt(
            node.inner(),
            context,
            &self.graph,
            &self.run_dir,
            &self.services,
        )
        .await
    }

    async fn context_for_edge_selection(
        &self,
        context: &Context,
        _graph: &WorkflowGraph,
    ) -> CoreResult<Context> {
        artifact::resolve_context_for_edge_selection(context, &self.services.run.run_store)
            .await
            .map_err(|err| {
                CoreError::handler(HandlerErrorDetail {
                    retryable: true,
                    failure:   err.to_failure_detail(),
                })
            })
    }

    fn retry_policy(&self, node: &WorkflowNode, _graph: &WorkflowGraph) -> CoreRetryPolicy {
        let gv_node = node.inner();
        build_retry_policy(gv_node, &self.graph)
    }

    fn on_retries_exhausted(&self, node: &WorkflowNode, last_outcome: Outcome) -> Outcome {
        finalize_retries_exhausted(node.inner(), last_outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fabro_core::executor::ExecutorBuilder;
    use fabro_core::lifecycle::NoopLifecycle;
    use fabro_core::outcome::StageOutcome;
    use fabro_core::state::ExecutionState;
    use fabro_graphviz::graph::AttrValue;
    use fabro_graphviz::graph::types::{Edge, Graph, Node};

    use super::*;
    use crate::event::Emitter;
    use crate::graph::WorkflowGraph;
    use crate::interview_runtime::RunInterviewBlocker;

    /// Minimal spike handler that always succeeds — proves the trait plumbing.
    pub(crate) struct SpikeHandler;

    #[async_trait]
    impl NodeHandler<WorkflowGraph> for SpikeHandler {
        async fn execute(
            &self,
            _node: &WorkflowNode,
            _context: &Context,
            _graph: &WorkflowGraph,
        ) -> CoreResult<Outcome> {
            Ok(Outcome::success())
        }

        fn retry_policy(&self, _node: &WorkflowNode, _graph: &WorkflowGraph) -> CoreRetryPolicy {
            CoreRetryPolicy::none()
        }
    }

    #[tokio::test]
    async fn spike_core_executor_runs_start_to_exit() {
        // Build a minimal graph: start [Mdiamond] → exit [Msquare]
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        graph.nodes.insert("exit".to_string(), exit);
        graph.edges.push(Edge::new("start", "exit"));

        let wf_graph = WorkflowGraph(Arc::new(graph));
        let handler: Arc<dyn NodeHandler<WorkflowGraph>> = Arc::new(SpikeHandler);
        let state = ExecutionState::new(&wf_graph).unwrap();

        let executor = ExecutorBuilder::new(handler)
            .lifecycle(Box::new(NoopLifecycle))
            .build();
        let (result, _) = executor.run(&wf_graph, state).await.unwrap();
        assert_eq!(result.status, StageOutcome::Succeeded);
    }

    #[tokio::test(start_paused = true)]
    async fn node_timeout_does_not_count_own_interview_wait() {
        let blocker = Arc::new(RunInterviewBlocker::new());
        let emitter = Arc::new(Emitter::default());
        let stage_id = StageId::new("agent", 1);
        let block_state = blocker.subscribe();
        let guard = blocker.block(emitter, stage_id.clone());

        let result = timeout_excluding_interview_wait(
            Duration::from_millis(50),
            &stage_id,
            block_state,
            async move {
                sleep(Duration::from_millis(100)).await;
                guard.resolve();
                sleep(Duration::from_millis(40)).await;
                "completed"
            },
        )
        .await;

        assert_eq!(result, Some("completed"));
    }

    #[tokio::test(start_paused = true)]
    async fn node_timeout_still_limits_active_work_after_interview() {
        let blocker = Arc::new(RunInterviewBlocker::new());
        let emitter = Arc::new(Emitter::default());
        let stage_id = StageId::new("agent", 1);
        let block_state = blocker.subscribe();
        let guard = blocker.block(emitter, stage_id.clone());

        let result = timeout_excluding_interview_wait(
            Duration::from_millis(50),
            &stage_id,
            block_state,
            async move {
                sleep(Duration::from_millis(100)).await;
                guard.resolve();
                sleep(Duration::from_millis(60)).await;
            },
        )
        .await;

        assert_eq!(result, None);
    }

    #[tokio::test(start_paused = true)]
    async fn node_timeout_does_not_pause_for_another_stage_interview() {
        let blocker = Arc::new(RunInterviewBlocker::new());
        let emitter = Arc::new(Emitter::default());
        let blocked_stage = StageId::new("agent_a", 1);
        let active_stage = StageId::new("agent_b", 1);
        let block_state = blocker.subscribe();
        let guard = blocker.block(emitter, blocked_stage);

        let result = timeout_excluding_interview_wait(
            Duration::from_millis(50),
            &active_stage,
            block_state,
            sleep(Duration::from_millis(100)),
        )
        .await;
        guard.resolve();

        assert_eq!(result, None);
    }
}
