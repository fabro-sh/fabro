use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use fabro_util::env::Env;
use minijinja::value::{Object, Value};
use minijinja::{AutoEscape, Environment, ErrorKind, UndefinedBehavior};
use thiserror::Error;

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
#[derive(Debug, Error)]
pub enum TemplateError {
    #[error("template syntax error{location}", location = fmt_location(*line))]
    Syntax {
        line:   Option<u32>,
        #[source]
        source: minijinja::Error,
    },
    #[error(
        "undefined template variable{expr}{location}",
        expr = fmt_expr(expression.as_deref()),
        location = fmt_location(*line),
    )]
    UndefinedVariable {
        expression: Option<String>,
        line:       Option<u32>,
        #[source]
        source:     minijinja::Error,
    },
    #[error("template render error{location}", location = fmt_location(*line))]
    Render {
        line:   Option<u32>,
        #[source]
        source: minijinja::Error,
    },
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
        match error.kind() {
            ErrorKind::SyntaxError => Self::Syntax {
                line,
                source: error,
            },
            ErrorKind::UndefinedError => {
                let expression = extract_expression(&error);
                Self::UndefinedVariable {
                    expression,
                    line,
                    source: error,
                }
            }
            _ => Self::Render {
                line,
                source: error,
            },
        }
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
    render_with(template, ctx, UndefinedBehavior::Strict)
}

/// Render with chainable undefined handling: undefined variables and attribute
/// chains render as empty strings instead of erroring. Use for structural
/// passes (e.g. manifest scanning, `fabro validate` on a bare `.fabro`) where
/// the user has not yet bound inputs — strict checking happens elsewhere.
pub fn render_lenient(template: &str, ctx: &TemplateContext) -> Result<String, TemplateError> {
    render_with(template, ctx, UndefinedBehavior::Chainable)
}

fn render_with(
    template: &str,
    ctx: &TemplateContext,
    undefined: UndefinedBehavior,
) -> Result<String, TemplateError> {
    if is_plain_text(template) {
        return Ok(template.to_owned());
    }
    let mut env = Environment::new();
    env.set_undefined_behavior(undefined);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env.set_debug(true);
    env.render_str(template, ctx.clone().into_value())
        .map_err(TemplateError::from)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_util::env::TestEnv;
    use toml::map::Map;

    use super::*;

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
