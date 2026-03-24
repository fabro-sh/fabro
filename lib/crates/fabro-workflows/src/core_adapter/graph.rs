use std::collections::HashMap;
use std::sync::Arc;

use fabro_core::context::Context as CoreContext;
use fabro_core::error::{CoreError, Result as CoreResult};
use fabro_core::graph::{EdgeSelection, EdgeSpec, Graph, NodeSpec};
use fabro_core::outcome::Outcome as CoreOutcome;
use fabro_graphviz::graph::types::{Edge as GvEdge, Graph as GvGraph, Node as GvNode};

use crate::engine;

// ---- WorkflowNode ----

#[derive(Debug, Clone)]
pub struct WorkflowNode(pub Arc<GvNode>);

impl WorkflowNode {
    pub fn inner(&self) -> &GvNode {
        &self.0
    }
}

impl NodeSpec for WorkflowNode {
    fn id(&self) -> &str {
        &self.0.id
    }

    fn is_terminal(&self) -> bool {
        engine::is_terminal(&self.0)
    }

    fn max_visits(&self) -> Option<usize> {
        self.0.max_visits().map(|v| v.max(0) as usize)
    }
}

// ---- WorkflowEdge ----

#[derive(Debug, Clone)]
pub struct WorkflowEdge(pub Arc<GvEdge>);

impl WorkflowEdge {
    pub fn inner(&self) -> &GvEdge {
        &self.0
    }
}

impl EdgeSpec for WorkflowEdge {
    fn target(&self) -> &str {
        &self.0.to
    }

    fn label(&self) -> Option<&str> {
        self.0.label()
    }

    fn is_loop_restart(&self) -> bool {
        self.0.loop_restart()
    }
}

// ---- WorkflowGraph ----

#[derive(Debug, Clone)]
pub struct WorkflowGraph(pub Arc<GvGraph>);

impl WorkflowGraph {
    pub fn inner(&self) -> &GvGraph {
        &self.0
    }
}

impl Graph for WorkflowGraph {
    type Node = WorkflowNode;
    type Edge = WorkflowEdge;

    fn get_node(&self, id: &str) -> Option<Self::Node> {
        self.0
            .nodes
            .get(id)
            .map(|n| WorkflowNode(Arc::new(n.clone())))
    }

    fn find_start_node(&self) -> CoreResult<Self::Node> {
        self.0
            .find_start_node()
            .map(|n| WorkflowNode(Arc::new(n.clone())))
            .ok_or(CoreError::NoStartNode)
    }

    fn outgoing_edges(&self, node_id: &str) -> Vec<Self::Edge> {
        self.0
            .outgoing_edges(node_id)
            .into_iter()
            .map(|e| WorkflowEdge(Arc::new(e.clone())))
            .collect()
    }

    fn select_edge(
        &self,
        node: &Self::Node,
        outcome: &CoreOutcome,
        _context: &CoreContext,
    ) -> Option<EdgeSelection<Self>> {
        // Convert core outcome to workflow outcome for edge selection
        let wf_outcome = super::outcome::core_to_wf_outcome(outcome);
        let wf_context = crate::context::Context::new();
        let selection = engine::select_edge(
            node.inner(),
            &wf_outcome,
            &wf_context,
            self.inner(),
            node.inner().selection(),
        );
        selection.map(|sel| EdgeSelection {
            edge: WorkflowEdge(Arc::new(sel.edge.clone())),
            reason: sel.reason,
        })
    }

    fn check_goal_gates(
        &self,
        outcomes: &HashMap<String, CoreOutcome>,
    ) -> std::result::Result<(), String> {
        let wf_outcomes: HashMap<String, crate::outcome::Outcome> = outcomes
            .iter()
            .map(|(k, v)| (k.clone(), super::outcome::core_to_wf_outcome(v)))
            .collect();
        engine::check_goal_gates(self.inner(), &wf_outcomes)
    }

    fn get_retry_target(&self, failed_node_id: &str) -> Option<String> {
        engine::get_retry_target(failed_node_id, self.inner())
    }
}
