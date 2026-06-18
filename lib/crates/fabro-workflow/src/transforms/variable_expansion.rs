use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_template::{
    TemplateContext, TemplateError, TemplateRenderMode, TemplateSource, TemplateSourceOrigin,
    TemplateStore,
};
use fabro_util::error::collect_chain;
use fabro_validate::{Diagnostic, Severity};

use super::Transform;
use crate::error::Error;
use crate::pipeline::types::{GOAL_SELF_REFERENCE_RULE, TEMPLATE_UNDEFINED_VARIABLE_RULE};
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

#[derive(Clone)]
pub(crate) struct TemplateRenderTarget {
    pub source_name: Option<String>,
    pub node_id:     Option<String>,
    pub edge:        Option<(String, String)>,
    pub owner:       String,
    source_origin:   Option<TemplateSourceOrigin>,
    template_store:  Option<TemplateRenderStore>,
}

#[derive(Clone)]
pub(crate) struct TemplateRenderStore {
    source: TemplateSource,
    store:  Arc<dyn TemplateStore>,
}

impl TemplateRenderStore {
    #[must_use]
    pub(crate) fn new(source: TemplateSource, store: Arc<dyn TemplateStore>) -> Self {
        Self { source, store }
    }

    fn render(
        &self,
        text: &str,
        ctx: &TemplateContext,
        mode: TemplateRenderMode,
        origin: Option<&TemplateSourceOrigin>,
    ) -> Result<String, TemplateError> {
        let mut source = match origin {
            Some(origin) => self.source.clone().with_origin(origin.clone()),
            None => self.source.clone(),
        };
        text.clone_into(&mut source.content);
        fabro_template::render_source(&source, ctx, Arc::clone(&self.store), mode)
    }
}

impl TemplateRenderTarget {
    #[must_use]
    pub(crate) fn graph_attr(source_name: Option<String>, attr_name: impl Into<String>) -> Self {
        let attr_name = attr_name.into();
        Self {
            source_name,
            node_id: None,
            edge: None,
            owner: format!("graph attribute `{attr_name}`"),
            source_origin: None,
            template_store: None,
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
            node_id: Some(node_id.clone()),
            edge: None,
            owner: format!("node `{node_id}` attribute `{attr_name}`"),
            source_origin: None,
            template_store: None,
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
            node_id: None,
            edge: Some((from.clone(), to.clone())),
            owner: format!("edge `{from} -> {to}` attribute `{attr_name}`"),
            source_origin: None,
            template_store: None,
        }
    }

    #[must_use]
    pub(crate) fn with_source_name(mut self, source_name: impl Into<String>) -> Self {
        self.source_name = Some(source_name.into());
        self
    }

    #[must_use]
    pub(crate) fn with_source_origin(mut self, source_text: Option<&str>, value: &str) -> Self {
        self.source_origin = source_text.and_then(|source_text| {
            TemplateSourceOrigin::from_first_fragment_match(source_text, value)
        });
        self
    }

    #[must_use]
    pub(crate) fn with_template_store(mut self, template_store: TemplateRenderStore) -> Self {
        self.template_store = Some(template_store);
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
    match render_mode {
        RenderMode::Strict => {
            render_template_with_mode(text, ctx, TemplateRenderMode::Strict, target)
                .map_err(|err| template_error_for_target(target, err))
        }
        RenderMode::Structural => {
            match render_template_with_mode(text, ctx, TemplateRenderMode::Strict, target) {
                Ok(rendered) => Ok(rendered),
                Err(err @ TemplateError::UndefinedVariable { .. }) => {
                    diagnostics.push(template_diagnostic(&err, target));
                    render_template_with_mode(text, ctx, TemplateRenderMode::Lenient, target)
                        .map_err(|err| template_error_for_target(target, err))
                }
                Err(err) => Err(template_error_for_target(target, err)),
            }
        }
    }
}

fn render_template_with_mode(
    text: &str,
    ctx: &TemplateContext,
    mode: TemplateRenderMode,
    target: &TemplateRenderTarget,
) -> Result<String, TemplateError> {
    match target.template_store.as_ref() {
        Some(template_store) => {
            template_store.render(text, ctx, mode, target.source_origin.as_ref())
        }
        None => fabro_template::render_named_with_origin(
            target.template_source_name(),
            text,
            ctx,
            mode,
            target.source_origin.as_ref(),
        ),
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

    let location = error.location();

    Diagnostic {
        rule: TEMPLATE_UNDEFINED_VARIABLE_RULE.to_owned(),
        severity: Severity::Warning,
        message,
        node_id: target.node_id.clone(),
        edge: target.edge.clone(),
        fix: Some(format!(
            "bind `{name}` via `[run.inputs]` in workflow.toml, or pass `--input {name}=<value>`"
        )),
        source_path: location.source_name.or_else(|| target.source_name.clone()),
        line: location.line,
        column: location.column,
        span_start: location.span_start,
        span_len: location.span_len,
        related: Vec::new(),
    }
}

const DETEMPLATED_ATTRIBUTE_RULE: &str = "detemplated_attribute";

/// Warning emitted when an attribute that is no longer a template still
/// contains template syntax — the syntax is now treated as literal text.
fn detemplated_attribute_diagnostic(attr_name: &str, target: &TemplateRenderTarget) -> Diagnostic {
    Diagnostic {
        rule: DETEMPLATED_ATTRIBUTE_RULE.to_owned(),
        severity: Severity::Warning,
        message: format!(
            "`{attr_name}` in {} is no longer a template; `{{{{ … }}}}` / `{{% … %}}` is treated \
             as literal text. Only node `prompt` and graph `goal` support templating.",
            target.owner
        ),
        node_id: target.node_id.clone(),
        edge: target.edge.clone(),
        fix: Some(format!(
            "remove the template syntax from `{attr_name}`, or move the dynamic value into a \
             `prompt`/`goal`"
        )),
        source_path: target.source_name.clone(),
        ..Diagnostic::default()
    }
}

/// Error emitted when the graph `goal` references `{{ goal }}` — a goal cannot
/// reference itself. Prompts may reference the rendered goal; the goal renders
/// without `goal` in scope, so a self-reference is always a mistake.
fn goal_self_reference_diagnostic(
    target: &TemplateRenderTarget,
    error: Option<&TemplateError>,
) -> Diagnostic {
    let location = error.map(TemplateError::location).unwrap_or_default();
    Diagnostic {
        rule: GOAL_SELF_REFERENCE_RULE.to_owned(),
        severity: Severity::Error,
        message: format!(
            "the graph `goal` cannot reference itself (`{{{{ goal }}}}`) in {}",
            target.owner
        ),
        node_id: target.node_id.clone(),
        edge: target.edge.clone(),
        fix: Some(
            "remove the `{{ goal }}` reference from the goal; a node `prompt` can reference the \
             goal instead"
                .to_string(),
        ),
        source_path: location.source_name.or_else(|| target.source_name.clone()),
        line: location.line,
        column: location.column,
        span_start: location.span_start,
        span_len: location.span_len,
        ..Diagnostic::default()
    }
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
        let target = TemplateRenderTarget::graph_attr(self.source_name.clone(), "goal")
            .with_source_origin(self.source_text.as_deref(), goal);
        // The goal renders with no `goal` in scope, so it cannot reference
        // itself. Flag the self-reference with a friendly diagnostic before the
        // render would otherwise produce a generic "undefined variable `goal`".
        if fabro_template::references_top_level_variable(goal, "goal") {
            let location_error = self.goal_self_reference_location(goal, &target);
            diagnostics.push(goal_self_reference_diagnostic(
                &target,
                location_error.as_ref(),
            ));
            return Ok(goal.to_string());
        }
        let ctx = TemplateContext::new().with_inputs(self.inputs.clone());
        render_template_for_target(goal, &ctx, self.render_mode, &target, diagnostics)
    }

    fn goal_self_reference_location(
        &self,
        goal: &str,
        target: &TemplateRenderTarget,
    ) -> Option<TemplateError> {
        let ctx = TemplateContext::new().with_inputs(self.inputs.clone());
        match render_template_with_mode(goal, &ctx, TemplateRenderMode::Strict, target) {
            Err(err @ TemplateError::UndefinedVariable { .. })
                if err.expression() == Some("goal") =>
            {
                Some(err)
            }
            _ => None,
        }
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
                // The graph `goal` is rendered separately and must not be
                // re-rendered here.
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
                    .with_source_origin(source_text, text);
                if matches!(scope, AttributeScope::Node) && attr_name == "prompt" {
                    // `prompt` is the only templated node attribute.
                    *text =
                        render_template_for_target(text, ctx, render_mode, &target, diagnostics)?;
                } else if fabro_template::contains_template_syntax(text) {
                    // Every other attribute is no longer a template (`label`,
                    // `model`, `provider`, `speed`, `condition`, edge `label`,
                    // …): leave it literal and warn so authors can migrate.
                    diagnostics.push(detemplated_attribute_diagnostic(attr_name, &target));
                }
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
    fn template_transform_renders_prompt_and_leaves_other_attrs_literal() {
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
        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        // `prompt` is the only templated attribute and is still rendered.
        assert_eq!(
            graph.nodes["plan"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Achieve: Fix bugs now")
        );
        // `label` (node, graph, edge) is no longer a template: left literal.
        assert_eq!(
            graph.nodes["plan"].attrs.get("label"),
            Some(&AttrValue::String("{{ inputs.name }}".to_string()))
        );
        assert_eq!(
            graph.attrs.get("label"),
            Some(&AttrValue::String("Workflow: {{ goal }}".to_string()))
        );
        assert_eq!(
            graph.edges[0].attrs.get("label"),
            Some(&AttrValue::String("{{ inputs.greeting }}".to_string()))
        );
        // Each demoted `label` still containing template syntax warns.
        let detemplated = diagnostics
            .iter()
            .filter(|d| d.rule == DETEMPLATED_ATTRIBUTE_RULE)
            .count();
        assert_eq!(
            detemplated, 3,
            "expected a migration warning per demoted label, got: {diagnostics:?}"
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
    fn template_transform_rejects_goal_self_reference() {
        let source = r#"digraph Test {
            graph [goal="Improve on {{ goal }}"]
        }"#;
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Improve on {{ goal }}".to_string()),
        );
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Work: {{ goal }}".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = TemplateTransform {
            inputs:      HashMap::new(),
            source_name: Some("workflow.fabro".to_string()),
            source_text: Some(source.to_string()),
            render_mode: RenderMode::Structural,
        };
        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        let self_ref: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.rule == GOAL_SELF_REFERENCE_RULE)
            .collect();
        assert_eq!(
            self_ref.len(),
            1,
            "expected one goal_self_reference diagnostic"
        );
        assert_eq!(self_ref[0].severity, Severity::Error);
        assert!(self_ref[0].message.contains("cannot reference itself"));
        assert_eq!(self_ref[0].source_path.as_deref(), Some("workflow.fabro"));
        assert_eq!(self_ref[0].line, Some(2));
        assert!(self_ref[0].span_start.is_some());
        assert_eq!(
            graph.attrs.get("goal").and_then(AttrValue::as_str),
            Some("Improve on {{ goal }}")
        );
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
