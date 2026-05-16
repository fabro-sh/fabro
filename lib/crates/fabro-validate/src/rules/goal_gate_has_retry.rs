use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "goal_gate_has_retry"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if node.goal_gate() {
                let has_node_retry =
                    node.retry_target().is_some() || node.fallback_retry_target().is_some();
                let has_graph_retry =
                    graph.retry_target().is_some() || graph.fallback_retry_target().is_some();
                if !has_node_retry && !has_graph_retry {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!(
                            "Node '{}' has goal_gate=true but no retry_target or fallback_retry_target",
                            node.id
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some(
                            "Add retry_target or fallback_retry_target attribute".to_string(),
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
    use fabro_graphviz::graph::{AttrValue, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn goal_gate_has_retry_rule_no_retry() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn goal_gate_has_retry_rule_with_retry() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
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
    fn goal_gate_has_retry_rule_with_graph_retry_target() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), node);
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn goal_gate_has_retry_rule_with_fallback_retry_target() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
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
    fn goal_gate_has_retry_rule_not_goal_gate() {
        let mut g = minimal_graph();
        let node = Node::new("work");
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: prompt_on_llm_nodes with label ---

    #[test]
    fn goal_gate_has_retry_rule_with_graph_fallback_retry_target() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), node);
        g.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("start".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- freeform_edge_count: with explicit type=human ---

    #[test]
    fn goal_gate_has_retry_rule_explicit_false() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(false));
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- stylesheet_syntax: empty string stylesheet ---
}
