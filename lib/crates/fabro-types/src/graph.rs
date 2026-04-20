use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Typed attribute values for nodes, edges, and graph-level attributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttrValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Duration(Duration),
}

impl AttrValue {
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(n) => Some(*n),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(n) => Some(*n),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_duration(&self) -> Option<Duration> {
        match self {
            Self::Duration(d) => Some(*d),
            _ => None,
        }
    }

    /// Convert any variant to its string representation.
    #[must_use]
    pub fn to_string_value(&self) -> String {
        match self {
            Self::String(s) => s.clone(),
            Self::Integer(n) => n.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Boolean(b) => b.to_string(),
            Self::Duration(d) => format!("{}ms", d.as_millis()),
        }
    }
}

/// Returns true if the handler type is an LLM-based handler (agent or prompt,
/// including legacy aliases).
#[must_use]
pub fn is_llm_handler_type(handler_type: Option<&str>) -> bool {
    matches!(
        handler_type,
        Some("agent" | "agent_loop" | "prompt" | "one_shot")
    )
}

/// Maps Graphviz shapes to handler type strings (Section 2.8).
#[must_use]
pub fn shape_to_handler_type(shape: &str) -> Option<&'static str> {
    match shape {
        "Mdiamond" => Some("start"),
        "Msquare" => Some("exit"),
        "box" => Some("agent"),
        "tab" => Some("prompt"),
        "hexagon" => Some("human"),
        "diamond" => Some("conditional"),
        "component" => Some("parallel"),
        "tripleoctagon" => Some("parallel.fan_in"),
        "parallelogram" => Some("command"),
        "house" => Some("stack.manager_loop"),
        "insulator" => Some("wait"),
        _ => None,
    }
}

/// A node in the workflow graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id:      String,
    pub attrs:   HashMap<String, AttrValue>,
    /// CSS-like classes for model stylesheet targeting (from `class` attr and
    /// subgraph derivation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classes: Vec<String>,
}

impl Node {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id:      id.into(),
            attrs:   HashMap::new(),
            classes: Vec::new(),
        }
    }

    fn str_attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).and_then(AttrValue::as_str)
    }

    fn bool_attr(&self, key: &str) -> Option<bool> {
        self.attrs.get(key).and_then(AttrValue::as_bool)
    }

    fn int_attr(&self, key: &str) -> Option<i64> {
        self.attrs.get(key).and_then(AttrValue::as_i64)
    }

    #[must_use]
    pub fn label(&self) -> &str {
        self.str_attr("label").unwrap_or(&self.id)
    }

    #[must_use]
    pub fn shape(&self) -> &str {
        self.str_attr("shape").unwrap_or("box")
    }

    #[must_use]
    pub fn node_type(&self) -> Option<&str> {
        self.str_attr("type")
    }

    #[must_use]
    pub fn prompt(&self) -> Option<&str> {
        self.str_attr("prompt")
    }

    #[must_use]
    pub fn max_retries(&self) -> Option<i64> {
        self.int_attr("max_retries")
    }

    #[must_use]
    pub fn max_visits(&self) -> Option<i64> {
        self.int_attr("max_visits")
    }

    #[must_use]
    pub fn goal_gate(&self) -> bool {
        self.bool_attr("goal_gate").unwrap_or(false)
    }

    #[must_use]
    pub fn retry_target(&self) -> Option<&str> {
        self.str_attr("retry_target")
    }

    #[must_use]
    pub fn fallback_retry_target(&self) -> Option<&str> {
        self.str_attr("fallback_retry_target")
    }

    #[must_use]
    pub fn fidelity(&self) -> Option<&str> {
        self.str_attr("fidelity")
    }

    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        self.str_attr("thread_id")
    }

    #[must_use]
    pub fn class(&self) -> Option<&str> {
        self.str_attr("class")
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.attrs.get("timeout").and_then(AttrValue::as_duration)
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.str_attr("model")
    }

    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.str_attr("provider")
    }

    #[must_use]
    pub fn max_tokens(&self) -> Option<i64> {
        self.int_attr("max_tokens").filter(|&v| v > 0)
    }

    #[must_use]
    pub fn reasoning_effort(&self) -> &str {
        self.str_attr("reasoning_effort").unwrap_or("high")
    }

    #[must_use]
    pub fn speed(&self) -> Option<&str> {
        self.str_attr("speed")
    }

    #[must_use]
    pub fn auto_status(&self) -> bool {
        self.bool_attr("auto_status").unwrap_or(false)
    }

    #[must_use]
    pub fn allow_partial(&self) -> bool {
        self.bool_attr("allow_partial").unwrap_or(false)
    }

    #[must_use]
    pub fn project_memory(&self) -> bool {
        self.bool_attr("project_memory").unwrap_or(true)
    }

    #[must_use]
    pub fn retry_policy(&self) -> Option<&str> {
        self.str_attr("retry_policy")
    }

    #[must_use]
    pub fn backend(&self) -> Option<&str> {
        self.str_attr("backend")
    }

    #[must_use]
    pub fn selection(&self) -> &str {
        self.str_attr("selection").unwrap_or("deterministic")
    }

    /// Resolve the handler type for this node using explicit type or shape
    /// mapping.
    #[must_use]
    pub fn handler_type(&self) -> Option<&str> {
        if let Some(t) = self.node_type() {
            return Some(t);
        }
        shape_to_handler_type(self.shape())
    }
}

/// An edge connecting two nodes in the workflow graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from:  String,
    pub to:    String,
    pub attrs: HashMap<String, AttrValue>,
}

impl Edge {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from:  from.into(),
            to:    to.into(),
            attrs: HashMap::new(),
        }
    }

    fn str_attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).and_then(AttrValue::as_str)
    }

    fn bool_attr(&self, key: &str) -> Option<bool> {
        self.attrs.get(key).and_then(AttrValue::as_bool)
    }

    fn int_attr(&self, key: &str) -> Option<i64> {
        self.attrs.get(key).and_then(AttrValue::as_i64)
    }

    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.str_attr("label")
    }

    #[must_use]
    pub fn condition(&self) -> Option<&str> {
        self.str_attr("condition")
    }

    #[must_use]
    pub fn weight(&self) -> i64 {
        self.int_attr("weight").unwrap_or(0)
    }

    #[must_use]
    pub fn fidelity(&self) -> Option<&str> {
        self.str_attr("fidelity")
    }

    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        self.str_attr("thread_id")
    }

    #[must_use]
    pub fn loop_restart(&self) -> bool {
        self.bool_attr("loop_restart").unwrap_or(false)
    }

    #[must_use]
    pub fn freeform(&self) -> bool {
        self.bool_attr("freeform").unwrap_or(false)
    }
}

/// The parsed workflow graph containing nodes, edges, and graph-level
/// attributes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Graph {
    pub name:  String,
    pub nodes: HashMap<String, Node>,
    pub edges: Vec<Edge>,
    pub attrs: HashMap<String, AttrValue>,
}

impl Graph {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name:  name.into(),
            nodes: HashMap::new(),
            edges: Vec::new(),
            attrs: HashMap::new(),
        }
    }

    /// Returns all outgoing edges from the given node.
    #[must_use]
    pub fn outgoing_edges(&self, node_id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.from == node_id).collect()
    }

    /// Returns all incoming edges to the given node.
    #[must_use]
    pub fn incoming_edges(&self, node_id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.to == node_id).collect()
    }

    /// Find the start node: shape=Mdiamond, or id "start"/"Start".
    #[must_use]
    pub fn find_start_node(&self) -> Option<&Node> {
        // First: look for shape=Mdiamond
        let by_shape = self.nodes.values().find(|n| n.shape() == "Mdiamond");
        if by_shape.is_some() {
            return by_shape;
        }
        // Second: look for id "start" or "Start"
        self.nodes.get("start").or_else(|| self.nodes.get("Start"))
    }

    /// Find the exit node: shape=Msquare, or id "exit"/"Exit".
    #[must_use]
    pub fn find_exit_node(&self) -> Option<&Node> {
        let by_shape = self.nodes.values().find(|n| n.shape() == "Msquare");
        if by_shape.is_some() {
            return by_shape;
        }
        self.nodes
            .get("exit")
            .or_else(|| self.nodes.get("Exit"))
            .or_else(|| self.nodes.get("end"))
            .or_else(|| self.nodes.get("End"))
    }

    /// Graph-level goal attribute.
    pub fn goal(&self) -> &str {
        self.attrs
            .get("goal")
            .and_then(AttrValue::as_str)
            .unwrap_or("")
    }

    /// Graph-level model stylesheet attribute.
    pub fn model_stylesheet(&self) -> &str {
        self.attrs
            .get("model_stylesheet")
            .and_then(AttrValue::as_str)
            .unwrap_or("")
    }

    /// Graph-level `default_max_retries` (default 0).
    pub fn default_max_retries(&self) -> i64 {
        self.attrs
            .get("default_max_retries")
            .and_then(AttrValue::as_i64)
            .unwrap_or(0)
    }

    /// Graph-level `retry_target`.
    pub fn retry_target(&self) -> Option<&str> {
        self.attrs.get("retry_target").and_then(AttrValue::as_str)
    }

    /// Graph-level `fallback_retry_target`.
    pub fn fallback_retry_target(&self) -> Option<&str> {
        self.attrs
            .get("fallback_retry_target")
            .and_then(AttrValue::as_str)
    }

    /// Graph-level `default_fidelity`.
    pub fn default_fidelity(&self) -> Option<&str> {
        self.attrs
            .get("default_fidelity")
            .and_then(AttrValue::as_str)
    }

    /// Graph-level `default_thread`.
    pub fn default_thread(&self) -> Option<&str> {
        self.attrs.get("default_thread").and_then(AttrValue::as_str)
    }

    /// Graph-level `loop_restart_signature_limit` (default 3).
    /// When the same failure signature repeats this many times, the pipeline
    /// aborts.
    pub fn loop_restart_signature_limit(&self) -> usize {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "Values below 1 are filtered out before this usize conversion."
        )]
        self.attrs
            .get("loop_restart_signature_limit")
            .and_then(AttrValue::as_i64)
            .filter(|&v| v >= 1)
            .map_or(3, |v| v as usize)
    }

    /// Graph-level `stall_timeout`. Defaults to 1800s. Returns `None` when set
    /// to zero (disabled).
    pub fn stall_timeout(&self) -> Option<Duration> {
        match self
            .attrs
            .get("stall_timeout")
            .and_then(AttrValue::as_duration)
        {
            Some(d) if d.is_zero() => None,
            Some(d) => Some(d),
            None => Some(Duration::from_mins(30)),
        }
    }

    /// Graph-level `max_node_visits` (default 0 = disabled).
    pub fn max_node_visits(&self) -> u64 {
        self.attrs
            .get("max_node_visits")
            .and_then(AttrValue::as_i64)
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_value_as_str() {
        let val = AttrValue::String("hello".to_string());
        assert_eq!(val.as_str(), Some("hello"));
        assert_eq!(AttrValue::Integer(1).as_str(), None);
    }

    #[test]
    fn attr_value_as_i64() {
        assert_eq!(AttrValue::Integer(42).as_i64(), Some(42));
        assert_eq!(AttrValue::String("x".to_string()).as_i64(), None);
    }

    #[test]
    fn attr_value_as_f64() {
        assert_eq!(AttrValue::Float(3.15).as_f64(), Some(3.15));
        assert_eq!(AttrValue::Integer(1).as_f64(), None);
    }

    #[test]
    fn attr_value_as_bool() {
        assert_eq!(AttrValue::Boolean(true).as_bool(), Some(true));
        assert_eq!(AttrValue::String("true".to_string()).as_bool(), None);
    }

    #[test]
    fn attr_value_as_duration() {
        let d = Duration::from_secs(10);
        assert_eq!(AttrValue::Duration(d).as_duration(), Some(d));
        assert_eq!(AttrValue::Integer(10).as_duration(), None);
    }

    #[test]
    fn shape_to_handler_type_mappings() {
        assert_eq!(shape_to_handler_type("Mdiamond"), Some("start"));
        assert_eq!(shape_to_handler_type("Msquare"), Some("exit"));
        assert_eq!(shape_to_handler_type("box"), Some("agent"));
        assert_eq!(shape_to_handler_type("tab"), Some("prompt"));
        assert_eq!(shape_to_handler_type("hexagon"), Some("human"));
        assert_eq!(shape_to_handler_type("diamond"), Some("conditional"));
        assert_eq!(shape_to_handler_type("component"), Some("parallel"));
        assert_eq!(
            shape_to_handler_type("tripleoctagon"),
            Some("parallel.fan_in")
        );
        assert_eq!(shape_to_handler_type("parallelogram"), Some("command"));
        assert_eq!(shape_to_handler_type("house"), Some("stack.manager_loop"));
        assert_eq!(shape_to_handler_type("insulator"), Some("wait"));
        assert_eq!(shape_to_handler_type("unknown"), None);
    }

    #[test]
    fn is_llm_handler_type_checks() {
        assert!(is_llm_handler_type(Some("agent")));
        assert!(is_llm_handler_type(Some("agent_loop")));
        assert!(is_llm_handler_type(Some("prompt")));
        assert!(is_llm_handler_type(Some("one_shot")));
        assert!(!is_llm_handler_type(Some("command")));
        assert!(!is_llm_handler_type(Some("human")));
        assert!(!is_llm_handler_type(None));
    }

    #[test]
    fn node_defaults() {
        let node = Node::new("test");
        assert_eq!(node.id, "test");
        assert_eq!(node.label(), "test");
        assert_eq!(node.shape(), "box");
        assert_eq!(node.node_type(), None);
        assert_eq!(node.prompt(), None);
        assert_eq!(node.max_retries(), None);
        assert!(!node.goal_gate());
        assert_eq!(node.retry_target(), None);
        assert_eq!(node.fallback_retry_target(), None);
        assert_eq!(node.fidelity(), None);
        assert_eq!(node.thread_id(), None);
        assert_eq!(node.class(), None);
        assert_eq!(node.timeout(), None);
        assert_eq!(node.model(), None);
        assert_eq!(node.provider(), None);
        assert_eq!(node.reasoning_effort(), "high");
        assert_eq!(node.speed(), None);
        assert!(!node.auto_status());
        assert!(!node.allow_partial());
        assert_eq!(node.retry_policy(), None);
        assert_eq!(node.max_visits(), None);
        assert!(node.project_memory());
    }

    #[test]
    fn node_project_memory_false_overrides_default() {
        let mut node = Node::new("x");
        node.attrs
            .insert("project_memory".to_string(), AttrValue::Boolean(false));
        assert!(!node.project_memory());
    }

    #[test]
    fn node_with_attrs() {
        let mut node = Node::new("plan");
        node.attrs.insert(
            "label".to_string(),
            AttrValue::String("Plan step".to_string()),
        );
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("diamond".to_string()),
        );
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        node.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(3));

        assert_eq!(node.label(), "Plan step");
        assert_eq!(node.shape(), "diamond");
        assert!(node.goal_gate());
        assert_eq!(node.max_retries(), Some(3));
    }

    #[test]
    fn node_max_visits_returns_value() {
        let mut node = Node::new("test");
        node.attrs
            .insert("max_visits".to_string(), AttrValue::Integer(5));
        assert_eq!(node.max_visits(), Some(5));
    }

    #[test]
    fn node_handler_type_explicit() {
        let mut node = Node::new("gate");
        node.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));
        assert_eq!(node.handler_type(), Some("human"));
    }

    #[test]
    fn node_handler_type_from_shape() {
        let mut node = Node::new("entry");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        assert_eq!(node.handler_type(), Some("start"));
    }

    #[test]
    fn edge_defaults() {
        let edge = Edge::new("a", "b");
        assert_eq!(edge.from, "a");
        assert_eq!(edge.to, "b");
        assert_eq!(edge.label(), None);
        assert_eq!(edge.condition(), None);
        assert_eq!(edge.weight(), 0);
        assert_eq!(edge.fidelity(), None);
        assert_eq!(edge.thread_id(), None);
        assert!(!edge.loop_restart());
        assert!(!edge.freeform());
    }

    #[test]
    fn edge_with_attrs() {
        let mut edge = Edge::new("a", "b");
        edge.attrs
            .insert("label".to_string(), AttrValue::String("next".to_string()));
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        edge.attrs
            .insert("weight".to_string(), AttrValue::Integer(5));
        edge.attrs
            .insert("loop_restart".to_string(), AttrValue::Boolean(true));
        edge.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));

        assert_eq!(edge.label(), Some("next"));
        assert_eq!(edge.condition(), Some("outcome=success"));
        assert_eq!(edge.weight(), 5);
        assert!(edge.loop_restart());
        assert!(edge.freeform());
    }

    fn sample_graph() -> Graph {
        let mut g = Graph::new("test_pipeline");

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        let work = Node::new("work");
        g.nodes.insert("work".to_string(), work);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        g.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Run tests".to_string()),
        );

        g
    }

    #[test]
    fn graph_find_start_node() {
        let g = sample_graph();
        let start = g.find_start_node().unwrap();
        assert_eq!(start.id, "start");
    }

    #[test]
    fn graph_find_exit_node() {
        let g = sample_graph();
        let exit = g.find_exit_node().unwrap();
        assert_eq!(exit.id, "exit");
    }

    #[test]
    fn graph_find_exit_by_end_id() {
        let mut g = Graph::new("test");
        let node = Node::new("end");
        g.nodes.insert("end".to_string(), node);
        let exit = g.find_exit_node().unwrap();
        assert_eq!(exit.id, "end");
    }

    #[test]
    fn graph_outgoing_edges() {
        let g = sample_graph();
        let edges = g.outgoing_edges("start");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "work");
    }

    #[test]
    fn graph_incoming_edges() {
        let g = sample_graph();
        let edges = g.incoming_edges("exit");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "work");
    }

    #[test]
    fn graph_goal() {
        let g = sample_graph();
        assert_eq!(g.goal(), "Run tests");
    }

    #[test]
    fn graph_goal_default() {
        let g = Graph::new("empty");
        assert_eq!(g.goal(), "");
    }

    #[test]
    fn graph_model_stylesheet_default() {
        let g = Graph::new("empty");
        assert_eq!(g.model_stylesheet(), "");
    }

    #[test]
    fn graph_default_max_retries() {
        let g = Graph::new("empty");
        assert_eq!(g.default_max_retries(), 0);
    }

    #[test]
    fn graph_find_start_by_id_fallback() {
        let mut g = Graph::new("test");
        // No Mdiamond shape, but id is "start"
        let node = Node::new("start");
        g.nodes.insert("start".to_string(), node);
        assert!(g.find_start_node().is_some());
    }

    #[test]
    fn graph_no_start_node() {
        let g = Graph::new("empty");
        assert!(g.find_start_node().is_none());
    }

    #[test]
    fn graph_stall_timeout_default() {
        let g = Graph::new("empty");
        assert_eq!(g.stall_timeout(), Some(Duration::from_mins(30)));
    }

    #[test]
    fn graph_stall_timeout_set() {
        let mut g = Graph::new("test");
        g.attrs.insert(
            "stall_timeout".to_string(),
            AttrValue::Duration(Duration::from_millis(200)),
        );
        assert_eq!(g.stall_timeout(), Some(Duration::from_millis(200)));
    }

    #[test]
    fn graph_stall_timeout_zero_disables() {
        let mut g = Graph::new("test");
        g.attrs.insert(
            "stall_timeout".to_string(),
            AttrValue::Duration(Duration::ZERO),
        );
        assert_eq!(g.stall_timeout(), None);
    }

    #[test]
    fn graph_max_node_visits_default() {
        let g = Graph::new("empty");
        assert_eq!(g.max_node_visits(), 0);
    }

    #[test]
    fn graph_max_node_visits_set() {
        let mut g = Graph::new("test");
        g.attrs
            .insert("max_node_visits".to_string(), AttrValue::Integer(10));
        assert_eq!(g.max_node_visits(), 10);
    }

    #[test]
    fn graph_loop_restart_signature_limit_default() {
        let g = Graph::new("empty");
        assert_eq!(g.loop_restart_signature_limit(), 3);
    }

    #[test]
    fn graph_loop_restart_signature_limit_set() {
        let mut g = Graph::new("test");
        g.attrs.insert(
            "loop_restart_signature_limit".to_string(),
            AttrValue::Integer(5),
        );
        assert_eq!(g.loop_restart_signature_limit(), 5);
    }

    #[test]
    fn graph_loop_restart_signature_limit_invalid_falls_back() {
        let mut g = Graph::new("test");
        g.attrs.insert(
            "loop_restart_signature_limit".to_string(),
            AttrValue::Integer(0),
        );
        assert_eq!(g.loop_restart_signature_limit(), 3);

        g.attrs.insert(
            "loop_restart_signature_limit".to_string(),
            AttrValue::Integer(-1),
        );
        assert_eq!(g.loop_restart_signature_limit(), 3);
    }
}
