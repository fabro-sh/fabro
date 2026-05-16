use std::collections::HashMap;

use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_template::{TemplateContext, TemplateError, render as render_template, render_lenient};
use fabro_validate::Diagnostic;

use super::Transform;
use crate::error::Error;
use crate::pipeline::types::template_undefined_variable_diagnostic;
use crate::static_reference::{
    AttributeScope, ReferenceKind, reference_kind_for_attribute, validate_static_reference,
};

/// Expands `{{ goal }}` / `{{ inputs.* }}` across all string attributes.
pub struct TemplateTransform {
    pub inputs: HashMap<String, toml::Value>,
}

impl TemplateTransform {
    #[must_use]
    pub fn new(inputs: HashMap<String, toml::Value>) -> Self {
        Self { inputs }
    }

    /// Run the transform, returning the rendered graph together with any
    /// diagnostics collected during lenient undefined-variable rendering.
    pub fn apply_with_diagnostics(
        &self,
        mut graph: Graph,
    ) -> Result<(Graph, Vec<Diagnostic>), Error> {
        let mut diagnostics = Vec::new();

        let resolved_goal = self.resolve_goal(&graph, &mut diagnostics)?;
        graph
            .attrs
            .insert("goal".to_string(), AttrValue::String(resolved_goal.clone()));
        let ctx = TemplateContext::new()
            .with_goal(resolved_goal)
            .with_inputs(self.inputs.clone());

        Self::render_attrs(
            &mut graph.attrs,
            &ctx,
            AttributeScope::Graph,
            None,
            &mut diagnostics,
        )?;
        for (node_id, node) in &mut graph.nodes {
            Self::render_attrs(
                &mut node.attrs,
                &ctx,
                AttributeScope::Node,
                Some(node_id),
                &mut diagnostics,
            )?;
        }
        for edge in &mut graph.edges {
            Self::render_attrs(
                &mut edge.attrs,
                &ctx,
                AttributeScope::Edge,
                None,
                &mut diagnostics,
            )?;
        }

        Ok((graph, diagnostics))
    }

    fn resolve_goal(
        &self,
        graph: &Graph,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<String, Error> {
        let goal = graph.goal();
        if let Some(reference) = goal.strip_prefix('@') {
            validate_static_reference(reference, ReferenceKind::GraphGoalFile)
                .map_err(|error| Error::Validation(error.to_string()))?;
            return Ok(goal.to_string());
        }
        let ctx = TemplateContext::for_input_scan(self.inputs.clone());
        Self::render_text(goal, &ctx, None, diagnostics)
    }

    fn render_attrs(
        attrs: &mut HashMap<String, AttrValue>,
        ctx: &TemplateContext,
        scope: AttributeScope,
        node_id: Option<&str>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<(), Error> {
        for (key, value) in attrs {
            if let AttrValue::String(text) = value {
                if matches!(scope, AttributeScope::Graph) && key == "goal" {
                    continue;
                }
                if key == "stack.child_dot_source" {
                    continue;
                }
                if let Some(kind) = reference_kind_for_attribute(scope, key, text) {
                    validate_static_reference(text, kind)
                        .map_err(|error| Error::Validation(error.to_string()))?;
                    continue;
                }
                *text = Self::render_text(text, ctx, node_id, diagnostics)?;
            }
        }
        Ok(())
    }

    fn render_text(
        text: &str,
        ctx: &TemplateContext,
        node_id: Option<&str>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<String, Error> {
        match render_template(text, ctx) {
            Ok(rendered) => Ok(rendered),
            Err(TemplateError::UndefinedVariable {
                expression, line, ..
            }) => {
                diagnostics.push(template_undefined_variable_diagnostic(
                    expression.as_deref(),
                    line,
                    node_id,
                ));
                Ok(render_lenient(text, ctx)?)
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Transform for TemplateTransform {
    fn apply(&self, graph: Graph) -> Result<Graph, Error> {
        let (graph, diagnostics) = self.apply_with_diagnostics(graph)?;
        if !diagnostics.is_empty() {
            return Err(Error::ValidationFailed { diagnostics });
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
            from:  "start".to_string(),
            to:    "plan".to_string(),
            attrs: HashMap::from([(
                "label".to_string(),
                AttrValue::String("{{ inputs.greeting }}".to_string()),
            )]),
        });

        let transform = TemplateTransform::new(HashMap::from([
            (
                "name".to_string(),
                toml::Value::String("Planner".to_string()),
            ),
            (
                "greeting".to_string(),
                toml::Value::String("hello".to_string()),
            ),
        ]));
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

        let transform = TemplateTransform::new(HashMap::new());
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

        let transform = TemplateTransform::new(HashMap::new());
        let graph = transform.apply(graph).unwrap();

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: ");
    }

    #[test]
    fn template_transform_warns_on_undefined_variable() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("{{ inputs.missing }}".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = TemplateTransform::new(HashMap::new());
        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "");
        assert_eq!(diagnostics.len(), 1);
        let diag = &diagnostics[0];
        assert_eq!(diag.rule, "template_undefined_variable");
        assert!(
            diag.message.contains("inputs.missing"),
            "message: {}",
            diag.message
        );
        assert!(
            diag.message.contains("in node `plan`"),
            "message: {}",
            diag.message
        );
        assert_eq!(diag.node_id.as_deref(), Some("plan"));
    }

    #[test]
    fn template_transform_renders_graph_goal_once_before_other_attrs() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Demo {{ inputs.app_dir }}".to_string()),
        );
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Goal: {{ goal }}".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = TemplateTransform::new(HashMap::new());
        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert_eq!(
            graph.attrs.get("goal").and_then(AttrValue::as_str),
            Some("Demo ")
        );
        assert_eq!(
            graph.nodes["plan"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Goal: Demo ")
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule, "template_undefined_variable");
        assert_eq!(diagnostics[0].node_id, None);
    }

    #[test]
    fn template_transform_does_not_rerender_goal_output() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Demo {{ inputs.literal }}".to_string()),
        );
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Goal: {{ goal }}".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = TemplateTransform::new(HashMap::from([(
            "literal".to_string(),
            toml::Value::String("{{ inputs.should_not_render }}".to_string()),
        )]));
        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert!(diagnostics.is_empty());
        assert_eq!(
            graph.attrs.get("goal").and_then(AttrValue::as_str),
            Some("Demo {{ inputs.should_not_render }}")
        );
        assert_eq!(
            graph.nodes["plan"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Goal: Demo {{ inputs.should_not_render }}")
        );
    }

    #[test]
    fn template_transform_rejects_templated_child_workflow_path() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("child");
        node.attrs.insert(
            "stack.child_workflow".to_string(),
            AttrValue::String("../{{ inputs.child }}/workflow.fabro".to_string()),
        );
        graph.nodes.insert("child".to_string(), node);

        let err = TemplateTransform::new(HashMap::new())
            .apply(graph)
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("templates are not supported in child workflow references"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn template_transform_hard_fails_on_syntax_error() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do {{ unterminated".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let err = TemplateTransform::new(HashMap::new())
            .apply(graph)
            .unwrap_err();
        assert!(
            err.to_string().contains("template syntax error"),
            "unexpected error: {err}"
        );
    }
}
