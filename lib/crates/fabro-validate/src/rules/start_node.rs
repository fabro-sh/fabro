use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "start_node"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let start_count = graph
            .nodes
            .iter()
            .filter(|(id, n)| n.shape() == "Mdiamond" || *id == "start" || *id == "Start")
            .count();
        if start_count == 0 {
            return vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message:
                    "Pipeline must have exactly one start node (shape=Mdiamond or id start/Start)"
                        .to_string(),
                node_id: None,
                edge: None,
                fix: Some("Add a node with shape=Mdiamond or id 'start'".to_string()),

                ..Diagnostic::default()
            }];
        }
        if start_count > 1 {
            return vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message: format!(
                    "Pipeline has {start_count} start nodes but must have exactly one"
                ),
                node_id: None,
                edge: None,
                fix: Some("Remove extra start nodes".to_string()),

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
    fn start_node_rule_no_start() {
        let g = Graph::new("test");
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn start_node_rule_two_starts() {
        let mut g = Graph::new("test");
        let mut s1 = Node::new("s1");
        s1.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        let mut s2 = Node::new("s2");
        s2.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("s1".to_string(), s1);
        g.nodes.insert("s2".to_string(), s2);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn start_node_rule_one_start() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn start_node_rule_by_id() {
        let mut g = Graph::new("test");
        // Node with id "start" but no Mdiamond shape
        let node = Node::new("start");
        g.nodes.insert("start".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn start_node_rule_by_capitalized_id() {
        let mut g = Graph::new("test");
        let node = Node::new("Start");
        g.nodes.insert("Start".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
