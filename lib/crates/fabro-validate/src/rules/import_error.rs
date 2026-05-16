use fabro_graphviz::graph::{AttrValue, Graph};

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "import_error"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        for node in graph.nodes.values() {
            if let Some(AttrValue::String(message)) = node.attrs.get("import_error") {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: message.clone(),
                    node_id: Some(node.id.clone()),
                    edge: None,
                    fix: Some("Fix the imported workflow or import path".to_string()),

                    ..Diagnostic::default()
                });
            }

            if node.attrs.contains_key("import") {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: "unresolved import (no base directory available)".to_string(),
                    node_id: Some(node.id.clone()),
                    edge: None,
                    fix: Some(
                        "Load the workflow from a file so imports can resolve relative to it"
                            .to_string(),
                    ),

                    ..Diagnostic::default()
                });
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
    fn import_error_rule_fires_on_import_error_attr() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "import_error".to_string(),
            AttrValue::String("file not found: ./missing.fabro".to_string()),
        );
        g.nodes.insert("work".to_string(), node);

        let rule = Rule;
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

        let rule = Rule;
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
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
