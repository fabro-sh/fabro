use std::collections::HashMap;

use fabro_graphviz::graph::types::{Edge as GvEdge, Graph as GvGraph, Node as GvNode};
use rand::Rng;

use crate::condition::evaluate_condition;
use crate::context::Context;
use crate::outcome::{Outcome, StageStatus};

/// Result of edge selection: the chosen edge and the reason it was selected.
pub(crate) struct SelectedGraphEdge<'a> {
    pub(crate) edge: &'a GvEdge,
    pub(crate) reason: &'static str,
}

/// Check whether a node is a terminal (exit) node.
pub(crate) fn is_terminal(node: &GvNode) -> bool {
    node.shape() == "Msquare" || node.handler_type() == Some("exit")
}

/// Select the next edge from a node's outgoing edges (spec Section 3.3).
pub(crate) fn select_edge<'a>(
    node: &GvNode,
    outcome: &Outcome,
    context: &Context,
    graph: &'a GvGraph,
    selection: &str,
) -> Option<SelectedGraphEdge<'a>> {
    let node_id = &node.id;
    let edges = graph.outgoing_edges(node_id);
    if edges.is_empty() {
        return None;
    }

    let condition_matched: Vec<&GvEdge> = edges
        .iter()
        .filter(|e| {
            e.condition()
                .is_some_and(|c| !c.is_empty() && evaluate_condition(c, outcome, context))
        })
        .copied()
        .collect();
    if !condition_matched.is_empty() {
        return pick_edge(&condition_matched, selection).map(|edge| SelectedGraphEdge {
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
                        return Some(SelectedGraphEdge {
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
                return Some(SelectedGraphEdge {
                    edge,
                    reason: "suggested_next",
                });
            }
        }
    }

    if blocks_unconditional_failure_fallthrough(node, outcome) {
        return None;
    }

    let unconditional: Vec<&GvEdge> = edges
        .iter()
        .filter(|e| e.condition().is_none_or(str::is_empty))
        .copied()
        .collect();
    if !unconditional.is_empty() {
        return pick_edge(&unconditional, selection).map(|edge| SelectedGraphEdge {
            edge,
            reason: "unconditional",
        });
    }

    None
}

/// Check if all goal gates have been satisfied.
/// Returns Ok(()) if all gates passed, or Err with the failed node ID.
pub(crate) fn check_goal_gates(
    graph: &GvGraph,
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
pub(crate) fn get_retry_target(failed_node_id: &str, graph: &GvGraph) -> Option<String> {
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

/// Normalize a label for comparison: lowercase, trim, strip accelerator prefixes.
/// Patterns: "[Y] ", "Y) ", "Y - "
fn normalize_label(label: &str) -> String {
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
fn best_by_weight_then_lexical<'a>(edges: &[&'a GvEdge]) -> Option<&'a GvEdge> {
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
fn weighted_random<'a>(edges: &[&'a GvEdge]) -> Option<&'a GvEdge> {
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
fn pick_edge<'a>(edges: &[&'a GvEdge], selection: &str) -> Option<&'a GvEdge> {
    match selection {
        "random" => weighted_random(edges),
        _ => best_by_weight_then_lexical(edges),
    }
}

fn blocks_unconditional_failure_fallthrough(node: &GvNode, outcome: &Outcome) -> bool {
    node.handler_type() == Some("human")
        && outcome.status == StageStatus::Fail
        && outcome.preferred_label.is_none()
        && outcome.suggested_next_ids.is_empty()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};

    use super::*;
    use crate::context::Context;
    use crate::outcome::{Outcome, OutcomeExt, StageStatus};

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
        assert!(get_retry_target("work", &g).is_none());
    }

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
}
