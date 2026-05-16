use std::collections::{HashSet, VecDeque};

use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "reachability"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let Some(start) = graph.find_start_node() else {
            return Vec::new();
        };

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(start.id.clone());
        visited.insert(start.id.clone());

        while let Some(node_id) = queue.pop_front() {
            for edge in graph.outgoing_edges(&node_id) {
                if visited.insert(edge.to.clone()) {
                    queue.push_back(edge.to.clone());
                }
            }
        }

        let mut unreachable: Vec<&str> = graph
            .nodes
            .keys()
            .filter(|id| !visited.contains(id.as_str()))
            .map(std::string::String::as_str)
            .collect();
        unreachable.sort_unstable();

        unreachable
            .into_iter()
            .map(|node_id| Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Warning,
                message: format!("Node '{node_id}' is not reachable from the start node"),
                node_id: Some(node_id.to_string()),
                edge: None,
                fix: Some(format!(
                    "Add an edge path from the start node to '{node_id}'"
                )),

                ..Diagnostic::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{Edge, Graph, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn reachability_rule_unreachable_node() {
        let mut g = minimal_graph();
        g.nodes.insert("orphan".to_string(), Node::new("orphan"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].node_id, Some("orphan".to_string()));
    }

    #[test]
    fn reachability_rule_all_reachable() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn reachability_rule_no_start_node() {
        let mut g = Graph::new("test");
        g.nodes.insert("orphan".to_string(), Node::new("orphan"));
        let rule = Rule;
        let d = rule.apply(&g);
        // No start node found, rule returns empty
        assert!(d.is_empty());
    }

    // --- Additional coverage: retry_target_exists fallback and graph-level ---

    #[test]
    fn reachability_rule_multiple_unreachable() {
        let mut g = minimal_graph();
        g.nodes
            .insert("orphan_a".to_string(), Node::new("orphan_a"));
        g.nodes
            .insert("orphan_b".to_string(), Node::new("orphan_b"));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[1].severity, Severity::Warning);
    }

    // --- type_known: all known handler types are accepted ---

    #[test]
    fn reachability_rule_chain_all_reachable() {
        let mut g = minimal_graph();
        g.nodes.insert("a".to_string(), Node::new("a"));
        g.nodes.insert("b".to_string(), Node::new("b"));
        g.edges = vec![
            Edge::new("start", "a"),
            Edge::new("a", "b"),
            Edge::new("b", "exit"),
        ];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- edge_target_exists: no edges at all ---
}
