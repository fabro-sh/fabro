use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
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
                message:
                    "Pipeline must have exactly one terminal node (shape=Msquare or id exit/end)"
                        .to_string(),
                node_id: None,
                edge: None,
                fix: Some("Add a node with shape=Msquare or id 'exit'/'end'".to_string()),

                ..Diagnostic::default()
            }];
        }
        if terminal_count > 1 {
            return vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message: format!(
                    "Pipeline must have exactly one terminal node, found {terminal_count}"
                ),
                node_id: None,
                edge: None,
                fix: Some("Remove extra terminal nodes so exactly one remains".to_string()),

                ..Diagnostic::default()
            }];
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Graph, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn terminal_node_rule_no_terminal() {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn terminal_node_rule_with_terminal() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn terminal_node_rule_by_exit_id() {
        let mut g = Graph::new("test");
        // Node with id "exit" but no Msquare shape
        let node = Node::new("exit");
        g.nodes.insert("exit".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn terminal_node_rule_by_end_id() {
        let mut g = Graph::new("test");
        let node = Node::new("end");
        g.nodes.insert("end".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn terminal_node_rule_by_capitalized_end_id() {
        let mut g = Graph::new("test");
        let node = Node::new("End");
        g.nodes.insert("End".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

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
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("exactly one"));
    }

    // --- Additional coverage: edge_target_exists missing source ---

    #[test]
    fn terminal_node_rule_by_exit_capitalized_id() {
        let mut g = Graph::new("test");
        let node = Node::new("Exit");
        g.nodes.insert("Exit".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- edge_target_exists: both source and target missing ---
}
