use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;

use fabro_core::context::Context as CoreContext;
use fabro_core::error::{CoreError, HandlerErrorDetail, Result as CoreResult};
use fabro_core::handler::NodeHandler;
use fabro_core::outcome::FailureCategory;
use fabro_core::retry::RetryPolicy as CoreRetryPolicy;

use super::graph::WorkflowGraph;
use super::WorkflowNode;
use crate::engine;
use crate::handler::EngineServices;
use crate::outcome::{Outcome, StageStatus};

/// Production node handler that bridges fabro-core's NodeHandler to the
/// existing fabro-workflows Handler trait via EngineServices.
pub struct WorkflowNodeHandler {
    pub services: Arc<EngineServices>,
    pub run_dir: PathBuf,
}

#[async_trait]
impl NodeHandler<WorkflowGraph> for WorkflowNodeHandler {
    async fn execute(
        &self,
        node: &WorkflowNode,
        _context: &CoreContext,
        _graph: &WorkflowGraph,
    ) -> CoreResult<Outcome> {
        let gv_node = node.inner();
        let handler = self.services.registry.resolve(gv_node);

        let wf_context = crate::context::Context::new();
        let wf_graph = fabro_graphviz::graph::types::Graph::new("stub");

        // Timeout from the node
        let node_timeout = gv_node.timeout();

        // Wrap with panic catch + timeout
        let run_dir = self.run_dir.clone();
        let future = crate::handler::dispatch_handler(
            handler,
            gv_node,
            &wf_context,
            &wf_graph,
            &run_dir,
            &self.services,
        );
        let panic_safe = AssertUnwindSafe(future).catch_unwind();

        let timed_result = if let Some(duration) = node_timeout {
            match tokio::time::timeout(duration, panic_safe).await {
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
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    format!("handler panicked: {s}")
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    format!("handler panicked: {s}")
                } else {
                    "handler panicked".to_string()
                };
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
        let gv_graph = fabro_graphviz::graph::types::Graph::new("stub");
        let wf_policy = engine::build_retry_policy(gv_node, &gv_graph);
        CoreRetryPolicy {
            max_attempts: wf_policy.max_attempts,
            backoff: wf_policy.backoff,
        }
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

    use super::super::graph::WorkflowGraph;
    use super::*;

    /// Minimal spike handler that always succeeds — proves the trait plumbing.
    pub struct SpikeHandler;

    #[async_trait]
    impl NodeHandler<WorkflowGraph> for SpikeHandler {
        async fn execute(
            &self,
            _node: &WorkflowNode,
            _context: &CoreContext,
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
        let result = executor.run(&wf_graph, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }
}
