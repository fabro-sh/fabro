use std::collections::HashMap;
use std::fmt::Write as _;

use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_template::{TemplateContext, TemplateError, render_lenient_named, render_named};
use fabro_util::error::collect_chain;
use fabro_validate::{Diagnostic, Severity};

use super::Transform;
use crate::error::Error;
use crate::pipeline::types::TEMPLATE_UNDEFINED_VARIABLE_RULE;
use crate::static_reference::{
    AttributeScope, ReferenceKind, reference_kind_for_attribute, validate_static_reference,
};

/// How the template-expansion pass should treat undefined input variables.
///
/// Validate is structural — it should not fail just because the user has not
/// bound `{{ inputs.* }}` yet. Run-start is strict — missing inputs are real
/// errors. Splitting the two lets validate work on a bare `.fabro` while
/// run-start preserves its current hard-fail behavior.
#[derive(Clone, Copy, Debug)]
pub enum RenderMode {
    /// Undefined inputs are hard errors. Used by run-create.
    Strict,
    /// Undefined inputs render as empty and become warning diagnostics on the
    /// returned `Validated`, so structural lints still run. Used by
    /// `fabro validate`.
    Structural,
}

#[derive(Clone, Debug)]
pub(crate) struct TemplateRenderTarget {
    pub source_name:   Option<String>,
    pub source_text:   Option<String>,
    pub source_offset: Option<usize>,
    pub node_id:       Option<String>,
    pub edge:          Option<(String, String)>,
    pub owner:         String,
}

impl TemplateRenderTarget {
    #[must_use]
    pub(crate) fn graph_attr(source_name: Option<String>, attr_name: impl Into<String>) -> Self {
        let attr_name = attr_name.into();
        Self {
            source_name,
            source_text: None,
            source_offset: None,
            node_id: None,
            edge: None,
            owner: format!("graph attribute `{attr_name}`"),
        }
    }

    #[must_use]
    pub(crate) fn node_attr(
        source_name: Option<String>,
        node_id: impl Into<String>,
        attr_name: impl Into<String>,
    ) -> Self {
        let node_id = node_id.into();
        let attr_name = attr_name.into();
        Self {
            source_name,
            source_text: None,
            source_offset: None,
            node_id: Some(node_id.clone()),
            edge: None,
            owner: format!("node `{node_id}` attribute `{attr_name}`"),
        }
    }

    #[must_use]
    pub(crate) fn edge_attr(
        source_name: Option<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        attr_name: impl Into<String>,
    ) -> Self {
        let from = from.into();
        let to = to.into();
        let attr_name = attr_name.into();
        Self {
            source_name,
            source_text: None,
            source_offset: None,
            node_id: None,
            edge: Some((from.clone(), to.clone())),
            owner: format!("edge `{from} -> {to}` attribute `{attr_name}`"),
        }
    }

    #[must_use]
    pub(crate) fn with_source_name(mut self, source_name: impl Into<String>) -> Self {
        self.source_name = Some(source_name.into());
        self
    }

    #[must_use]
    pub(crate) fn with_source_text(mut self, source_text: Option<&str>, value: &str) -> Self {
        self.source_text = source_text.map(ToOwned::to_owned);
        self.source_offset = source_text.and_then(|source_text| source_text.find(value));
        self
    }

    #[must_use]
    fn template_source_name(&self) -> String {
        self.source_name
            .clone()
            .unwrap_or_else(|| "workflow".to_string())
    }
}

pub(crate) fn render_template_for_target(
    text: &str,
    ctx: &TemplateContext,
    render_mode: RenderMode,
    target: &TemplateRenderTarget,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<String, Error> {
    let source_name = target.template_source_name();
    match render_mode {
        RenderMode::Strict => render_named(source_name, text, ctx)
            .map_err(|err| template_error_for_target(target, err)),
        RenderMode::Structural => match render_named(source_name.clone(), text, ctx) {
            Ok(rendered) => Ok(rendered),
            Err(err @ TemplateError::UndefinedVariable { .. }) => {
                diagnostics.push(template_diagnostic(&err, target));
                render_lenient_named(source_name, text, ctx)
                    .map_err(|err| template_error_for_target(target, err))
            }
            Err(err) => Err(template_error_for_target(target, err)),
        },
    }
}

fn template_error_for_target(target: &TemplateRenderTarget, err: TemplateError) -> Error {
    let rendered = collect_chain(&err).join(": ");
    Error::template(
        format!("template expansion failed in {}: {rendered}", target.owner),
        err,
    )
}

fn template_diagnostic(error: &TemplateError, target: &TemplateRenderTarget) -> Diagnostic {
    let expression = error.expression();
    let name = expression.unwrap_or("<unknown>");
    let mut message = match expression {
        Some(expr) => format!("undefined template variable `{expr}`"),
        None => "undefined template variable".to_string(),
    };
    let _ = write!(message, " in {}", target.owner);

    let source_location = target
        .source_text
        .as_deref()
        .zip(target.source_offset)
        .zip(error.span())
        .and_then(|((source_text, source_offset), span)| {
            let absolute_offset = source_offset.checked_add(span.offset())?;
            let (line, column) = source_position(source_text, absolute_offset)?;
            Some((line, column, absolute_offset, span.len()))
        });

    Diagnostic {
        rule: TEMPLATE_UNDEFINED_VARIABLE_RULE.to_owned(),
        severity: Severity::Warning,
        message,
        node_id: target.node_id.clone(),
        edge: target.edge.clone(),
        fix: Some(format!(
            "bind `{name}` via `[run.inputs]` in workflow.toml, or pass `--input {name}=<value>`"
        )),
        source_path: error
            .source_name()
            .map(ToOwned::to_owned)
            .or_else(|| target.source_name.clone()),
        line: source_location
            .map(|(line, _, _, _)| line)
            .or_else(|| error.line()),
        column: source_location
            .map(|(_, column, _, _)| column)
            .or_else(|| error.column()),
        span_start: source_location
            .map(|(_, _, span_start, _)| span_start)
            .or_else(|| error.span().map(|span| span.offset())),
        span_len: source_location
            .map(|(_, _, _, span_len)| span_len)
            .or_else(|| error.span().map(|span| span.len())),
        related: Vec::new(),
    }
}

fn source_position(source_text: &str, offset: usize) -> Option<(u32, u32)> {
    if offset > source_text.len() || !source_text.is_char_boundary(offset) {
        return None;
    }
    let line = source_text[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let line_start = source_text[..offset]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let column = source_text[line_start..offset].chars().count() + 1;
    Some((u32::try_from(line).ok()?, u32::try_from(column).ok()?))
}

/// Expands `{{ goal }}` / `{{ inputs.* }}` across all string attributes.
pub struct TemplateTransform {
    pub inputs:      HashMap<String, toml::Value>,
    pub source_name: Option<String>,
    pub source_text: Option<String>,
    pub render_mode: RenderMode,
}

impl TemplateTransform {
    #[must_use]
    pub fn new(inputs: HashMap<String, toml::Value>) -> Self {
        Self {
            inputs,
            source_name: None,
            source_text: None,
            render_mode: RenderMode::Structural,
        }
    }

    pub(crate) fn resolved_goal(
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
        let target = TemplateRenderTarget::graph_attr(self.source_name.clone(), "goal")
            .with_source_text(self.source_text.as_deref(), goal);
        render_template_for_target(goal, &ctx, self.render_mode, &target, diagnostics)
    }

    fn render_attrs(
        attrs: &mut HashMap<String, AttrValue>,
        ctx: &TemplateContext,
        source_name: Option<&String>,
        source_text: Option<&str>,
        render_mode: RenderMode,
        scope: AttributeScope,
        owner_for_attr: impl Fn(&str) -> TemplateRenderTarget,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<(), Error> {
        for (attr_name, value) in attrs {
            if let AttrValue::String(text) = value {
                if matches!(scope, AttributeScope::Graph) && attr_name == "goal" {
                    continue;
                }
                if attr_name == "stack.child_dot_source" {
                    continue;
                }
                if let Some(kind) = reference_kind_for_attribute(scope, attr_name, text) {
                    validate_static_reference(text, kind)
                        .map_err(|error| Error::Validation(error.to_string()))?;
                    continue;
                }
                let target = owner_for_attr(attr_name)
                    .with_source_name(source_name.cloned().unwrap_or_else(|| "workflow".into()))
                    .with_source_text(source_text, text);
                *text = render_template_for_target(text, ctx, render_mode, &target, diagnostics)?;
            }
        }
        Ok(())
    }

    pub(crate) fn apply_with_diagnostics(
        &self,
        graph: Graph,
    ) -> Result<(Graph, Vec<Diagnostic>), Error> {
        let mut diagnostics = Vec::new();
        let mut graph = graph;
        let resolved_goal = self.resolved_goal(&graph, &mut diagnostics)?;
        graph
            .attrs
            .insert("goal".to_string(), AttrValue::String(resolved_goal.clone()));
        let ctx = TemplateContext::new()
            .with_goal(resolved_goal)
            .with_inputs(self.inputs.clone());

        Self::render_attrs(
            &mut graph.attrs,
            &ctx,
            self.source_name.as_ref(),
            self.source_text.as_deref(),
            self.render_mode,
            AttributeScope::Graph,
            |attr_name| TemplateRenderTarget::graph_attr(self.source_name.clone(), attr_name),
            &mut diagnostics,
        )?;
        for (node_id, node) in &mut graph.nodes {
            Self::render_attrs(
                &mut node.attrs,
                &ctx,
                self.source_name.as_ref(),
                self.source_text.as_deref(),
                self.render_mode,
                AttributeScope::Node,
                |attr_name| {
                    TemplateRenderTarget::node_attr(
                        self.source_name.clone(),
                        node_id.clone(),
                        attr_name,
                    )
                },
                &mut diagnostics,
            )?;
        }
        for edge in &mut graph.edges {
            let from = edge.from.clone();
            let to = edge.to.clone();
            Self::render_attrs(
                &mut edge.attrs,
                &ctx,
                self.source_name.as_ref(),
                self.source_text.as_deref(),
                self.render_mode,
                AttributeScope::Edge,
                |attr_name| {
                    TemplateRenderTarget::edge_attr(
                        self.source_name.clone(),
                        from.clone(),
                        to.clone(),
                        attr_name,
                    )
                },
                &mut diagnostics,
            )?;
        }

        Ok((graph, diagnostics))
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

    #[test]
    fn template_transform_reports_structural_diagnostics_with_owner_context() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("{{ inputs.missing }}".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = TemplateTransform {
            inputs:      HashMap::new(),
            source_name: Some("workflow.fabro".to_string()),
            source_text: None,
            render_mode: RenderMode::Structural,
        };
        let (_, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].node_id.as_deref(), Some("plan"));
        assert_eq!(
            diagnostics[0].source_path.as_deref(),
            Some("workflow.fabro")
        );
        assert!(
            diagnostics[0]
                .message
                .contains("node `plan` attribute `prompt`")
        );
    }
}
