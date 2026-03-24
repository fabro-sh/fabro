use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{EdgeContext, NodeDecision, RunLifecycle};
use fabro_core::state::RunState;

use super::super::graph::WorkflowGraph;
use super::super::WorkflowNode;
use crate::context::keys;
use crate::engine;
use crate::outcome::StageUsage;
use crate::preamble::build_preamble;

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeDecision = NodeDecision<Option<StageUsage>>;

/// Graphviz edge captured from edge selection, passed to the next node's before_node
/// for fidelity/thread resolution.
#[derive(Debug, Clone)]
struct IncomingEdgeData {
    edge: Arc<fabro_graphviz::graph::types::Edge>,
}

/// Sub-lifecycle responsible for fidelity/thread resolution and context key setup.
pub struct FidelityLifecycle {
    pub graph: Arc<fabro_graphviz::graph::types::Graph>,
    incoming_edge_data: Mutex<Option<IncomingEdgeData>>,
    /// True on the first node after checkpoint resume when prior fidelity was Full.
    degrade_fidelity_on_resume: Mutex<bool>,
}

impl FidelityLifecycle {
    pub fn new(graph: Arc<fabro_graphviz::graph::types::Graph>) -> Self {
        Self {
            graph,
            incoming_edge_data: Mutex::new(None),
            degrade_fidelity_on_resume: Mutex::new(false),
        }
    }

    pub fn set_degrade_fidelity_on_resume(&self, flag: bool) {
        *self.degrade_fidelity_on_resume.lock().unwrap() = flag;
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for FidelityLifecycle {
    async fn on_run_start(
        &self,
        _graph: &WorkflowGraph,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        // Clear incoming edge data (restart target must not inherit pre-restart edge)
        *self.incoming_edge_data.lock().unwrap() = None;
        Ok(())
    }

    async fn before_node(
        &self,
        node: &WorkflowNode,
        state: &WfRunState,
    ) -> fabro_core::error::Result<WfNodeDecision> {
        let incoming = self.incoming_edge_data.lock().unwrap().take();
        let gv_node = node.inner();

        // 1. Fidelity resolution via resolve_fidelity: edge → node → graph default → Compact
        let incoming_edge_ref = incoming.as_ref().map(|d| d.edge.as_ref());
        let fidelity = engine::resolve_fidelity(incoming_edge_ref, gv_node, &self.graph);

        // 2. Fidelity degradation on resume (full → summary:high)
        let fidelity = {
            let mut degrade = self.degrade_fidelity_on_resume.lock().unwrap();
            if *degrade {
                *degrade = false;
                fidelity.degraded()
            } else {
                fidelity
            }
        };

        // 3. Set INTERNAL_FIDELITY
        state.context.set(
            keys::INTERNAL_FIDELITY,
            serde_json::json!(fidelity.to_string()),
        );

        // 4. Preamble building: if Full, empty preamble; otherwise build from context
        let preamble = {
            let wf_context = crate::context::Context::from_values(state.context.snapshot());
            build_preamble(
                fidelity,
                &wf_context,
                &self.graph,
                &state.completed_nodes,
                &state.node_outcomes,
            )
        };
        state
            .context
            .set(keys::CURRENT_PREAMBLE, serde_json::json!(preamble));

        // 5. Thread ID resolution via resolve_thread_id: edge → node → graph default → class → previous
        let thread_id = engine::resolve_thread_id(
            incoming_edge_ref,
            gv_node,
            &self.graph,
            state.previous_node_id.as_deref(),
        );

        // 6. Set thread.{tid}.current_node
        if let Some(ref tid) = thread_id {
            let key = format!("thread.{tid}.current_node");
            state.context.set(key, serde_json::json!(node.id()));
        }

        // 7. Set INTERNAL_THREAD_ID (or null)
        match thread_id {
            Some(tid) => {
                state
                    .context
                    .set(keys::INTERNAL_THREAD_ID, serde_json::json!(tid));
            }
            None => {
                state
                    .context
                    .set(keys::INTERNAL_THREAD_ID, serde_json::Value::Null);
            }
        }

        // 8. Set INTERNAL_NODE_VISIT_COUNT and CURRENT_NODE
        let visits = state.node_visits.get(node.id()).copied().unwrap_or(0);
        state
            .context
            .set(keys::CURRENT_NODE, serde_json::json!(node.id()));
        state
            .context
            .set(keys::INTERNAL_NODE_VISIT_COUNT, serde_json::json!(visits));

        Ok(NodeDecision::Continue)
    }

    async fn on_edge_selected(
        &self,
        ctx: &EdgeContext<'_, WorkflowGraph>,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<fabro_core::lifecycle::EdgeDecision> {
        // Capture fidelity/thread from edge for next node
        if let Some(ref edge) = ctx.edge {
            let gv_edge = edge.inner();
            let edge_data = IncomingEdgeData {
                edge: Arc::new(gv_edge.clone()),
            };
            *self.incoming_edge_data.lock().unwrap() = Some(edge_data);
        }
        Ok(fabro_core::lifecycle::EdgeDecision::Continue)
    }
}
