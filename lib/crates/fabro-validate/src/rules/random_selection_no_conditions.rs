use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
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
        let rule = Rule;
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
            AttrValue::String("outcome=succeeded".to_string()),
        );
        g.edges.push(e);
        g.edges.push(Edge::new("pick", "start"));
        let rule = Rule;
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
            AttrValue::String("outcome=succeeded".to_string()),
        );
        g.edges.push(e);
        g.edges.push(Edge::new("gate", "start"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
