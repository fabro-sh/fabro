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

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TemplateError {
    #[error("template syntax error: {message}")]
    Syntax { message: String },
    #[error("template referenced an undefined variable: {message}")]
    UndefinedVariable { message: String },
    #[error("template render error: {message}")]
    Render { message: String },
}

impl From<minijinja::Error> for TemplateError {
    fn from(error: minijinja::Error) -> Self {
        let message = error.to_string();
        match error.kind() {
            ErrorKind::SyntaxError => Self::Syntax { message },
            ErrorKind::UndefinedError => Self::UndefinedVariable { message },
            _ => Self::Render { message },
        }
    }
}

/// Returns `true` when the string contains no MiniJinja delimiters and can
/// be returned as-is without paying for a full template parse+render cycle.
fn is_plain_text(template: &str) -> bool {
    !template.contains("{{") && !template.contains("{%") && !template.contains("{#")
}

pub fn render(template: &str, ctx: &TemplateContext) -> Result<String, TemplateError> {
    if is_plain_text(template) {
        return Ok(template.to_owned());
    }
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env.render_str(template, ctx.clone().into_value())
        .map_err(TemplateError::from)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_util::env::TestEnv;

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
            toml::Value::Table(toml::map::Map::from_iter([(
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
    fn rejects_undefined_variables_in_strict_mode() {
        let ctx = TemplateContext::new();

        let err = render("{{ missing }}", &ctx).unwrap_err();

        assert!(matches!(err, TemplateError::UndefinedVariable { .. }));
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
