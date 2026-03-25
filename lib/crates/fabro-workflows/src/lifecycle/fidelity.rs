use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{EdgeContext, NodeDecision, RunLifecycle};
use fabro_core::state::RunState;
use fabro_graphviz::graph::types::{Edge as GvEdge, Graph as GvGraph, Node as GvNode};

use crate::context::keys;
use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::handler::llm::preamble::build_preamble;
use crate::outcome::StageUsage;

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeDecision = NodeDecision<Option<StageUsage>>;

/// Graphviz edge captured from edge selection, passed to the next node's before_node
/// for fidelity/thread resolution.
#[derive(Debug, Clone)]
struct IncomingEdgeData {
    edge: Arc<GvEdge>,
}

/// Sub-lifecycle responsible for fidelity/thread resolution and context key setup.
pub struct FidelityLifecycle {
    pub graph: Arc<GvGraph>,
    incoming_edge_data: Mutex<Option<IncomingEdgeData>>,
    /// True on the first node after checkpoint resume when prior fidelity was Full.
    degrade_fidelity_on_resume: Mutex<bool>,
}

impl FidelityLifecycle {
    pub fn new(graph: Arc<GvGraph>) -> Self {
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
        let fidelity = resolve_fidelity(incoming_edge_ref, gv_node, &self.graph);

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
        let preamble = build_preamble(
            fidelity,
            &state.context,
            &self.graph,
            &state.completed_nodes,
            &state.node_outcomes,
        );
        state
            .context
            .set(keys::CURRENT_PREAMBLE, serde_json::json!(preamble));

        // 5. Thread ID resolution via resolve_thread_id: edge → node → graph default → class → previous
        let thread_id = resolve_thread_id(
            incoming_edge_ref,
            gv_node,
            &self.graph,
            state.previous_node_id.as_deref(),
        );

        // 6. Set thread.{tid}.current_node
        if let Some(ref tid) = thread_id {
            let key = keys::thread_current_node_key(tid);
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
        let visits = state.node_visits.get(node.id()).copied().unwrap_or(1);
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

/// Resolve the context fidelity for a node, following the precedence:
/// 1. Incoming edge `fidelity` attribute
/// 2. Target node `fidelity` attribute
/// 3. Graph `default_fidelity` attribute
/// 4. Default: Compact
fn resolve_fidelity(
    incoming_edge: Option<&GvEdge>,
    node: &GvNode,
    graph: &GvGraph,
) -> keys::Fidelity {
    let (resolved, source) = if let Some(f) = incoming_edge
        .and_then(|e| e.fidelity())
        .and_then(|s| s.parse().ok())
    {
        (f, "edge")
    } else if let Some(f) = node.fidelity().and_then(|s| s.parse().ok()) {
        (f, "node")
    } else if let Some(f) = graph.default_fidelity().and_then(|s| s.parse().ok()) {
        (f, "graph")
    } else {
        (keys::Fidelity::default(), "default")
    };

    tracing::debug!(
        node = %node.id,
        fidelity = %resolved,
        source = source,
        "Fidelity resolved"
    );

    resolved
}

/// Resolve the thread ID for a node, following the precedence:
/// 1. Incoming edge `thread_id` attribute
/// 2. Target node `thread_id` attribute
/// 3. Graph-level default thread
/// 4. Derived class from enclosing subgraph (first class from the node's classes list)
/// 5. Fallback to previous node ID
fn resolve_thread_id(
    incoming_edge: Option<&GvEdge>,
    node: &GvNode,
    graph: &GvGraph,
    previous_node_id: Option<&str>,
) -> Option<String> {
    if let Some(edge) = incoming_edge {
        if let Some(tid) = edge.thread_id() {
            return Some(tid.to_string());
        }
    }
    if let Some(tid) = node.thread_id() {
        return Some(tid.to_string());
    }
    if let Some(tid) = graph.default_thread() {
        return Some(tid.to_string());
    }
    if let Some(first_class) = node.classes.first() {
        return Some(first_class.clone());
    }
    previous_node_id.map(String::from)
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};

    use super::*;
    use crate::context::keys::Fidelity;

    #[test]
    fn fidelity_defaults_to_compact() {
        let node = Node::new("work");
        let graph = Graph::new("test");
        assert_eq!(resolve_fidelity(None, &node, &graph), Fidelity::Compact);
    }

    #[test]
    fn fidelity_from_graph_default() {
        let node = Node::new("work");
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        assert_eq!(resolve_fidelity(None, &node, &graph), Fidelity::Truncate);
    }

    #[test]
    fn fidelity_from_node_overrides_graph() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        assert_eq!(resolve_fidelity(None, &node, &graph), Fidelity::Full);
    }

    #[test]
    fn fidelity_from_edge_overrides_node() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        let mut edge = Edge::new("a", "work");
        edge.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("summary:high".to_string()),
        );
        let graph = Graph::new("test");
        assert_eq!(
            resolve_fidelity(Some(&edge), &node, &graph),
            Fidelity::SummaryHigh
        );
    }

    #[test]
    fn thread_id_from_node_attribute() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("main-thread".to_string()),
        );
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev")),
            Some("main-thread".to_string())
        );
    }

    #[test]
    fn thread_id_from_edge_attribute() {
        let node = Node::new("work");
        let mut edge = Edge::new("prev", "work");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("edge-thread".to_string()),
        );
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(Some(&edge), &node, &graph, Some("prev")),
            Some("edge-thread".to_string())
        );
    }

    #[test]
    fn thread_id_node_used_when_no_edge_thread() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("node-thread".to_string()),
        );
        let edge = Edge::new("prev", "work");
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(Some(&edge), &node, &graph, Some("prev")),
            Some("node-thread".to_string())
        );
    }

    #[test]
    fn thread_id_edge_overrides_node() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("node-thread".to_string()),
        );
        let mut edge = Edge::new("prev", "work");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("edge-thread".to_string()),
        );
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(Some(&edge), &node, &graph, Some("prev")),
            Some("edge-thread".to_string()),
            "edge thread_id should override node thread_id"
        );
    }

    #[test]
    fn thread_id_from_graph_default_thread() {
        let node = Node::new("work");
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("shared-thread".to_string()),
        );
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev")),
            Some("shared-thread".to_string())
        );
    }

    #[test]
    fn thread_id_edge_overrides_graph_default() {
        let node = Node::new("work");
        let mut edge = Edge::new("prev", "work");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("edge-thread".to_string()),
        );
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("shared-thread".to_string()),
        );
        assert_eq!(
            resolve_thread_id(Some(&edge), &node, &graph, Some("prev")),
            Some("edge-thread".to_string())
        );
    }

    #[test]
    fn thread_id_graph_default_overrides_class() {
        let mut node = Node::new("work");
        node.classes = vec!["planning".to_string()];
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("shared-thread".to_string()),
        );
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev")),
            Some("shared-thread".to_string())
        );
    }

    #[test]
    fn thread_id_from_node_class() {
        let mut node = Node::new("work");
        node.classes = vec!["planning".to_string(), "review".to_string()];
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev")),
            Some("planning".to_string())
        );
    }

    #[test]
    fn thread_id_fallback_to_previous_node() {
        let node = Node::new("work");
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev_node")),
            Some("prev_node".to_string())
        );
    }

    #[test]
    fn thread_id_none_when_no_sources() {
        let node = Node::new("start");
        let graph = Graph::new("test");
        assert_eq!(resolve_thread_id(None, &node, &graph, None), None);
    }
}
