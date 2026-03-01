pub mod rules;

use serde::{Deserialize, Serialize};

use crate::error::ArcError;
use crate::graph::Graph;

/// Severity level for validation diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A validation diagnostic produced by a lint rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    pub node_id: Option<String>,
    pub edge: Option<(String, String)>,
    pub fix: Option<String>,
}

/// A lint rule that validates a graph.
pub trait LintRule {
    fn name(&self) -> &'static str;
    fn apply(&self, graph: &Graph) -> Vec<Diagnostic>;
}

/// Run all built-in lint rules (and any extra rules) against the graph.
#[must_use]
pub fn validate(graph: &Graph, extra_rules: &[&dyn LintRule]) -> Vec<Diagnostic> {
    let built_in = rules::built_in_rules();
    let mut diagnostics = Vec::new();
    for rule in &built_in {
        diagnostics.extend(rule.apply(graph));
    }
    for rule in extra_rules {
        diagnostics.extend(rule.apply(graph));
    }
    diagnostics
}

/// Run all built-in lint rules (and any extra rules). Returns Err if any Error-severity
/// diagnostics are found.
///
/// # Errors
/// Returns `ArcError::Validation` if any Error-severity diagnostics are found.
pub fn validate_or_raise(
    graph: &Graph,
    extra_rules: &[&dyn LintRule],
) -> Result<Vec<Diagnostic>, ArcError> {
    let diagnostics = validate(graph, extra_rules);
    let errors: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    if !errors.is_empty() {
        let messages: Vec<String> = errors.iter().map(|d| d.message.clone()).collect();
        return Err(ArcError::Validation(messages.join("; ")));
    }
    Ok(diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{AttrValue, Edge, Graph, Node};

    fn minimal_valid_graph() -> Graph {
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

    #[test]
    fn validate_minimal_valid_graph_has_no_errors() {
        let g = minimal_valid_graph();
        let diagnostics = validate(&g, &[]);
        let errors: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_or_raise_passes_for_valid_graph() {
        let g = minimal_valid_graph();
        let result = validate_or_raise(&g, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_or_raise_fails_for_missing_start() {
        let mut g = Graph::new("test");
        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);
        let result = validate_or_raise(&g, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn validate_or_raise_fails_for_missing_exit() {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);
        let result = validate_or_raise(&g, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn validate_runs_extra_rules() {
        struct AlwaysWarnRule;
        impl LintRule for AlwaysWarnRule {
            fn name(&self) -> &'static str {
                "always_warn"
            }
            fn apply(&self, _graph: &Graph) -> Vec<Diagnostic> {
                vec![Diagnostic {
                    rule: "always_warn".to_string(),
                    severity: Severity::Warning,
                    message: "custom warning".to_string(),
                    node_id: None,
                    edge: None,
                    fix: None,
                }]
            }
        }
        let g = minimal_valid_graph();
        let extra = AlwaysWarnRule;
        let diagnostics = validate(&g, &[&extra]);
        let custom: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.rule == "always_warn")
            .collect();
        assert_eq!(custom.len(), 1);
    }

    #[test]
    fn diagnostic_severity_eq() {
        assert_eq!(Severity::Error, Severity::Error);
        assert_ne!(Severity::Error, Severity::Warning);
        assert_ne!(Severity::Warning, Severity::Info);
    }
}
