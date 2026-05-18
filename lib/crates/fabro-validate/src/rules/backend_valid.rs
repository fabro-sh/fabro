use fabro_acp::{AcpCommandError, AcpProcessSpec};
use fabro_graphviz::graph::{Graph, Node};
use fabro_types::AgentBackend;

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
                match node.agent_backend() {
                    Some(Err(_)) => {
                        diagnostics.push(unsupported_backend_diagnostic(
                            self.name(),
                            node,
                            backend,
                        ));
                    }
                    Some(Ok(AgentBackend::Acp)) => {
                        diagnostics.extend(validate_acp_node(self.name(), node));
                    }
                    Some(Ok(_)) | None => {}
                }
            }
        }
        diagnostics
    }
}

fn unsupported_backend_diagnostic(rule: &str, node: &Node, backend: &str) -> Diagnostic {
    if backend == "cli" {
        return Diagnostic {
            rule: rule.to_string(),
            severity: Severity::Error,
            message: "backend=\"cli\" is no longer supported; external agents must be launched \
                      through backend=\"acp\" with acp.command or acp.config"
                .to_string(),
            node_id: Some(node.id.clone()),
            edge: None,
            fix: Some(
                "Use backend=\"api\" for Fabro-owned provider execution, or backend=\"acp\" with \
                 acp.command/acp.config for a user-supplied ACP process"
                    .to_string(),
            ),

            ..Diagnostic::default()
        };
    }

    let expected = AgentBackend::expected_values();
    Diagnostic {
        rule: rule.to_string(),
        severity: Severity::Error,
        message: format!("unsupported agent backend \"{backend}\"; expected one of: {expected}"),
        node_id: Some(node.id.clone()),
        edge: None,
        fix: Some(format!("Use one of: {expected}")),

        ..Diagnostic::default()
    }
}

fn validate_acp_node(rule: &str, node: &Node) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    if node.handler_type() != Some("agent") {
        diagnostics.push(Diagnostic {
            rule: rule.to_string(),
            severity: Severity::Error,
            message: "backend=\"acp\" is only valid on agent nodes; prompt nodes are API-only"
                .to_string(),
            node_id: Some(node.id.clone()),
            edge: None,
            fix: Some("Use backend=\"api\" on prompt nodes".to_string()),

            ..Diagnostic::default()
        });
    }

    if let Err(error) = AcpProcessSpec::from_attrs(
        node.legacy_acp_command_attr(),
        node.acp_command_attr(),
        node.acp_config_attr(),
    ) {
        diagnostics.push(acp_process_diagnostic(rule, node, &error));
    }

    let api_only_attrs = api_only_attrs_present(node);
    if !api_only_attrs.is_empty() {
        diagnostics.push(Diagnostic {
            rule: rule.to_string(),
            severity: Severity::Error,
            message: format!(
                "backend=\"acp\" does not support API-only attributes: {}",
                api_only_attrs.join(", ")
            ),
            node_id: Some(node.id.clone()),
            edge: None,
            fix: Some("Remove API model/provider/control attributes from ACP nodes".to_string()),

            ..Diagnostic::default()
        });
    }

    diagnostics
}

fn acp_process_diagnostic(rule: &str, node: &Node, error: &AcpCommandError) -> Diagnostic {
    match error {
        AcpCommandError::LegacyCommandAttribute => Diagnostic {
            rule: rule.to_string(),
            severity: Severity::Error,
            message: "acp_command is no longer supported; use acp.command for shell commands or \
                      acp.config for JSON stdio ACP configs"
                .to_string(),
            node_id: Some(node.id.clone()),
            edge: None,
            fix: Some("Rename acp_command to acp.command".to_string()),

            ..Diagnostic::default()
        },
        AcpCommandError::EmptyOverride
        | AcpCommandError::MissingOverride
        | AcpCommandError::InvalidCommandString => Diagnostic {
            rule: rule.to_string(),
            severity: Severity::Error,
            message: render_acp_process_error(error),
            node_id: Some(node.id.clone()),
            edge: None,
            fix: Some(
                "Set acp.command to a shell command, or acp.config to a JSON stdio ACP config"
                    .to_string(),
            ),

            ..Diagnostic::default()
        },
        AcpCommandError::InvalidConfigJson(_)
        | AcpCommandError::InvalidConfigShape(_)
        | AcpCommandError::UnsupportedTransport => Diagnostic {
            rule: rule.to_string(),
            severity: Severity::Error,
            message: format!(
                "acp.config must be a JSON stdio ACP config: {}",
                render_acp_process_error(error)
            ),
            node_id: Some(node.id.clone()),
            edge: None,
            fix: Some(
                "Provide a JSON config with type=\"stdio\", command, and optional args".to_string(),
            ),

            ..Diagnostic::default()
        },
    }
}

fn render_acp_process_error(error: &AcpCommandError) -> String {
    error.to_string()
}

fn api_only_attrs_present(node: &Node) -> Vec<&'static str> {
    const API_ONLY_ATTRS: &[&str] = &[
        "model",
        "provider",
        "reasoning_effort",
        "max_tokens",
        "speed",
    ];
    API_ONLY_ATTRS
        .iter()
        .copied()
        .filter(|attr| node.attrs.contains_key(*attr))
        .collect()
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn backend_valid_accepts_absent_and_api() {
        for backend in [None, Some("api")] {
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
                .contains("unsupported agent backend \"codex\"; expected one of: api, acp")
        );
    }

    #[test]
    fn backend_valid_requires_acp_process_attr_for_acp_backend() {
        let mut graph = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        graph.nodes.insert("work".to_string(), node);

        let diagnostics = Rule.apply(&graph);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert!(
            diagnostics[0]
                .message
                .contains("requires exactly one of acp.command or acp.config")
        );
    }

    #[test]
    fn backend_valid_accepts_acp_backend_with_acp_command_attr() {
        let mut graph = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("agent-acp".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        assert!(Rule.apply(&graph).is_empty());
    }

    #[test]
    fn backend_valid_rejects_cli_backend_with_migration_guidance() {
        let mut graph = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("cli".to_string()));
        graph.nodes.insert("work".to_string(), node);

        let diagnostics = Rule.apply(&graph);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert!(
            diagnostics[0]
                .message
                .contains("backend=\"cli\" is no longer supported")
        );
        assert!(
            diagnostics[0]
                .fix
                .as_deref()
                .unwrap()
                .contains("backend=\"acp\"")
        );
    }

    #[test]
    fn backend_valid_requires_exactly_one_acp_process_attr() {
        let mut missing = minimal_graph();
        let mut missing_node = Node::new("missing");
        missing_node
            .attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        missing.nodes.insert("missing".to_string(), missing_node);

        let diagnostics = Rule.apply(&missing);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("requires exactly one of acp.command or acp.config")
        );

        let mut both = minimal_graph();
        let mut both_node = Node::new("both");
        both_node
            .attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        both_node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("python3 agent.py".to_string()),
        );
        both_node.attrs.insert(
            "acp.config".to_string(),
            AttrValue::String(
                r#"{"type":"stdio","name":"agent","command":"python3","args":["agent.py"]}"#
                    .to_string(),
            ),
        );
        both.nodes.insert("both".to_string(), both_node);

        let diagnostics = Rule.apply(&both);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("requires exactly one of acp.command or acp.config")
        );
    }

    #[test]
    fn backend_valid_rejects_legacy_acp_command_attr() {
        let mut graph = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp_command".to_string(),
            AttrValue::String("python3 agent.py".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let diagnostics = Rule.apply(&graph);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("acp_command is no longer supported")
        );
    }

    #[test]
    fn backend_valid_rejects_acp_on_prompt_nodes_and_api_only_attrs() {
        let mut graph = minimal_graph();
        let mut node = Node::new("prompt");
        node.attrs
            .insert("type".to_string(), AttrValue::String("prompt".to_string()));
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("python3 agent.py".to_string()),
        );
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("gpt-5.4".to_string()),
        );
        node.attrs.insert(
            "reasoning_effort".to_string(),
            AttrValue::String("high".to_string()),
        );
        graph.nodes.insert("prompt".to_string(), node);

        let diagnostics = Rule.apply(&graph);
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("backend=\"acp\" is only valid on agent nodes")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("backend=\"acp\" does not support API-only attributes")
        }));
    }

    #[test]
    fn backend_valid_rejects_invalid_acp_config_but_accepts_json_shaped_command() {
        let mut command_graph = minimal_graph();
        let mut command_node = Node::new("command");
        command_node
            .attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        command_node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String(r#"{"type":"stdio"}"#.to_string()),
        );
        command_graph
            .nodes
            .insert("command".to_string(), command_node);
        assert!(Rule.apply(&command_graph).is_empty());

        let mut config_graph = minimal_graph();
        let mut config_node = Node::new("config");
        config_node
            .attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        config_node.attrs.insert(
            "acp.config".to_string(),
            AttrValue::String("python3 agent.py".to_string()),
        );
        config_graph.nodes.insert("config".to_string(), config_node);

        let diagnostics = Rule.apply(&config_graph);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("acp.config must be a JSON stdio ACP config")
        );
    }

    #[test]
    fn backend_valid_rejects_invalid_acp_command() {
        let mut graph = minimal_graph();
        let mut node = Node::new("command");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("python 'unterminated".to_string()),
        );
        graph.nodes.insert("command".to_string(), node);

        let diagnostics = Rule.apply(&graph);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("failed to parse acp.command as a shell command")
        );
    }
}
