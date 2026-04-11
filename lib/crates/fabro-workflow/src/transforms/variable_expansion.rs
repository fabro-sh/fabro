use std::collections::HashMap;

use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_template::{TemplateContext, render as render_template};

use super::Transform;
use crate::error::FabroError;

/// Expands `{{ goal }}` / `{{ inputs.* }}` across all string attributes.
pub struct TemplateTransform {
    pub inputs: HashMap<String, toml::Value>,
}

impl TemplateTransform {
    fn render_attrs(
        attrs: &mut HashMap<String, AttrValue>,
        ctx: &TemplateContext,
    ) -> Result<(), FabroError> {
        for value in attrs.values_mut() {
            if let AttrValue::String(text) = value {
                let rendered = render_template(text, ctx)
                    .map_err(|error| FabroError::Validation(error.to_string()))?;
                *text = rendered;
            }
        }
        Ok(())
    }

    fn resolved_goal(&self, graph: &Graph) -> Result<String, FabroError> {
        let ctx = TemplateContext::new()
            .with_goal("{{ goal }}")
            .with_inputs(self.inputs.clone());
        render_template(graph.goal(), &ctx)
            .map_err(|error| FabroError::Validation(error.to_string()))
    }
}

impl Transform for TemplateTransform {
    fn apply(&self, graph: Graph) -> Result<Graph, FabroError> {
        let mut graph = graph;
        let resolved_goal = self.resolved_goal(&graph)?;
        let ctx = TemplateContext::new()
            .with_goal(resolved_goal)
            .with_inputs(self.inputs.clone());

        Self::render_attrs(&mut graph.attrs, &ctx)?;
        for node in graph.nodes.values_mut() {
            Self::render_attrs(&mut node.attrs, &ctx)?;
        }
        for edge in &mut graph.edges {
            Self::render_attrs(&mut edge.attrs, &ctx)?;
        }

        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};

    use super::*;

    #[test]
    fn template_transform_replaces_goal_and_inputs_across_string_attrs() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );
        graph.attrs.insert(
            "label".to_string(),
            AttrValue::String("Workflow: {{ goal }}".to_string()),
        );

        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Achieve: {{ goal }} now".to_string()),
        );
        node.attrs.insert(
            "label".to_string(),
            AttrValue::String("{{ inputs.name }}".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        graph.edges.push(Edge {
            from: "start".to_string(),
            to: "plan".to_string(),
            attrs: HashMap::from([(
                "label".to_string(),
                AttrValue::String("{{ inputs.greeting }}".to_string()),
            )]),
        });

        let transform = TemplateTransform {
            inputs: HashMap::from([
                (
                    "name".to_string(),
                    toml::Value::String("Planner".to_string()),
                ),
                (
                    "greeting".to_string(),
                    toml::Value::String("hello".to_string()),
                ),
            ]),
        };
        let graph = transform.apply(graph).unwrap();

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Achieve: Fix bugs now");
        assert_eq!(
            graph.nodes["plan"].attrs.get("label"),
            Some(&AttrValue::String("Planner".to_string()))
        );
        assert_eq!(
            graph.attrs.get("label"),
            Some(&AttrValue::String("Workflow: Fix bugs".to_string()))
        );
        assert_eq!(
            graph.edges[0].attrs.get("label"),
            Some(&AttrValue::String("hello".to_string()))
        );
    }

    #[test]
    fn template_transform_leaves_non_string_attrs_unchanged() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("plan");
        node.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(3));
        graph.nodes.insert("plan".to_string(), node);

        let transform = TemplateTransform {
            inputs: HashMap::new(),
        };
        let graph = transform.apply(graph).unwrap();

        assert_eq!(
            graph.nodes["plan"].attrs.get("max_retries"),
            Some(&AttrValue::Integer(3))
        );
    }

    #[test]
    fn template_transform_supports_empty_goal() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Goal: {{ goal }}".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = TemplateTransform {
            inputs: HashMap::new(),
        };
        let graph = transform.apply(graph).unwrap();

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: ");
    }

    #[test]
    fn template_transform_errors_on_undefined_variable() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("{{ inputs.missing }}".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = TemplateTransform {
            inputs: HashMap::new(),
        };
        let err = transform.apply(graph).unwrap_err();
        assert!(err.to_string().contains("undefined"));
    }
}
