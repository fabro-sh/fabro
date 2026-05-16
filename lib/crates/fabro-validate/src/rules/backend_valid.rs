use fabro_graphviz::graph::{Graph, Node};
use fabro_types::LlmBackend;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "backend_valid"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if let Some(backend) = node.backend() {
                match node.llm_backend() {
                    Some(Err(_)) => {
                        let expected = LlmBackend::expected_values();
                        diagnostics.push(Diagnostic {
                            rule: self.name().to_string(),
                            severity: Severity::Error,
                            message: format!(
                                "unsupported LLM backend \"{backend}\"; expected one of: {expected}"
                            ),
                            node_id: Some(node.id.clone()),
                            edge: None,
                            fix: Some(format!("Use one of: {expected}")),

                            ..Diagnostic::default()
                        });
                    }
                    Some(Ok(LlmBackend::Acp)) if acp_command_missing(node) => {
                        diagnostics.push(Diagnostic {
                            rule: self.name().to_string(),
                            severity: Severity::Error,
                            message: "backend=\"acp\" requires acp_command because Fabro does \
                                       not install ACP agents"
                                .to_string(),
                            node_id: Some(node.id.clone()),
                            edge: None,
                            fix: Some(
                                "Set acp_command to a stdio ACP command available in the sandbox"
                                    .to_string(),
                            ),

                            ..Diagnostic::default()
                        });
                    }
                    Some(Ok(_)) | None => {}
                }
            }
        }
        diagnostics
    }
}

fn acp_command_missing(node: &Node) -> bool {
    match node.acp_command() {
        Some(command) => command.trim().is_empty(),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn backend_valid_accepts_absent_api_and_cli() {
        for backend in [None, Some("api"), Some("cli")] {
            let mut graph = minimal_graph();
            let mut node = Node::new("work");
            if let Some(backend) = backend {
                node.attrs.insert(
                    "backend".to_string(),
                    AttrValue::String(backend.to_string()),
                );
            }
            graph.nodes.insert("work".to_string(), node);

            assert!(Rule.apply(&graph).is_empty(), "backend: {backend:?}");
        }
    }

    #[test]
    fn backend_valid_rejects_unknown_backend() {
        let mut graph = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "backend".to_string(),
            AttrValue::String("codex".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let diagnostics = Rule.apply(&graph);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert!(
            diagnostics[0]
                .message
                .contains("unsupported LLM backend \"codex\"; expected one of: api, cli, acp")
        );
    }

    #[test]
    fn backend_valid_requires_acp_command_for_acp_backend() {
        let mut graph = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        graph.nodes.insert("work".to_string(), node);

        let diagnostics = Rule.apply(&graph);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert!(diagnostics[0].message.contains(
            "backend=\"acp\" requires acp_command because Fabro does not install ACP agents"
        ));
    }

    #[test]
    fn backend_valid_accepts_acp_backend_with_acp_command() {
        let mut graph = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp_command".to_string(),
            AttrValue::String("agent-acp".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        assert!(Rule.apply(&graph).is_empty());
    }
}
