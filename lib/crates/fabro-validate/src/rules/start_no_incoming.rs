use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "start_no_incoming"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let Some(start) = graph.find_start_node() else {
            return Vec::new();
        };
        let incoming = graph.incoming_edges(&start.id);
        if !incoming.is_empty() {
            return vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message: format!(
                    "Start node '{}' has {} incoming edge(s) but must have none",
                    start.id,
                    incoming.len()
                ),
                node_id: Some(start.id.clone()),
                edge: None,
                fix: Some("Remove incoming edges to the start node".to_string()),

                ..Diagnostic::default()
            }];
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{Edge, Graph, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn start_no_incoming_rule_with_incoming() {
        let mut g = minimal_graph();
        g.edges.push(Edge::new("exit", "start"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn start_no_incoming_rule_clean() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn start_no_incoming_rule_no_start_node() {
        let g = Graph::new("test");
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: exit_no_outgoing by id variants ---

    #[test]
    fn start_no_incoming_rule_multiple_incoming() {
        let mut g = minimal_graph();
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.edges.push(Edge::new("exit", "start"));
        g.edges.push(Edge::new("a", "start"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains('2'));
    }

    // --- prompt_on_llm_nodes: empty prompt string still triggers ---
}
