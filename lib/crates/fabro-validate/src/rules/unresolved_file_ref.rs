use fabro_graphviz::graph::{AttrValue, Graph};

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "unresolved_file_ref"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        for node in graph.nodes.values() {
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                if prompt.starts_with('@') {
                    diagnostics.push(Diagnostic {
                        rule: self.name().to_string(),
                        severity: Severity::Error,
                        message: format!(
                            "Node '{}' has unresolved file reference: {prompt}",
                            node.id
                        ),
                        node_id: Some(node.id.clone()),
                        edge: None,
                        fix: Some("Check that the path is relative to the workflow file's directory and the file exists".to_string()),

                    ..Diagnostic::default()});
                }
            }
        }

        if let Some(AttrValue::String(goal)) = graph.attrs.get("goal") {
            if goal.starts_with('@') {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: format!("Graph goal has unresolved file reference: {goal}"),
                    node_id: None,
                    edge: None,
                    fix: Some("Check that the path is relative to the workflow file's directory and the file exists".to_string()),

                ..Diagnostic::default()});
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
    fn unresolved_file_ref_rule_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("@prompts/simplify.md".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("@prompts/simplify.md"));
        assert_eq!(d[0].node_id, Some("work".to_string()));
    }

    #[test]
    fn unresolved_file_ref_rule_goal() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "goal".to_string(),
            AttrValue::String("@goal.md".to_string()),
        );

        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("@goal.md"));
    }

    #[test]
    fn unresolved_file_ref_rule_resolved_prompt() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the work".to_string()),
        );
        g.nodes.insert("work".to_string(), node);

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
