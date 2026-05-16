use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl Rule {
    const FIX: &str = "Add fidelity=\"full\" to enable session reuse, or remove thread_id";
}

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "thread_id_requires_fidelity_full"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let graph_default_full = graph.default_fidelity() == Some("full");

        for node in graph.nodes.values() {
            if node.thread_id().is_some() && node.fidelity() != Some("full") && !graph_default_full
            {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "Node '{}' has thread_id but fidelity is not 'full'",
                        node.id
                    ),
                    node_id: Some(node.id.clone()),
                    edge: None,
                    fix: Some(Self::FIX.to_string()),

                    ..Diagnostic::default()
                });
            }
        }

        for edge in &graph.edges {
            if edge.thread_id().is_some() {
                let edge_full = edge.fidelity() == Some("full");
                let target_full =
                    graph.nodes.get(&edge.to).and_then(|n| n.fidelity()) == Some("full");
                if !edge_full && !target_full && !graph_default_full {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Edge {} -> {} has thread_id but fidelity is not 'full'",
                            edge.from, edge.to
                        ),
                        node_id: None,
                        edge: Some((edge.from.clone(), edge.to.clone())),
                        fix: Some(Self::FIX.to_string()),

                        ..Diagnostic::default()
                    });
                }
            }
        }

        if graph.default_thread().is_some() && !graph_default_full {
            diagnostics.push(Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Warning,
                message: "Graph has default_thread but default_fidelity is not 'full'".to_string(),
                node_id: None,
                edge: None,
                fix: Some(Self::FIX.to_string()),

                ..Diagnostic::default()
            });
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
    fn thread_id_requires_fidelity_full_node_warns() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].node_id, Some("work".to_string()));
    }

    #[test]
    fn thread_id_requires_fidelity_full_node_ok() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn thread_id_requires_fidelity_full_node_graph_default_ok() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn thread_id_requires_fidelity_full_edge_warns() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.edges = vec![edge];

        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].edge, Some(("start".to_string(), "exit".to_string())));
    }

    #[test]
    fn thread_id_requires_fidelity_full_edge_ok() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        edge.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        g.edges = vec![edge];

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn thread_id_requires_fidelity_full_edge_target_node_ok() {
        let mut g = minimal_graph();
        if let Some(exit_node) = g.nodes.get_mut("exit") {
            exit_node.attrs.insert(
                "fidelity".to_string(),
                AttrValue::String("full".to_string()),
            );
        }
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.edges = vec![edge];

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn thread_id_requires_fidelity_full_graph_warns() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("session1".to_string()),
        );

        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].node_id.is_none());
        assert!(d[0].edge.is_none());
    }

    #[test]
    fn thread_id_requires_fidelity_full_graph_ok() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("session1".to_string()),
        );
        g.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
