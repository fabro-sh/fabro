use fabro_graphviz::graph::{AttrValue, Graph};

use super::Transform;
use crate::error::Error;

/// For nodes whose fidelity is not `Full`, prepend a context mode preamble to
/// the prompt.
pub struct PreambleTransform;

impl Transform for PreambleTransform {
    fn apply(&self, graph: Graph) -> Result<Graph, Error> {
        use crate::context::keys::Fidelity;

        let mut graph = graph;
        let default_fidelity = graph
            .default_fidelity()
            .and_then(|s| s.parse::<Fidelity>().ok())
            .unwrap_or(Fidelity::Full);
        for node in graph.nodes.values_mut() {
            let fidelity = node
                .fidelity()
                .and_then(|s| s.parse::<Fidelity>().ok())
                .unwrap_or(default_fidelity);
            if fidelity == Fidelity::Full {
                continue;
            }
            let preamble = format!("[Context mode: {fidelity}]\n");
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                let new_prompt = format!("{preamble}{prompt}");
                node.attrs
                    .insert("prompt".to_string(), AttrValue::String(new_prompt));
            }
        }

        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Graph, Node};

    use super::*;

    #[test]
    fn preamble_transform_prepends_for_non_full_fidelity() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the thing".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let graph = PreambleTransform.apply(graph).unwrap();

        let prompt = graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "[Context mode: truncate]\nDo the thing");
    }

    #[test]
    fn preamble_transform_skips_full_fidelity() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the thing".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let graph = PreambleTransform.apply(graph).unwrap();

        let prompt = graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Do the thing");
    }

    #[test]
    fn preamble_transform_uses_graph_default_fidelity() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("compact".to_string()),
        );
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the thing".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let graph = PreambleTransform.apply(graph).unwrap();

        let prompt = graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "[Context mode: compact]\nDo the thing");
    }

    #[test]
    fn preamble_transform_no_prompt_skips() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let graph = PreambleTransform.apply(graph).unwrap();

        assert!(!graph.nodes["work"].attrs.contains_key("prompt"));
    }
}
