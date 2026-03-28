use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;

use fabro_core::error::{CoreError, HandlerErrorDetail, Result as CoreResult};
use fabro_core::handler::NodeHandler;
use fabro_core::outcome::FailureCategory;
use fabro_core::retry::RetryPolicy as CoreRetryPolicy;

use crate::context::Context;

use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::handler::{dispatch_handler, format_panic_message, EngineServices};
use crate::outcome::{Outcome, StageStatus};
use crate::retry::build_retry_policy;
use crate::run_dir;
use fabro_graphviz::graph::types::Graph as GvGraph;
use tokio::time::timeout;

/// Production node handler that bridges fabro-core's NodeHandler to the
/// existing fabro-workflows Handler trait via EngineServices.
///
/// On each `execute()` call, forks the context, runs the handler,
/// then diffs and applies changes back.
pub struct WorkflowNodeHandler {
    pub services: Arc<EngineServices>,
    pub run_dir: PathBuf,
    pub graph: Arc<GvGraph>,
}

#[async_trait]
impl NodeHandler<WorkflowGraph> for WorkflowNodeHandler {
    async fn execute(
        &self,
        node: &WorkflowNode,
        context: &Context,
        _graph: &WorkflowGraph,
    ) -> CoreResult<Outcome> {
        let gv_node = node.inner();
        let handler = self.services.registry.resolve(gv_node);

        // Fork the context so handler writes don't leak back unless we diff+apply.
        let snapshot = context.snapshot();
        let wf_context = context.fork();

        // Timeout from the node
        let node_timeout = gv_node.timeout();

        // Wrap with panic catch + timeout
        let run_dir = self.run_dir.clone();
        let future = dispatch_handler(
            handler,
            gv_node,
            &wf_context,
            &self.graph,
            &run_dir,
            &self.services,
        );
        let panic_safe = AssertUnwindSafe(future).catch_unwind();

        let timed_result = if let Some(duration) = node_timeout {
            match timeout(duration, panic_safe).await {
                Ok(inner) => inner,
                Err(_elapsed) => {
                    return Err(CoreError::handler(HandlerErrorDetail {
                        message: format!("handler timed out after {}ms", duration.as_millis()),
                        retryable: true,
                        category: Some(FailureCategory::TransientInfra),
                        signature: None,
                    }));
                }
            }
        } else {
            panic_safe.await
        };

        // 2. After handler returns, diff the forked context against the snapshot
        //    and apply changes back to the original context
        let new_values = wf_context.snapshot();
        for (k, v) in &new_values {
            if snapshot.get(k) != Some(v) {
                context.set(k.clone(), v.clone());
            }
        }

        match timed_result {
            Ok(Ok(wf_outcome)) => Ok(wf_outcome),
            Ok(Err(fabro_err)) => {
                let retryable = handler.should_retry(&fabro_err);
                Err(CoreError::handler(HandlerErrorDetail {
                    message: fabro_err.to_string(),
                    retryable,
                    category: Some(fabro_err.failure_category()),
                    signature: fabro_err.failure_signature_hint(),
                }))
            }
            Err(panic_payload) => {
                let msg = format_panic_message(panic_payload);
                let visit = context.node_visit_count().max(1);
                let panic_dir = run_dir::node_dir(&self.run_dir, &gv_node.id, visit);
                let _ = std::fs::create_dir_all(&panic_dir);
                let _ = std::fs::write(panic_dir.join("panic.txt"), &msg);
                Err(CoreError::handler(HandlerErrorDetail {
                    message: msg,
                    retryable: false,
                    category: Some(FailureCategory::Deterministic),
                    signature: None,
                }))
            }
        }
    }

    fn retry_policy(&self, node: &WorkflowNode, _graph: &WorkflowGraph) -> CoreRetryPolicy {
        let gv_node = node.inner();
        build_retry_policy(gv_node, &self.graph)
    }

    fn on_retries_exhausted(&self, node: &WorkflowNode, last_outcome: Outcome) -> Outcome {
        let gv_node = node.inner();
        if gv_node.allow_partial() {
            Outcome {
                status: StageStatus::PartialSuccess,
                ..last_outcome
            }
        } else {
            Outcome {
                status: StageStatus::Fail,
                ..last_outcome
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fabro_core::executor::ExecutorBuilder;
    use fabro_core::lifecycle::NoopLifecycle;
    use fabro_core::outcome::StageStatus;
    use fabro_core::state::RunState;
    use fabro_graphviz::graph::types::{Edge, Graph, Node};
    use fabro_graphviz::graph::AttrValue;

    use super::*;
    use crate::graph::WorkflowGraph;

    /// Minimal spike handler that always succeeds — proves the trait plumbing.
    pub struct SpikeHandler;

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
        let state = RunState::new(&wf_graph).unwrap();

        let executor = ExecutorBuilder::new(handler)
            .lifecycle(Box::new(NoopLifecycle))
            .build();
        let (result, _) = executor.run(&wf_graph, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }
}
