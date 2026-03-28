use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;

use crate::context::Context;
use crate::error::{CoreError, HandlerErrorDetail, Result};
use crate::graph::{EdgeSelection, EdgeSpec, Graph, NodeSpec};
use crate::handler::NodeHandler;
use crate::outcome::{Outcome, StageStatus};
use crate::retry::RetryPolicy;

// ---- Test node ----

#[derive(Debug, Clone)]
pub struct TestNode {
    pub id: String,
    pub terminal: bool,
    pub max_visits: Option<usize>,
    pub goal_gate: Option<(String, StageStatus)>,
}

impl TestNode {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            terminal: false,
            max_visits: None,
            goal_gate: None,
        }
    }

    pub fn terminal(id: &str) -> Self {
        Self {
            id: id.to_string(),
            terminal: true,
            max_visits: None,
            goal_gate: None,
        }
    }

    pub fn with_max_visits(mut self, max: usize) -> Self {
        self.max_visits = Some(max);
        self
    }

    pub fn with_goal_gate(mut self, node_id: &str, required_status: StageStatus) -> Self {
        self.goal_gate = Some((node_id.to_string(), required_status));
        self
    }
}

impl NodeSpec for TestNode {
    fn id(&self) -> &str {
        &self.id
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn max_visits(&self) -> Option<usize> {
        self.max_visits
    }
}

// ---- Test edge ----

#[derive(Debug, Clone)]
pub struct TestEdge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
    pub loop_restart: bool,
}

impl TestEdge {
    pub fn new(from: &str, to: &str) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            label: None,
            loop_restart: false,
        }
    }

    pub fn with_label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }

    pub fn with_loop_restart(mut self) -> Self {
        self.loop_restart = true;
        self
    }
}

impl EdgeSpec for TestEdge {
    fn target(&self) -> &str {
        &self.to
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn is_loop_restart(&self) -> bool {
        self.loop_restart
    }
}

// ---- Test graph ----

#[derive(Debug, Clone)]
pub struct TestGraph {
    pub nodes: Vec<TestNode>,
    pub edges: Vec<TestEdge>,
    pub start_node_id: String,
    pub retry_targets: HashMap<String, String>,
}

impl TestGraph {
    pub fn new(nodes: Vec<TestNode>, edges: Vec<TestEdge>, start: &str) -> Self {
        Self {
            nodes,
            edges,
            start_node_id: start.to_string(),
            retry_targets: HashMap::new(),
        }
    }

    pub fn with_retry_target(mut self, from: &str, to: &str) -> Self {
        self.retry_targets.insert(from.to_string(), to.to_string());
        self
    }
}

impl Graph for TestGraph {
    type Node = TestNode;
    type Edge = TestEdge;
    type Meta = ();

    fn get_node(&self, id: &str) -> Option<Self::Node> {
        self.nodes.iter().find(|n| n.id == id).cloned()
    }

    fn find_start_node(&self) -> Result<Self::Node> {
        self.get_node(&self.start_node_id)
            .ok_or(CoreError::NoStartNode)
    }

    fn outgoing_edges(&self, node_id: &str) -> Vec<Self::Edge> {
        self.edges
            .iter()
            .filter(|e| e.from == node_id)
            .cloned()
            .collect()
    }

    fn select_edge(
        &self,
        node: &Self::Node,
        outcome: &Outcome,
        _context: &Context,
    ) -> Option<EdgeSelection<Self>> {
        let edges = self.outgoing_edges(node.id());
        if edges.is_empty() {
            return None;
        }

        // First: match by preferred_label
        if let Some(ref label) = outcome.preferred_label {
            if let Some(e) = edges
                .iter()
                .find(|e| e.label.as_deref() == Some(label.as_str()))
            {
                return Some(EdgeSelection {
                    edge: e.clone(),
                    reason: "preferred_label",
                });
            }
        }

        // Second: match by status label (e.g. "fail", "success")
        let status_label = outcome.status.to_string();
        if let Some(e) = edges
            .iter()
            .find(|e| e.label.as_deref() == Some(status_label.as_str()))
        {
            return Some(EdgeSelection {
                edge: e.clone(),
                reason: "condition",
            });
        }

        // Third: match by suggested_next_ids
        for suggested in &outcome.suggested_next_ids {
            if let Some(e) = edges.iter().find(|e| e.to == *suggested) {
                return Some(EdgeSelection {
                    edge: e.clone(),
                    reason: "suggested_next",
                });
            }
        }

        // Fourth: unconditional (no label)
        if let Some(e) = edges.iter().find(|e| e.label.is_none()) {
            return Some(EdgeSelection {
                edge: e.clone(),
                reason: "unconditional",
            });
        }

        None
    }

    fn check_goal_gates(
        &self,
        outcomes: &HashMap<String, Outcome>,
    ) -> std::result::Result<(), String> {
        for node in &self.nodes {
            if let Some((ref required_node, ref required_status)) = node.goal_gate {
                if node.is_terminal() {
                    match outcomes.get(required_node) {
                        Some(o) if o.status == *required_status => {}
                        _ => {
                            // Return the failed node id (the node whose gate is
                            // checked), matching fabro-workflows convention
                            return Err(required_node.clone());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn get_retry_target(&self, failed_node_id: &str) -> Option<String> {
        self.retry_targets.get(failed_node_id).cloned()
    }
}

// ---- Test handlers ----

pub struct AlwaysSucceedHandler;

#[async_trait]
impl NodeHandler<TestGraph> for AlwaysSucceedHandler {
    async fn execute(
        &self,
        _node: &TestNode,
        _context: &Context,
        _graph: &TestGraph,
    ) -> Result<Outcome> {
        Ok(Outcome::success())
    }
}

pub struct AlwaysFailHandler {
    pub message: String,
}

impl AlwaysFailHandler {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

#[async_trait]
impl NodeHandler<TestGraph> for AlwaysFailHandler {
    async fn execute(
        &self,
        _node: &TestNode,
        _context: &Context,
        _graph: &TestGraph,
    ) -> Result<Outcome> {
        Ok(Outcome::fail(&self.message))
    }
}

pub struct CountingHandler {
    pub call_count: AtomicU32,
    pub outcomes: std::sync::Mutex<Vec<std::result::Result<Outcome, CoreError>>>,
    pub retry_policy: RetryPolicy,
}

impl CountingHandler {
    pub fn new(outcomes: Vec<std::result::Result<Outcome, CoreError>>) -> Self {
        Self {
            call_count: AtomicU32::new(0),
            outcomes: std::sync::Mutex::new(outcomes),
            retry_policy: RetryPolicy::none(),
        }
    }

    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    pub fn calls(&self) -> u32 {
        self.call_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl NodeHandler<TestGraph> for CountingHandler {
    async fn execute(
        &self,
        _node: &TestNode,
        _context: &Context,
        _graph: &TestGraph,
    ) -> Result<Outcome> {
        let count = self.call_count.fetch_add(1, Ordering::Relaxed);
        let mut outcomes = self.outcomes.lock().unwrap();
        if (count as usize) < outcomes.len() {
            outcomes.remove(0)
        } else {
            Ok(Outcome::success())
        }
    }

    fn retry_policy(&self, _node: &TestNode, _graph: &TestGraph) -> RetryPolicy {
        self.retry_policy.clone()
    }
}

/// A handler that dispatches based on node ID.
pub struct DispatchHandler {
    handlers: HashMap<String, Arc<dyn NodeHandler<TestGraph>>>,
    default: Arc<dyn NodeHandler<TestGraph>>,
}

impl DispatchHandler {
    pub fn new(default: Arc<dyn NodeHandler<TestGraph>>) -> Self {
        Self {
            handlers: HashMap::new(),
            default,
        }
    }

    pub fn with_handler(mut self, node_id: &str, handler: Arc<dyn NodeHandler<TestGraph>>) -> Self {
        self.handlers.insert(node_id.to_string(), handler);
        self
    }
}

#[async_trait]
impl NodeHandler<TestGraph> for DispatchHandler {
    async fn execute(
        &self,
        node: &TestNode,
        context: &Context,
        graph: &TestGraph,
    ) -> Result<Outcome> {
        let handler = self.handlers.get(node.id()).unwrap_or(&self.default);
        handler.execute(node, context, graph).await
    }

    fn retry_policy(&self, node: &TestNode, graph: &TestGraph) -> RetryPolicy {
        let handler = self.handlers.get(node.id()).unwrap_or(&self.default);
        handler.retry_policy(node, graph)
    }

    fn on_retries_exhausted(&self, node: &TestNode, last_outcome: Outcome) -> Outcome {
        let handler = self.handlers.get(node.id()).unwrap_or(&self.default);
        handler.on_retries_exhausted(node, last_outcome)
    }
}

/// A handler that returns Err(CoreError::Handler) with configurable retryability.
pub struct ErrorHandler {
    pub detail: HandlerErrorDetail,
    pub retry_policy: RetryPolicy,
}

impl ErrorHandler {
    pub fn retryable(message: &str, policy: RetryPolicy) -> Self {
        Self {
            detail: HandlerErrorDetail {
                message: message.to_string(),
                retryable: true,
                category: None,
                signature: None,
            },
            retry_policy: policy,
        }
    }

    pub fn non_retryable(message: &str) -> Self {
        Self {
            detail: HandlerErrorDetail {
                message: message.to_string(),
                retryable: false,
                category: None,
                signature: None,
            },
            retry_policy: RetryPolicy::none(),
        }
    }
}

#[async_trait]
impl NodeHandler<TestGraph> for ErrorHandler {
    async fn execute(
        &self,
        _node: &TestNode,
        _context: &Context,
        _graph: &TestGraph,
    ) -> Result<Outcome> {
        Err(CoreError::handler(self.detail.clone()))
    }

    fn retry_policy(&self, _node: &TestNode, _graph: &TestGraph) -> RetryPolicy {
        self.retry_policy.clone()
    }
}

// ---- Helper for building common graphs ----

/// Build a linear graph: start → a → b → ... → end
pub fn linear_graph(node_ids: &[&str]) -> TestGraph {
    assert!(node_ids.len() >= 2, "need at least start and end nodes");
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for (i, id) in node_ids.iter().enumerate() {
        if i == node_ids.len() - 1 {
            nodes.push(TestNode::terminal(id));
        } else {
            nodes.push(TestNode::new(id));
            edges.push(TestEdge::new(id, node_ids[i + 1]));
        }
    }

    TestGraph::new(nodes, edges, node_ids[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graph_finds_start_node() {
        let g = linear_graph(&["start", "end"]);
        let start = g.find_start_node().unwrap();
        assert_eq!(start.id(), "start");
    }

    #[test]
    fn test_graph_gets_node_by_id() {
        let g = linear_graph(&["start", "work", "end"]);
        let node = g.get_node("work").unwrap();
        assert_eq!(node.id(), "work");
        assert!(!node.is_terminal());
    }

    #[test]
    fn test_graph_returns_none_for_missing() {
        let g = linear_graph(&["start", "end"]);
        assert!(g.get_node("nonexistent").is_none());
    }

    #[test]
    fn test_graph_outgoing_edges() {
        let g = linear_graph(&["start", "mid", "end"]);
        let edges = g.outgoing_edges("start");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target(), "mid");
    }

    #[test]
    fn test_graph_terminal_detection() {
        let g = linear_graph(&["start", "end"]);
        assert!(!g.get_node("start").unwrap().is_terminal());
        assert!(g.get_node("end").unwrap().is_terminal());
    }

    #[test]
    fn test_graph_edge_selection_by_label() {
        let g = TestGraph::new(
            vec![
                TestNode::new("start"),
                TestNode::new("a"),
                TestNode::new("b"),
                TestNode::terminal("end"),
            ],
            vec![
                TestEdge::new("start", "a").with_label("success"),
                TestEdge::new("start", "b").with_label("fail"),
            ],
            "start",
        );
        let node = g.get_node("start").unwrap();
        let outcome = Outcome::fail("oops");
        let ctx = Context::new();
        let sel = g.select_edge(&node, &outcome, &ctx).unwrap();
        assert_eq!(sel.edge.target(), "b");
        assert_eq!(sel.reason, "condition");
    }

    #[test]
    fn test_graph_edge_selection_unconditional() {
        let g = linear_graph(&["start", "end"]);
        let node = g.get_node("start").unwrap();
        let outcome = Outcome::success();
        let ctx = Context::new();
        let sel = g.select_edge(&node, &outcome, &ctx).unwrap();
        assert_eq!(sel.edge.target(), "end");
        assert_eq!(sel.reason, "unconditional");
    }

    #[test]
    fn test_graph_goal_gates_pass() {
        let g = TestGraph::new(
            vec![
                TestNode::new("work"),
                TestNode::terminal("end").with_goal_gate("work", StageStatus::Success),
            ],
            vec![TestEdge::new("work", "end")],
            "work",
        );
        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), Outcome::success());
        assert!(g.check_goal_gates(&outcomes).is_ok());
    }

    #[test]
    fn test_graph_goal_gates_fail() {
        let g = TestGraph::new(
            vec![
                TestNode::new("work"),
                TestNode::terminal("end").with_goal_gate("work", StageStatus::Success),
            ],
            vec![TestEdge::new("work", "end")],
            "work",
        );
        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), Outcome::fail("oops"));
        assert!(g.check_goal_gates(&outcomes).is_err());
    }

    #[test]
    fn test_graph_retry_target() {
        let g = linear_graph(&["start", "end"]).with_retry_target("start", "start");
        assert_eq!(g.get_retry_target("start").as_deref(), Some("start"));
        assert!(g.get_retry_target("end").is_none());
    }

    #[tokio::test]
    async fn always_succeed_handler() {
        let h = AlwaysSucceedHandler;
        let g = linear_graph(&["start", "end"]);
        let node = g.get_node("start").unwrap();
        let ctx = Context::new();
        let result = h.execute(&node, &ctx, &g).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn always_fail_handler() {
        let h = AlwaysFailHandler::new("boom");
        let g = linear_graph(&["start", "end"]);
        let node = g.get_node("start").unwrap();
        let ctx = Context::new();
        let result = h.execute(&node, &ctx, &g).await.unwrap();
        assert_eq!(result.status, StageStatus::Fail);
        assert_eq!(result.failure.unwrap().message, "boom");
    }

    #[tokio::test]
    async fn counting_handler_tracks_calls() {
        let h = CountingHandler::new(vec![Ok(Outcome::fail("first")), Ok(Outcome::success())]);
        let g = linear_graph(&["start", "end"]);
        let node = g.get_node("start").unwrap();
        let ctx = Context::new();

        let r1 = h.execute(&node, &ctx, &g).await.unwrap();
        assert_eq!(r1.status, StageStatus::Fail);
        assert_eq!(h.calls(), 1);

        let r2 = h.execute(&node, &ctx, &g).await.unwrap();
        assert_eq!(r2.status, StageStatus::Success);
        assert_eq!(h.calls(), 2);

        // Past end of outcomes list → default success
        let r3 = h.execute(&node, &ctx, &g).await.unwrap();
        assert_eq!(r3.status, StageStatus::Success);
        assert_eq!(h.calls(), 3);
    }
}
