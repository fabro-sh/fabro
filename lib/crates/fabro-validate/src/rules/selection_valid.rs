use fabro_graphviz::graph::{AttrValue, Graph};

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

const VALID_SELECTIONS: &[&str] = &["deterministic", "random"];

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "selection_valid"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if let Some(sel) = node.attrs.get("selection").and_then(AttrValue::as_str) {
                if !VALID_SELECTIONS.contains(&sel) {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!("Node '{}' has invalid selection mode '{sel}'", node.id),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(format!("Use one of: {}", VALID_SELECTIONS.join(", "))),

                        ..Diagnostic::default()
                    });
                }
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn selection_valid_known_values() {
        let mut g = minimal_graph();
        let mut node = Node::new("pick");
        node.attrs.insert(
            "selection".to_string(),
            AttrValue::String("random".to_string()),
        );
        g.nodes.insert("pick".to_string(), node);
        let rule = Rule;
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
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].node_id.as_deref(), Some("pick"));
    }

    #[test]
    fn selection_valid_no_attr_ok() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
