use std::collections::{HashSet, VecDeque};

use crate::graph::{AttrValue, Graph};

use super::{Diagnostic, LintRule, Severity};

/// Returns all 15 built-in lint rules.
#[must_use]
pub fn built_in_rules() -> Vec<Box<dyn LintRule>> {
    vec![
        Box::new(StartNodeRule),
        Box::new(TerminalNodeRule),
        Box::new(ReachabilityRule),
        Box::new(EdgeTargetExistsRule),
        Box::new(StartNoIncomingRule),
        Box::new(ExitNoOutgoingRule),
        Box::new(ConditionSyntaxRule),
        Box::new(StylesheetSyntaxRule),
        Box::new(TypeKnownRule),
        Box::new(FidelityValidRule),
        Box::new(RetryTargetExistsRule),
        Box::new(GoalGateHasRetryRule),
        Box::new(PromptOnLlmNodesRule),
        Box::new(FreeformEdgeCountRule),
        Box::new(DirectionValidRule),
    ]
}

// --- Rule 1: start_node (ERROR) ---

struct StartNodeRule;

impl LintRule for StartNodeRule {
    fn name(&self) -> &'static str {
        "start_node"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let start_count = graph
            .nodes
            .iter()
            .filter(|(id, n)| {
                n.shape() == "Mdiamond" || *id == "start" || *id == "Start"
            })
            .count();
        if start_count == 0 {
            return vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message: "Pipeline must have exactly one start node (shape=Mdiamond or id start/Start)".to_string(),
                node_id: None,
                edge: None,
                fix: Some("Add a node with shape=Mdiamond or id 'start'".to_string()),
            }];
        }
        if start_count > 1 {
            return vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message: format!(
                    "Pipeline has {start_count} start nodes but must have exactly one"
                ),
                node_id: None,
                edge: None,
                fix: Some("Remove extra start nodes".to_string()),
            }];
        }
        Vec::new()
    }
}

// --- Rule 2: terminal_node (ERROR) ---

struct TerminalNodeRule;

impl LintRule for TerminalNodeRule {
    fn name(&self) -> &'static str {
        "terminal_node"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let terminal_count = graph
            .nodes
            .iter()
            .filter(|(id, n)| {
                n.shape() == "Msquare"
                    || *id == "exit"
                    || *id == "Exit"
                    || *id == "end"
                    || *id == "End"
            })
            .count();
        if terminal_count == 0 {
            return vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message: "Pipeline must have at least one terminal node (shape=Msquare or id exit/end)".to_string(),
                node_id: None,
                edge: None,
                fix: Some("Add a node with shape=Msquare or id 'exit'/'end'".to_string()),
            }];
        }
        Vec::new()
    }
}

// --- Rule 3: reachability (ERROR) ---

struct ReachabilityRule;

impl LintRule for ReachabilityRule {
    fn name(&self) -> &'static str {
        "reachability"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let Some(start) = graph.find_start_node() else {
            return Vec::new();
        };

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(start.id.clone());
        visited.insert(start.id.clone());

        while let Some(node_id) = queue.pop_front() {
            for edge in graph.outgoing_edges(&node_id) {
                if visited.insert(edge.to.clone()) {
                    queue.push_back(edge.to.clone());
                }
            }
        }

        let mut unreachable: Vec<&str> = graph
            .nodes
            .keys()
            .filter(|id| !visited.contains(id.as_str()))
            .map(std::string::String::as_str)
            .collect();
        unreachable.sort_unstable();

        unreachable
            .into_iter()
            .map(|node_id| Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Warning,
                message: format!("Node '{node_id}' is not reachable from the start node"),
                node_id: Some(node_id.to_string()),
                edge: None,
                fix: Some(format!(
                    "Add an edge path from the start node to '{node_id}'"
                )),
            })
            .collect()
    }
}

// --- Rule 4: edge_target_exists (ERROR) ---

struct EdgeTargetExistsRule;

impl LintRule for EdgeTargetExistsRule {
    fn name(&self) -> &'static str {
        "edge_target_exists"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for edge in &graph.edges {
            if !graph.nodes.contains_key(&edge.to) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: format!(
                        "Edge from '{}' targets non-existent node '{}'",
                        edge.from, edge.to
                    ),
                    node_id: None,
                    edge: Some((edge.from.clone(), edge.to.clone())),
                    fix: Some(format!("Define node '{}' or fix the edge target", edge.to)),
                });
            }
            if !graph.nodes.contains_key(&edge.from) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: format!(
                        "Edge source '{}' references non-existent node",
                        edge.from
                    ),
                    node_id: None,
                    edge: Some((edge.from.clone(), edge.to.clone())),
                    fix: Some(format!(
                        "Define node '{}' or fix the edge source",
                        edge.from
                    )),
                });
            }
        }
        diagnostics
    }
}

// --- Rule 5: start_no_incoming (ERROR) ---

struct StartNoIncomingRule;

impl LintRule for StartNoIncomingRule {
    fn name(&self) -> &'static str {
        "start_no_incoming"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let Some(start) = graph.find_start_node() else {
            return Vec::new();
        };
        let incoming = graph.incoming_edges(&start.id);
        if !incoming.is_empty() {
            return vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message: format!(
                    "Start node '{}' has {} incoming edge(s) but must have none",
                    start.id,
                    incoming.len()
                ),
                node_id: Some(start.id.clone()),
                edge: None,
                fix: Some("Remove incoming edges to the start node".to_string()),
            }];
        }
        Vec::new()
    }
}

// --- Rule 6: exit_no_outgoing (ERROR) ---

struct ExitNoOutgoingRule;

impl LintRule for ExitNoOutgoingRule {
    fn name(&self) -> &'static str {
        "exit_no_outgoing"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for (id, node) in &graph.nodes {
            let is_terminal = node.shape() == "Msquare"
                || *id == "exit"
                || *id == "Exit"
                || *id == "end"
                || *id == "End";
            if is_terminal {
                let outgoing = graph.outgoing_edges(&node.id);
                if !outgoing.is_empty() {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Error,
                        message: format!(
                            "Exit node '{}' has {} outgoing edge(s) but must have none",
                            node.id,
                            outgoing.len()
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some("Remove outgoing edges from the exit node".to_string()),
                    });
                }
            }
        }
        diagnostics
    }
}

// --- Rule 7: condition_syntax (ERROR) ---

struct ConditionSyntaxRule;

impl LintRule for ConditionSyntaxRule {
    fn name(&self) -> &'static str {
        "condition_syntax"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for edge in &graph.edges {
            let Some(condition) = edge.condition() else {
                continue;
            };
            if condition.is_empty() {
                continue;
            }
            for clause in condition.split("&&") {
                let clause = clause.trim();
                if clause.is_empty() {
                    continue;
                }
                // A clause must contain = or != operator, or be a bare key (truthy check)
                let has_operator = clause.contains("!=") || clause.contains('=');
                if !has_operator && clause.contains(' ') && !clause.starts_with("context.") {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Error,
                        message: format!(
                            "Invalid condition clause '{clause}' on edge {} -> {}",
                            edge.from, edge.to
                        ),
                        node_id: None,
                        edge: Some((edge.from.clone(), edge.to.clone())),
                        fix: Some("Use key=value or key!=value syntax".to_string()),
                    });
                }
            }
        }
        diagnostics
    }
}

// --- Rule 8: stylesheet_syntax (ERROR) ---

struct StylesheetSyntaxRule;

impl LintRule for StylesheetSyntaxRule {
    fn name(&self) -> &'static str {
        "stylesheet_syntax"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let stylesheet = graph.model_stylesheet();
        if stylesheet.is_empty() {
            return Vec::new();
        }
        match crate::stylesheet::parse_stylesheet(stylesheet) {
            Ok(_) => Vec::new(),
            Err(e) => vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message: format!("Model stylesheet parse error: {e}"),
                node_id: None,
                edge: None,
                fix: Some("Fix the model_stylesheet syntax".to_string()),
            }],
        }
    }
}

// --- Rule 9: type_known (WARNING) ---

struct TypeKnownRule;

const KNOWN_HANDLER_TYPES: &[&str] = &[
    "start",
    "exit",
    "codergen",
    "wait.human",
    "conditional",
    "parallel",
    "parallel.fan_in",
    "tool",
    "stack.manager_loop",
];

impl LintRule for TypeKnownRule {
    fn name(&self) -> &'static str {
        "type_known"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if let Some(node_type) = node.node_type() {
                if !KNOWN_HANDLER_TYPES.contains(&node_type) {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Node '{}' has unrecognized type '{node_type}'",
                            node.id
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(format!("Use one of: {}", KNOWN_HANDLER_TYPES.join(", "))),
                    });
                }
            }
        }
        diagnostics
    }
}

// --- Rule 10: fidelity_valid (WARNING) ---

struct FidelityValidRule;

const VALID_FIDELITY_MODES: &[&str] = &[
    "full",
    "truncate",
    "compact",
    "summary:low",
    "summary:medium",
    "summary:high",
];

impl LintRule for FidelityValidRule {
    fn name(&self) -> &'static str {
        "fidelity_valid"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if let Some(fidelity) = node.fidelity() {
                if !VALID_FIDELITY_MODES.contains(&fidelity) {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Node '{}' has invalid fidelity mode '{fidelity}'",
                            node.id
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(format!(
                            "Use one of: {}",
                            VALID_FIDELITY_MODES.join(", ")
                        )),
                    });
                }
            }
        }
        for edge in &graph.edges {
            if let Some(fidelity) = edge.fidelity() {
                if !VALID_FIDELITY_MODES.contains(&fidelity) {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Edge {} -> {} has invalid fidelity mode '{fidelity}'",
                            edge.from, edge.to
                        ),
                        node_id: None,
                        edge: Some((edge.from.clone(), edge.to.clone())),
                        fix: Some(format!(
                            "Use one of: {}",
                            VALID_FIDELITY_MODES.join(", ")
                        )),
                    });
                }
            }
        }
        if let Some(fidelity) = graph.default_fidelity() {
            if !VALID_FIDELITY_MODES.contains(&fidelity) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!("Graph has invalid default_fidelity '{fidelity}'"),
                    node_id: None,
                    edge: None,
                    fix: Some(format!(
                        "Use one of: {}",
                        VALID_FIDELITY_MODES.join(", ")
                    )),
                });
            }
        }
        diagnostics
    }
}

// --- Rule 11: retry_target_exists (WARNING) ---

struct RetryTargetExistsRule;

impl LintRule for RetryTargetExistsRule {
    fn name(&self) -> &'static str {
        "retry_target_exists"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if let Some(target) = node.retry_target() {
                if !graph.nodes.contains_key(target) {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Node '{}' has retry_target '{}' that does not exist",
                            node.id, target
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(format!("Define node '{target}' or fix retry_target")),
                    });
                }
            }
            if let Some(target) = node.fallback_retry_target() {
                if !graph.nodes.contains_key(target) {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Node '{}' has fallback_retry_target '{}' that does not exist",
                            node.id, target
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(format!(
                            "Define node '{target}' or fix fallback_retry_target"
                        )),
                    });
                }
            }
        }
        if let Some(target) = graph.retry_target() {
            if !graph.nodes.contains_key(target) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!("Graph has retry_target '{target}' that does not exist"),
                    node_id: None,
                    edge: None,
                    fix: Some(format!("Define node '{target}' or fix graph retry_target")),
                });
            }
        }
        if let Some(target) = graph.fallback_retry_target() {
            if !graph.nodes.contains_key(target) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "Graph has fallback_retry_target '{target}' that does not exist"
                    ),
                    node_id: None,
                    edge: None,
                    fix: Some(format!(
                        "Define node '{target}' or fix graph fallback_retry_target"
                    )),
                });
            }
        }
        diagnostics
    }
}

// --- Rule 12: goal_gate_has_retry (WARNING) ---

struct GoalGateHasRetryRule;

impl LintRule for GoalGateHasRetryRule {
    fn name(&self) -> &'static str {
        "goal_gate_has_retry"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if node.goal_gate() {
                let has_node_retry =
                    node.retry_target().is_some() || node.fallback_retry_target().is_some();
                let has_graph_retry =
                    graph.retry_target().is_some() || graph.fallback_retry_target().is_some();
                if !has_node_retry && !has_graph_retry {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Node '{}' has goal_gate=true but no retry_target or fallback_retry_target",
                            node.id
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(
                            "Add retry_target or fallback_retry_target attribute".to_string(),
                        ),
                    });
                }
            }
        }
        diagnostics
    }
}

// --- Rule 13: prompt_on_llm_nodes (WARNING) ---

struct PromptOnLlmNodesRule;

impl LintRule for PromptOnLlmNodesRule {
    fn name(&self) -> &'static str {
        "prompt_on_llm_nodes"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if node.handler_type() == Some("codergen") {
                let has_prompt = node.prompt().is_some_and(|p| !p.is_empty());
                let has_label = node
                    .attrs
                    .get("label")
                    .and_then(AttrValue::as_str)
                    .is_some_and(|l| !l.is_empty());
                if !has_prompt && !has_label {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Codergen node '{}' has no prompt or label attribute",
                            node.id
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some("Add a prompt or label attribute".to_string()),
                    });
                }
            }
        }
        diagnostics
    }
}

// --- Rule 14: freeform_edge_count (ERROR) ---

struct FreeformEdgeCountRule;

impl LintRule for FreeformEdgeCountRule {
    fn name(&self) -> &'static str {
        "freeform_edge_count"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if node.handler_type() == Some("wait.human") {
                let freeform_count = graph
                    .outgoing_edges(&node.id)
                    .iter()
                    .filter(|e| e.freeform())
                    .count();
                if freeform_count > 1 {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Error,
                        message: format!(
                            "wait.human node '{}' has {freeform_count} freeform edges but at most one is allowed",
                            node.id
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(
                            "Remove extra freeform=true edges so at most one remains".to_string(),
                        ),
                    });
                }
            }
        }
        diagnostics
    }
}

// --- Rule 15: direction_valid (WARNING) ---

struct DirectionValidRule;

const VALID_DIRECTIONS: &[&str] = &["TB", "LR", "BT", "RL"];

impl LintRule for DirectionValidRule {
    fn name(&self) -> &'static str {
        "direction_valid"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let Some(rankdir) = graph.attrs.get("rankdir").and_then(AttrValue::as_str) else {
            return Vec::new();
        };
        if VALID_DIRECTIONS.contains(&rankdir) {
            return Vec::new();
        }
        vec![Diagnostic {
            rule: self.name().to_string(),
            severity: Severity::Warning,
            message: format!(
                "Graph has invalid rankdir '{rankdir}'"
            ),
            node_id: None,
            edge: None,
            fix: Some(format!("Use one of: {}", VALID_DIRECTIONS.join(", "))),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{AttrValue, Edge, Node};

    fn minimal_graph() -> Graph {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start
            .attrs
            .insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        g.nodes.insert("start".to_string(), start);

        let mut exit = Node::new("exit");
        exit.attrs
            .insert("shape".to_string(), AttrValue::String("Msquare".to_string()));
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "exit"));
        g
    }

    // start_node rule tests

    #[test]
    fn start_node_rule_no_start() {
        let g = Graph::new("test");
        let rule = StartNodeRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn start_node_rule_two_starts() {
        let mut g = Graph::new("test");
        let mut s1 = Node::new("s1");
        s1.attrs
            .insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        let mut s2 = Node::new("s2");
        s2.attrs
            .insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        g.nodes.insert("s1".to_string(), s1);
        g.nodes.insert("s2".to_string(), s2);
        let rule = StartNodeRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn start_node_rule_one_start() {
        let g = minimal_graph();
        let rule = StartNodeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn start_node_rule_by_id() {
        let mut g = Graph::new("test");
        // Node with id "start" but no Mdiamond shape
        let node = Node::new("start");
        g.nodes.insert("start".to_string(), node);
        let rule = StartNodeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn start_node_rule_by_capitalized_id() {
        let mut g = Graph::new("test");
        let node = Node::new("Start");
        g.nodes.insert("Start".to_string(), node);
        let rule = StartNodeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // terminal_node rule tests

    #[test]
    fn terminal_node_rule_no_terminal() {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start
            .attrs
            .insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        g.nodes.insert("start".to_string(), start);
        let rule = TerminalNodeRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn terminal_node_rule_with_terminal() {
        let g = minimal_graph();
        let rule = TerminalNodeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn terminal_node_rule_by_exit_id() {
        let mut g = Graph::new("test");
        // Node with id "exit" but no Msquare shape
        let node = Node::new("exit");
        g.nodes.insert("exit".to_string(), node);
        let rule = TerminalNodeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn terminal_node_rule_by_end_id() {
        let mut g = Graph::new("test");
        let node = Node::new("end");
        g.nodes.insert("end".to_string(), node);
        let rule = TerminalNodeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn terminal_node_rule_by_capitalized_end_id() {
        let mut g = Graph::new("test");
        let node = Node::new("End");
        g.nodes.insert("End".to_string(), node);
        let rule = TerminalNodeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // reachability rule tests

    #[test]
    fn reachability_rule_unreachable_node() {
        let mut g = minimal_graph();
        g.nodes.insert("orphan".to_string(), Node::new("orphan"));
        let rule = ReachabilityRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].node_id, Some("orphan".to_string()));
    }

    #[test]
    fn reachability_rule_all_reachable() {
        let g = minimal_graph();
        let rule = ReachabilityRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // edge_target_exists rule tests

    #[test]
    fn edge_target_exists_rule_missing_target() {
        let mut g = minimal_graph();
        g.edges.push(Edge::new("start", "nonexistent"));
        let rule = EdgeTargetExistsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn edge_target_exists_rule_valid() {
        let g = minimal_graph();
        let rule = EdgeTargetExistsRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // start_no_incoming rule tests

    #[test]
    fn start_no_incoming_rule_with_incoming() {
        let mut g = minimal_graph();
        g.edges.push(Edge::new("exit", "start"));
        let rule = StartNoIncomingRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn start_no_incoming_rule_clean() {
        let g = minimal_graph();
        let rule = StartNoIncomingRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // exit_no_outgoing rule tests

    #[test]
    fn exit_no_outgoing_rule_with_outgoing() {
        let mut g = minimal_graph();
        g.edges.push(Edge::new("exit", "start"));
        let rule = ExitNoOutgoingRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn exit_no_outgoing_rule_clean() {
        let g = minimal_graph();
        let rule = ExitNoOutgoingRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // type_known rule tests

    #[test]
    fn type_known_rule_unknown_type() {
        let mut g = minimal_graph();
        let mut node = Node::new("custom");
        node.attrs.insert(
            "type".to_string(),
            AttrValue::String("unknown_type".to_string()),
        );
        g.nodes.insert("custom".to_string(), node);
        let rule = TypeKnownRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn type_known_rule_known_type() {
        let mut g = minimal_graph();
        let mut node = Node::new("gate");
        node.attrs.insert(
            "type".to_string(),
            AttrValue::String("wait.human".to_string()),
        );
        g.nodes.insert("gate".to_string(), node);
        let rule = TypeKnownRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // fidelity_valid rule tests

    #[test]
    fn fidelity_valid_rule_invalid_mode() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("invalid_mode".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = FidelityValidRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn fidelity_valid_rule_valid_mode() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = FidelityValidRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // freeform_edge_count rule tests

    #[test]
    fn freeform_edge_count_rule_two_freeform() {
        let mut g = minimal_graph();
        let mut gate = Node::new("gate");
        gate.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        g.nodes.insert("gate".to_string(), gate);
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.nodes.insert("b".to_string(), Node::new("b"));

        let mut e1 = Edge::new("gate", "a");
        e1.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        let mut e2 = Edge::new("gate", "b");
        e2.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        g.edges.push(e1);
        g.edges.push(e2);

        let rule = FreeformEdgeCountRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn freeform_edge_count_rule_one_freeform() {
        let mut g = minimal_graph();
        let mut gate = Node::new("gate");
        gate.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        g.nodes.insert("gate".to_string(), gate);
        g.nodes.insert("a".to_string(), Node::new("a"));

        let mut e1 = Edge::new("gate", "a");
        e1.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        g.edges.push(e1);

        let rule = FreeformEdgeCountRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // goal_gate_has_retry rule tests

    #[test]
    fn goal_gate_has_retry_rule_no_retry() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), node);
        let rule = GoalGateHasRetryRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn goal_gate_has_retry_rule_with_retry() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        node.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = GoalGateHasRetryRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // prompt_on_llm_nodes rule tests

    #[test]
    fn prompt_on_llm_nodes_rule_no_prompt_no_label() {
        let mut g = minimal_graph();
        let node = Node::new("work");
        g.nodes.insert("work".to_string(), node);
        let rule = PromptOnLlmNodesRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn prompt_on_llm_nodes_rule_with_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the thing".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = PromptOnLlmNodesRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // condition_syntax rule tests

    #[test]
    fn condition_syntax_rule_valid_condition() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // stylesheet_syntax rule tests

    #[test]
    fn stylesheet_syntax_rule_unbalanced() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { llm_model: foo;".to_string()),
        );
        let rule = StylesheetSyntaxRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn stylesheet_syntax_rule_balanced() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { llm_model: foo; }".to_string()),
        );
        let rule = StylesheetSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // retry_target_exists rule tests

    #[test]
    fn retry_target_exists_rule_missing() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn retry_target_exists_rule_valid() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // built_in_rules tests

    #[test]
    fn built_in_rules_returns_15_rules() {
        let rules = built_in_rules();
        assert_eq!(rules.len(), 15);
    }

    // direction_valid rule tests

    #[test]
    fn direction_valid_rule_no_rankdir() {
        let g = minimal_graph();
        let rule = DirectionValidRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn direction_valid_rule_valid_directions() {
        let rule = DirectionValidRule;
        let mut g = minimal_graph();
        g.attrs.insert(
            "rankdir".to_string(),
            AttrValue::String("LR".to_string()),
        );
        assert!(rule.apply(&g).is_empty());

        g.attrs.insert(
            "rankdir".to_string(),
            AttrValue::String("TB".to_string()),
        );
        assert!(rule.apply(&g).is_empty());

        g.attrs.insert(
            "rankdir".to_string(),
            AttrValue::String("BT".to_string()),
        );
        assert!(rule.apply(&g).is_empty());

        g.attrs.insert(
            "rankdir".to_string(),
            AttrValue::String("RL".to_string()),
        );
        assert!(rule.apply(&g).is_empty());
    }

    #[test]
    fn direction_valid_rule_invalid_direction() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "rankdir".to_string(),
            AttrValue::String("XY".to_string()),
        );
        let rule = DirectionValidRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    // stylesheet_syntax with full parse tests

    #[test]
    fn stylesheet_syntax_rule_malformed_selector() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { garbage garbage }".to_string()),
        );
        let rule = StylesheetSyntaxRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }
}