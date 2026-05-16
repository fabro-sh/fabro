use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "freeform_edge_count"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if node.handler_type() == Some("human") {
                let freeform_count = graph
                    .outgoing_edges(&node.id)
                    .iter()
                    .filter(|e| e.freeform())
                    .count();
                if freeform_count > 1 {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Error,
                        message: format!(
                            "wait.human node '{}' has {freeform_count} freeform edges but at most one is allowed",
                            node.id
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(
                            "Remove extra freeform=true edges so at most one remains".to_string(),
                        ),

                    ..Diagnostic::default()});
                }
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
    fn freeform_edge_count_rule_two_freeform() {
        let mut g = minimal_graph();
        let mut gate = Node::new("gate");
        gate.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        g.nodes.insert("gate".to_string(), gate);
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.nodes.insert("b".to_string(), Node::new("b"));

        let mut e1 = Edge::new("gate", "a");
        e1.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        let mut e2 = Edge::new("gate", "b");
        e2.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        g.edges.push(e1);
        g.edges.push(e2);

        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn freeform_edge_count_rule_one_freeform() {
        let mut g = minimal_graph();
        let mut gate = Node::new("gate");
        gate.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        g.nodes.insert("gate".to_string(), gate);
        g.nodes.insert("a".to_string(), Node::new("a"));

        let mut e1 = Edge::new("gate", "a");
        e1.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        g.edges.push(e1);

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn freeform_edge_count_rule_non_wait_human_ignored() {
        let mut g = minimal_graph();
        // Regular codergen node (box shape) with multiple freeform edges should not
        // trigger
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.nodes.insert("b".to_string(), Node::new("b"));
        g.nodes.insert("work".to_string(), Node::new("work"));

        let mut e1 = Edge::new("work", "a");
        e1.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        let mut e2 = Edge::new("work", "b");
        e2.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        g.edges.push(e1);
        g.edges.push(e2);

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn freeform_edge_count_rule_zero_freeform() {
        let mut g = minimal_graph();
        let mut gate = Node::new("gate");
        gate.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));
        g.nodes.insert("gate".to_string(), gate);
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.edges.push(Edge::new("gate", "a"));

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: stylesheet_syntax no stylesheet ---

    #[test]
    fn freeform_edge_count_rule_explicit_type_two_freeform() {
        let mut g = minimal_graph();
        let mut gate = Node::new("gate");
        gate.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));
        g.nodes.insert("gate".to_string(), gate);
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.nodes.insert("b".to_string(), Node::new("b"));

        let mut e1 = Edge::new("gate", "a");
        e1.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        let mut e2 = Edge::new("gate", "b");
        e2.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        g.edges.push(e1);
        g.edges.push(e2);

        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    // --- exit_no_outgoing: by "Exit" capitalized id ---

    #[test]
    fn freeform_edge_count_rule_freeform_false_ignored() {
        let mut g = minimal_graph();
        let mut gate = Node::new("gate");
        gate.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        g.nodes.insert("gate".to_string(), gate);
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.nodes.insert("b".to_string(), Node::new("b"));

        let mut e1 = Edge::new("gate", "a");
        e1.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(false));
        let mut e2 = Edge::new("gate", "b");
        e2.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(false));
        g.edges.push(e1);
        g.edges.push(e2);

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- reachability: chain of reachable nodes ---
}
