use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use fabro_graphviz::graph::{AttrValue, Graph, Node};
use fabro_template::{
    TemplateContext, TemplateError, TemplateRenderMode, TemplateSource, TemplateSourceOrigin,
    TemplateStore, validate_static_reference,
};
use fabro_types::diagnostic::{Diagnostic, Severity};
use fabro_types::graph::{AttributeScope, ReferenceKind, reference_kind_for_attribute};
use fabro_types::settings::interp::Namespace;
use fabro_types::settings::{InterpString, ResolveCtx, ResolveError, ResolveErrorKind};
use fabro_util::error::collect_chain;
use fabro_util::shell;

use super::Transform;
use crate::error::Error;
use crate::pipeline::types::{GOAL_SELF_REFERENCE_RULE, TEMPLATE_UNDEFINED_VARIABLE_RULE};

/// How the template-expansion pass should treat undefined input variables.
///
/// Both validate and run-create render structurally so they can report every
/// unbound `{{ inputs.* }}` variable in one pass rather than aborting on the
/// first. Run-create then promotes the resulting warnings to errors, which
/// keeps its hard-fail behavior.
#[derive(Clone, Copy, Debug)]
pub enum RenderMode {
    /// Undefined inputs abort the pass with a hard error. No production caller
    /// uses this today; run-create promotes structural warnings instead.
    Strict,
    /// Undefined inputs render as empty and become warning diagnostics on the
    /// returned `Validated`, so structural lints still run. Used by
    /// `fabro validate` and by run-create.
    Structural,
}

#[derive(Clone)]
pub(crate) struct TemplateRenderTarget {
    pub source_name:          Option<String>,
    pub node_id:              Option<String>,
    pub edge:                 Option<(String, String)>,
    pub owner:                String,
    /// Fix text for undefined variables outside `inputs`/`vars`, set by
    /// targets whose template context is a restricted projection.
    restricted_namespace_fix: Option<String>,
    source_origin:            Option<TemplateSourceOrigin>,
    template_store:           Option<TemplateRenderStore>,
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
            restricted_namespace_fix: None,
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
            restricted_namespace_fix: None,
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
            restricted_namespace_fix: None,
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
    pub(crate) fn with_restricted_namespace_fix(mut self, fix: impl Into<String>) -> Self {
        self.restricted_namespace_fix = Some(fix.into());
        self
    }

    #[must_use]
    fn template_source_name(&self) -> String {
        self.source_name
            .clone()
            .unwrap_or_else(|| "workflow".to_string())
    }
}

pub(crate) enum TemplateRenderOutcome {
    Rendered(String),
    Unresolved,
}

pub(crate) fn render_template_for_target(
    text: &str,
    ctx: &TemplateContext,
    render_mode: RenderMode,
    target: &TemplateRenderTarget,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<String, Error> {
    match render_template_for_target_outcome(text, ctx, render_mode, target, diagnostics)? {
        TemplateRenderOutcome::Rendered(rendered) => Ok(rendered),
        TemplateRenderOutcome::Unresolved => {
            render_template_with_mode(text, ctx, TemplateRenderMode::Lenient, target)
                .map_err(|err| template_error_for_target(target, err))
        }
    }
}

pub(crate) fn render_template_for_target_outcome(
    text: &str,
    ctx: &TemplateContext,
    render_mode: RenderMode,
    target: &TemplateRenderTarget,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<TemplateRenderOutcome, Error> {
    match render_mode {
        RenderMode::Strict => {
            render_template_with_mode(text, ctx, TemplateRenderMode::Strict, target)
                .map(TemplateRenderOutcome::Rendered)
                .map_err(|err| template_error_for_target(target, err))
        }
        RenderMode::Structural => {
            match render_template_with_mode(text, ctx, TemplateRenderMode::Strict, target) {
                Ok(rendered) => Ok(TemplateRenderOutcome::Rendered(rendered)),
                Err(err @ TemplateError::UndefinedVariable { .. }) => {
                    diagnostics.push(template_diagnostic(&err, target));
                    Ok(TemplateRenderOutcome::Unresolved)
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
        fix: Some(template_variable_fix(expression, target)),
        source_path: location.source_name.or_else(|| target.source_name.clone()),
        line: location.line,
        column: location.column,
        span_start: location.span_start,
        span_len: location.span_len,
        related: Vec::new(),
    }
}

fn template_variable_fix(expression: Option<&str>, target: &TemplateRenderTarget) -> String {
    let mut parts = expression.unwrap_or_default().split('.');
    let namespace = parts.next().unwrap_or_default().parse::<Namespace>();
    let name = parts.next().unwrap_or("<name>");

    match (namespace, &target.restricted_namespace_fix) {
        (Ok(Namespace::Inputs), _) => input_binding_fix(name),
        (Ok(Namespace::Vars), _) => variable_binding_fix(name),
        (_, Some(fix)) => fix.clone(),
        (Ok(Namespace::Goal), None) => GOAL_BINDING_FIX.to_string(),
        (Ok(namespace), None) => format!("`{namespace}` is not available in workflow templates"),
        (Err(_), None) => {
            format!(
                "define `{}` in the template context",
                expression.unwrap_or("the value")
            )
        }
    }
}

fn input_binding_fix(name: &str) -> String {
    format!("bind `{name}` via `[run.inputs]` in workflow.toml, or pass `--input {name}=<value>`")
}

fn variable_binding_fix(name: &str) -> String {
    format!("set it with `fabro variable set {name} <value>`")
}

const GOAL_BINDING_FIX: &str = "set a graph `goal` on the workflow";

/// Substitutes `{{ goal }}`, `{{ inputs.* }}`, and `{{ vars.* }}` in one
/// command node `script`.
///
/// Scripts interpolate through [`InterpString`] tokens rather than the
/// MiniJinja pass that renders prompts. Shell source is full of brace syntax
/// that must survive untouched — `jq` filters, `awk` programs, Go templates,
/// brace expansion — and `InterpString` claims only the bare `{{ goal }}` and
/// `{{ <known-namespace>.NAME }}`, leaving everything else literal.
///
/// `env` and `secrets` are deliberately not wired, so a token in either
/// namespace fails as [`ResolveErrorKind::Unavailable`]. A script reads the
/// environment with `$NAME`, which needs no interpolation, and a resolved
/// secret would be baked into the `CommandStarted` event that records the
/// script verbatim.
///
/// Shell values are quoted as one argument. Python values are quoted as string
/// literals. In both languages the token must stand where one value is valid;
/// callers must not wrap it in another string literal.
fn interpolate_script<'a>(
    text: &'a str,
    ctx: &TemplateContext,
    language: &str,
    render_mode: RenderMode,
    target: &TemplateRenderTarget,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Cow<'a, str>, Error> {
    if !text.contains("{{") {
        return Ok(Cow::Borrowed(text));
    }

    let parsed = InterpString::parse(text);
    if parsed.is_literal() {
        return Ok(Cow::Borrowed(text));
    }

    let mut resolve_ctx = ResolveCtx::new()
        .with_inputs(|name| {
            ctx.input(name)
                .map(|value| quote_script_value(&value, language))
        })
        .with_vars(|name| {
            ctx.var(name)
                .map(|value| quote_script_value(&value, language))
        });
    // The graph goal is rendered before any node attribute, so by here it is
    // the final text. Substituting it does not re-interpolate: whatever the
    // goal contains lands in the script as literal characters.
    if let Some(goal) = ctx.goal() {
        resolve_ctx = resolve_ctx.with_goal(quote_script_value(goal, language));
    }
    match parsed.resolve_with(&mut resolve_ctx) {
        Ok(resolved) => Ok(Cow::Owned(resolved)),
        // An unbound input or variable is the same authoring gap the prompt
        // pass reports, so it follows the same mode split: a hard error at
        // run-create, a diagnostic during `fabro validate`.
        Err(err) if err.kind == ResolveErrorKind::Missing => match render_mode {
            RenderMode::Strict => Err(script_interpolation_error(target, err, language)),
            RenderMode::Structural => {
                diagnostics.push(script_undefined_variable_diagnostic(&err, target));
                // Leave the script in source form. Validation never executes
                // it, and showing the unresolved token beats emptying it.
                Ok(Cow::Borrowed(text))
            }
        },
        // An unsupported namespace can never resolve here, however the inputs
        // are bound, so it fails in both modes — the same treatment
        // `render_attrs` gives an invalid static reference.
        Err(err) => Err(script_interpolation_error(target, err, language)),
    }
}

fn quote_script_value(value: &str, language: &str) -> String {
    if language == "python" {
        serde_json::to_string(value).expect("serializing a string to JSON should not fail")
    } else {
        shell::shell_quote(value)
    }
}

fn script_interpolation_error(
    target: &TemplateRenderTarget,
    source: ResolveError,
    language: &str,
) -> Error {
    let fix = script_interpolation_fix(&source, Some(language));
    Error::ScriptInterpolation {
        owner: target.owner.clone(),
        fix,
        source,
    }
}

fn script_undefined_variable_diagnostic(
    err: &ResolveError,
    target: &TemplateRenderTarget,
) -> Diagnostic {
    Diagnostic {
        rule: TEMPLATE_UNDEFINED_VARIABLE_RULE.to_owned(),
        severity: Severity::Warning,
        message: format!("{err} in {}", target.owner),
        node_id: target.node_id.clone(),
        edge: target.edge.clone(),
        fix: Some(script_interpolation_fix(err, None)),
        source_path: target.source_name.clone(),
        ..Diagnostic::default()
    }
}

fn script_interpolation_fix(err: &ResolveError, language: Option<&str>) -> String {
    let name = &err.name;
    match err.namespace {
        Namespace::Inputs => input_binding_fix(name),
        Namespace::Vars => variable_binding_fix(name),
        Namespace::Env if language == Some("python") => format!(
            "`script` does not interpolate environment variables; read it in Python as \
             `os.environ[\"{name}\"]` instead"
        ),
        Namespace::Env => format!(
            "`script` does not interpolate environment variables; read it in the shell as \
             `${name}` instead"
        ),
        Namespace::Secrets if language == Some("python") => format!(
            "`script` does not interpolate secrets; expose `{name}` to the sandbox through \
             `[environments.<slug>.env]` and read it in Python as `os.environ[\"{name}\"]`"
        ),
        Namespace::Secrets => format!(
            "`script` does not interpolate secrets; expose `{name}` to the sandbox through \
             `[environments.<slug>.env]` and read it in the shell as `${name}`"
        ),
        Namespace::Goal => GOAL_BINDING_FIX.to_string(),
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
             as literal text. Node `prompt`, graph `goal`, and graph `model_stylesheet` support \
             templating. Node command `script` supports `{{{{ goal }}}}`, \
             `{{{{ inputs.* }}}}`, and `{{{{ vars.* }}}}` interpolation.",
            target.owner
        ),
        node_id: target.node_id.clone(),
        edge: target.edge.clone(),
        fix: Some(format!(
            "remove the template syntax from `{attr_name}`, or move the dynamic value into a \
             `prompt`/`goal`/`model_stylesheet`"
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

/// Renders graph goals and node prompts, and diagnoses template syntax in
/// attributes that do not support it.
pub struct TemplateTransform {
    pub context:     TemplateContext,
    pub source_name: Option<String>,
    pub source_text: Option<String>,
    pub render_mode: RenderMode,
}

impl TemplateTransform {
    #[must_use]
    pub fn new(inputs: HashMap<String, toml::Value>) -> Self {
        Self {
            context:     TemplateContext::new().with_inputs(inputs),
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
        let ctx = self.context.clone();
        render_template_for_target(goal, &ctx, self.render_mode, &target, diagnostics)
    }

    fn goal_self_reference_location(
        &self,
        goal: &str,
        target: &TemplateRenderTarget,
    ) -> Option<TemplateError> {
        let ctx = self.context.clone();
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
                // The root model stylesheet has its own restricted template
                // pass after imports are expanded. Imported stylesheets stay
                // ignored and are diagnosed by ImportTransform.
                if matches!(scope, AttributeScope::Graph) && attr_name == "model_stylesheet" {
                    continue;
                }
                if attr_name == "stack.child_dot_source" {
                    continue;
                }
                // Command scripts use narrow value interpolation in a separate
                // one-shot transform after imports are expanded.
                if matches!(scope, AttributeScope::Node) && attr_name == "script" {
                    continue;
                }
                if let Some(kind) = reference_kind_for_attribute(scope, attr_name, text) {
                    validate_static_reference(text, kind.into())
                        .map_err(|error| Error::Validation(error.to_string()))?;
                    continue;
                }
                let target = owner_for_attr(attr_name)
                    .with_source_name(source_name.cloned().unwrap_or_else(|| "workflow".into()))
                    .with_source_origin(source_text, text);
                if matches!(scope, AttributeScope::Node) && attr_name == "prompt" {
                    // `prompt` is the only node attribute rendered as a full
                    // MiniJinja template.
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
        let ctx = self.context.clone().with_goal(resolved_goal);

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

/// Interpolates command node scripts once, after import expansion is complete.
///
/// Keeping this pass separate from [`TemplateTransform`] prevents imported
/// scripts from being scanned once in their source graph and again after they
/// are merged into the root graph.
pub struct ScriptInterpolationTransform {
    pub context:     TemplateContext,
    pub source_name: Option<String>,
    pub render_mode: RenderMode,
}

impl ScriptInterpolationTransform {
    fn command_script_language(node: &Node) -> Option<&'static str> {
        let is_command = matches!(node.handler_type(), Some("command" | "tool"));
        is_command.then(|| {
            if node.attrs.get("language").and_then(AttrValue::as_str) == Some("python") {
                "python"
            } else {
                "shell"
            }
        })
    }

    pub(crate) fn apply_with_diagnostics(
        &self,
        graph: Graph,
    ) -> Result<(Graph, Vec<Diagnostic>), Error> {
        let mut graph = graph;
        let mut diagnostics = Vec::new();
        let ctx = self.context.clone().with_goal(graph.goal().to_string());

        for (node_id, node) in &mut graph.nodes {
            let language = Self::command_script_language(node);
            let Some(AttrValue::String(text)) = node.attrs.get_mut("script") else {
                continue;
            };
            let target = TemplateRenderTarget::node_attr(
                self.source_name.clone(),
                node_id.clone(),
                "script",
            )
            .with_source_name(
                self.source_name
                    .clone()
                    .unwrap_or_else(|| "workflow".to_string()),
            );

            if let Some(language) = language {
                if let Cow::Owned(resolved) = interpolate_script(
                    text,
                    &ctx,
                    language,
                    self.render_mode,
                    &target,
                    &mut diagnostics,
                )? {
                    *text = resolved;
                }
            } else if fabro_template::contains_template_syntax(text) {
                diagnostics.push(detemplated_attribute_diagnostic("script", &target));
            }
        }

        Ok((graph, diagnostics))
    }
}

impl Transform for ScriptInterpolationTransform {
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

    /// Build a one-node graph whose `test` node carries `script`.
    fn script_graph(script: &str) -> Graph {
        let mut graph = Graph::new("test");
        graph
            .attrs
            .insert("goal".to_string(), AttrValue::String("Ship it".to_string()));
        let mut node = Node::new("test");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        node.attrs
            .insert("script".to_string(), AttrValue::String(script.to_string()));
        graph.nodes.insert("test".to_string(), node);
        graph
    }

    fn script_transform(
        inputs: &[(&str, toml::Value)],
        vars: &[(&str, &str)],
        render_mode: RenderMode,
    ) -> ScriptInterpolationTransform {
        ScriptInterpolationTransform {
            context: TemplateContext::new()
                .with_inputs(
                    inputs
                        .iter()
                        .map(|(k, v)| ((*k).to_string(), v.clone()))
                        .collect(),
                )
                .with_vars(
                    vars.iter()
                        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                        .collect(),
                ),
            source_name: None,
            render_mode,
        }
    }

    fn script_of(graph: &Graph) -> &str {
        graph.nodes["test"]
            .attrs
            .get("script")
            .and_then(AttrValue::as_str)
            .expect("script attribute should still be a string")
    }

    #[test]
    fn script_interpolates_inputs_and_vars() {
        let graph = script_graph("cargo test -p {{ inputs.crate }} --profile {{ vars.PROFILE }}");
        let transform = script_transform(
            &[("crate", toml::Value::String("fabro-workflow".into()))],
            &[("PROFILE", "ci")],
            RenderMode::Structural,
        );

        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert_eq!(
            script_of(&graph),
            "cargo test -p fabro-workflow --profile ci"
        );
        assert!(diagnostics.is_empty(), "unexpected: {diagnostics:?}");
    }

    #[test]
    fn script_substitutes_the_rendered_goal() {
        let graph = script_graph("gh pr create --title {{ goal }}");
        let transform = script_transform(&[], &[], RenderMode::Structural);

        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert_eq!(script_of(&graph), "gh pr create --title 'Ship it'");
        assert!(diagnostics.is_empty(), "unexpected: {diagnostics:?}");
    }

    #[test]
    fn shell_script_quotes_substituted_values_as_one_argument() {
        let graph = script_graph("deploy --release {{ inputs.release }}");
        let transform = script_transform(
            &[(
                "release",
                toml::Value::String("stable; touch /tmp/pwned".to_string()),
            )],
            &[],
            RenderMode::Strict,
        );

        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert_eq!(
            script_of(&graph),
            "deploy --release 'stable; touch /tmp/pwned'"
        );
        assert!(diagnostics.is_empty(), "unexpected: {diagnostics:?}");
    }

    #[test]
    fn python_script_quotes_substituted_values_as_string_literals() {
        let mut graph = script_graph("print({{ inputs.value }})");
        graph.nodes.get_mut("test").unwrap().attrs.insert(
            "language".to_string(),
            AttrValue::String("python".to_string()),
        );
        let transform = script_transform(
            &[(
                "value",
                toml::Value::String("'); __import__('os').system('id'); #".to_string()),
            )],
            &[],
            RenderMode::Strict,
        );

        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert_eq!(
            script_of(&graph),
            r#"print("'); __import__('os').system('id'); #")"#
        );
        assert!(diagnostics.is_empty(), "unexpected: {diagnostics:?}");
    }

    /// The reason `script` uses `InterpString` rather than MiniJinja: shell
    /// source is full of brace syntax that must reach the shell untouched.
    #[test]
    fn script_leaves_non_token_braces_literal() {
        let script = "jq '{name: .name}' f.json | awk '{print $1}'; echo {{ .Values.image }}; \
                      touch {a,b}.txt; {% raw %}";
        let graph = script_graph(script);
        let transform = script_transform(&[], &[], RenderMode::Structural);

        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert_eq!(script_of(&graph), script);
        assert!(diagnostics.is_empty(), "unexpected: {diagnostics:?}");
    }

    #[test]
    fn script_rejects_secrets_tokens_in_both_render_modes() {
        for render_mode in [RenderMode::Structural, RenderMode::Strict] {
            let graph = script_graph("curl -H \"Authorization: {{ secrets.API_KEY }}\" $URL");
            let transform = script_transform(&[], &[], render_mode);

            let err = transform
                .apply_with_diagnostics(graph)
                .expect_err("secrets must never interpolate into a script");

            let message = err.to_string();
            assert!(
                message.contains("does not interpolate secrets"),
                "unexpected message: {message}"
            );
            assert!(
                message.contains("$API_KEY"),
                "should point at the shell alternative: {message}"
            );
        }
    }

    #[test]
    fn script_rejects_env_tokens_and_points_at_shell_expansion() {
        let graph = script_graph("echo {{ env.HOME }}");
        let transform = script_transform(&[], &[], RenderMode::Structural);

        let err = transform
            .apply_with_diagnostics(graph)
            .expect_err("env is not interpolated in a script");

        let message = err.to_string();
        assert!(message.contains("$HOME"), "unexpected message: {message}");
    }

    #[test]
    fn script_missing_input_warns_and_preserves_source_in_structural_mode() {
        let script = "cargo test -p {{ inputs.crate }}";
        let graph = script_graph(script);
        let transform = script_transform(&[], &[], RenderMode::Structural);

        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert_eq!(script_of(&graph), script);
        let diagnostic = diagnostics
            .iter()
            .find(|d| d.rule == TEMPLATE_UNDEFINED_VARIABLE_RULE)
            .expect("an unbound input should warn");
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert_eq!(diagnostic.node_id.as_deref(), Some("test"));
        assert!(
            diagnostic
                .fix
                .as_deref()
                .unwrap_or_default()
                .contains("--input crate="),
            "unexpected fix: {:?}",
            diagnostic.fix
        );
    }

    #[test]
    fn script_missing_input_is_an_error_in_strict_mode() {
        let graph = script_graph("cargo test -p {{ inputs.crate }}");
        let transform = script_transform(&[], &[], RenderMode::Strict);

        let err = transform
            .apply_with_diagnostics(graph)
            .expect_err("strict mode must reject an unbound input");

        assert!(
            err.to_string().contains("inputs.crate"),
            "unexpected message: {err}"
        );
        let source = std::error::Error::source(&err)
            .expect("script interpolation errors should preserve ResolveError as their source");
        assert!(
            source.to_string().contains("inputs.crate"),
            "unexpected source: {source}"
        );
    }

    /// A typed input must produce the same text in a script as in a prompt, so
    /// authors do not have to reason about two stringification rules.
    #[test]
    fn script_and_prompt_stringify_typed_inputs_identically() {
        let mut graph =
            script_graph("retry --times {{ inputs.attempts }} --fast {{ inputs.fast }}");
        graph.nodes.get_mut("test").unwrap().attrs.insert(
            "prompt".to_string(),
            AttrValue::String(
                "retry --times {{ inputs.attempts }} --fast {{ inputs.fast }}".to_string(),
            ),
        );
        let context = TemplateContext::new().with_inputs(HashMap::from([
            ("attempts".to_string(), toml::Value::Integer(3)),
            ("fast".to_string(), toml::Value::Boolean(true)),
        ]));
        let (graph, _) = TemplateTransform {
            context:     context.clone(),
            source_name: None,
            source_text: None,
            render_mode: RenderMode::Structural,
        }
        .apply_with_diagnostics(graph)
        .unwrap();
        let (graph, _) = ScriptInterpolationTransform {
            context,
            source_name: None,
            render_mode: RenderMode::Structural,
        }
        .apply_with_diagnostics(graph)
        .unwrap();

        assert_eq!(script_of(&graph), "retry --times 3 --fast true");
        assert_eq!(
            graph.nodes["test"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some(script_of(&graph)),
        );
    }

    /// `script` is node-scoped. A graph or edge attribute of the same name is
    /// not a command node script and keeps the demoted-attribute behavior.
    #[test]
    fn script_interpolation_is_node_scoped() {
        let mut graph = script_graph("echo ok");
        graph.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo {{ inputs.crate }}".to_string()),
        );
        let (graph, mut diagnostics) = TemplateTransform::new(HashMap::new())
            .apply_with_diagnostics(graph)
            .unwrap();
        let transform = script_transform(&[], &[], RenderMode::Structural);
        let (graph, script_diagnostics) = transform.apply_with_diagnostics(graph).unwrap();
        diagnostics.extend(script_diagnostics);

        assert_eq!(
            graph.attrs.get("script"),
            Some(&AttrValue::String("echo {{ inputs.crate }}".to_string()))
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.rule == DETEMPLATED_ATTRIBUTE_RULE),
            "graph-scope `script` should still warn: {diagnostics:?}"
        );
    }

    #[test]
    fn non_command_node_script_stays_literal() {
        let mut graph = script_graph("echo {{ inputs.value }}");
        graph
            .nodes
            .get_mut("test")
            .unwrap()
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        let transform = script_transform(
            &[("value", toml::Value::String("changed".to_string()))],
            &[],
            RenderMode::Structural,
        );

        let (graph, diagnostics) = transform.apply_with_diagnostics(graph).unwrap();

        assert_eq!(script_of(&graph), "echo {{ inputs.value }}");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.rule == DETEMPLATED_ATTRIBUTE_RULE),
            "non-command scripts should keep the literal-template warning: {diagnostics:?}"
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
            context:     TemplateContext::new(),
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
            context:     TemplateContext::new(),
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
