use fabro_graphviz::graph::{AttrValue, Graph, is_llm_handler_type};

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "prompt_on_llm_nodes"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if is_llm_handler_type(node.handler_type()) {
                let has_prompt = node.prompt().is_some_and(|p| !p.is_empty());
                let has_label = node
                    .attrs
                    .get("label")
                    .and_then(AttrValue::as_str)
                    .is_some_and(|l| !l.is_empty());
                if !has_prompt && !has_label {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Warning,
                        message: format!("LLM node '{}' has no prompt or label attribute", node.id),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some("Add a prompt or label attribute".to_string()),

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
    use fabro_graphviz::graph::{AttrValue, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn prompt_on_llm_nodes_rule_no_prompt_no_label() {
        let mut g = minimal_graph();
        let node = Node::new("work");
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn prompt_on_llm_nodes_rule_with_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the thing".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn prompt_on_llm_nodes_rule_with_label() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "label".to_string(),
            AttrValue::String("Do something".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn prompt_on_llm_nodes_rule_non_codergen_no_warning() {
        let mut g = minimal_graph();
        let mut node = Node::new("gate");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        g.nodes.insert("gate".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: fidelity_valid edge and graph-level ---

    #[test]
    fn prompt_on_llm_nodes_rule_explicit_agent_type_no_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("type".to_string(), AttrValue::String("agent".to_string()));
        // No shape=box, but explicit type=agent
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("diamond".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    // --- goal_gate_has_retry: satisfied by graph-level fallback_retry_target ---

    #[test]
    fn prompt_on_llm_nodes_rule_empty_prompt_no_label() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("prompt".to_string(), AttrValue::String(String::new()));
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    // --- prompt_on_llm_nodes: empty label string still triggers ---

    #[test]
    fn prompt_on_llm_nodes_rule_empty_label_no_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("label".to_string(), AttrValue::String(String::new()));
        g.nodes.insert("work".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    // --- condition_syntax: no condition attribute at all ---
}
