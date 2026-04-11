use fabro_graphviz::graph::{AttrValue, Graph};

use super::Transform;
use crate::error::FabroError;

/// Resolves model aliases to canonical IDs and infers the provider from the
/// model catalog.
pub struct ModelResolutionTransform;

impl Transform for ModelResolutionTransform {
    fn apply(&self, graph: Graph) -> Result<Graph, FabroError> {
        let mut graph = graph;
        for node in graph.nodes.values_mut() {
            let model = node
                .attrs
                .get("model")
                .and_then(AttrValue::as_str)
                .map(String::from);
            if let Some(model) = model {
                if let Some(info) = fabro_model::Catalog::builtin().get(&model) {
                    let canonical_id = info.id.clone();
                    let provider = info.provider.to_string();
                    // Resolve alias to canonical model ID
                    if model != canonical_id {
                        node.attrs
                            .insert("model".to_string(), AttrValue::String(canonical_id));
                    }
                    if !node.attrs.contains_key("provider") {
                        node.attrs
                            .insert("provider".to_string(), AttrValue::String(provider));
                    }
                }
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
    fn provider_inference_sets_provider_from_catalog() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("claude-sonnet-4-5".to_string()),
        );
        graph.nodes.insert("a".to_string(), node);

        let graph = ModelResolutionTransform.apply(graph).unwrap();

        assert_eq!(
            graph.nodes["a"]
                .attrs
                .get("provider")
                .and_then(AttrValue::as_str),
            Some("anthropic")
        );
    }

    #[test]
    fn provider_inference_does_not_override_explicit_provider() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("claude-sonnet-4-5".to_string()),
        );
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("custom".to_string()),
        );
        graph.nodes.insert("a".to_string(), node);

        let graph = ModelResolutionTransform.apply(graph).unwrap();

        assert_eq!(
            graph.nodes["a"]
                .attrs
                .get("provider")
                .and_then(AttrValue::as_str),
            Some("custom")
        );
    }

    #[test]
    fn provider_inference_unknown_model_leaves_no_provider() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("unknown-model-xyz".to_string()),
        );
        graph.nodes.insert("a".to_string(), node);

        let graph = ModelResolutionTransform.apply(graph).unwrap();

        assert_eq!(graph.nodes["a"].attrs.get("provider"), None);
    }

    #[test]
    fn provider_inference_no_model_no_change() {
        let mut graph = Graph::new("test");
        let node = Node::new("a");
        graph.nodes.insert("a".to_string(), node);

        let graph = ModelResolutionTransform.apply(graph).unwrap();

        assert_eq!(graph.nodes["a"].attrs.get("provider"), None);
    }

    #[test]
    fn model_resolution_resolves_alias_to_canonical_id() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.attrs
            .insert("model".to_string(), AttrValue::String("gpt-54".to_string()));
        graph.nodes.insert("a".to_string(), node);

        let graph = ModelResolutionTransform.apply(graph).unwrap();

        assert_eq!(
            graph.nodes["a"]
                .attrs
                .get("model")
                .and_then(AttrValue::as_str),
            Some("gpt-5.4")
        );
        assert_eq!(
            graph.nodes["a"]
                .attrs
                .get("provider")
                .and_then(AttrValue::as_str),
            Some("openai")
        );
    }

    #[test]
    fn model_resolution_keeps_canonical_id_unchanged() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("gpt-5.4".to_string()),
        );
        graph.nodes.insert("a".to_string(), node);

        let graph = ModelResolutionTransform.apply(graph).unwrap();

        assert_eq!(
            graph.nodes["a"]
                .attrs
                .get("model")
                .and_then(AttrValue::as_str),
            Some("gpt-5.4")
        );
    }
}
