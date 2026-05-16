use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

const DOT_RESERVED_KEYWORDS: &[&str] = &[
    "graph", "digraph", "subgraph", "node", "edge", "strict", "if",
];

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "reserved_keyword_node_id"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        graph
            .nodes
            .values()
            .filter(|node| DOT_RESERVED_KEYWORDS.contains(&node.id.to_lowercase().as_str()))
            .map(|node| Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Warning,
                message: format!(
                    "Node ID '{}' is a DOT reserved keyword and may cause parsing failures",
                    node.id
                ),
                node_id: Some(node.id.clone()),
                edge: None,
                fix: Some(format!(
                    "Rename '{}' to '{}_step' or another non-reserved ID",
                    node.id,
                    node.id.to_lowercase()
                )),

                ..Diagnostic::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{Edge, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn reserved_keyword_node_id_warns_on_keyword() {
        let mut g = minimal_graph();
        g.nodes.insert("graph".to_string(), Node::new("graph"));
        g.edges.push(Edge::new("start", "graph"));
        g.edges.push(Edge::new("graph", "exit"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].node_id.as_deref(), Some("graph"));
    }

    #[test]
    fn reserved_keyword_node_id_case_insensitive() {
        let mut g = minimal_graph();
        g.nodes.insert("Node".to_string(), Node::new("Node"));
        g.nodes.insert("EDGE".to_string(), Node::new("EDGE"));
        g.edges.push(Edge::new("start", "Node"));
        g.edges.push(Edge::new("Node", "EDGE"));
        g.edges.push(Edge::new("EDGE", "exit"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn reserved_keyword_node_id_normal_id_no_warning() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn reserved_keyword_node_id_multiple_keywords() {
        let mut g = minimal_graph();
        g.nodes.insert("strict".to_string(), Node::new("strict"));
        g.nodes.insert("digraph".to_string(), Node::new("digraph"));
        g.nodes.insert("if".to_string(), Node::new("if"));
        g.edges.push(Edge::new("start", "strict"));
        g.edges.push(Edge::new("strict", "digraph"));
        g.edges.push(Edge::new("digraph", "if"));
        g.edges.push(Edge::new("if", "exit"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 3);
    }
}
