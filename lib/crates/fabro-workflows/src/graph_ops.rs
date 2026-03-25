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
