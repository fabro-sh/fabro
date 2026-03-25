use std::collections::HashMap;
use std::time::Duration;

use fabro_graphviz::graph::{Edge, Graph, Node};
use fabro_hooks::HookContext;
use fabro_util::backoff::BackoffPolicy;
use rand::Rng;

use crate::condition::evaluate_condition;
use crate::context::{self, Context};
use crate::error::FailureCategory;
use crate::outcome::{Outcome, OutcomeExt, StageStatus};

/// Populate node-related fields on a `HookContext` from a graph `Node`.
pub(crate) fn set_hook_node(ctx: &mut HookContext, node: &Node) {
    ctx.node_id = Some(node.id.clone());
    ctx.node_label = Some(node.label().to_string());
    ctx.handler_type = node.handler_type().map(String::from);
}

/// Classify the failure mode of a completed outcome.
///
/// Returns `None` for `Success`, `PartialSuccess`, and `Skipped` outcomes.
/// For failures, checks (in priority order):
/// 1. Handler hint in `context_updates["failure_class"]`
/// 2. String heuristics on `failure_reason`
/// 3. Default to `Deterministic`
#[must_use]
pub(crate) fn classify_outcome(outcome: &Outcome) -> Option<FailureCategory> {
    match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess | StageStatus::Skipped => None,
        StageStatus::Fail | StageStatus::Retry => outcome
            .failure_category()
            .or(Some(FailureCategory::Deterministic)),
    }
}

/// Retry policy for node execution.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: BackoffPolicy,
}

impl RetryPolicy {
    const DEFAULT_BACKOFF: BackoffPolicy = BackoffPolicy {
        initial_delay: Duration::from_millis(5_000),
        factor: 2.0,
        max_delay: Duration::from_millis(60_000),
        jitter: true,
    };

    /// No retries -- fail immediately.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            backoff: Self::DEFAULT_BACKOFF,
        }
    }

    /// Standard retry policy: 5 attempts, 5s initial, 2x factor.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            max_attempts: 5,
            backoff: Self::DEFAULT_BACKOFF,
        }
    }

    /// Aggressive retry: 5 attempts, 500ms initial, 2x factor.
    #[must_use]
    pub fn aggressive() -> Self {
        Self {
            max_attempts: 5,
            backoff: BackoffPolicy {
                initial_delay: Duration::from_millis(500),
                ..Self::DEFAULT_BACKOFF
            },
        }
    }

    /// Linear retry: 3 attempts, 500ms fixed delay.
    #[must_use]
    pub fn linear() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffPolicy {
                initial_delay: Duration::from_millis(500),
                factor: 1.0,
                ..Self::DEFAULT_BACKOFF
            },
        }
    }

    /// Patient retry: 3 attempts, 2000ms initial, 3x factor.
    #[must_use]
    pub fn patient() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffPolicy {
                initial_delay: Duration::from_millis(2000),
                factor: 3.0,
                ..Self::DEFAULT_BACKOFF
            },
        }
    }
}

/// Build a retry policy from node and graph attributes.
/// If the node has a `retry_policy` attribute naming a preset, use that.
/// Otherwise, fall back to `max_retries` / graph default.
pub(crate) fn build_retry_policy(node: &Node, graph: &Graph) -> RetryPolicy {
    if let Some(preset) = node.retry_policy() {
        match preset {
            "none" => return RetryPolicy::none(),
            "standard" => return RetryPolicy::standard(),
            "aggressive" => return RetryPolicy::aggressive(),
            "linear" => return RetryPolicy::linear(),
            "patient" => return RetryPolicy::patient(),
            _ => {}
        }
    }
    let max_retries = node
        .max_retries()
        .unwrap_or_else(|| graph.default_max_retries());
    let max_attempts = u32::try_from(max_retries + 1).unwrap_or(1).max(1);
    RetryPolicy {
        max_attempts,
        backoff: RetryPolicy::DEFAULT_BACKOFF,
    }
}

/// Resolve the context fidelity for a node, following the precedence:
/// 1. Incoming edge `fidelity` attribute
/// 2. Target node `fidelity` attribute
/// 3. Graph `default_fidelity` attribute
/// 4. Default: Compact
#[must_use]
pub fn resolve_fidelity(
    incoming_edge: Option<&Edge>,
    node: &Node,
    graph: &Graph,
) -> context::keys::Fidelity {
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
        (context::keys::Fidelity::default(), "default")
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
#[must_use]
pub fn resolve_thread_id(
    incoming_edge: Option<&Edge>,
    node: &Node,
    graph: &Graph,
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

/// Normalize a label for comparison: lowercase, trim, strip accelerator prefixes.
/// Patterns: "[Y] ", "Y) ", "Y - "
pub(crate) fn normalize_label(label: &str) -> String {
    let s = label.trim().to_lowercase();
    if s.starts_with('[') {
        if let Some(rest) = s
            .strip_prefix('[')
            .and_then(|s| s.find(']').map(|i| s[i + 1..].trim_start().to_string()))
        {
            return rest;
        }
    }
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if bytes.get(1) == Some(&b')') {
            return s[2..].trim_start().to_string();
        }
    }
    if s.len() >= 3 {
        if let Some(rest) = s.get(1..).and_then(|r| r.strip_prefix(" - ")) {
            return rest.to_string();
        }
    }
    s
}

/// Pick the best edge by highest weight, then lexical target node ID tiebreak.
pub(crate) fn best_by_weight_then_lexical<'a>(edges: &[&'a Edge]) -> Option<&'a Edge> {
    if edges.is_empty() {
        return None;
    }
    let mut best = edges[0];
    for &edge in &edges[1..] {
        if edge.weight() > best.weight() || (edge.weight() == best.weight() && edge.to < best.to) {
            best = edge;
        }
    }
    Some(best)
}

/// Pick a random edge using weighted-random selection.
/// Edges with `weight <= 0` are treated as weight 1 for probability calculation.
pub(crate) fn weighted_random<'a>(edges: &[&'a Edge]) -> Option<&'a Edge> {
    if edges.is_empty() {
        return None;
    }
    if edges.len() == 1 {
        return Some(edges[0]);
    }
    let weights: Vec<f64> = edges
        .iter()
        .map(|e| {
            let w = e.weight();
            if w <= 0 {
                1.0
            } else {
                w as f64
            }
        })
        .collect();
    let total: f64 = weights.iter().sum();
    let mut rng = rand::thread_rng();
    let mut roll: f64 = rng.gen_range(0.0..total);
    for (i, &w) in weights.iter().enumerate() {
        roll -= w;
        if roll < 0.0 {
            return Some(edges[i]);
        }
    }
    Some(edges[edges.len() - 1])
}

/// Dispatch to the appropriate edge-picking strategy.
fn pick_edge<'a>(edges: &[&'a Edge], selection: &str) -> Option<&'a Edge> {
    match selection {
        "random" => weighted_random(edges),
        _ => best_by_weight_then_lexical(edges),
    }
}

/// Result of edge selection: the chosen edge and the reason it was selected.
pub struct EdgeSelection<'a> {
    pub edge: &'a Edge,
    pub reason: &'static str,
}

fn blocks_unconditional_failure_fallthrough(node: &Node, outcome: &Outcome) -> bool {
    node.handler_type() == Some("human")
        && outcome.status == StageStatus::Fail
        && outcome.preferred_label.is_none()
        && outcome.suggested_next_ids.is_empty()
}

/// Select the next edge from a node's outgoing edges (spec Section 3.3).
#[must_use]
pub fn select_edge<'a>(
    node: &Node,
    outcome: &Outcome,
    context: &Context,
    graph: &'a Graph,
    selection: &str,
) -> Option<EdgeSelection<'a>> {
    let node_id = &node.id;
    let edges = graph.outgoing_edges(node_id);
    if edges.is_empty() {
        return None;
    }

    let condition_matched: Vec<&Edge> = edges
        .iter()
        .filter(|e| {
            e.condition()
                .is_some_and(|c| !c.is_empty() && evaluate_condition(c, outcome, context))
        })
        .copied()
        .collect();
    if !condition_matched.is_empty() {
        return pick_edge(&condition_matched, selection).map(|edge| EdgeSelection {
            edge,
            reason: "condition",
        });
    }

    if let Some(pref) = &outcome.preferred_label {
        let normalized_pref = normalize_label(pref);
        for edge in &edges {
            if edge.condition().is_none_or(str::is_empty) {
                if let Some(label) = edge.label() {
                    if normalize_label(label) == normalized_pref {
                        return Some(EdgeSelection {
                            edge,
                            reason: "preferred_label",
                        });
                    }
                }
            }
        }
    }

    for suggested_id in &outcome.suggested_next_ids {
        for edge in &edges {
            if edge.condition().is_none_or(str::is_empty) && edge.to == *suggested_id {
                return Some(EdgeSelection {
                    edge,
                    reason: "suggested_next",
                });
            }
        }
    }

    if blocks_unconditional_failure_fallthrough(node, outcome) {
        return None;
    }

    let unconditional: Vec<&Edge> = edges
        .iter()
        .filter(|e| e.condition().is_none_or(str::is_empty))
        .copied()
        .collect();
    if !unconditional.is_empty() {
        return pick_edge(&unconditional, selection).map(|edge| EdgeSelection {
            edge,
            reason: "unconditional",
        });
    }

    None
}

/// Check if all goal gates have been satisfied.
/// Returns Ok(()) if all gates passed, or Err with the failed node ID.
pub(crate) fn check_goal_gates(
    graph: &Graph,
    node_outcomes: &HashMap<String, Outcome>,
) -> std::result::Result<(), String> {
    for (node_id, outcome) in node_outcomes {
        if let Some(node) = graph.nodes.get(node_id) {
            if node.goal_gate()
                && outcome.status != StageStatus::Success
                && outcome.status != StageStatus::PartialSuccess
            {
                return Err(node_id.clone());
            }
        }
    }
    Ok(())
}

/// Resolve the retry target for a failed goal gate node.
pub(crate) fn get_retry_target(failed_node_id: &str, graph: &Graph) -> Option<String> {
    if let Some(node) = graph.nodes.get(failed_node_id) {
        if let Some(target) = node.retry_target() {
            if graph.nodes.contains_key(target) {
                return Some(target.to_string());
            }
        }
        if let Some(target) = node.fallback_retry_target() {
            if graph.nodes.contains_key(target) {
                return Some(target.to_string());
            }
        }
    }
    if let Some(target) = graph.retry_target() {
        if graph.nodes.contains_key(target) {
            return Some(target.to_string());
        }
    }
    if let Some(target) = graph.fallback_retry_target() {
        if graph.nodes.contains_key(target) {
            return Some(target.to_string());
        }
    }
    None
}

/// Check whether a node is a terminal (exit) node.
pub(crate) fn is_terminal(node: &Node) -> bool {
    node.shape() == "Msquare" || node.handler_type() == Some("exit")
}

pub(crate) fn node_script(node: &Node) -> Option<String> {
    node.attrs
        .get("script")
        .or_else(|| node.attrs.get("tool_command"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::error::FailureCategory;
    use crate::outcome::{Outcome, OutcomeExt, StageStatus};
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use std::collections::HashMap;
    use std::time::Duration;

    // --- RetryPolicy preset tests ---

    #[test]
    fn retry_policy_none() {
        let policy = RetryPolicy::none();
        assert_eq!(policy.max_attempts, 1);
    }

    #[test]
    fn retry_policy_standard() {
        let policy = RetryPolicy::standard();
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(5_000));
    }

    #[test]
    fn retry_policy_aggressive() {
        let policy = RetryPolicy::aggressive();
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(500));
    }

    #[test]
    fn retry_policy_linear() {
        let policy = RetryPolicy::linear();
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.backoff.factor, 1.0);
    }

    #[test]
    fn retry_policy_patient() {
        let policy = RetryPolicy::patient();
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(2000));
    }

    // --- build_retry_policy tests ---

    #[test]
    fn build_retry_policy_from_node() {
        let mut node = Node::new("n");
        node.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(3));
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 4); // 3 retries + 1 initial
    }

    #[test]
    fn build_retry_policy_from_graph_default() {
        let node = Node::new("n");
        let mut graph = Graph::new("test");
        graph
            .attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(2));
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 3); // 2 retries + 1 initial
    }

    #[test]
    fn build_retry_policy_no_attrs_uses_graph_default_0() {
        let node = Node::new("n");
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 1); // default_max_retries=0 + 1
    }

    #[test]
    fn build_retry_policy_from_retry_policy_attr() {
        let mut node = Node::new("n");
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String("aggressive".to_string()),
        );
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(500));
    }

    #[test]
    fn build_retry_policy_fallback_when_no_retry_policy_attr() {
        let mut node = Node::new("n");
        node.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(3));
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 4); // 3 retries + 1 initial
                                            // Should use default backoff, not a preset's backoff
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(5_000));
    }

    #[test]
    fn build_retry_policy_all_presets() {
        let presets = [
            ("none", 1u32),
            ("standard", 5),
            ("aggressive", 5),
            ("linear", 3),
            ("patient", 3),
        ];
        let graph = Graph::new("test");
        let (name, expected) = presets[0];
        let mut node = Node::new("n");
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[1];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[2];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[3];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[4];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);
    }

    #[test]
    fn build_retry_policy_unknown_preset_falls_back() {
        let mut node = Node::new("n");
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String("unknown_preset".to_string()),
        );
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        // Unknown preset should fall back to graph default_max_retries=0
        assert_eq!(policy.max_attempts, 1);
    }

    // --- normalize_label tests ---

    #[test]
    fn normalize_label_lowercase_and_trim() {
        assert_eq!(normalize_label("  Yes  "), "yes");
    }

    #[test]
    fn normalize_label_strip_bracket_prefix() {
        assert_eq!(normalize_label("[A] Approve"), "approve");
        assert_eq!(normalize_label("[F] Fix"), "fix");
    }

    #[test]
    fn normalize_label_strip_paren_prefix() {
        assert_eq!(normalize_label("Y) Yes"), "yes");
    }

    #[test]
    fn normalize_label_strip_dash_prefix() {
        assert_eq!(normalize_label("Y - Yes"), "yes");
    }

    #[test]
    fn normalize_label_plain() {
        assert_eq!(normalize_label("next"), "next");
    }

    // --- best_by_weight_then_lexical tests ---

    #[test]
    fn best_by_weight_highest_wins() {
        let e1 = Edge::new("a", "x");
        let mut e2 = Edge::new("a", "y");
        e2.attrs.insert("weight".to_string(), AttrValue::Integer(5));
        let result = best_by_weight_then_lexical(&[&e1, &e2]).unwrap();
        assert_eq!(result.to, "y");
    }

    #[test]
    fn best_by_weight_lexical_tiebreak() {
        let e1 = Edge::new("a", "beta");
        let e2 = Edge::new("a", "alpha");
        let result = best_by_weight_then_lexical(&[&e1, &e2]).unwrap();
        assert_eq!(result.to, "alpha");
    }

    #[test]
    fn best_by_weight_empty_returns_none() {
        let result = best_by_weight_then_lexical(&[]);
        assert!(result.is_none());
    }

    // --- weighted_random tests ---

    #[test]
    fn weighted_random_empty_returns_none() {
        assert!(weighted_random(&[]).is_none());
    }

    #[test]
    fn weighted_random_single_edge() {
        let e = Edge::new("a", "b");
        let result = weighted_random(&[&e]).unwrap();
        assert_eq!(result.to, "b");
    }

    #[test]
    fn weighted_random_zero_weight_all_selected() {
        let e1 = Edge::new("a", "b");
        let e2 = Edge::new("a", "c");
        let edges = vec![&e1, &e2];
        let mut seen_b = false;
        let mut seen_c = false;
        for _ in 0..200 {
            let pick = weighted_random(&edges).unwrap();
            if pick.to == "b" {
                seen_b = true;
            }
            if pick.to == "c" {
                seen_c = true;
            }
        }
        assert!(seen_b, "expected target 'b' to be selected at least once");
        assert!(seen_c, "expected target 'c' to be selected at least once");
    }

    #[test]
    fn weighted_random_high_weight_dominates() {
        let mut heavy = Edge::new("a", "heavy");
        heavy
            .attrs
            .insert("weight".to_string(), AttrValue::Integer(100));
        let mut light = Edge::new("a", "light");
        light
            .attrs
            .insert("weight".to_string(), AttrValue::Integer(1));
        let edges = vec![&heavy, &light];
        let mut heavy_count = 0;
        for _ in 0..500 {
            let pick = weighted_random(&edges).unwrap();
            if pick.to == "heavy" {
                heavy_count += 1;
            }
        }
        let ratio = heavy_count as f64 / 500.0;
        assert!(
            ratio > 0.90,
            "expected heavy edge to win >90% of the time, got {ratio:.2}"
        );
    }

    // --- select_edge tests ---

    fn make_graph_with_edges(edges: Vec<Edge>) -> Graph {
        let mut g = Graph::new("test");
        for edge in &edges {
            if !g.nodes.contains_key(&edge.from) {
                g.nodes.insert(edge.from.clone(), Node::new(&edge.from));
            }
            if !g.nodes.contains_key(&edge.to) {
                g.nodes.insert(edge.to.clone(), Node::new(&edge.to));
            }
        }
        g.edges = edges;
        g
    }

    #[test]
    fn select_edge_no_edges() {
        let g = Graph::new("test");
        let node = Node::new("a");
        let outcome = Outcome::success();
        let context = Context::new();
        assert!(select_edge(&node, &outcome, &context, &g, "deterministic").is_none());
    }

    #[test]
    fn select_edge_single_unconditional() {
        let g = make_graph_with_edges(vec![Edge::new("a", "b")]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "b");
        assert_eq!(sel.reason, "unconditional");
    }

    #[test]
    fn select_edge_condition_match() {
        let mut e1 = Edge::new("a", "fail_path");
        e1.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        let mut e2 = Edge::new("a", "success_path");
        e2.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "success_path");
        assert_eq!(sel.reason, "condition");
    }

    #[test]
    fn select_edge_preferred_label() {
        let mut e1 = Edge::new("a", "approve");
        e1.attrs.insert(
            "label".to_string(),
            AttrValue::String("[A] Approve".to_string()),
        );
        let mut e2 = Edge::new("a", "fix");
        e2.attrs.insert(
            "label".to_string(),
            AttrValue::String("[F] Fix".to_string()),
        );
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let mut outcome = Outcome::success();
        outcome.preferred_label = Some("Fix".to_string());
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "fix");
        assert_eq!(sel.reason, "preferred_label");
    }

    #[test]
    fn select_edge_suggested_next_ids() {
        let e1 = Edge::new("a", "path1");
        let e2 = Edge::new("a", "path2");
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let mut outcome = Outcome::success();
        outcome.suggested_next_ids = vec!["path2".to_string()];
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "path2");
        assert_eq!(sel.reason, "suggested_next");
    }

    #[test]
    fn select_edge_weight_tiebreak() {
        let mut e1 = Edge::new("a", "low");
        e1.attrs.insert("weight".to_string(), AttrValue::Integer(1));
        let mut e2 = Edge::new("a", "high");
        e2.attrs
            .insert("weight".to_string(), AttrValue::Integer(10));
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "high");
        assert_eq!(sel.reason, "unconditional");
    }

    #[test]
    fn select_edge_lexical_tiebreak() {
        let e1 = Edge::new("a", "charlie");
        let e2 = Edge::new("a", "alpha");
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "alpha");
        assert_eq!(sel.reason, "unconditional");
    }

    #[test]
    fn select_edge_condition_beats_unconditional() {
        let mut e_cond = Edge::new("a", "cond_path");
        e_cond.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        let e_uncond = Edge::new("a", "uncond_path");
        let g = make_graph_with_edges(vec![e_cond, e_uncond]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "cond_path");
        assert_eq!(sel.reason, "condition");
    }

    #[test]
    fn select_edge_random_returns_some_edge() {
        let e1 = Edge::new("a", "b");
        let e2 = Edge::new("a", "c");
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "random").unwrap();
        assert!(sel.edge.to == "b" || sel.edge.to == "c");
        assert_eq!(sel.reason, "unconditional");
    }

    #[test]
    fn select_edge_random_preferred_label_still_wins() {
        let mut e1 = Edge::new("a", "approve");
        e1.attrs.insert(
            "label".to_string(),
            AttrValue::String("Approve".to_string()),
        );
        let e2 = Edge::new("a", "other");
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let mut outcome = Outcome::success();
        outcome.preferred_label = Some("Approve".to_string());
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "random").unwrap();
        assert_eq!(sel.edge.to, "approve");
        assert_eq!(sel.reason, "preferred_label");
    }

    #[test]
    fn select_edge_failed_human_gate_does_not_fall_through_to_unconditional() {
        let g = make_graph_with_edges(vec![
            Edge::new("gate", "approve"),
            Edge::new("gate", "skip"),
        ]);
        let mut node = g.nodes.get("gate").unwrap().clone();
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        let outcome =
            Outcome::fail_deterministic("human interaction aborted before an answer was provided");
        let context = Context::new();

        assert!(select_edge(&node, &outcome, &context, &g, "deterministic").is_none());
    }

    #[test]
    fn select_edge_failed_human_gate_routes_via_fail_condition() {
        let mut fail = Edge::new("gate", "retry");
        fail.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        let approve = Edge::new("gate", "approve");
        let g = make_graph_with_edges(vec![fail, approve]);
        let mut node = g.nodes.get("gate").unwrap().clone();
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        let outcome =
            Outcome::fail_deterministic("human interaction aborted before an answer was provided");
        let context = Context::new();

        let sel = select_edge(&node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "retry");
        assert_eq!(sel.reason, "condition");
    }

    #[test]
    fn select_edge_deterministic_no_fallback_when_no_condition_matches() {
        let mut e1 = Edge::new("a", "path1");
        e1.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        let mut e2 = Edge::new("a", "path2");
        e2.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=error".to_string()),
        );
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        assert!(select_edge(node, &outcome, &context, &g, "deterministic").is_none());
    }

    #[test]
    fn select_edge_random_no_fallback_when_no_condition_matches() {
        let mut e1 = Edge::new("a", "path1");
        e1.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        let mut e2 = Edge::new("a", "path2");
        e2.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=error".to_string()),
        );
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        assert!(select_edge(node, &outcome, &context, &g, "random").is_none());
    }

    // --- check_goal_gates tests ---

    #[test]
    fn goal_gates_all_satisfied() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), n);

        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), Outcome::success());

        assert!(check_goal_gates(&g, &outcomes).is_ok());
    }

    #[test]
    fn goal_gates_partial_success_counts() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), n);

        let mut outcomes = HashMap::new();
        let mut o = Outcome::success();
        o.status = StageStatus::PartialSuccess;
        outcomes.insert("work".to_string(), o);

        assert!(check_goal_gates(&g, &outcomes).is_ok());
    }

    #[test]
    fn goal_gates_failed_returns_node_id() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), n);

        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), Outcome::fail_classify("test"));

        assert_eq!(check_goal_gates(&g, &outcomes), Err("work".to_string()));
    }

    #[test]
    fn goal_gates_non_gate_nodes_ignored() {
        let mut g = Graph::new("test");
        g.nodes.insert("work".to_string(), Node::new("work"));

        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), Outcome::fail_classify("test"));

        assert!(check_goal_gates(&g, &outcomes).is_ok());
    }

    // --- get_retry_target tests ---

    #[test]
    fn retry_target_from_node() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("plan".to_string()),
        );
        g.nodes.insert("work".to_string(), n);
        g.nodes.insert("plan".to_string(), Node::new("plan"));

        assert_eq!(get_retry_target("work", &g), Some("plan".to_string()));
    }

    #[test]
    fn retry_target_from_fallback() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("plan".to_string()),
        );
        g.nodes.insert("work".to_string(), n);
        g.nodes.insert("plan".to_string(), Node::new("plan"));

        assert_eq!(get_retry_target("work", &g), Some("plan".to_string()));
    }

    #[test]
    fn retry_target_from_graph() {
        let mut g = Graph::new("test");
        g.nodes.insert("work".to_string(), Node::new("work"));
        g.nodes.insert("plan".to_string(), Node::new("plan"));
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("plan".to_string()),
        );

        assert_eq!(get_retry_target("work", &g), Some("plan".to_string()));
    }

    #[test]
    fn retry_target_none_when_missing() {
        let mut g = Graph::new("test");
        g.nodes.insert("work".to_string(), Node::new("work"));
        assert!(get_retry_target("work", &g).is_none());
    }

    #[test]
    fn retry_target_skips_nonexistent_node() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        g.nodes.insert("work".to_string(), n);
        // No "nonexistent" node -- should fall through to graph-level
        assert!(get_retry_target("work", &g).is_none());
    }

    // --- is_terminal tests ---

    #[test]
    fn terminal_by_shape() {
        let mut n = Node::new("exit");
        n.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        assert!(is_terminal(&n));
    }

    #[test]
    fn terminal_by_type() {
        let mut n = Node::new("end");
        n.attrs
            .insert("type".to_string(), AttrValue::String("exit".to_string()));
        assert!(is_terminal(&n));
    }

    #[test]
    fn non_terminal_node() {
        let n = Node::new("work");
        assert!(!is_terminal(&n));
    }

    // --- resolve_fidelity tests ---

    #[test]
    fn fidelity_defaults_to_compact() {
        use crate::context::keys::Fidelity;
        let node = Node::new("work");
        let graph = Graph::new("test");
        assert_eq!(resolve_fidelity(None, &node, &graph), Fidelity::Compact);
    }

    #[test]
    fn fidelity_from_graph_default() {
        use crate::context::keys::Fidelity;
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
        use crate::context::keys::Fidelity;
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
        use crate::context::keys::Fidelity;
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

    // --- resolve_thread_id tests ---

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
        // When the edge has no thread_id, the node's thread_id is used.
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
        // Edge thread_id should take precedence over node thread_id,
        // matching the fidelity precedence where edge > node.
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

    // --- classify_outcome tests ---

    #[test]
    fn classify_outcome_returns_none_for_success() {
        assert!(classify_outcome(&Outcome::success()).is_none());
    }

    #[test]
    fn classify_outcome_returns_none_for_skipped() {
        assert!(classify_outcome(&Outcome::skipped("")).is_none());
    }

    #[test]
    fn classify_outcome_returns_none_for_partial_success() {
        let outcome = Outcome {
            status: StageStatus::PartialSuccess,
            ..Outcome::success()
        };
        assert!(classify_outcome(&outcome).is_none());
    }

    #[test]
    fn classify_outcome_reads_failure_detail() {
        let mut outcome = Outcome::fail_classify("some error");
        // Override the FailureDetail's class directly
        outcome.failure.as_mut().unwrap().category = FailureCategory::BudgetExhausted;
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::BudgetExhausted)
        );
    }

    #[test]
    fn classify_outcome_uses_failure_reason_heuristics() {
        let outcome = Outcome::fail_classify("rate limited by provider");
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::TransientInfra)
        );
    }

    #[test]
    fn classify_outcome_defaults_to_deterministic() {
        let outcome = Outcome::fail_classify("something went wrong");
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::Deterministic)
        );
    }

    #[test]
    fn classify_outcome_fail_no_reason_is_deterministic() {
        let outcome = Outcome {
            status: StageStatus::Fail,
            failure: None,
            ..Outcome::success()
        };
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::Deterministic)
        );
    }

    #[test]
    fn classify_outcome_retry_status_uses_heuristics() {
        let outcome = Outcome::retry_classify("connection refused");
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::TransientInfra)
        );
    }
}
