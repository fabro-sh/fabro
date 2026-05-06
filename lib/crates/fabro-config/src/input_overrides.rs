use std::collections::HashMap;

use fabro_types::WorkflowSettings;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum InputOverrideParseError {
    #[error("input override `{raw}` is missing `=`; expected KEY=VALUE")]
    MissingEquals { raw: String },
    #[error("input override key cannot be empty")]
    EmptyKey,
    #[error("input override `{key}` does not support {kind} values")]
    UnsupportedValue { key: String, kind: &'static str },
}

#[must_use]
fn unsupported_kind(value: &toml::Value) -> Option<&'static str> {
    match value {
        toml::Value::String(_)
        | toml::Value::Integer(_)
        | toml::Value::Float(_)
        | toml::Value::Boolean(_) => None,
        toml::Value::Datetime(_) => Some("datetime"),
        toml::Value::Array(_) => Some("array"),
        toml::Value::Table(_) => Some("inline table"),
    }
}

fn parse_input_value(key: &str, raw_value: &str) -> Result<toml::Value, InputOverrideParseError> {
    if raw_value.is_empty() {
        return Ok(toml::Value::String(String::new()));
    }

    let document = format!("value = {raw_value}");
    match document.parse::<toml::Value>() {
        Ok(value) => {
            let value = value
                .get("value")
                .cloned()
                .unwrap_or_else(|| toml::Value::String(raw_value.to_string()));
            if let Some(kind) = unsupported_kind(&value) {
                return Err(InputOverrideParseError::UnsupportedValue {
                    key: key.to_string(),
                    kind,
                });
            }
            Ok(value)
        }
        Err(_) => Ok(toml::Value::String(raw_value.to_string())),
    }
}

pub fn parse_input_overrides(
    raw_inputs: &[String],
) -> Result<HashMap<String, toml::Value>, InputOverrideParseError> {
    let mut parsed = HashMap::new();
    for raw in raw_inputs {
        let Some((key, raw_value)) = raw.split_once('=') else {
            return Err(InputOverrideParseError::MissingEquals { raw: raw.clone() });
        };
        if key.is_empty() {
            return Err(InputOverrideParseError::EmptyKey);
        }
        parsed.insert(key.to_string(), parse_input_value(key, raw_value)?);
    }
    Ok(parsed)
}

pub fn apply_input_overrides(
    settings: &mut WorkflowSettings,
    input_overrides: HashMap<String, toml::Value>,
) {
    settings.run.inputs.extend(input_overrides);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkflowSettingsBuilder;

    fn parse_one(raw: &str) -> Result<toml::Value, InputOverrideParseError> {
        parse_input_overrides(&[raw.to_string()]).map(|mut parsed| {
            parsed
                .remove(raw.split_once('=').map_or("", |(key, _)| key))
                .expect("input should be present")
        })
    }

    #[test]
    fn empty_value_is_empty_string() {
        assert_eq!(
            parse_one("foo=").unwrap(),
            toml::Value::String(String::new())
        );
    }

    #[test]
    fn bare_value_falls_back_to_string() {
        assert_eq!(
            parse_one("foo=bar").unwrap(),
            toml::Value::String("bar".to_string())
        );
    }

    #[test]
    fn quoted_toml_string_is_accepted() {
        assert_eq!(
            parse_one("foo=\"bar\"").unwrap(),
            toml::Value::String("bar".to_string())
        );
    }

    #[test]
    fn boolean_is_accepted() {
        assert_eq!(parse_one("foo=false").unwrap(), toml::Value::Boolean(false));
    }

    #[test]
    fn integer_is_accepted() {
        assert_eq!(parse_one("foo=3").unwrap(), toml::Value::Integer(3));
    }

    #[test]
    fn float_is_accepted() {
        assert_eq!(parse_one("foo=0.75").unwrap(), toml::Value::Float(0.75));
    }

    #[test]
    fn datetime_is_rejected() {
        let err = parse_one("foo=2026-05-06").unwrap_err();
        assert_eq!(err, InputOverrideParseError::UnsupportedValue {
            key:  "foo".to_string(),
            kind: "datetime",
        });
        assert!(err.to_string().contains("foo"));
        assert!(!err.to_string().contains("2026-05-06"));
    }

    #[test]
    fn array_is_rejected() {
        let err = parse_one("foo=[1]").unwrap_err();
        assert_eq!(err, InputOverrideParseError::UnsupportedValue {
            key:  "foo".to_string(),
            kind: "array",
        });
        assert!(!err.to_string().contains("[1]"));
    }

    #[test]
    fn inline_table_is_rejected() {
        let err = parse_one("foo={a=1}").unwrap_err();
        assert_eq!(err, InputOverrideParseError::UnsupportedValue {
            key:  "foo".to_string(),
            kind: "inline table",
        });
        assert!(!err.to_string().contains("{a=1}"));
    }

    #[test]
    fn missing_equals_is_rejected() {
        let err = parse_input_overrides(&["foo".to_string()]).unwrap_err();
        assert_eq!(err, InputOverrideParseError::MissingEquals {
            raw: "foo".to_string(),
        });
    }

    #[test]
    fn empty_key_is_rejected() {
        let err = parse_input_overrides(&["=bar".to_string()]).unwrap_err();
        assert_eq!(err, InputOverrideParseError::EmptyKey);
        assert!(!err.to_string().contains("=bar"));
    }

    #[test]
    fn duplicate_key_uses_last_value() {
        let parsed =
            parse_input_overrides(&["foo=first".to_string(), "foo=second".to_string()]).unwrap();
        assert_eq!(
            parsed.get("foo"),
            Some(&toml::Value::String("second".to_string()))
        );
    }

    #[test]
    fn apply_preserves_unrelated_existing_inputs() {
        let mut settings = WorkflowSettingsBuilder::from_toml(
            r#"
_version = 1

[run.inputs]
keep = "project"
override = "project"
"#,
        )
        .unwrap();
        let input_overrides = HashMap::from([(
            "override".to_string(),
            toml::Value::String("cli".to_string()),
        )]);

        apply_input_overrides(&mut settings, input_overrides);

        assert_eq!(
            settings.run.inputs.get("keep"),
            Some(&toml::Value::String("project".to_string()))
        );
        assert_eq!(
            settings.run.inputs.get("override"),
            Some(&toml::Value::String("cli".to_string()))
        );
    }
}
