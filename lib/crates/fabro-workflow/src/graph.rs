mod routing;

use std::collections::HashMap;
use std::sync::Arc;

use fabro_core::error::{CoreError, Result as CoreResult};
use fabro_core::graph::{EdgeSelection as CoreEdgeSelection, EdgeSpec, Graph, NodeSpec};
use fabro_graphviz::graph::types::{Edge as GvEdge, Graph as GvGraph, Node as GvNode};

use crate::context::Context;
use crate::outcome::{BilledModelUsage, Outcome};

// ---- WorkflowNode ----

#[derive(Debug, Clone)]
pub(crate) struct WorkflowNode(pub Arc<GvNode>);

impl WorkflowNode {
    pub(crate) fn inner(&self) -> &GvNode {
        &self.0
    }
}

impl NodeSpec for WorkflowNode {
    fn id(&self) -> &str {
        &self.0.id
    }

    fn is_terminal(&self) -> bool {
        routing::is_terminal(&self.0)
    }

    fn max_visits(&self) -> Option<usize> {
        self.0
            .max_visits()
            .map(|v| usize::try_from(v.max(0)).unwrap())
    }
}

// ---- WorkflowEdge ----

#[derive(Debug, Clone)]
pub(crate) struct WorkflowEdge(pub Arc<GvEdge>);

impl WorkflowEdge {
    pub(crate) fn inner(&self) -> &GvEdge {
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
pub(crate) struct WorkflowGraph(pub Arc<GvGraph>);

impl WorkflowGraph {
    pub(crate) fn inner(&self) -> &GvGraph {
        &self.0
    }
}

impl Graph for WorkflowGraph {
    type Node = WorkflowNode;
    type Edge = WorkflowEdge;
    type Meta = Option<BilledModelUsage>;

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
        outcome: &Outcome,
        context: &Context,
    ) -> Option<CoreEdgeSelection<Self>> {
        let selection = routing::select_edge(
            node.inner(),
            outcome,
            context,
            self.inner(),
            node.inner().selection(),
        );
        selection.map(|sel| CoreEdgeSelection {
            edge:   WorkflowEdge(Arc::new(sel.edge.clone())),
            reason: sel.reason,
        })
    }

    fn check_goal_gates(
        &self,
        outcomes: &HashMap<String, Outcome>,
    ) -> std::result::Result<(), String> {
        routing::check_goal_gates(self.inner(), outcomes)
    }

    fn get_retry_target(&self, failed_node_id: &str) -> Option<String> {
        routing::get_retry_target(failed_node_id, self.inner())
    }
}
