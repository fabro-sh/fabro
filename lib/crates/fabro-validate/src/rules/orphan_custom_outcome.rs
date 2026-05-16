use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
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

                ..Diagnostic::default()});
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Edge, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn orphan_custom_outcome_rule_outcome_eq_no_fallback() {
        let mut g = minimal_graph();
        g.nodes.insert("work".to_string(), Node::new("work"));
        g.edges.push({
            let mut e = Edge::new("work", "exit");
            e.attrs.insert(
                "condition".to_string(),
                AttrValue::String("outcome=succeeded".to_string()),
            );
            e
        });
        g.edges.push({
            let mut e = Edge::new("work", "start");
            e.attrs.insert(
                "condition".to_string(),
                AttrValue::String("outcome=failed".to_string()),
            );
            e
        });
        let rule = Rule;
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
                AttrValue::String("outcome=succeeded".to_string()),
            );
            e
        });
        g.edges.push(Edge::new("work", "start")); // unconditional fallback
        let rule = Rule;
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
                AttrValue::String("outcome!=failed".to_string()),
            );
            e
        });
        let rule = Rule;
        let d = rule.apply(&g);
        // outcome!= is not outcome= equality, so rule doesn't fire
        assert!(d.is_empty());
    }

    #[test]
    fn orphan_custom_outcome_rule_no_outcome_conditions() {
        let g = minimal_graph(); // no outcome conditions at all
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- condition_syntax + parse_condition (condition_eval) tests ---
}
