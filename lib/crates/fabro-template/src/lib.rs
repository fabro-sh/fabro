use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use fabro_types::ManifestPath;
use fabro_util::env::Env;
use miette::{LabeledSpan, NamedSource, SourceCode, SourceSpan};
use minijinja::value::{Object, Value};
use minijinja::{AutoEscape, Environment, ErrorKind, UndefinedBehavior};

mod dependency;
mod store;

pub use dependency::{
    ExtractedTemplateDependencies, TemplateDependency, TemplateDependencyClosure,
    TemplateDependencyKind, TemplateDiscoveryError, discover_static_dependency_closure,
    extract_template_dependencies,
};
pub use store::{
    BundleTemplateStore, CachedTemplateStore, FilesystemTemplateStore, RecordingTemplateStore,
    TemplateIncludeResolver, TemplateLoadError, TemplateSource, TemplateStore,
};

pub type TemplateLoader = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateRenderMode {
    Strict,
    Lenient,
}

impl TemplateRenderMode {
    fn undefined_behavior(self) -> UndefinedBehavior {
        match self {
            Self::Strict => UndefinedBehavior::Strict,
            Self::Lenient => UndefinedBehavior::Chainable,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct TemplateContext {
    goal:   Option<String>,
    inputs: HashMap<String, toml::Value>,
    env:    Option<Value>,
}

impl TemplateContext {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_goal(mut self, goal: impl Into<String>) -> Self {
        self.goal = Some(goal.into());
        self
    }

    #[must_use]
    pub fn with_inputs(mut self, inputs: HashMap<String, toml::Value>) -> Self {
        self.inputs = inputs;
        self
    }

    /// Context that interpolates inputs but leaves `{{ goal }}` as a literal
    /// pass-through — used for structural pre-rendering before the goal is
    /// known (e.g. manifest scanning, import resolution).
    #[must_use]
    pub fn for_input_scan(inputs: HashMap<String, toml::Value>) -> Self {
        Self::new().with_goal("{{ goal }}").with_inputs(inputs)
    }

    #[must_use]
    pub fn with_env_lookup<E>(mut self, env: &E) -> Self
    where
        E: Env + Clone + Send + Sync + fmt::Debug + 'static,
    {
        self.env = Some(Value::from_object(EnvLookup {
            env:       env.clone(),
            allowlist: None,
        }));
        self
    }

    #[must_use]
    pub fn with_env_lookup_allowed<E>(mut self, env: &E, allowlist: &[String]) -> Self
    where
        E: Env + Clone + Send + Sync + fmt::Debug + 'static,
    {
        self.env = Some(Value::from_object(EnvLookup {
            env:       env.clone(),
            allowlist: Some(allowlist.to_vec()),
        }));
        self
    }

    fn into_value(self) -> Value {
        let goal = self.goal.map(Value::from);
        let inputs = Value::from_serialize(self.inputs);
        let env = self.env;
        Value::from_object(RenderContext { goal, inputs, env })
    }
}

#[derive(Debug, Clone)]
struct RenderContext {
    goal:   Option<Value>,
    inputs: Value,
    env:    Option<Value>,
}

impl Object for RenderContext {
    fn get_value_by_str(self: &Arc<Self>, key: &str) -> Option<Value> {
        match key {
            "goal" => self.goal.clone(),
            "inputs" => Some(self.inputs.clone()),
            "env" => self.env.clone(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EnvLookup<E> {
    env:       E,
    allowlist: Option<Vec<String>>,
}

impl<E> Object for EnvLookup<E>
where
    E: Env + Send + Sync + fmt::Debug + 'static,
{
    fn get_value_by_str(self: &Arc<Self>, key: &str) -> Option<Value> {
        if let Some(allowlist) = &self.allowlist {
            if !allowlist.iter().any(|allowed| allowed == key) {
                return None;
            }
        }

        self.env.var(key).ok().map(Value::from)
    }
}

/// Errors from rendering a template. Each variant carries the typed fields
/// MiniJinja knows about (offending expression, line) plus the original
/// `minijinja::Error` as `#[source]`, so the cause chain is preserved across
/// boundaries that walk `Error::source()` (anyhow, miette, `collect_chain`).
#[derive(Debug)]
pub enum TemplateError {
    LoaderDependentString {
        source_name: Option<String>,
        tag:         TemplateDependencyKind,
    },
    Load {
        source_name: Option<String>,
        source:      Box<TemplateLoadError>,
    },
    Syntax {
        line:        Option<u32>,
        source_name: Option<String>,
        source_text: Option<String>,
        span:        Option<SourceSpan>,
        source_code: Option<Box<NamedSource<String>>>,
        source:      Box<minijinja::Error>,
    },
    UndefinedVariable {
        expression:  Option<String>,
        line:        Option<u32>,
        source_name: Option<String>,
        source_text: Option<String>,
        span:        Option<SourceSpan>,
        source_code: Option<Box<NamedSource<String>>>,
        source:      Box<minijinja::Error>,
    },
    Render {
        line:        Option<u32>,
        source_name: Option<String>,
        source_text: Option<String>,
        span:        Option<SourceSpan>,
        source_code: Option<Box<NamedSource<String>>>,
        source:      Box<minijinja::Error>,
    },
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoaderDependentString { tag, .. } => {
                write!(
                    f,
                    "loader-dependent template tag `{tag:?}` requires a rooted template source"
                )
            }
            Self::Load { .. } => write!(f, "template load error"),
            Self::Syntax { line, .. } => {
                write!(f, "template syntax error{}", fmt_location(*line))
            }
            Self::UndefinedVariable {
                expression, line, ..
            } => write!(
                f,
                "undefined template variable{}{}",
                fmt_expr(expression.as_deref()),
                fmt_location(*line)
            ),
            Self::Render { line, .. } => {
                write!(f, "template render error{}", fmt_location(*line))
            }
        }
    }
}

impl std::error::Error for TemplateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LoaderDependentString { .. } => None,
            Self::Load { source, .. } => Some(source.as_ref()),
            Self::Syntax { source, .. }
            | Self::UndefinedVariable { source, .. }
            | Self::Render { source, .. } => Some(source.as_ref()),
        }
    }
}

fn fmt_expr(expression: Option<&str>) -> String {
    expression.map(|e| format!(" `{e}`")).unwrap_or_default()
}

fn fmt_location(line: Option<u32>) -> String {
    line.map(|l| format!(" at line {l}")).unwrap_or_default()
}

/// Extract the failing expression from the template source using the byte
/// range MiniJinja attaches to errors when debug mode is on.
fn extract_expression(error: &minijinja::Error) -> Option<String> {
    let range = error.range()?;
    let source = error.template_source()?;
    Some(source.get(range)?.trim().to_owned())
}

impl From<minijinja::Error> for TemplateError {
    fn from(error: minijinja::Error) -> Self {
        let line = error.line().and_then(|n| u32::try_from(n).ok());
        let source_name = error.name().map(str::to_owned);
        let source_text = error.template_source().map(str::to_owned);
        let span = error.range().and_then(|range| {
            let start = range.start;
            let len = range.end.checked_sub(range.start)?;
            Some((start, len).into())
        });
        let source_code = source_name
            .as_ref()
            .zip(source_text.as_ref())
            .map(|(name, source)| Box::new(NamedSource::new(name.clone(), source.clone())));
        match error.kind() {
            ErrorKind::SyntaxError => Self::Syntax {
                line,
                source_name,
                source_text,
                span,
                source_code,
                source: Box::new(error),
            },
            ErrorKind::UndefinedError => {
                let expression = extract_expression(&error);
                Self::UndefinedVariable {
                    expression,
                    line,
                    source_name,
                    source_text,
                    span,
                    source_code,
                    source: Box::new(error),
                }
            }
            _ => Self::Render {
                line,
                source_name,
                source_text,
                span,
                source_code,
                source: Box::new(error),
            },
        }
    }
}

impl TemplateError {
    #[must_use]
    pub fn expression(&self) -> Option<&str> {
        match self {
            Self::UndefinedVariable { expression, .. } => expression.as_deref(),
            Self::LoaderDependentString { .. }
            | Self::Load { .. }
            | Self::Syntax { .. }
            | Self::Render { .. } => None,
        }
    }

    #[must_use]
    pub fn line(&self) -> Option<u32> {
        match self {
            Self::LoaderDependentString { .. } | Self::Load { .. } => None,
            Self::Syntax { line, .. }
            | Self::UndefinedVariable { line, .. }
            | Self::Render { line, .. } => *line,
        }
    }

    #[must_use]
    pub fn source_name(&self) -> Option<&str> {
        match self {
            Self::LoaderDependentString { source_name, .. } | Self::Load { source_name, .. } => {
                source_name.as_deref()
            }
            Self::Syntax { source_name, .. }
            | Self::UndefinedVariable { source_name, .. }
            | Self::Render { source_name, .. } => source_name.as_deref(),
        }
    }

    #[must_use]
    pub fn source_text(&self) -> Option<&str> {
        match self {
            Self::LoaderDependentString { .. } | Self::Load { .. } => None,
            Self::Syntax { source_text, .. }
            | Self::UndefinedVariable { source_text, .. }
            | Self::Render { source_text, .. } => source_text.as_deref(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Option<SourceSpan> {
        match self {
            Self::LoaderDependentString { .. } | Self::Load { .. } => None,
            Self::Syntax { span, .. }
            | Self::UndefinedVariable { span, .. }
            | Self::Render { span, .. } => *span,
        }
    }

    #[must_use]
    pub fn column(&self) -> Option<u32> {
        let source_text = self.source_text()?;
        let offset = self.span()?.offset();
        if offset > source_text.len() || !source_text.is_char_boundary(offset) {
            return None;
        }
        let line_start = source_text[..offset]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        u32::try_from(source_text[line_start..offset].chars().count() + 1).ok()
    }

    fn source_code_ref(&self) -> Option<&NamedSource<String>> {
        match self {
            Self::LoaderDependentString { .. } | Self::Load { .. } => None,
            Self::Syntax { source_code, .. }
            | Self::UndefinedVariable { source_code, .. }
            | Self::Render { source_code, .. } => source_code.as_deref(),
        }
    }
}

impl miette::Diagnostic for TemplateError {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        let code = match self {
            Self::LoaderDependentString { .. } => "fabro::template::loader_dependent_string",
            Self::Load { .. } => "fabro::template::load",
            Self::Syntax { .. } => "fabro::template::syntax",
            Self::UndefinedVariable { .. } => "fabro::template::undefined_variable",
            Self::Render { .. } => "fabro::template::render",
        };
        Some(Box::new(code))
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        self.source_code_ref()
            .map(|source| source as &dyn SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let span = self.span()?;
        let label = match self {
            Self::LoaderDependentString { .. } => "loader-dependent tag".to_string(),
            Self::Load { .. } => "template load".to_string(),
            Self::UndefinedVariable { expression, .. } => expression.as_ref().map_or_else(
                || "undefined variable".to_string(),
                |expr| format!("`{expr}`"),
            ),
            Self::Syntax { .. } => "syntax error".to_string(),
            Self::Render { .. } => "render error".to_string(),
        };
        Some(Box::new(
            vec![LabeledSpan::new_primary_with_span(Some(label), span)].into_iter(),
        ))
    }
}

/// Returns `true` when the string contains MiniJinja delimiter syntax.
#[must_use]
pub fn contains_template_syntax(template: &str) -> bool {
    template.contains("{{") || template.contains("{%") || template.contains("{#")
}

/// Returns `true` when the string contains no MiniJinja delimiters and can
/// be returned as-is without paying for a full template parse+render cycle.
fn is_plain_text(template: &str) -> bool {
    !contains_template_syntax(template)
}

pub fn render(template: &str, ctx: &TemplateContext) -> Result<String, TemplateError> {
    render_with(None, template, ctx, UndefinedBehavior::Strict, None)
}

pub fn render_named(
    name: impl Into<String>,
    template: &str,
    ctx: &TemplateContext,
) -> Result<String, TemplateError> {
    render_with(
        Some(name.into()),
        template,
        ctx,
        UndefinedBehavior::Strict,
        None,
    )
}

pub fn render_named_with_loader(
    name: impl Into<String>,
    template: &str,
    ctx: &TemplateContext,
    loader: &TemplateLoader,
) -> Result<String, TemplateError> {
    render_with(
        Some(name.into()),
        template,
        ctx,
        UndefinedBehavior::Strict,
        Some(loader),
    )
}

/// Render with chainable undefined handling: undefined variables and attribute
/// chains render as empty strings instead of erroring. Use for structural
/// passes (e.g. manifest scanning, `fabro validate` on a bare `.fabro`) where
/// the user has not yet bound inputs — strict checking happens elsewhere.
pub fn render_lenient(template: &str, ctx: &TemplateContext) -> Result<String, TemplateError> {
    render_with(None, template, ctx, UndefinedBehavior::Chainable, None)
}

pub fn render_lenient_named(
    name: impl Into<String>,
    template: &str,
    ctx: &TemplateContext,
) -> Result<String, TemplateError> {
    render_with(
        Some(name.into()),
        template,
        ctx,
        UndefinedBehavior::Chainable,
        None,
    )
}

pub fn render_lenient_named_with_loader(
    name: impl Into<String>,
    template: &str,
    ctx: &TemplateContext,
    loader: &TemplateLoader,
) -> Result<String, TemplateError> {
    render_with(
        Some(name.into()),
        template,
        ctx,
        UndefinedBehavior::Chainable,
        Some(loader),
    )
}

fn render_with(
    name: Option<String>,
    template: &str,
    ctx: &TemplateContext,
    undefined: UndefinedBehavior,
    loader: Option<&TemplateLoader>,
) -> Result<String, TemplateError> {
    if is_plain_text(template) {
        return Ok(template.to_owned());
    }
    if loader.is_none() {
        reject_loader_dependent_string(name.as_deref(), template)?;
    }
    let mut env = Environment::new();
    env.set_undefined_behavior(undefined);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env.set_debug(true);
    if let Some(loader) = loader {
        let loader = Arc::clone(loader);
        env.set_loader(move |name| Ok(loader(name)));
    }
    match name {
        Some(name) => env.render_named_str(&name, template, ctx.clone().into_value()),
        None => env.render_str(template, ctx.clone().into_value()),
    }
    .map_err(TemplateError::from)
}

pub fn render_source(
    source: &TemplateSource,
    ctx: &TemplateContext,
    store: Arc<dyn TemplateStore>,
    mode: TemplateRenderMode,
) -> Result<String, TemplateError> {
    render_rooted_source(source, ctx, store, mode.undefined_behavior())
}

fn render_rooted_source(
    source: &TemplateSource,
    ctx: &TemplateContext,
    store: Arc<dyn TemplateStore>,
    undefined: UndefinedBehavior,
) -> Result<String, TemplateError> {
    if is_plain_text(&source.content) {
        return Ok(source.content.clone());
    }
    let mut env = Environment::new();
    env.set_undefined_behavior(undefined);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env.set_debug(true);
    let root = source.root.clone();
    env.set_path_join_callback(move |name, parent| {
        joined_template_path(&root, name, parent).into()
    });

    let load_error = Arc::new(Mutex::new(None));
    let loader_error = Arc::clone(&load_error);
    let loader_parent = TemplateSource::new(
        ManifestPath::from_wire(".").expect("root manifest path should parse"),
        source.root.clone(),
        String::new(),
    );
    env.set_loader(move |name| match store.load(&loader_parent, name) {
        Ok(source) => Ok(source.map(|source| source.content)),
        Err(error) => {
            *loader_error
                .lock()
                .expect("template load error mutex should not be poisoned") = Some(error);
            Err(minijinja::Error::new(
                ErrorKind::InvalidOperation,
                "template load failed",
            ))
        }
    });

    env.render_named_str(
        &source.path.to_string(),
        &source.content,
        ctx.clone().into_value(),
    )
    .map_err(|error| {
        if let Some(error) = load_error
            .lock()
            .expect("template load error mutex should not be poisoned")
            .take()
        {
            TemplateError::Load {
                source_name: Some(source.path.to_string()),
                source:      Box::new(error),
            }
        } else {
            TemplateError::from(error)
        }
    })
}

fn joined_template_path(root: &ManifestPath, name: &str, parent: &str) -> String {
    let Some(parent) = ManifestPath::from_wire(parent) else {
        return name.to_owned();
    };
    TemplateIncludeResolver::new(root.clone())
        .resolve(&parent, name)
        .map_or_else(|_| name.to_owned(), |path| path.to_string())
}

fn reject_loader_dependent_string(name: Option<&str>, template: &str) -> Result<(), TemplateError> {
    let source_name = name.unwrap_or("string");
    if let Some(tag) = dependency::has_loader_dependent_tags(source_name, template)? {
        return Err(TemplateError::LoaderDependentString {
            source_name: name.map(ToOwned::to_owned),
            tag,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_util::env::TestEnv;
    use toml::map::Map;

    use super::*;

    fn manifest_path(value: &str) -> ManifestPath {
        ManifestPath::from_wire(value).expect("path should parse")
    }

    fn bundle_store(files: &[(&str, &str)]) -> Arc<dyn TemplateStore> {
        Arc::new(BundleTemplateStore::new(
            files
                .iter()
                .map(|(path, content)| (manifest_path(path), (*content).to_string()))
                .collect(),
        ))
    }

    #[test]
    fn renders_simple_goal_variable() {
        let ctx = TemplateContext::new().with_goal("Fix bugs");

        let rendered = render("Goal: {{ goal }}", &ctx).unwrap();

        assert_eq!(rendered, "Goal: Fix bugs");
    }

    #[test]
    fn renders_typed_input_values() {
        let ctx = TemplateContext::new().with_inputs(HashMap::from([
            ("enabled".to_string(), toml::Value::Boolean(true)),
            ("count".to_string(), toml::Value::Integer(3)),
        ]));

        let rendered = render(
            "{% if inputs.enabled %}count={{ inputs.count }}{% endif %}",
            &ctx,
        )
        .unwrap();

        assert_eq!(rendered, "count=3");
    }

    #[test]
    fn renders_nested_input_variable() {
        let ctx = TemplateContext::new().with_inputs(HashMap::from([(
            "repo".to_string(),
            toml::Value::Table(Map::from_iter([(
                "name".to_string(),
                toml::Value::String("fabro".to_string()),
            )])),
        )]));

        let rendered = render("Repo {{ inputs.repo.name }}", &ctx).unwrap();

        assert_eq!(rendered, "Repo fabro");
    }

    #[test]
    fn renders_env_variable() {
        let env = TestEnv(HashMap::from([(
            "API_KEY".to_string(),
            "secret".to_string(),
        )]));
        let ctx = TemplateContext::new().with_env_lookup(&env);

        let rendered = render("{{ env.API_KEY }}", &ctx).unwrap();

        assert_eq!(rendered, "secret");
    }

    #[test]
    fn renders_allowlisted_env_variable() {
        let env = TestEnv(HashMap::from([("TOKEN".to_string(), "abc123".to_string())]));
        let ctx = TemplateContext::new().with_env_lookup_allowed(&env, &["TOKEN".to_string()]);

        let rendered = render("Bearer {{ env.TOKEN }}", &ctx).unwrap();

        assert_eq!(rendered, "Bearer abc123");
    }

    #[test]
    fn rejects_non_allowlisted_env_variable() {
        let env = TestEnv(HashMap::from([("SECRET".to_string(), "shh".to_string())]));
        let ctx = TemplateContext::new().with_env_lookup_allowed(&env, &[]);

        let err = render("{{ env.SECRET }}", &ctx).unwrap_err();

        assert!(matches!(err, TemplateError::UndefinedVariable { .. }));
    }

    #[test]
    fn render_lenient_treats_undefined_as_empty() {
        let ctx = TemplateContext::new();

        let rendered = render_lenient("before [{{ inputs.app_dir }}] after", &ctx).unwrap();

        assert_eq!(rendered, "before [] after");
    }

    #[test]
    fn render_lenient_still_errors_on_syntax_problems() {
        let ctx = TemplateContext::new();

        let err = render_lenient("{{ unterminated", &ctx).unwrap_err();

        assert!(matches!(err, TemplateError::Syntax { .. }));
    }

    #[test]
    fn render_named_reports_source_name_expression_and_span() {
        let ctx = TemplateContext::new();
        let err = render_named("prompts/test.md", "Hello {{ inputs.foo }}", &ctx).unwrap_err();

        let TemplateError::UndefinedVariable {
            expression,
            line,
            source_name,
            span,
            ..
        } = err
        else {
            panic!("expected undefined variable error");
        };

        assert_eq!(expression.as_deref(), Some("inputs.foo"));
        assert_eq!(line, Some(1));
        assert_eq!(source_name.as_deref(), Some("prompts/test.md"));
        assert!(span.is_some());
    }

    #[test]
    fn render_named_with_loader_supports_include() {
        let ctx = TemplateContext::new();
        let loader: TemplateLoader =
            Arc::new(|name| (name == "partial.md").then(|| "included content".to_string()));

        let rendered =
            render_named_with_loader("prompt.md", r#"{% include "partial.md" %}"#, &ctx, &loader)
                .unwrap();

        assert_eq!(rendered, "included content");
    }

    #[test]
    fn render_source_supports_rooted_include() {
        let ctx = TemplateContext::new();
        let source = TemplateSource::new(
            manifest_path("prompts/main.md"),
            manifest_path("prompts"),
            r#"{% include "partial.md" %}"#,
        );

        let rendered = render_source(
            &source,
            &ctx,
            bundle_store(&[("prompts/partial.md", "included content")]),
            TemplateRenderMode::Strict,
        )
        .unwrap();

        assert_eq!(rendered, "included content");
    }

    #[test]
    fn render_source_supports_nested_include() {
        let ctx = TemplateContext::new();
        let source = TemplateSource::new(
            manifest_path("prompts/main.md"),
            manifest_path("prompts"),
            r#"{% include "partial.md" %}"#,
        );

        let rendered = render_source(
            &source,
            &ctx,
            bundle_store(&[
                ("prompts/partial.md", r#"{% include "nested.md" %}"#),
                ("prompts/nested.md", "nested content"),
            ]),
            TemplateRenderMode::Strict,
        )
        .unwrap();

        assert_eq!(rendered, "nested content");
    }

    #[test]
    fn render_source_supports_extends() {
        let ctx = TemplateContext::new();
        let source = TemplateSource::new(
            manifest_path("pages/main.md"),
            manifest_path("pages"),
            r#"{% extends "layout.md" %}{% block body %}Body{% endblock %}"#,
        );

        let rendered = render_source(
            &source,
            &ctx,
            bundle_store(&[(
                "pages/layout.md",
                "prefix:{% block body %}{% endblock %}:suffix",
            )]),
            TemplateRenderMode::Strict,
        )
        .unwrap();

        assert_eq!(rendered, "prefix:Body:suffix");
    }

    #[test]
    fn render_source_supports_import() {
        let ctx = TemplateContext::new();
        let source = TemplateSource::new(
            manifest_path("prompts/main.md"),
            manifest_path("prompts"),
            r#"{% import "macros.md" as macros %}{{ macros.greet("Ada") }}"#,
        );

        let rendered = render_source(
            &source,
            &ctx,
            bundle_store(&[(
                "prompts/macros.md",
                r"{% macro greet(name) %}hi {{ name }}{% endmacro %}",
            )]),
            TemplateRenderMode::Strict,
        )
        .unwrap();

        assert_eq!(rendered, "hi Ada");
    }

    #[test]
    fn render_source_supports_from_import() {
        let ctx = TemplateContext::new();
        let source = TemplateSource::new(
            manifest_path("prompts/main.md"),
            manifest_path("prompts"),
            r#"{% from "macros.md" import greet %}{{ greet("Ada") }}"#,
        );

        let rendered = render_source(
            &source,
            &ctx,
            bundle_store(&[(
                "prompts/macros.md",
                r"{% macro greet(name) %}hi {{ name }}{% endmacro %}",
            )]),
            TemplateRenderMode::Strict,
        )
        .unwrap();

        assert_eq!(rendered, "hi Ada");
    }

    #[test]
    fn render_source_rejects_unsafe_include() {
        let ctx = TemplateContext::new();
        let source = TemplateSource::new(
            manifest_path("prompts/main.md"),
            manifest_path("prompts"),
            r#"{% include "../outside.md" %}"#,
        );
        let store: Arc<dyn TemplateStore> = Arc::new(BundleTemplateStore::new(HashMap::new()));

        let err = render_source(&source, &ctx, store, TemplateRenderMode::Strict).unwrap_err();

        assert!(matches!(
            err,
            TemplateError::Load {
                source,
                ..
            } if matches!(*source, TemplateLoadError::EscapesRoot { .. })
        ));
    }

    #[test]
    fn render_source_uses_source_root_for_sibling_include() {
        let ctx = TemplateContext::new();
        let source = TemplateSource::new(
            manifest_path("prompts/audits/audit.prompt.md"),
            manifest_path("prompts"),
            r#"{% include "../partials/audit.partial.tpl" %}"#,
        );

        let rendered = render_source(
            &source,
            &ctx,
            Arc::new(BundleTemplateStore::new(HashMap::from([(
                manifest_path("prompts/partials/audit.partial.tpl"),
                "shared partial".to_string(),
            )]))),
            TemplateRenderMode::Strict,
        )
        .unwrap();

        assert_eq!(rendered, "shared partial");
    }

    #[test]
    fn render_named_rejects_loader_dependent_tags_without_root() {
        let ctx = TemplateContext::new();

        let err = render_named("main.md", r#"{% include "partial.md" %}"#, &ctx).unwrap_err();

        assert!(matches!(err, TemplateError::LoaderDependentString { .. }));
    }

    #[test]
    fn extractor_ignores_comments_raw_blocks_and_plain_text() {
        let source = r#"
            {# {% include "comment.md" %} #}
            {% raw %}{% include "raw.md" %}{% endraw %}
            text {% include "text.md" %}
        "#;

        let dependencies = extract_template_dependencies("test.md", source).unwrap();

        assert_eq!(dependencies.static_references, vec![TemplateDependency {
            kind:      TemplateDependencyKind::Include,
            reference: "text.md".to_string(),
        }]);
        assert!(dependencies.dynamic_references.is_empty());
    }

    #[test]
    fn static_dependency_closure_collects_both_branches() {
        let source = TemplateSource::new(
            manifest_path("main.md"),
            manifest_path("."),
            r#"{% if inputs.use_a %}{% include "a.md" %}{% else %}{% include "b.md" %}{% endif %}"#,
        );

        let closure = discover_static_dependency_closure(
            [source],
            bundle_store(&[("a.md", "A"), ("b.md", "B")]).as_ref(),
        )
        .unwrap();

        assert!(closure.sources.contains_key(&manifest_path("a.md")));
        assert!(closure.sources.contains_key(&manifest_path("b.md")));
    }

    #[test]
    fn static_dependency_closure_collects_unused_macro_body_dependencies() {
        let source = TemplateSource::new(
            manifest_path("main.md"),
            manifest_path("."),
            r#"{% from "helpers.md" import render_advanced_prompt %}"#,
        );

        let closure = discover_static_dependency_closure(
            [source],
            bundle_store(&[
                (
                    "helpers.md",
                    r#"{% macro render_advanced_prompt() %}{% include "advanced.md" %}{% endmacro %}"#,
                ),
                ("advanced.md", "advanced"),
            ])
            .as_ref(),
        )
        .unwrap();

        assert!(closure.sources.contains_key(&manifest_path("helpers.md")));
        assert!(closure.sources.contains_key(&manifest_path("advanced.md")));
    }

    #[test]
    fn static_dependency_closure_rejects_dynamic_include() {
        let source = TemplateSource::new(
            manifest_path("main.md"),
            manifest_path("."),
            r"{% include inputs.partial %}",
        );

        let err =
            discover_static_dependency_closure([source], bundle_store(&[]).as_ref()).unwrap_err();

        assert!(matches!(err, TemplateDiscoveryError::Dynamic { .. }));
    }

    #[test]
    fn render_lenient_named_preserves_source_name_for_syntax_errors() {
        let ctx = TemplateContext::new();
        let err = render_lenient_named("workflow.fabro", "{{ unterminated", &ctx).unwrap_err();

        let TemplateError::Syntax { source_name, .. } = err else {
            panic!("expected syntax error");
        };

        assert_eq!(source_name.as_deref(), Some("workflow.fabro"));
    }

    #[test]
    fn rejects_undefined_variables_in_strict_mode() {
        let ctx = TemplateContext::new();

        let err = render("{{ missing }}", &ctx).unwrap_err();

        assert!(matches!(err, TemplateError::UndefinedVariable { .. }));
    }

    #[test]
    fn undefined_variable_error_captures_expression_and_line() {
        let ctx = TemplateContext::new();

        let err = render("hi\n{{ inputs.app_dir }}", &ctx).unwrap_err();

        let TemplateError::UndefinedVariable {
            expression, line, ..
        } = &err
        else {
            panic!("expected UndefinedVariable, got {err:?}");
        };
        assert_eq!(expression.as_deref(), Some("inputs.app_dir"));
        assert_eq!(*line, Some(2));
    }

    #[test]
    fn undefined_variable_error_display_includes_expression_and_line() {
        let ctx = TemplateContext::new();

        let err = render("hi\n{{ inputs.app_dir }}", &ctx).unwrap_err();

        let rendered = err.to_string();
        assert!(
            rendered.contains("inputs.app_dir"),
            "missing variable name in: {rendered}"
        );
        assert!(rendered.contains("line 2"), "missing line in: {rendered}");
    }

    #[test]
    fn template_error_preserves_minijinja_source_chain() {
        use std::error::Error as _;

        let ctx = TemplateContext::new();

        let err = render("{{ missing }}", &ctx).unwrap_err();

        let source = err.source().expect("source should be present");
        assert!(
            source.is::<minijinja::Error>(),
            "expected minijinja::Error as source, got {source:?}"
        );
    }

    #[test]
    fn supports_partial_interpolation() {
        let ctx = TemplateContext::new().with_goal("ship it");

        let rendered = render("Please {{ goal }} today", &ctx).unwrap();

        assert_eq!(rendered, "Please ship it today");
    }

    #[test]
    fn preserves_passthrough_goal_literal() {
        let ctx = TemplateContext::new().with_goal("{{ goal }}");

        let rendered = render("{{ goal }}", &ctx).unwrap();

        assert_eq!(rendered, "{{ goal }}");
    }

    #[test]
    fn renders_empty_goal() {
        let ctx = TemplateContext::new().with_goal("");

        let rendered = render("Goal={{ goal }}", &ctx).unwrap();

        assert_eq!(rendered, "Goal=");
    }

    #[test]
    fn leaves_dollar_signs_untouched() {
        let ctx = TemplateContext::new().with_goal("ignored");

        let rendered = render("price is $5", &ctx).unwrap();

        assert_eq!(rendered, "price is $5");
    }

    #[test]
    fn passes_through_plain_text() {
        let ctx = TemplateContext::new();

        let rendered = render("just text", &ctx).unwrap();

        assert_eq!(rendered, "just text");
    }

    #[test]
    fn supports_raw_block_escape() {
        let ctx = TemplateContext::new();

        let rendered = render("{% raw %}{{ goal }}{% endraw %}", &ctx).unwrap();

        assert_eq!(rendered, "{{ goal }}");
    }
}
