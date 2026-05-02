use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl Rule {
    fn fix_message() -> String {
        use fabro_graphviz::Fidelity;
        let modes: Vec<_> = Fidelity::variants()
            .iter()
            .map(ToString::to_string)
            .collect();
        format!("Use one of: {}", modes.join(", "))
    }
}

impl LintRule for Rule {
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

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Edge, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn fidelity_valid_rule_invalid_mode() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("invalid_mode".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
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
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn fidelity_valid_rule_invalid_edge_fidelity() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("bogus".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
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
        let rule = Rule;
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
        let rule = Rule;
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
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn fidelity_valid_rule_all_summary_modes() {
        let rule = Rule;

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

        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 3);
    }

    // --- retry_target_exists: both retry_target and fallback on same node,
    // both invalid ---
}
