use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
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
                        rule: self.name().to_string(),
                        severity: Severity::Error,
                        message: format!(
                            "Exit node '{}' has {} outgoing edge(s) but must have none",
                            node.id,
                            outgoing.len()
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some("Remove outgoing edges from the exit node".to_string()),

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
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn exit_no_outgoing_rule_with_outgoing() {
        let mut g = minimal_graph();
        g.edges.push(Edge::new("exit", "start"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn exit_no_outgoing_rule_clean() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

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
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].node_id, Some("end".to_string()));
    }

    // --- condition_syntax: bare key (truthy check) is valid ---

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
        let rule = Rule;
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
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].node_id, Some("End".to_string()));
    }

    // --- stylesheet_syntax: valid multi-rule stylesheet ---
}
