use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
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

                    ..Diagnostic::default()
                });
            }
            if !graph.nodes.contains_key(&edge.from) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: format!("Edge source '{}' references non-existent node", edge.from),
                    node_id: None,
                    edge: Some((edge.from.clone(), edge.to.clone())),
                    fix: Some(format!(
                        "Define node '{}' or fix the edge source",
                        edge.from
                    )),

                    ..Diagnostic::default()
                });
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::Edge;

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn edge_target_exists_rule_missing_target() {
        let mut g = minimal_graph();
        g.edges.push(Edge::new("start", "nonexistent"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn edge_target_exists_rule_valid() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn edge_target_exists_rule_missing_source() {
        let mut g = minimal_graph();
        g.edges.push(Edge::new("nonexistent_source", "exit"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("nonexistent_source"));
    }

    // --- Additional coverage: reachability no start node ---

    #[test]
    fn edge_target_exists_rule_both_missing() {
        let mut g = minimal_graph();
        g.edges
            .push(Edge::new("nonexistent_source", "nonexistent_target"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[1].severity, Severity::Error);
    }

    // --- reachability: multiple unreachable nodes ---

    #[test]
    fn edge_target_exists_rule_no_edges() {
        let mut g = minimal_graph();
        g.edges.clear();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- goal_gate_has_retry: goal_gate=false explicitly ---
}
