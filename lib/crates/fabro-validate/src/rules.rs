use std::collections::{HashSet, VecDeque};
use std::str::FromStr;

use fabro_graphviz::condition::parse_condition;
use fabro_graphviz::graph::{AttrValue, Graph, is_llm_handler_type};
use fabro_graphviz::stylesheet::{Selector, parse_stylesheet};

use crate::{Diagnostic, LintRule, Severity};

/// Returns all built-in lint rules.
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
        Box::new(ReservedKeywordNodeIdRule),
        Box::new(AllConditionalEdgesRule),
        Box::new(OrphanCustomOutcomeRule),
        Box::new(ScriptAbsoluteCdRule),
        Box::new(StylesheetModelKnownRule),
        Box::new(NodeModelKnownRule),
        Box::new(ImportErrorRule),
        Box::new(UnresolvedFileRefRule),
        Box::new(ThreadIdRequiresFidelityFullRule),
        Box::new(SelectionValidRule),
        Box::new(RandomSelectionNoConditionsRule),
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
            .filter(|(id, n)| n.shape() == "Mdiamond" || *id == "start" || *id == "Start")
            .count();
        if start_count == 0 {
            return vec![Diagnostic {
                rule:     self.name().to_string(),
                severity: Severity::Error,
                message:
                    "Pipeline must have exactly one start node (shape=Mdiamond or id start/Start)"
                        .to_string(),
                node_id:  None,
                edge:     None,
                fix:      Some("Add a node with shape=Mdiamond or id 'start'".to_string()),
            }];
        }
        if start_count > 1 {
            return vec![Diagnostic {
                rule:     self.name().to_string(),
                severity: Severity::Error,
                message:  format!(
                    "Pipeline has {start_count} start nodes but must have exactly one"
                ),
                node_id:  None,
                edge:     None,
                fix:      Some("Remove extra start nodes".to_string()),
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
                rule:     self.name().to_string(),
                severity: Severity::Error,
                message:
                    "Pipeline must have exactly one terminal node (shape=Msquare or id exit/end)"
                        .to_string(),
                node_id:  None,
                edge:     None,
                fix:      Some("Add a node with shape=Msquare or id 'exit'/'end'".to_string()),
            }];
        }
        if terminal_count > 1 {
            return vec![Diagnostic {
                rule:     self.name().to_string(),
                severity: Severity::Error,
                message:  format!(
                    "Pipeline must have exactly one terminal node, found {terminal_count}"
                ),
                node_id:  None,
                edge:     None,
                fix:      Some("Remove extra terminal nodes so exactly one remains".to_string()),
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
                rule:     self.name().to_string(),
                severity: Severity::Warning,
                message:  format!("Node '{node_id}' is not reachable from the start node"),
                node_id:  Some(node_id.to_string()),
                edge:     None,
                fix:      Some(format!(
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
                    rule:     self.name().to_string(),
                    severity: Severity::Error,
                    message:  format!(
                        "Edge from '{}' targets non-existent node '{}'",
                        edge.from, edge.to
                    ),
                    node_id:  None,
                    edge:     Some((edge.from.clone(), edge.to.clone())),
                    fix:      Some(format!("Define node '{}' or fix the edge target", edge.to)),
                });
            }
            if !graph.nodes.contains_key(&edge.from) {
                diagnostics.push(Diagnostic {
                    rule:     self.name().to_string(),
                    severity: Severity::Error,
                    message:  format!("Edge source '{}' references non-existent node", edge.from),
                    node_id:  None,
                    edge:     Some((edge.from.clone(), edge.to.clone())),
                    fix:      Some(format!(
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
                rule:     self.name().to_string(),
                severity: Severity::Error,
                message:  format!(
                    "Start node '{}' has {} incoming edge(s) but must have none",
                    start.id,
                    incoming.len()
                ),
                node_id:  Some(start.id.clone()),
                edge:     None,
                fix:      Some("Remove incoming edges to the start node".to_string()),
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
                        rule:     self.name().to_string(),
                        severity: Severity::Error,
                        message:  format!(
                            "Exit node '{}' has {} outgoing edge(s) but must have none",
                            node.id,
                            outgoing.len()
                        ),
                        node_id:  Some(node.id.clone()),
                        edge:     None,
                        fix:      Some("Remove outgoing edges from the exit node".to_string()),
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
            if let Err(e) = parse_condition(condition) {
                diagnostics.push(Diagnostic {
                    rule:     self.name().to_string(),
                    severity: Severity::Error,
                    message:  format!(
                        "Condition '{condition}' on edge {} -> {} failed parse: {e}",
                        edge.from, edge.to
                    ),
                    node_id:  None,
                    edge:     Some((edge.from.clone(), edge.to.clone())),
                    fix:      Some(
                        "Use key=value, key!=value, key>value, key contains value, \
                         key matches pattern, or bare key syntax"
                            .to_string(),
                    ),
                });
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
        match parse_stylesheet(stylesheet) {
            Ok(_) => Vec::new(),
            Err(e) => vec![Diagnostic {
                rule:     self.name().to_string(),
                severity: Severity::Error,
                message:  format!("Model stylesheet parse error: {e}"),
                node_id:  None,
                edge:     None,
                fix:      Some("Fix the model_stylesheet syntax".to_string()),
            }],
        }
    }
}

// --- Rule 9: type_known (WARNING) ---

struct TypeKnownRule;

const KNOWN_HANDLER_TYPES: &[&str] = &[
    "start",
    "exit",
    "agent",
    "agent_loop", // legacy alias
    "prompt",
    "one_shot", // legacy alias
    "human",
    "conditional",
    "parallel",
    "parallel.fan_in",
    "command",
    "tool",
    "stack.manager_loop",
    "wait",
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
                        rule:     self.name().to_string(),
                        severity: Severity::Warning,
                        message:  format!("Node '{}' has unrecognized type '{node_type}'", node.id),
                        node_id:  Some(node.id.clone()),
                        edge:     None,
                        fix:      Some(format!("Use one of: {}", KNOWN_HANDLER_TYPES.join(", "))),
                    });
                }
            }
        }
        diagnostics
    }
}

// --- Rule 10: fidelity_valid (WARNING) ---

struct FidelityValidRule;

impl FidelityValidRule {
    fn fix_message() -> String {
        use fabro_graphviz::Fidelity;
        let modes: Vec<_> = [
            Fidelity::Full,
            Fidelity::Truncate,
            Fidelity::Compact,
            Fidelity::SummaryLow,
            Fidelity::SummaryMedium,
            Fidelity::SummaryHigh,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        format!("Use one of: {}", modes.join(", "))
    }
}

impl LintRule for FidelityValidRule {
    fn name(&self) -> &'static str {
        "fidelity_valid"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        use fabro_graphviz::Fidelity;

        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if let Some(fidelity) = node.fidelity() {
                if fidelity.parse::<Fidelity>().is_err() {
                    diagnostics.push(Diagnostic {
                        rule:     self.name().to_string(),
                        severity: Severity::Warning,
                        message:  format!(
                            "Node '{}' has invalid fidelity mode '{fidelity}'",
                            node.id
                        ),
                        node_id:  Some(node.id.clone()),
                        edge:     None,
                        fix:      Some(Self::fix_message()),
                    });
                }
            }
        }
        for edge in &graph.edges {
            if let Some(fidelity) = edge.fidelity() {
                if fidelity.parse::<Fidelity>().is_err() {
                    diagnostics.push(Diagnostic {
                        rule:     self.name().to_string(),
                        severity: Severity::Warning,
                        message:  format!(
                            "Edge {} -> {} has invalid fidelity mode '{fidelity}'",
                            edge.from, edge.to
                        ),
                        node_id:  None,
                        edge:     Some((edge.from.clone(), edge.to.clone())),
                        fix:      Some(Self::fix_message()),
                    });
                }
            }
        }
        if let Some(fidelity) = graph.default_fidelity() {
            if fidelity.parse::<Fidelity>().is_err() {
                diagnostics.push(Diagnostic {
                    rule:     self.name().to_string(),
                    severity: Severity::Warning,
                    message:  format!("Graph has invalid default_fidelity '{fidelity}'"),
                    node_id:  None,
                    edge:     None,
                    fix:      Some(Self::fix_message()),
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
                        rule:     self.name().to_string(),
                        severity: Severity::Warning,
                        message:  format!(
                            "Node '{}' has retry_target '{}' that does not exist",
                            node.id, target
                        ),
                        node_id:  Some(node.id.clone()),
                        edge:     None,
                        fix:      Some(format!("Define node '{target}' or fix retry_target")),
                    });
                }
            }
            if let Some(target) = node.fallback_retry_target() {
                if !graph.nodes.contains_key(target) {
                    diagnostics.push(Diagnostic {
                        rule:     self.name().to_string(),
                        severity: Severity::Warning,
                        message:  format!(
                            "Node '{}' has fallback_retry_target '{}' that does not exist",
                            node.id, target
                        ),
                        node_id:  Some(node.id.clone()),
                        edge:     None,
                        fix:      Some(format!(
                            "Define node '{target}' or fix fallback_retry_target"
                        )),
                    });
                }
            }
        }
        if let Some(target) = graph.retry_target() {
            if !graph.nodes.contains_key(target) {
                diagnostics.push(Diagnostic {
                    rule:     self.name().to_string(),
                    severity: Severity::Warning,
                    message:  format!("Graph has retry_target '{target}' that does not exist"),
                    node_id:  None,
                    edge:     None,
                    fix:      Some(format!("Define node '{target}' or fix graph retry_target")),
                });
            }
        }
        if let Some(target) = graph.fallback_retry_target() {
            if !graph.nodes.contains_key(target) {
                diagnostics.push(Diagnostic {
                    rule:     self.name().to_string(),
                    severity: Severity::Warning,
                    message:  format!(
                        "Graph has fallback_retry_target '{target}' that does not exist"
                    ),
                    node_id:  None,
                    edge:     None,
                    fix:      Some(format!(
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
            if is_llm_handler_type(node.handler_type()) {
                let has_prompt = node.prompt().is_some_and(|p| !p.is_empty());
                let has_label = node
                    .attrs
                    .get("label")
                    .and_then(AttrValue::as_str)
                    .is_some_and(|l| !l.is_empty());
                if !has_prompt && !has_label {
                    diagnostics.push(Diagnostic {
                        rule:     self.name().to_string(),
                        severity: Severity::Warning,
                        message:  format!(
                            "LLM node '{}' has no prompt or label attribute",
                            node.id
                        ),
                        node_id:  Some(node.id.clone()),
                        edge:     None,
                        fix:      Some("Add a prompt or label attribute".to_string()),
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
            if node.handler_type() == Some("human") {
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
            rule:     self.name().to_string(),
            severity: Severity::Warning,
            message:  format!("Graph has invalid rankdir '{rankdir}'"),
            node_id:  None,
            edge:     None,
            fix:      Some(format!("Use one of: {}", VALID_DIRECTIONS.join(", "))),
        }]
    }
}

// --- Rule 16: reserved_keyword_node_id (WARNING) ---

struct ReservedKeywordNodeIdRule;

const DOT_RESERVED_KEYWORDS: &[&str] = &[
    "graph", "digraph", "subgraph", "node", "edge", "strict", "if",
];

impl LintRule for ReservedKeywordNodeIdRule {
    fn name(&self) -> &'static str {
        "reserved_keyword_node_id"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        graph
            .nodes
            .values()
            .filter(|node| DOT_RESERVED_KEYWORDS.contains(&node.id.to_lowercase().as_str()))
            .map(|node| Diagnostic {
                rule:     self.name().to_string(),
                severity: Severity::Warning,
                message:  format!(
                    "Node ID '{}' is a DOT reserved keyword and may cause parsing failures",
                    node.id
                ),
                node_id:  Some(node.id.clone()),
                edge:     None,
                fix:      Some(format!(
                    "Rename '{}' to '{}_step' or another non-reserved ID",
                    node.id,
                    node.id.to_lowercase()
                )),
            })
            .collect()
    }
}

// --- Rule 17: all_conditional_edges (ERROR) ---

struct AllConditionalEdgesRule;

impl LintRule for AllConditionalEdgesRule {
    fn name(&self) -> &'static str {
        "all_conditional_edges"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            let outgoing = graph.outgoing_edges(&node.id);
            if outgoing.is_empty() {
                continue;
            }
            let all_conditional = outgoing
                .iter()
                .all(|e| e.condition().is_some_and(|c| !c.is_empty()));
            if all_conditional {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: format!(
                        "Node '{}' has all conditional outgoing edges with no unconditional fallback",
                        node.id
                    ),
                    node_id: Some(node.id.clone()),
                    edge: None,
                    fix: Some(
                        "Add at least one unconditional edge as a fallback".to_string(),
                    ),
                });
            }
        }
        diagnostics
    }
}

// --- Rule 18: orphan_custom_outcome (WARNING) ---

struct OrphanCustomOutcomeRule;

impl LintRule for OrphanCustomOutcomeRule {
    fn name(&self) -> &'static str {
        "orphan_custom_outcome"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            let outgoing = graph.outgoing_edges(&node.id);
            if outgoing.is_empty() {
                continue;
            }
            // Check if any edge uses outcome=<value> (equality, not !=)
            let has_outcome_eq = outgoing.iter().any(|e| {
                e.condition().is_some_and(|c| {
                    c.split("&&")
                        .any(|clause| clause.trim().starts_with("outcome="))
                })
            });
            if !has_outcome_eq {
                continue;
            }
            // Check if there's at least one unconditional edge
            let has_unconditional = outgoing
                .iter()
                .any(|e| e.condition().is_none_or(str::is_empty));
            if !has_unconditional {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "Node '{}' uses outcome-based routing but has no unconditional fallback edge",
                        node.id
                    ),
                    node_id: Some(node.id.clone()),
                    edge: None,
                    fix: Some(
                        "Add an unconditional edge as a safety net for unmatched outcomes"
                            .to_string(),
                    ),
                });
            }
        }
        diagnostics
    }
}

// --- Rule 19: script_absolute_cd (WARNING) ---

struct ScriptAbsoluteCdRule;

/// Returns true if `text` contains `cd` followed by whitespace and then `/`.
fn contains_cd_absolute(text: &str) -> bool {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i + 3 < len {
        if bytes[i] == b'c' && bytes[i + 1] == b'd' && bytes[i + 2].is_ascii_whitespace() {
            // found "cd<ws>", scan past remaining whitespace to check for '/'
            let mut j = i + 2;
            while j < len && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < len && bytes[j] == b'/' {
                return true;
            }
        }
        i += 1;
    }
    false
}

impl LintRule for ScriptAbsoluteCdRule {
    fn name(&self) -> &'static str {
        "script_absolute_cd"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if node.handler_type() != Some("command") {
                continue;
            }
            let script = node
                .attrs
                .get("script")
                .or_else(|| node.attrs.get("tool_command"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if contains_cd_absolute(script) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "Script node '{}' contains `cd /…` with an absolute path",
                        node.id
                    ),
                    node_id: Some(node.id.clone()),
                    edge: None,
                    fix: Some(
                        "Use a relative path; the engine sets the working directory to the worktree automatically"
                            .to_string(),
                    ),
                });
            }
        }
        diagnostics
    }
}

// --- Shared helpers for model/provider validation ---

fn check_model_known(
    rule_name: &str,
    model: &str,
    context: &str,
    node_id: Option<String>,
) -> Option<Diagnostic> {
    if fabro_model::Catalog::builtin().get(model).is_some() {
        return None;
    }
    Some(Diagnostic {
        rule: rule_name.to_string(),
        severity: Severity::Warning,
        message: format!(
            "Unknown model '{model}' {context}. Run `fabro model list` to see available models"
        ),
        node_id,
        edge: None,
        fix: Some("Use a model ID from `fabro model list`".to_string()),
    })
}

fn check_provider_known(
    rule_name: &str,
    provider: &str,
    context: &str,
    node_id: Option<String>,
) -> Option<Diagnostic> {
    if fabro_model::Provider::from_str(provider).is_ok() {
        return None;
    }
    let valid: Vec<&str> = fabro_model::Provider::ALL
        .iter()
        .map(|p| p.as_str())
        .collect();
    let valid_str = valid.join(", ");
    Some(Diagnostic {
        rule: rule_name.to_string(),
        severity: Severity::Warning,
        message: format!("Unknown provider '{provider}' {context}. Valid providers: {valid_str}"),
        node_id,
        edge: None,
        fix: Some(format!("Use one of: {valid_str}")),
    })
}

// --- Rule 20: stylesheet_model_known (WARNING) ---

struct StylesheetModelKnownRule;

impl StylesheetModelKnownRule {
    fn selector_label(selector: &Selector) -> String {
        match selector {
            Selector::Universal => "*".to_string(),
            Selector::Shape(s) => s.clone(),
            Selector::Class(c) => format!(".{c}"),
            Selector::Id(id) => format!("#{id}"),
        }
    }
}

impl LintRule for StylesheetModelKnownRule {
    fn name(&self) -> &'static str {
        "stylesheet_model_known"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let stylesheet_str = graph.model_stylesheet();
        if stylesheet_str.is_empty() {
            return Vec::new();
        }
        let Ok(stylesheet) = parse_stylesheet(stylesheet_str) else {
            return Vec::new(); // syntax errors caught by stylesheet_syntax rule
        };

        let mut diagnostics = Vec::new();
        for rule in &stylesheet.rules {
            let label = Self::selector_label(&rule.selector);
            for decl in &rule.declarations {
                let context = format!("in stylesheet rule '{label}'");
                match decl.property.as_str() {
                    "model" => {
                        if let Some(d) = check_model_known(self.name(), &decl.value, &context, None)
                        {
                            diagnostics.push(d);
                        }
                    }
                    "provider" => {
                        if let Some(d) =
                            check_provider_known(self.name(), &decl.value, &context, None)
                        {
                            diagnostics.push(d);
                        }
                    }
                    _ => {}
                }
            }
        }
        diagnostics
    }
}

// --- Rule 21: node_model_known (WARNING) ---

struct NodeModelKnownRule;

impl LintRule for NodeModelKnownRule {
    fn name(&self) -> &'static str {
        "node_model_known"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            let context = format!("on node '{}'", node.id);
            let node_id = Some(node.id.clone());
            if let Some(model) = node.model() {
                if let Some(d) = check_model_known(self.name(), model, &context, node_id.clone()) {
                    diagnostics.push(d);
                }
            }
            if let Some(provider) = node.provider() {
                if let Some(d) =
                    check_provider_known(self.name(), provider, &context, node_id.clone())
                {
                    diagnostics.push(d);
                }
            }
        }
        diagnostics
    }
}

// --- Rule 22: import_error (ERROR) ---

struct ImportErrorRule;

impl LintRule for ImportErrorRule {
    fn name(&self) -> &'static str {
        "import_error"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        for node in graph.nodes.values() {
            if let Some(AttrValue::String(message)) = node.attrs.get("import_error") {
                diagnostics.push(Diagnostic {
                    rule:     self.name().to_string(),
                    severity: Severity::Error,
                    message:  message.clone(),
                    node_id:  Some(node.id.clone()),
                    edge:     None,
                    fix:      Some("Fix the imported workflow or import path".to_string()),
                });
            }

            if node.attrs.contains_key("import") {
                diagnostics.push(Diagnostic {
                    rule:     self.name().to_string(),
                    severity: Severity::Error,
                    message:  "unresolved import (no base directory available)".to_string(),
                    node_id:  Some(node.id.clone()),
                    edge:     None,
                    fix:      Some(
                        "Load the workflow from a file so imports can resolve relative to it"
                            .to_string(),
                    ),
                });
            }
        }

        diagnostics
    }
}

// --- Rule 23: unresolved_file_ref (ERROR) ---

struct UnresolvedFileRefRule;

impl LintRule for UnresolvedFileRefRule {
    fn name(&self) -> &'static str {
        "unresolved_file_ref"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        for node in graph.nodes.values() {
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                if prompt.starts_with('@') {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Error,
                        message: format!(
                            "Node '{}' has unresolved file reference: {prompt}",
                            node.id
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some("Check that the path is relative to the workflow file's directory and the file exists".to_string()),
                    });
                }
            }
        }

        if let Some(AttrValue::String(goal)) = graph.attrs.get("goal") {
            if goal.starts_with('@') {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: format!("Graph goal has unresolved file reference: {goal}"),
                    node_id: None,
                    edge: None,
                    fix: Some("Check that the path is relative to the workflow file's directory and the file exists".to_string()),
                });
            }
        }

        diagnostics
    }
}

// --- Rule 24: thread_id_requires_fidelity_full (WARNING) ---

struct ThreadIdRequiresFidelityFullRule;

impl ThreadIdRequiresFidelityFullRule {
    const FIX: &str = "Add fidelity=\"full\" to enable session reuse, or remove thread_id";
}

impl LintRule for ThreadIdRequiresFidelityFullRule {
    fn name(&self) -> &'static str {
        "thread_id_requires_fidelity_full"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let graph_default_full = graph.default_fidelity() == Some("full");

        for node in graph.nodes.values() {
            if node.thread_id().is_some() && node.fidelity() != Some("full") && !graph_default_full
            {
                diagnostics.push(Diagnostic {
                    rule:     self.name().to_string(),
                    severity: Severity::Warning,
                    message:  format!(
                        "Node '{}' has thread_id but fidelity is not 'full'",
                        node.id
                    ),
                    node_id:  Some(node.id.clone()),
                    edge:     None,
                    fix:      Some(Self::FIX.to_string()),
                });
            }
        }

        for edge in &graph.edges {
            if edge.thread_id().is_some() {
                let edge_full = edge.fidelity() == Some("full");
                let target_full =
                    graph.nodes.get(&edge.to).and_then(|n| n.fidelity()) == Some("full");
                if !edge_full && !target_full && !graph_default_full {
                    diagnostics.push(Diagnostic {
                        rule:     self.name().to_string(),
                        severity: Severity::Warning,
                        message:  format!(
                            "Edge {} -> {} has thread_id but fidelity is not 'full'",
                            edge.from, edge.to
                        ),
                        node_id:  None,
                        edge:     Some((edge.from.clone(), edge.to.clone())),
                        fix:      Some(Self::FIX.to_string()),
                    });
                }
            }
        }

        if graph.default_thread().is_some() && !graph_default_full {
            diagnostics.push(Diagnostic {
                rule:     self.name().to_string(),
                severity: Severity::Warning,
                message:  "Graph has default_thread but default_fidelity is not 'full'".to_string(),
                node_id:  None,
                edge:     None,
                fix:      Some(Self::FIX.to_string()),
            });
        }

        diagnostics
    }
}

// --- Rule 23: selection_valid (WARNING) ---

struct SelectionValidRule;

const VALID_SELECTIONS: &[&str] = &["deterministic", "random"];

impl LintRule for SelectionValidRule {
    fn name(&self) -> &'static str {
        "selection_valid"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if let Some(sel) = node.attrs.get("selection").and_then(AttrValue::as_str) {
                if !VALID_SELECTIONS.contains(&sel) {
                    diagnostics.push(Diagnostic {
                        rule:     self.name().to_string(),
                        severity: Severity::Warning,
                        message:  format!("Node '{}' has invalid selection mode '{sel}'", node.id),
                        node_id:  Some(node.id.clone()),
                        edge:     None,
                        fix:      Some(format!("Use one of: {}", VALID_SELECTIONS.join(", "))),
                    });
                }
            }
        }
        diagnostics
    }
}

// --- Rule 24: random_selection_no_conditions (ERROR) ---

struct RandomSelectionNoConditionsRule;

impl LintRule for RandomSelectionNoConditionsRule {
    fn name(&self) -> &'static str {
        "random_selection_no_conditions"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if node.selection() != "random" {
                continue;
            }
            let has_conditional = graph
                .outgoing_edges(&node.id)
                .iter()
                .any(|e| e.condition().is_some_and(|c| !c.is_empty()));
            if has_conditional {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: format!(
                        "Node '{}' has selection=\"random\" but also has conditional edges; random selection and conditions cannot be combined",
                        node.id
                    ),
                    node_id: Some(node.id.clone()),
                    edge: None,
                    fix: Some(
                        "Remove the condition attributes from outgoing edges, or remove selection=\"random\" from the node".to_string(),
                    ),
                });
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Edge, Node};

    use super::*;

    fn minimal_graph() -> Graph {
        let mut g = Graph::new("test");
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
        s1.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        let mut s2 = Node::new("s2");
        s2.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
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
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
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
        node.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));
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
            AttrValue::String("* { model: foo;".to_string()),
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
            AttrValue::String("* { model: foo; }".to_string()),
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
        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("LR".to_string()));
        assert!(rule.apply(&g).is_empty());

        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("TB".to_string()));
        assert!(rule.apply(&g).is_empty());

        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("BT".to_string()));
        assert!(rule.apply(&g).is_empty());

        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("RL".to_string()));
        assert!(rule.apply(&g).is_empty());
    }

    #[test]
    fn direction_valid_rule_invalid_direction() {
        let mut g = minimal_graph();
        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("XY".to_string()));
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

    // --- Additional coverage: condition_syntax invalid case ---

    #[test]
    fn condition_syntax_rule_invalid_clause() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("bad clause here".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn condition_syntax_rule_not_equals() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome!=failure".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_empty_condition() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs
            .insert("condition".to_string(), AttrValue::String(String::new()));
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_compound_and() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success && retries=0".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: terminal_node two terminals ---

    #[test]
    fn terminal_node_rule_two_terminals() {
        let mut g = Graph::new("test");
        let mut e1 = Node::new("e1");
        e1.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        let mut e2 = Node::new("e2");
        e2.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("e1".to_string(), e1);
        g.nodes.insert("e2".to_string(), e2);
        let rule = TerminalNodeRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("exactly one"));
    }

    // --- Additional coverage: edge_target_exists missing source ---

    #[test]
    fn edge_target_exists_rule_missing_source() {
        let mut g = minimal_graph();
        g.edges.push(Edge::new("nonexistent_source", "exit"));
        let rule = EdgeTargetExistsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("nonexistent_source"));
    }

    // --- Additional coverage: reachability no start node ---

    #[test]
    fn reachability_rule_no_start_node() {
        let mut g = Graph::new("test");
        g.nodes.insert("orphan".to_string(), Node::new("orphan"));
        let rule = ReachabilityRule;
        let d = rule.apply(&g);
        // No start node found, rule returns empty
        assert!(d.is_empty());
    }

    // --- Additional coverage: retry_target_exists fallback and graph-level ---

    #[test]
    fn retry_target_exists_rule_fallback_missing() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("fallback_retry_target"));
    }

    #[test]
    fn retry_target_exists_rule_fallback_valid() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn retry_target_exists_rule_graph_level_missing() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("Graph"));
    }

    #[test]
    fn retry_target_exists_rule_graph_level_valid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn retry_target_exists_rule_graph_fallback_missing() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("fallback_retry_target"));
    }

    #[test]
    fn retry_target_exists_rule_graph_fallback_valid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("exit".to_string()),
        );
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: goal_gate_has_retry with graph-level retry ---

    #[test]
    fn goal_gate_has_retry_rule_with_graph_retry_target() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), node);
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        let rule = GoalGateHasRetryRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn goal_gate_has_retry_rule_with_fallback_retry_target() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        node.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = GoalGateHasRetryRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn goal_gate_has_retry_rule_not_goal_gate() {
        let mut g = minimal_graph();
        let node = Node::new("work");
        g.nodes.insert("work".to_string(), node);
        let rule = GoalGateHasRetryRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: prompt_on_llm_nodes with label ---

    #[test]
    fn prompt_on_llm_nodes_rule_with_label() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "label".to_string(),
            AttrValue::String("Do something".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = PromptOnLlmNodesRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn prompt_on_llm_nodes_rule_non_codergen_no_warning() {
        let mut g = minimal_graph();
        let mut node = Node::new("gate");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        g.nodes.insert("gate".to_string(), node);
        let rule = PromptOnLlmNodesRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: fidelity_valid edge and graph-level ---

    #[test]
    fn fidelity_valid_rule_invalid_edge_fidelity() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("bogus".to_string()),
        );
        g.edges = vec![edge];
        let rule = FidelityValidRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].edge.is_some());
    }

    #[test]
    fn fidelity_valid_rule_valid_edge_fidelity() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("compact".to_string()),
        );
        g.edges = vec![edge];
        let rule = FidelityValidRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn fidelity_valid_rule_invalid_graph_default() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("wrong".to_string()),
        );
        let rule = FidelityValidRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("default_fidelity"));
    }

    #[test]
    fn fidelity_valid_rule_valid_graph_default() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("summary:high".to_string()),
        );
        let rule = FidelityValidRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn fidelity_valid_rule_all_summary_modes() {
        let rule = FidelityValidRule;

        let mut g = minimal_graph();
        let mut node = Node::new("w1");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("summary:low".to_string()),
        );
        g.nodes.insert("w1".to_string(), node);
        assert!(rule.apply(&g).is_empty());

        let mut g = minimal_graph();
        let mut node = Node::new("w2");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("summary:medium".to_string()),
        );
        g.nodes.insert("w2".to_string(), node);
        assert!(rule.apply(&g).is_empty());

        let mut g = minimal_graph();
        let mut node = Node::new("w3");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        g.nodes.insert("w3".to_string(), node);
        assert!(rule.apply(&g).is_empty());
    }

    // --- Additional coverage: freeform_edge_count non-wait.human ---

    #[test]
    fn freeform_edge_count_rule_non_wait_human_ignored() {
        let mut g = minimal_graph();
        // Regular codergen node (box shape) with multiple freeform edges should not
        // trigger
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.nodes.insert("b".to_string(), Node::new("b"));
        g.nodes.insert("work".to_string(), Node::new("work"));

        let mut e1 = Edge::new("work", "a");
        e1.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        let mut e2 = Edge::new("work", "b");
        e2.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        g.edges.push(e1);
        g.edges.push(e2);

        let rule = FreeformEdgeCountRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn freeform_edge_count_rule_zero_freeform() {
        let mut g = minimal_graph();
        let mut gate = Node::new("gate");
        gate.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));
        g.nodes.insert("gate".to_string(), gate);
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.edges.push(Edge::new("gate", "a"));

        let rule = FreeformEdgeCountRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: stylesheet_syntax no stylesheet ---

    #[test]
    fn stylesheet_syntax_rule_no_stylesheet() {
        let g = minimal_graph();
        let rule = StylesheetSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: type_known no type attr ---

    #[test]
    fn type_known_rule_no_type_attr() {
        let g = minimal_graph();
        let rule = TypeKnownRule;
        let d = rule.apply(&g);
        // Nodes without explicit type attr should not trigger warning
        assert!(d.is_empty());
    }

    // --- Additional coverage: start_no_incoming no start node ---

    #[test]
    fn start_no_incoming_rule_no_start_node() {
        let g = Graph::new("test");
        let rule = StartNoIncomingRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: exit_no_outgoing by id variants ---

    #[test]
    fn exit_no_outgoing_rule_end_id_with_outgoing() {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);
        let end_node = Node::new("end");
        g.nodes.insert("end".to_string(), end_node);
        g.edges.push(Edge::new("start", "end"));
        g.edges.push(Edge::new("end", "start"));
        let rule = ExitNoOutgoingRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].node_id, Some("end".to_string()));
    }

    // --- condition_syntax: bare key (truthy check) is valid ---

    #[test]
    fn condition_syntax_rule_bare_key_truthy() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.passed".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- condition_syntax: context-prefixed clause with spaces is rejected ---

    #[test]
    fn condition_syntax_rule_context_prefix_with_space() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.foo bar".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        // "context.foo bar" has an unexpected trailing word — parse error
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    // --- terminal_node: by "Exit" capitalized id ---

    #[test]
    fn terminal_node_rule_by_exit_capitalized_id() {
        let mut g = Graph::new("test");
        let node = Node::new("Exit");
        g.nodes.insert("Exit".to_string(), node);
        let rule = TerminalNodeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- edge_target_exists: both source and target missing ---

    #[test]
    fn edge_target_exists_rule_both_missing() {
        let mut g = minimal_graph();
        g.edges
            .push(Edge::new("nonexistent_source", "nonexistent_target"));
        let rule = EdgeTargetExistsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[1].severity, Severity::Error);
    }

    // --- reachability: multiple unreachable nodes ---

    #[test]
    fn reachability_rule_multiple_unreachable() {
        let mut g = minimal_graph();
        g.nodes
            .insert("orphan_a".to_string(), Node::new("orphan_a"));
        g.nodes
            .insert("orphan_b".to_string(), Node::new("orphan_b"));
        let rule = ReachabilityRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[1].severity, Severity::Warning);
    }

    // --- type_known: all known handler types are accepted ---

    #[test]
    fn type_known_rule_all_known_types_accepted() {
        let mut g = minimal_graph();

        let mut n1 = Node::new("n1");
        n1.attrs
            .insert("type".to_string(), AttrValue::String("agent".to_string()));
        g.nodes.insert("n1".to_string(), n1);

        let mut n2 = Node::new("n2");
        n2.attrs.insert(
            "type".to_string(),
            AttrValue::String("conditional".to_string()),
        );
        g.nodes.insert("n2".to_string(), n2);

        let mut n3 = Node::new("n3");
        n3.attrs.insert(
            "type".to_string(),
            AttrValue::String("parallel".to_string()),
        );
        g.nodes.insert("n3".to_string(), n3);

        let mut n4 = Node::new("n4");
        n4.attrs.insert(
            "type".to_string(),
            AttrValue::String("parallel.fan_in".to_string()),
        );
        g.nodes.insert("n4".to_string(), n4);

        let mut n5 = Node::new("n5");
        n5.attrs
            .insert("type".to_string(), AttrValue::String("command".to_string()));
        g.nodes.insert("n5".to_string(), n5);

        let mut n6 = Node::new("n6");
        n6.attrs.insert(
            "type".to_string(),
            AttrValue::String("stack.manager_loop".to_string()),
        );
        g.nodes.insert("n6".to_string(), n6);

        let rule = TypeKnownRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- prompt_on_llm_nodes: explicit type=agent without prompt/label ---

    #[test]
    fn prompt_on_llm_nodes_rule_explicit_agent_type_no_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("type".to_string(), AttrValue::String("agent".to_string()));
        // No shape=box, but explicit type=agent
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("diamond".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = PromptOnLlmNodesRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    // --- goal_gate_has_retry: satisfied by graph-level fallback_retry_target ---

    #[test]
    fn goal_gate_has_retry_rule_with_graph_fallback_retry_target() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), node);
        g.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        let rule = GoalGateHasRetryRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- freeform_edge_count: with explicit type=human ---

    #[test]
    fn freeform_edge_count_rule_explicit_type_two_freeform() {
        let mut g = minimal_graph();
        let mut gate = Node::new("gate");
        gate.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));
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

    // --- exit_no_outgoing: by "Exit" capitalized id ---

    #[test]
    fn exit_no_outgoing_rule_exit_capitalized_with_outgoing() {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);
        let exit_node = Node::new("Exit");
        g.nodes.insert("Exit".to_string(), exit_node);
        g.edges.push(Edge::new("start", "Exit"));
        g.edges.push(Edge::new("Exit", "start"));
        let rule = ExitNoOutgoingRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].node_id, Some("Exit".to_string()));
    }

    // --- exit_no_outgoing: by "End" capitalized id ---

    #[test]
    fn exit_no_outgoing_rule_end_capitalized_with_outgoing() {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);
        let end_node = Node::new("End");
        g.nodes.insert("End".to_string(), end_node);
        g.edges.push(Edge::new("start", "End"));
        g.edges.push(Edge::new("End", "start"));
        let rule = ExitNoOutgoingRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].node_id, Some("End".to_string()));
    }

    // --- stylesheet_syntax: valid multi-rule stylesheet ---

    #[test]
    fn stylesheet_syntax_rule_multi_rule_valid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String(
                "* { model: gpt-4; } .fast { model: gpt-3.5; reasoning_effort: low; }".to_string(),
            ),
        );
        let rule = StylesheetSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- fidelity_valid: multiple simultaneous violations ---

    #[test]
    fn fidelity_valid_rule_node_and_edge_and_graph_all_invalid() {
        let mut g = minimal_graph();

        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("invalid_node".to_string()),
        );
        g.nodes.insert("work".to_string(), node);

        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("invalid_edge".to_string()),
        );
        g.edges = vec![edge];

        g.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("invalid_graph".to_string()),
        );

        let rule = FidelityValidRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 3);
    }

    // --- retry_target_exists: both retry_target and fallback on same node, both
    // invalid ---

    #[test]
    fn retry_target_exists_rule_both_node_targets_invalid() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("missing_a".to_string()),
        );
        node.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("missing_b".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[1].severity, Severity::Warning);
    }

    // --- retry_target_exists: both graph-level targets invalid ---

    #[test]
    fn retry_target_exists_rule_both_graph_targets_invalid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("missing_a".to_string()),
        );
        g.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("missing_b".to_string()),
        );
        let rule = RetryTargetExistsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[1].severity, Severity::Warning);
    }

    // --- start_no_incoming: multiple incoming edges ---

    #[test]
    fn start_no_incoming_rule_multiple_incoming() {
        let mut g = minimal_graph();
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.edges.push(Edge::new("exit", "start"));
        g.edges.push(Edge::new("a", "start"));
        let rule = StartNoIncomingRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains('2'));
    }

    // --- prompt_on_llm_nodes: empty prompt string still triggers ---

    #[test]
    fn prompt_on_llm_nodes_rule_empty_prompt_no_label() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("prompt".to_string(), AttrValue::String(String::new()));
        g.nodes.insert("work".to_string(), node);
        let rule = PromptOnLlmNodesRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    // --- prompt_on_llm_nodes: empty label string still triggers ---

    #[test]
    fn prompt_on_llm_nodes_rule_empty_label_no_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("label".to_string(), AttrValue::String(String::new()));
        g.nodes.insert("work".to_string(), node);
        let rule = PromptOnLlmNodesRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    // --- condition_syntax: no condition attribute at all ---

    #[test]
    fn condition_syntax_rule_no_condition_attr() {
        let g = minimal_graph();
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- freeform_edge_count: freeform=false does not count ---

    #[test]
    fn freeform_edge_count_rule_freeform_false_ignored() {
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
            .insert("freeform".to_string(), AttrValue::Boolean(false));
        let mut e2 = Edge::new("gate", "b");
        e2.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(false));
        g.edges.push(e1);
        g.edges.push(e2);

        let rule = FreeformEdgeCountRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- reachability: chain of reachable nodes ---

    #[test]
    fn reachability_rule_chain_all_reachable() {
        let mut g = minimal_graph();
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.nodes.insert("b".to_string(), Node::new("b"));
        g.edges = vec![
            Edge::new("start", "a"),
            Edge::new("a", "b"),
            Edge::new("b", "exit"),
        ];
        let rule = ReachabilityRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- edge_target_exists: no edges at all ---

    #[test]
    fn edge_target_exists_rule_no_edges() {
        let mut g = minimal_graph();
        g.edges.clear();
        let rule = EdgeTargetExistsRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- goal_gate_has_retry: goal_gate=false explicitly ---

    #[test]
    fn goal_gate_has_retry_rule_explicit_false() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(false));
        g.nodes.insert("work".to_string(), node);
        let rule = GoalGateHasRetryRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- stylesheet_syntax: empty string stylesheet ---

    #[test]
    fn stylesheet_syntax_rule_empty_string() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String(String::new()),
        );
        let rule = StylesheetSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- type_known: start and exit types from shape are not flagged ---

    #[test]
    fn type_known_rule_start_exit_shapes_no_warning() {
        // The minimal_graph has start (Mdiamond) and exit (Msquare), which resolve
        // to known handler types "start" and "exit" via shape mapping, not explicit
        // type. Since they have no explicit `type` attr, the rule should not
        // flag them.
        let g = minimal_graph();
        let rule = TypeKnownRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- reserved_keyword_node_id tests ---

    #[test]
    fn reserved_keyword_node_id_warns_on_keyword() {
        let mut g = minimal_graph();
        g.nodes.insert("graph".to_string(), Node::new("graph"));
        g.edges.push(Edge::new("start", "graph"));
        g.edges.push(Edge::new("graph", "exit"));
        let rule = ReservedKeywordNodeIdRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].node_id.as_deref(), Some("graph"));
    }

    #[test]
    fn reserved_keyword_node_id_case_insensitive() {
        let mut g = minimal_graph();
        g.nodes.insert("Node".to_string(), Node::new("Node"));
        g.nodes.insert("EDGE".to_string(), Node::new("EDGE"));
        g.edges.push(Edge::new("start", "Node"));
        g.edges.push(Edge::new("Node", "EDGE"));
        g.edges.push(Edge::new("EDGE", "exit"));
        let rule = ReservedKeywordNodeIdRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn reserved_keyword_node_id_normal_id_no_warning() {
        let g = minimal_graph();
        let rule = ReservedKeywordNodeIdRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn reserved_keyword_node_id_multiple_keywords() {
        let mut g = minimal_graph();
        g.nodes.insert("strict".to_string(), Node::new("strict"));
        g.nodes.insert("digraph".to_string(), Node::new("digraph"));
        g.nodes.insert("if".to_string(), Node::new("if"));
        g.edges.push(Edge::new("start", "strict"));
        g.edges.push(Edge::new("strict", "digraph"));
        g.edges.push(Edge::new("digraph", "if"));
        g.edges.push(Edge::new("if", "exit"));
        let rule = ReservedKeywordNodeIdRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 3);
    }

    // --- all_conditional_edges rule tests ---

    #[test]
    fn all_conditional_edges_rule_all_conditional() {
        let mut g = minimal_graph();
        g.nodes.insert("work".to_string(), Node::new("work"));
        g.edges.push({
            let mut e = Edge::new("work", "exit");
            e.attrs.insert(
                "condition".to_string(),
                AttrValue::String("outcome=success".to_string()),
            );
            e
        });
        g.edges.push({
            let mut e = Edge::new("work", "start");
            e.attrs.insert(
                "condition".to_string(),
                AttrValue::String("outcome=fail".to_string()),
            );
            e
        });
        let rule = AllConditionalEdgesRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].node_id.as_deref(), Some("work"));
    }

    #[test]
    fn all_conditional_edges_rule_mix_conditional_unconditional() {
        let mut g = minimal_graph();
        g.nodes.insert("work".to_string(), Node::new("work"));
        g.edges.push({
            let mut e = Edge::new("work", "exit");
            e.attrs.insert(
                "condition".to_string(),
                AttrValue::String("outcome=success".to_string()),
            );
            e
        });
        g.edges.push(Edge::new("work", "start")); // unconditional fallback
        let rule = AllConditionalEdgesRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn all_conditional_edges_rule_only_unconditional() {
        let g = minimal_graph(); // start -> exit is unconditional
        let rule = AllConditionalEdgesRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn all_conditional_edges_rule_exit_node_no_outgoing() {
        let g = minimal_graph(); // exit has no outgoing edges
        let rule = AllConditionalEdgesRule;
        let d = rule.apply(&g);
        // exit node has no outgoing edges, so rule doesn't fire
        assert!(d.is_empty());
    }

    // --- orphan_custom_outcome rule tests ---

    #[test]
    fn orphan_custom_outcome_rule_outcome_eq_no_fallback() {
        let mut g = minimal_graph();
        g.nodes.insert("work".to_string(), Node::new("work"));
        g.edges.push({
            let mut e = Edge::new("work", "exit");
            e.attrs.insert(
                "condition".to_string(),
                AttrValue::String("outcome=success".to_string()),
            );
            e
        });
        g.edges.push({
            let mut e = Edge::new("work", "start");
            e.attrs.insert(
                "condition".to_string(),
                AttrValue::String("outcome=fail".to_string()),
            );
            e
        });
        let rule = OrphanCustomOutcomeRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].node_id.as_deref(), Some("work"));
    }

    #[test]
    fn orphan_custom_outcome_rule_outcome_eq_with_fallback() {
        let mut g = minimal_graph();
        g.nodes.insert("work".to_string(), Node::new("work"));
        g.edges.push({
            let mut e = Edge::new("work", "exit");
            e.attrs.insert(
                "condition".to_string(),
                AttrValue::String("outcome=success".to_string()),
            );
            e
        });
        g.edges.push(Edge::new("work", "start")); // unconditional fallback
        let rule = OrphanCustomOutcomeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn orphan_custom_outcome_rule_outcome_neq_only() {
        let mut g = minimal_graph();
        g.nodes.insert("work".to_string(), Node::new("work"));
        g.edges.push({
            let mut e = Edge::new("work", "exit");
            e.attrs.insert(
                "condition".to_string(),
                AttrValue::String("outcome!=fail".to_string()),
            );
            e
        });
        let rule = OrphanCustomOutcomeRule;
        let d = rule.apply(&g);
        // outcome!= is not outcome= equality, so rule doesn't fire
        assert!(d.is_empty());
    }

    #[test]
    fn orphan_custom_outcome_rule_no_outcome_conditions() {
        let g = minimal_graph(); // no outcome conditions at all
        let rule = OrphanCustomOutcomeRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- condition_syntax + parse_condition (condition_eval) tests ---

    #[test]
    fn condition_syntax_rule_empty_key_fails_parse() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("=value".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        // parse_condition catches empty key
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("failed parse"));
    }

    #[test]
    fn condition_syntax_rule_valid_passes_both_checks() {
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

    // --- condition_syntax: new operators accepted ---

    #[test]
    fn condition_syntax_rule_accepts_or() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success || outcome=fail".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_accepts_not() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("!outcome=fail".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_accepts_contains() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.x contains y".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_accepts_numeric() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.score > 80".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_rejects_invalid_regex() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.x matches [bad".to_string()),
        );
        g.edges = vec![edge];
        let rule = ConditionSyntaxRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    // script_absolute_cd rule tests

    #[test]
    fn script_absolute_cd_warns_on_cd_abs_path() {
        let mut g = minimal_graph();
        let mut node = Node::new("run");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("cd /tmp && ls".to_string()),
        );
        g.nodes.insert("run".to_string(), node);
        let rule = ScriptAbsoluteCdRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn script_absolute_cd_no_warning_on_relative() {
        let mut g = minimal_graph();
        let mut node = Node::new("run");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("cd src && ls".to_string()),
        );
        g.nodes.insert("run".to_string(), node);
        let rule = ScriptAbsoluteCdRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn script_absolute_cd_warns_on_legacy_tool_command() {
        let mut g = minimal_graph();
        let mut node = Node::new("run");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        node.attrs.insert(
            "tool_command".to_string(),
            AttrValue::String("cd /home/user && make".to_string()),
        );
        g.nodes.insert("run".to_string(), node);
        let rule = ScriptAbsoluteCdRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn script_absolute_cd_skips_non_script_nodes() {
        let mut g = minimal_graph();
        let mut node = Node::new("gen");
        node.attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("cd /tmp and do stuff".to_string()),
        );
        g.nodes.insert("gen".to_string(), node);
        let rule = ScriptAbsoluteCdRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // stylesheet_model_known rule tests

    #[test]
    fn stylesheet_model_known_rule_valid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { model: claude-sonnet-4-5; provider: anthropic; }".to_string()),
        );
        let rule = StylesheetModelKnownRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn stylesheet_model_known_rule_unknown_model() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("#opus { model: claude-opus-4-5; }".to_string()),
        );
        let rule = StylesheetModelKnownRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("claude-opus-4-5"));
        assert!(d[0].message.contains("#opus"));
    }

    #[test]
    fn stylesheet_model_known_rule_unknown_provider() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { provider: google; }".to_string()),
        );
        let rule = StylesheetModelKnownRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("google"));
    }

    #[test]
    fn stylesheet_model_known_rule_alias() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { model: opus; }".to_string()),
        );
        let rule = StylesheetModelKnownRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn stylesheet_model_known_rule_no_stylesheet() {
        let g = minimal_graph();
        let rule = StylesheetModelKnownRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // node_model_known rule tests

    #[test]
    fn node_model_known_rule_valid_model() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("claude-sonnet-4-5".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = NodeModelKnownRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn node_model_known_rule_unknown_model() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("nonexistent-model-xyz".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = NodeModelKnownRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("nonexistent-model-xyz"));
        assert_eq!(d[0].node_id.as_deref(), Some("work"));
    }

    #[test]
    fn node_model_known_rule_alias() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("model".to_string(), AttrValue::String("opus".to_string()));
        g.nodes.insert("work".to_string(), node);
        let rule = NodeModelKnownRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn node_model_known_rule_unknown_provider() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("google".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = NodeModelKnownRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("google"));
        assert_eq!(d[0].node_id.as_deref(), Some("work"));
    }

    #[test]
    fn node_model_known_rule_no_model_no_provider() {
        let g = minimal_graph();
        let rule = NodeModelKnownRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // import_error rule tests

    #[test]
    fn import_error_rule_fires_on_import_error_attr() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "import_error".to_string(),
            AttrValue::String("file not found: ./missing.fabro".to_string()),
        );
        g.nodes.insert("work".to_string(), node);

        let rule = ImportErrorRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].message, "file not found: ./missing.fabro");
        assert_eq!(d[0].node_id.as_deref(), Some("work"));
    }

    #[test]
    fn import_error_rule_fires_on_unresolved_import_attr() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "import".to_string(),
            AttrValue::String("./validate.fabro".to_string()),
        );
        g.nodes.insert("work".to_string(), node);

        let rule = ImportErrorRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(
            d[0].message,
            "unresolved import (no base directory available)"
        );
        assert_eq!(d[0].node_id.as_deref(), Some("work"));
    }

    #[test]
    fn import_error_rule_silent_for_clean_nodes() {
        let g = minimal_graph();
        let rule = ImportErrorRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // unresolved_file_ref rule tests

    #[test]
    fn unresolved_file_ref_rule_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("@prompts/simplify.md".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let rule = UnresolvedFileRefRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("@prompts/simplify.md"));
        assert_eq!(d[0].node_id, Some("work".to_string()));
    }

    #[test]
    fn unresolved_file_ref_rule_goal() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "goal".to_string(),
            AttrValue::String("@goal.md".to_string()),
        );

        let rule = UnresolvedFileRefRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("@goal.md"));
    }

    #[test]
    fn unresolved_file_ref_rule_resolved_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the work".to_string()),
        );
        g.nodes.insert("work".to_string(), node);

        let rule = UnresolvedFileRefRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // thread_id_requires_fidelity_full rule tests

    #[test]
    fn thread_id_requires_fidelity_full_node_warns() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let rule = ThreadIdRequiresFidelityFullRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].node_id, Some("work".to_string()));
    }

    #[test]
    fn thread_id_requires_fidelity_full_node_ok() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let rule = ThreadIdRequiresFidelityFullRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn thread_id_requires_fidelity_full_node_graph_default_ok() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let rule = ThreadIdRequiresFidelityFullRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn thread_id_requires_fidelity_full_edge_warns() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.edges = vec![edge];

        let rule = ThreadIdRequiresFidelityFullRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].edge, Some(("start".to_string(), "exit".to_string())));
    }

    #[test]
    fn thread_id_requires_fidelity_full_edge_ok() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        edge.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        g.edges = vec![edge];

        let rule = ThreadIdRequiresFidelityFullRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn thread_id_requires_fidelity_full_edge_target_node_ok() {
        let mut g = minimal_graph();
        if let Some(exit_node) = g.nodes.get_mut("exit") {
            exit_node.attrs.insert(
                "fidelity".to_string(),
                AttrValue::String("full".to_string()),
            );
        }
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.edges = vec![edge];

        let rule = ThreadIdRequiresFidelityFullRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn thread_id_requires_fidelity_full_graph_warns() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("session1".to_string()),
        );

        let rule = ThreadIdRequiresFidelityFullRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].node_id.is_none());
        assert!(d[0].edge.is_none());
    }

    #[test]
    fn thread_id_requires_fidelity_full_graph_ok() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );

        let rule = ThreadIdRequiresFidelityFullRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- selection_valid rule tests ---

    #[test]
    fn selection_valid_known_values() {
        let mut g = minimal_graph();
        let mut node = Node::new("pick");
        node.attrs.insert(
            "selection".to_string(),
            AttrValue::String("random".to_string()),
        );
        g.nodes.insert("pick".to_string(), node);
        let rule = SelectionValidRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn selection_valid_unknown_value_warns() {
        let mut g = minimal_graph();
        let mut node = Node::new("pick");
        node.attrs.insert(
            "selection".to_string(),
            AttrValue::String("randon".to_string()),
        );
        g.nodes.insert("pick".to_string(), node);
        let rule = SelectionValidRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].node_id.as_deref(), Some("pick"));
    }

    #[test]
    fn selection_valid_no_attr_ok() {
        let g = minimal_graph();
        let rule = SelectionValidRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- random_selection_no_conditions rule tests ---

    #[test]
    fn random_selection_no_conditions_clean() {
        let mut g = minimal_graph();
        let mut node = Node::new("pick");
        node.attrs.insert(
            "selection".to_string(),
            AttrValue::String("random".to_string()),
        );
        g.nodes.insert("pick".to_string(), node);
        g.edges.push(Edge::new("pick", "start"));
        g.edges.push(Edge::new("pick", "exit"));
        let rule = RandomSelectionNoConditionsRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn random_selection_with_conditions_errors() {
        let mut g = minimal_graph();
        let mut node = Node::new("pick");
        node.attrs.insert(
            "selection".to_string(),
            AttrValue::String("random".to_string()),
        );
        g.nodes.insert("pick".to_string(), node);
        let mut e = Edge::new("pick", "exit");
        e.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        g.edges.push(e);
        g.edges.push(Edge::new("pick", "start"));
        let rule = RandomSelectionNoConditionsRule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].node_id.as_deref(), Some("pick"));
    }

    #[test]
    fn deterministic_selection_with_conditions_ok() {
        let mut g = minimal_graph();
        let mut node = Node::new("gate");
        node.attrs.insert(
            "selection".to_string(),
            AttrValue::String("deterministic".to_string()),
        );
        g.nodes.insert("gate".to_string(), node);
        let mut e = Edge::new("gate", "exit");
        e.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        g.edges.push(e);
        g.edges.push(Edge::new("gate", "start"));
        let rule = RandomSelectionNoConditionsRule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
