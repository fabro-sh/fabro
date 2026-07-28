use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_types::AgentBackend;

use super::Transform;
use crate::error::Error;

/// Materializes the graph-level `acp.command` / `acp.config` defaults onto ACP
/// nodes that do not name an ACP process themselves.
///
/// The two attributes are mutually exclusive, so they resolve as a *pair*
/// rather than attribute by attribute: a node that sets either one keeps its
/// own pair untouched, and only a node that sets neither inherits from the
/// graph. That lets a node switch from a shared `acp.command` to its own
/// `acp.config` without inheriting a conflict from the graph level.
///
/// Only nodes with `backend="acp"` inherit. Copying onto every node would put
/// the attributes on `start` / `exit` and on API nodes, where they are inert
/// but misleading.
pub struct AcpDefaultsTransform;

impl Transform for AcpDefaultsTransform {
    fn apply(&self, graph: Graph) -> Result<Graph, Error> {
        let mut graph = graph;

        let command = graph.acp_command_attr().map(ToString::to_string);
        let config = graph.acp_config_attr().map(ToString::to_string);
        if command.is_none() && config.is_none() {
            return Ok(graph);
        }

        for node in graph.nodes.values_mut() {
            if node.agent_backend() != Some(Ok(AgentBackend::Acp)) {
                continue;
            }
            if node.acp_command_attr().is_some() || node.acp_config_attr().is_some() {
                continue;
            }

            // A graph that sets both is ambiguous. Copy both so the existing
            // "requires exactly one" check reports it, rather than silently
            // picking one.
            if let Some(command) = &command {
                node.attrs.insert(
                    "acp.command".to_string(),
                    AttrValue::String(command.clone()),
                );
            }
            if let Some(config) = &config {
                node.attrs
                    .insert("acp.config".to_string(), AttrValue::String(config.clone()));
            }
        }

        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::Node;

    use super::*;

    fn acp_node(id: &str) -> Node {
        let mut node = Node::new(id);
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node
    }

    fn apply(graph: Graph) -> Graph {
        AcpDefaultsTransform.apply(graph).unwrap()
    }

    fn attr<'a>(graph: &'a Graph, node_id: &str, key: &str) -> Option<&'a str> {
        graph.nodes[node_id]
            .attrs
            .get(key)
            .and_then(AttrValue::as_str)
    }

    #[test]
    fn graph_command_fills_in_acp_nodes_that_set_neither_attribute() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("python3 agent.py".to_string()),
        );
        graph.nodes.insert("work".to_string(), acp_node("work"));

        let graph = apply(graph);

        assert_eq!(
            attr(&graph, "work", "acp.command"),
            Some("python3 agent.py")
        );
    }

    #[test]
    fn graph_config_fills_in_acp_nodes_that_set_neither_attribute() {
        let config = r#"{"type":"stdio","command":"python3","args":["agent.py"]}"#;
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "acp.config".to_string(),
            AttrValue::String(config.to_string()),
        );
        graph.nodes.insert("work".to_string(), acp_node("work"));

        let graph = apply(graph);

        assert_eq!(attr(&graph, "work", "acp.config"), Some(config));
    }

    #[test]
    fn node_command_wins_over_graph_command() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("python3 shared.py".to_string()),
        );
        let mut node = acp_node("work");
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("python3 own.py".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let graph = apply(graph);

        assert_eq!(attr(&graph, "work", "acp.command"), Some("python3 own.py"));
    }

    #[test]
    fn node_config_suppresses_the_graph_command() {
        // The pair resolves per source: a node naming its own process must not
        // inherit the other half from the graph and become ambiguous.
        let config = r#"{"type":"stdio","command":"python3","args":["own.py"]}"#;
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("python3 shared.py".to_string()),
        );
        let mut node = acp_node("work");
        node.attrs.insert(
            "acp.config".to_string(),
            AttrValue::String(config.to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let graph = apply(graph);

        assert_eq!(attr(&graph, "work", "acp.config"), Some(config));
        assert_eq!(attr(&graph, "work", "acp.command"), None);
    }

    #[test]
    fn non_acp_nodes_do_not_inherit() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("python3 agent.py".to_string()),
        );
        graph.nodes.insert("start".to_string(), Node::new("start"));
        let mut api_node = Node::new("ask");
        api_node
            .attrs
            .insert("backend".to_string(), AttrValue::String("api".to_string()));
        graph.nodes.insert("ask".to_string(), api_node);

        let graph = apply(graph);

        assert_eq!(attr(&graph, "start", "acp.command"), None);
        assert_eq!(attr(&graph, "ask", "acp.command"), None);
    }

    #[test]
    fn graph_without_acp_attributes_is_unchanged() {
        let mut graph = Graph::new("test");
        graph.nodes.insert("work".to_string(), acp_node("work"));

        let graph = apply(graph);

        assert_eq!(attr(&graph, "work", "acp.command"), None);
        assert_eq!(attr(&graph, "work", "acp.config"), None);
    }
}
