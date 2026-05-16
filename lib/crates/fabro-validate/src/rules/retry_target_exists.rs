use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "retry_target_exists"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if let Some(target) = node.retry_target() {
                if !graph.nodes.contains_key(target) {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Node '{}' has retry_target '{}' that does not exist",
                            node.id, target
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(format!("Define node '{target}' or fix retry_target")),

                        ..Diagnostic::default()
                    });
                }
            }
            if let Some(target) = node.fallback_retry_target() {
                if !graph.nodes.contains_key(target) {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Node '{}' has fallback_retry_target '{}' that does not exist",
                            node.id, target
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(format!(
                            "Define node '{target}' or fix fallback_retry_target"
                        )),

                        ..Diagnostic::default()
                    });
                }
            }
        }
        if let Some(target) = graph.retry_target() {
            if !graph.nodes.contains_key(target) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!("Graph has retry_target '{target}' that does not exist"),
                    node_id: None,
                    edge: None,
                    fix: Some(format!("Define node '{target}' or fix graph retry_target")),

                    ..Diagnostic::default()
                });
            }
        }
        if let Some(target) = graph.fallback_retry_target() {
            if !graph.nodes.contains_key(target) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "Graph has fallback_retry_target '{target}' that does not exist"
                    ),
                    node_id: None,
                    edge: None,
                    fix: Some(format!(
                        "Define node '{target}' or fix graph fallback_retry_target"
                    )),

                    ..Diagnostic::default()
                });
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn retry_target_exists_rule_missing() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn retry_target_exists_rule_valid() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn retry_target_exists_rule_fallback_missing() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("fallback_retry_target"));
    }

    #[test]
    fn retry_target_exists_rule_fallback_valid() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn retry_target_exists_rule_graph_level_missing() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("Graph"));
    }

    #[test]
    fn retry_target_exists_rule_graph_level_valid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn retry_target_exists_rule_graph_fallback_missing() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("fallback_retry_target"));
    }

    #[test]
    fn retry_target_exists_rule_graph_fallback_valid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("exit".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: goal_gate_has_retry with graph-level retry ---

    #[test]
    fn retry_target_exists_rule_both_node_targets_invalid() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("missing_a".to_string()),
        );
        node.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("missing_b".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[1].severity, Severity::Warning);
    }

    // --- retry_target_exists: both graph-level targets invalid ---

    #[test]
    fn retry_target_exists_rule_both_graph_targets_invalid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("missing_a".to_string()),
        );
        g.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("missing_b".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[1].severity, Severity::Warning);
    }

    // --- start_no_incoming: multiple incoming edges ---
}
