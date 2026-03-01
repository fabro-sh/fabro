use crate::graph::{AttrValue, Edge, Graph, Node};
use crate::stylesheet::{apply_stylesheet, parse_stylesheet};

/// A transform that modifies the pipeline graph after parsing and before validation.
pub trait Transform {
    fn apply(&self, graph: &mut Graph);
}

/// Expands `$goal` in node `prompt` attributes to the graph-level `goal` value.
pub struct VariableExpansionTransform;

impl Transform for VariableExpansionTransform {
    fn apply(&self, graph: &mut Graph) {
        let goal = graph.goal().to_string();
        for node in graph.nodes.values_mut() {
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                if prompt.contains("$goal") {
                    let expanded = prompt.replace("$goal", &goal);
                    node.attrs
                        .insert("prompt".to_string(), AttrValue::String(expanded));
                }
            }
        }
    }
}

/// For nodes whose fidelity is not "full", prepend a context mode preamble to the prompt.
pub struct PreambleTransform;

impl Transform for PreambleTransform {
    fn apply(&self, graph: &mut Graph) {
        let default_fidelity = graph.default_fidelity().unwrap_or("full").to_string();
        for node in graph.nodes.values_mut() {
            let fidelity = node.fidelity().unwrap_or(&default_fidelity).to_string();
            if fidelity == "full" {
                continue;
            }
            let preamble = format!("[Context mode: {fidelity}]\n");
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                let new_prompt = format!("{preamble}{prompt}");
                node.attrs
                    .insert("prompt".to_string(), AttrValue::String(new_prompt));
            }
        }
    }
}

/// Merges nodes and edges from secondary graphs into the primary graph.
/// Node IDs from secondary graphs are prefixed with a namespace to avoid collisions.
pub struct GraphMergeTransform {
    secondary_graphs: Vec<Graph>,
}

impl GraphMergeTransform {
    #[must_use]
    pub const fn new(secondary_graphs: Vec<Graph>) -> Self {
        Self { secondary_graphs }
    }
}

impl Transform for GraphMergeTransform {
    fn apply(&self, graph: &mut Graph) {
        for secondary in &self.secondary_graphs {
            let prefix = &secondary.name;

            for (id, node) in &secondary.nodes {
                let prefixed_id = format!("{prefix}.{id}");
                let mut merged_node = Node::new(&prefixed_id);
                merged_node.attrs = node.attrs.clone();
                merged_node.classes = node.classes.clone();
                graph.nodes.insert(prefixed_id, merged_node);
            }

            for edge in &secondary.edges {
                let mut merged_edge = Edge::new(
                    format!("{prefix}.{}", edge.from),
                    format!("{prefix}.{}", edge.to),
                );
                merged_edge.attrs = edge.attrs.clone();
                graph.edges.push(merged_edge);
            }
        }
    }
}

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
    use super::*;

    #[test]
    fn variable_expansion_replaces_goal() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );

        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Achieve: $goal now".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = VariableExpansionTransform;
        transform.apply(&mut graph);

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Achieve: Fix bugs now");
    }

    #[test]
    fn variable_expansion_no_goal_variable() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );

        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do something".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = VariableExpansionTransform;
        transform.apply(&mut graph);

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Do something");
    }

    #[test]
    fn variable_expansion_empty_goal() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Goal: $goal".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = VariableExpansionTransform;
        transform.apply(&mut graph);

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: ");
    }

    #[test]
    fn variable_expansion_no_prompt() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );
        let node = Node::new("plan");
        graph.nodes.insert("plan".to_string(), node);

        let transform = VariableExpansionTransform;
        // Should not panic
        transform.apply(&mut graph);
        assert!(!graph.nodes["plan"].attrs.contains_key("prompt"));
    }

    #[test]
    fn stylesheet_transform_empty_stylesheet() {
        let mut graph = Graph::new("test");
        graph.nodes.insert("a".to_string(), Node::new("a"));

        let transform = StylesheetApplicationTransform;
        // Should not panic with empty stylesheet
        transform.apply(&mut graph);
    }

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

        PreambleTransform.apply(&mut graph);

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

        PreambleTransform.apply(&mut graph);

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

        PreambleTransform.apply(&mut graph);

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

        PreambleTransform.apply(&mut graph);

        assert!(!graph.nodes["work"].attrs.contains_key("prompt"));
    }

    // -----------------------------------------------------------------------
    // GraphMergeTransform tests
    // -----------------------------------------------------------------------

    #[test]
    fn graph_merge_combines_nodes_and_edges() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("a".to_string(), Node::new("a"));
        primary.nodes.insert("b".to_string(), Node::new("b"));
        primary.edges.push(Edge::new("a", "b"));

        let mut secondary = Graph::new("secondary");
        secondary.nodes.insert("x".to_string(), Node::new("x"));
        secondary.nodes.insert("y".to_string(), Node::new("y"));
        secondary.edges.push(Edge::new("x", "y"));

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        // Primary should now have 4 nodes: a, b, secondary.x, secondary.y
        assert_eq!(primary.nodes.len(), 4);
        assert!(primary.nodes.contains_key("secondary.x"));
        assert!(primary.nodes.contains_key("secondary.y"));
        // Should have 2 edges: a->b and secondary.x->secondary.y
        assert_eq!(primary.edges.len(), 2);
    }

    #[test]
    fn graph_merge_prefixes_node_ids_to_avoid_collisions() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("work".to_string(), Node::new("work"));

        let mut secondary = Graph::new("sub");
        secondary
            .nodes
            .insert("work".to_string(), Node::new("work"));

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        // Primary "work" is preserved, secondary "work" becomes "sub.work"
        assert!(primary.nodes.contains_key("work"));
        assert!(primary.nodes.contains_key("sub.work"));
        assert_eq!(primary.nodes.len(), 2);
    }

    #[test]
    fn graph_merge_remaps_edges_to_prefixed_ids() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("a".to_string(), Node::new("a"));

        let mut secondary = Graph::new("sub");
        secondary.nodes.insert("x".to_string(), Node::new("x"));
        secondary.nodes.insert("y".to_string(), Node::new("y"));
        secondary.edges.push(Edge::new("x", "y"));

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        // The edge from secondary should be remapped to sub.x -> sub.y
        let merged_edge = primary
            .edges
            .iter()
            .find(|e| e.from == "sub.x")
            .expect("should have edge from sub.x");
        assert_eq!(merged_edge.to, "sub.y");
    }

    #[test]
    fn graph_merge_preserves_primary_attributes() {
        let mut primary = Graph::new("primary");
        primary.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Build feature".to_string()),
        );
        primary.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { llm_model: sonnet; }".to_string()),
        );

        let mut secondary = Graph::new("sub");
        secondary.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Sub goal".to_string()),
        );
        secondary.nodes.insert("x".to_string(), Node::new("x"));

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        assert_eq!(primary.goal(), "Build feature");
        assert_eq!(primary.model_stylesheet(), "* { llm_model: sonnet; }");
    }

    #[test]
    fn graph_merge_empty_secondary_is_noop() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("a".to_string(), Node::new("a"));
        primary.edges.push(Edge::new("a", "a"));

        let secondary = Graph::new("empty");

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        assert_eq!(primary.nodes.len(), 1);
        assert_eq!(primary.edges.len(), 1);
    }

    #[test]
    fn graph_merge_multiple_secondary_graphs() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("a".to_string(), Node::new("a"));

        let mut sub1 = Graph::new("sub1");
        sub1.nodes.insert("n1".to_string(), Node::new("n1"));

        let mut sub2 = Graph::new("sub2");
        sub2.nodes.insert("n2".to_string(), Node::new("n2"));

        let transform = GraphMergeTransform::new(vec![sub1, sub2]);
        transform.apply(&mut primary);

        assert_eq!(primary.nodes.len(), 3);
        assert!(primary.nodes.contains_key("a"));
        assert!(primary.nodes.contains_key("sub1.n1"));
        assert!(primary.nodes.contains_key("sub2.n2"));
    }

    #[test]
    fn graph_merge_preserves_node_attributes() {
        let mut primary = Graph::new("primary");

        let mut secondary = Graph::new("sub");
        let mut node = Node::new("worker");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the work".to_string()),
        );
        node.attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        secondary.nodes.insert("worker".to_string(), node);

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        let merged = &primary.nodes["sub.worker"];
        assert_eq!(merged.id, "sub.worker");
        assert_eq!(
            merged.attrs.get("prompt").and_then(AttrValue::as_str),
            Some("Do the work")
        );
        assert_eq!(
            merged.attrs.get("shape").and_then(AttrValue::as_str),
            Some("box")
        );
    }

    #[test]
    fn graph_merge_preserves_edge_attributes() {
        let mut primary = Graph::new("primary");

        let mut secondary = Graph::new("sub");
        secondary.nodes.insert("x".to_string(), Node::new("x"));
        secondary.nodes.insert("y".to_string(), Node::new("y"));
        let mut edge = Edge::new("x", "y");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        secondary.edges.push(edge);

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        let merged_edge = primary
            .edges
            .iter()
            .find(|e| e.from == "sub.x")
            .expect("should have merged edge");
        assert_eq!(merged_edge.to, "sub.y");
        assert_eq!(
            merged_edge
                .attrs
                .get("condition")
                .and_then(AttrValue::as_str),
            Some("outcome=success")
        );
    }
}
