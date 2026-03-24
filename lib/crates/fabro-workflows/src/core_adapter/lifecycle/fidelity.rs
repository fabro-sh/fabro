use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{EdgeContext, NodeDecision, RunLifecycle};
use fabro_core::state::RunState;

use super::super::graph::WorkflowGraph;
use super::super::WorkflowNode;
use crate::context::keys;
use crate::outcome::StageUsage;

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeDecision = NodeDecision<Option<StageUsage>>;

/// Data captured from an edge selection to pass to the next node's before_node.
#[derive(Debug, Clone)]
struct IncomingEdgeData {
    fidelity: Option<String>,
    thread_id: Option<String>,
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

        // Set context keys for the current node
        let visits = state.node_visits.get(node.id()).copied().unwrap_or(0);
        state
            .context
            .set(keys::CURRENT_NODE, serde_json::json!(node.id()));
        state
            .context
            .set(keys::INTERNAL_NODE_VISIT_COUNT, serde_json::json!(visits));

        // Fidelity resolution: edge → node → graph default → compact
        let fidelity = if let Some(ref edge_data) = incoming {
            edge_data
                .fidelity
                .as_deref()
                .or(gv_node.fidelity())
                .unwrap_or("compact")
                .to_string()
        } else {
            gv_node.fidelity().unwrap_or("compact").to_string()
        };

        // Fidelity degradation on resume
        let fidelity = {
            let mut degrade = self.degrade_fidelity_on_resume.lock().unwrap();
            if *degrade {
                *degrade = false;
                let parsed: keys::Fidelity = fidelity.parse().unwrap_or_default();
                parsed.degraded().to_string()
            } else {
                fidelity
            }
        };

        state
            .context
            .set(keys::INTERNAL_FIDELITY, serde_json::json!(fidelity));

        // Thread ID resolution: edge → node → graph default → previous node
        if let Some(ref edge_data) = incoming {
            if let Some(ref tid) = edge_data.thread_id {
                state
                    .context
                    .set(keys::INTERNAL_THREAD_ID, serde_json::json!(tid));
            }
        } else if let Some(tid) = gv_node.thread_id() {
            state
                .context
                .set(keys::INTERNAL_THREAD_ID, serde_json::json!(tid));
        }

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
                fidelity: gv_edge.fidelity().map(String::from),
                thread_id: gv_edge.thread_id().map(String::from),
            };
            *self.incoming_edge_data.lock().unwrap() = Some(edge_data);
        }
        Ok(fabro_core::lifecycle::EdgeDecision::Continue)
    }
}
