use fabro_graphviz::graph::Graph;

use super::stylesheet::{apply_stylesheet, parse_stylesheet};
use super::Transform;

/// Applies the `model_stylesheet` graph attribute to resolve LLM properties for each node.
pub struct StylesheetApplicationTransform;

impl Transform for StylesheetApplicationTransform {
    fn apply(&self, graph: &mut Graph) {
        let stylesheet_text = graph.model_stylesheet().to_string();
        if stylesheet_text.is_empty() {
            return;
        }
        let Ok(stylesheet) = parse_stylesheet(&stylesheet_text) else {
            return;
        };
        apply_stylesheet(&stylesheet, graph);
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{Graph, Node};

    use super::*;

    #[test]
    fn stylesheet_transform_empty_stylesheet() {
        let mut graph = Graph::new("test");
        graph.nodes.insert("a".to_string(), Node::new("a"));

        let transform = StylesheetApplicationTransform;
        // Should not panic with empty stylesheet
        transform.apply(&mut graph);
    }
}
