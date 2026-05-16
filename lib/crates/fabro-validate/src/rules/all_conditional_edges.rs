use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
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
    fn all_conditional_edges_rule_all_conditional() {
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
    fn all_conditional_edges_rule_only_unconditional() {
        let g = minimal_graph(); // start -> exit is unconditional
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn all_conditional_edges_rule_exit_node_no_outgoing() {
        let g = minimal_graph(); // exit has no outgoing edges
        let rule = Rule;
        let d = rule.apply(&g);
        // exit node has no outgoing edges, so rule doesn't fire
        assert!(d.is_empty());
    }
}
