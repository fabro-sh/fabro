//! Env var and run variable interpolation for config strings.
//!
//! Any string field may use `{{ env.NAME }}` tokens, either as a whole value or
//! as one or more substrings inside a larger string. Run-scoped settings may
//! additionally use non-sensitive `{{ vars.NAME }}` tokens. Resolution happens
//! only when the field is consumed, and provenance tracking lets outward-facing
//! renderers redact env-sourced values uniformly.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::variable::is_env_style_name;

/// A config string that may contain `{{ env.NAME }}` or `{{ vars.NAME }}`
/// tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterpString {
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    EnvVar(String),
    Variable(String),
}

impl InterpString {
    fn push_literal(segments: &mut Vec<Segment>, text: &str) {
        if text.is_empty() {
            return;
        }

        match segments.last_mut() {
            Some(Segment::Literal(existing)) => existing.push_str(text),
            Some(Segment::EnvVar(_) | Segment::Variable(_)) | None => {
                segments.push(Segment::Literal(text.to_owned()));
            }
        }
    }

    fn parse_env_token(token: &str) -> Option<String> {
        let trimmed = token.trim();
        let name = trimmed.strip_prefix("env.")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return None;
        }
        Some(name.to_owned())
    }

    fn parse_vars_token(token: &str) -> Option<String> {
        let trimmed = token.trim();
        let name = trimmed.strip_prefix("vars.")?;
        if is_env_style_name(name) {
            Some(name.to_owned())
        } else {
            None
        }
    }

    /// Parse a raw string into its literal/env-var segments.
    ///
    /// The [`From<String>`] and [`From<&str>`] impls delegate here.
    ///
    /// Parsing is infallible: the token grammar is intentionally permissive so
    /// that validation happens at consumption time along with env lookup.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        let mut segments: Vec<Segment> = Vec::new();
        let mut rest = input;

        while let Some(start) = rest.find("{{") {
            Self::push_literal(&mut segments, &rest[..start]);

            let after_open = &rest[start + 2..];
            if let Some(close) = after_open.find("}}") {
                let token = &after_open[..close];
                if let Some(name) = Self::parse_env_token(token) {
                    segments.push(Segment::EnvVar(name));
                } else if let Some(name) = Self::parse_vars_token(token) {
                    segments.push(Segment::Variable(name));
                } else {
                    Self::push_literal(&mut segments, &rest[start..start + 2 + close + 2]);
                }
                rest = &after_open[close + 2..];
            } else {
                // Unterminated token — treat the remainder as literal text.
                Self::push_literal(&mut segments, &rest[start..]);
                rest = "";
                break;
            }
        }

        if !rest.is_empty() {
            Self::push_literal(&mut segments, rest);
        }

        if segments.is_empty() {
            segments.push(Segment::Literal(String::new()));
        }

        Self { segments }
    }

    /// True when this string contains no interpolation tokens.
    #[must_use]
    pub fn is_literal(&self) -> bool {
        self.segments
            .iter()
            .all(|seg| matches!(seg, Segment::Literal(_)))
    }

    /// True when this string contains at least one env var token.
    #[must_use]
    pub fn references_env(&self) -> bool {
        self.segments
            .iter()
            .any(|seg| matches!(seg, Segment::EnvVar(_)))
    }

    /// True when this string contains at least one run variable token.
    #[must_use]
    pub fn references_vars(&self) -> bool {
        self.segments
            .iter()
            .any(|seg| matches!(seg, Segment::Variable(_)))
    }

    /// The env var names referenced by this string, in source order.
    #[must_use]
    pub fn env_var_names(&self) -> Vec<&str> {
        self.segments
            .iter()
            .filter_map(|seg| match seg {
                Segment::EnvVar(name) => Some(name.as_str()),
                Segment::Literal(_) | Segment::Variable(_) => None,
            })
            .collect()
    }

    /// The run variable names referenced by this string, in source order.
    #[must_use]
    pub fn var_names(&self) -> Vec<&str> {
        self.segments
            .iter()
            .filter_map(|seg| match seg {
                Segment::Variable(name) => Some(name.as_str()),
                Segment::Literal(_) | Segment::EnvVar(_) => None,
            })
            .collect()
    }

    /// The raw source string.
    #[must_use]
    pub fn as_source(&self) -> String {
        let mut out = String::new();
        for seg in &self.segments {
            match seg {
                Segment::Literal(text) => out.push_str(text),
                Segment::EnvVar(name) => {
                    out.push_str("{{ env.");
                    out.push_str(name);
                    out.push_str(" }}");
                }
                Segment::Variable(name) => {
                    out.push_str("{{ vars.");
                    out.push_str(name);
                    out.push_str(" }}");
                }
            }
        }
        out
    }

    /// Resolve this string using `lookup`, which should return the current
    /// value for a given env var name (or `None` if unset).
    ///
    /// On success the caller gets the final string plus provenance describing
    /// whether any env var contributed to the value. On failure the caller
    /// learns which env var was unresolved.
    pub fn resolve<F>(&self, mut lookup: F) -> Result<Resolved, ResolveEnvError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let mut value = String::new();
        let mut used = Vec::new();
        for seg in &self.segments {
            match seg {
                Segment::Literal(text) => value.push_str(text),
                Segment::EnvVar(name) => {
                    let Some(resolved) = lookup(name) else {
                        return Err(ResolveEnvError::missing_env(name));
                    };
                    value.push_str(&resolved);
                    used.push(name.clone());
                }
                Segment::Variable(name) => {
                    return Err(ResolveEnvError::unsupported_variable(name));
                }
            }
        }

        let provenance = if used.is_empty() {
            Provenance::Literal
        } else {
            Provenance::EnvSourced { names: used }
        };
        Ok(Resolved { value, provenance })
    }

    /// Resolve env and run variable tokens with separate lookup functions.
    ///
    /// Variables are non-sensitive, so variable-only interpolation does not
    /// mark the value as env-sourced for redaction.
    pub fn resolve_with_variables<F, G>(
        &self,
        mut env_lookup: F,
        mut variable_lookup: G,
    ) -> Result<Resolved, ResolveEnvError>
    where
        F: FnMut(&str) -> Option<String>,
        G: FnMut(&str) -> Option<String>,
    {
        let mut value = String::new();
        let mut used_env = Vec::new();
        for seg in &self.segments {
            match seg {
                Segment::Literal(text) => value.push_str(text),
                Segment::EnvVar(name) => {
                    let Some(resolved) = env_lookup(name) else {
                        return Err(ResolveEnvError::missing_env(name));
                    };
                    value.push_str(&resolved);
                    used_env.push(name.clone());
                }
                Segment::Variable(name) => {
                    let Some(resolved) = variable_lookup(name) else {
                        return Err(ResolveEnvError::missing_variable(name));
                    };
                    value.push_str(&resolved);
                }
            }
        }

        let provenance = if used_env.is_empty() {
            Provenance::Literal
        } else {
            Provenance::EnvSourced { names: used_env }
        };
        Ok(Resolved { value, provenance })
    }

    /// Substitute only `{{ vars.* }}` tokens while preserving `{{ env.* }}`
    /// tokens for their existing consumption-time env lookup.
    pub fn substitute_variables<F>(&self, mut lookup: F) -> Result<Self, ResolveEnvError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let mut segments = Vec::new();
        for seg in &self.segments {
            match seg {
                Segment::Literal(text) => Self::push_literal(&mut segments, text),
                Segment::EnvVar(name) => segments.push(Segment::EnvVar(name.clone())),
                Segment::Variable(name) => {
                    let Some(resolved) = lookup(name) else {
                        return Err(ResolveEnvError::missing_variable(name));
                    };
                    Self::push_literal(&mut segments, &resolved);
                }
            }
        }
        if segments.is_empty() {
            segments.push(Segment::Literal(String::new()));
        }
        Ok(Self { segments })
    }
}

impl From<String> for InterpString {
    fn from(value: String) -> Self {
        Self::parse(&value)
    }
}

impl From<&str> for InterpString {
    fn from(value: &str) -> Self {
        Self::parse(value)
    }
}

/// The outcome of a successful env interpolation resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub value:      String,
    pub provenance: Provenance,
}

/// Provenance metadata for resolved config values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// No env var contributed to this value.
    Literal,
    /// One or more env vars contributed to this value. Used by outward-facing
    /// renderers to redact env-sourced values uniformly.
    EnvSourced { names: Vec<String> },
}

/// An error returned when an env var referenced in a config string is not set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveEnvError {
    pub name: String,
    pub kind: ResolveEnvErrorKind,
}

impl ResolveEnvError {
    fn missing_env(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: ResolveEnvErrorKind::MissingEnv,
        }
    }

    fn missing_variable(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: ResolveEnvErrorKind::MissingVariable,
        }
    }

    fn unsupported_variable(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: ResolveEnvErrorKind::UnsupportedVariable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveEnvErrorKind {
    MissingEnv,
    MissingVariable,
    UnsupportedVariable,
}

impl fmt::Display for ResolveEnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ResolveEnvErrorKind::MissingEnv => write!(
                f,
                "environment variable {:?} referenced by {{{{ env.{} }}}} is not set",
                self.name, self.name
            ),
            ResolveEnvErrorKind::MissingVariable => write!(
                f,
                "variable {:?} referenced by {{{{ vars.{} }}}} is not set",
                self.name, self.name
            ),
            ResolveEnvErrorKind::UnsupportedVariable => write!(
                f,
                "variable {:?} referenced by {{{{ vars.{} }}}} is not supported in this \
                 interpolation context",
                self.name, self.name
            ),
        }
    }
}

impl std::error::Error for ResolveEnvError {}

impl Serialize for InterpString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_source())
    }
}

impl<'de> Deserialize<'de> for InterpString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct InterpStringVisitor;

        impl Visitor<'_> for InterpStringVisitor {
            type Value = InterpString;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string, optionally containing {{ env.NAME }} interpolation tokens")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<InterpString, E> {
                Ok(InterpString::parse(value))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<InterpString, E> {
                Ok(InterpString::parse(&value))
            }
        }

        deserializer.deserialize_str(InterpStringVisitor)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup_from(values: &[(&str, &str)]) -> impl FnMut(&str) -> Option<String> + 'static {
        let map: HashMap<String, String> = values
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn literal_string_has_no_env_refs() {
        let s = InterpString::parse("hello world");
        assert!(s.is_literal());
        assert!(!s.references_env());
        assert_eq!(s.env_var_names(), Vec::<&str>::new());
    }

    #[test]
    fn whole_value_env_reference() {
        let s = InterpString::parse("{{ env.API_KEY }}");
        assert!(!s.is_literal());
        assert_eq!(s.env_var_names(), vec!["API_KEY"]);
        assert_eq!(s.as_source(), "{{ env.API_KEY }}");
    }

    #[test]
    fn substring_env_reference() {
        let s = InterpString::parse("Bearer {{ env.TOKEN }}");
        assert_eq!(s.env_var_names(), vec!["TOKEN"]);
    }

    #[test]
    fn multi_token_env_reference() {
        let s = InterpString::parse("{{ env.USER }}@{{ env.HOST }}:{{env.PORT}}");
        assert_eq!(s.env_var_names(), vec!["USER", "HOST", "PORT"]);
    }

    #[test]
    fn resolve_literal_string() {
        let s = InterpString::parse("static");
        let resolved = s.resolve(lookup_from(&[])).unwrap();
        assert_eq!(resolved.value, "static");
        assert_eq!(resolved.provenance, Provenance::Literal);
    }

    #[test]
    fn resolve_whole_value() {
        let s = InterpString::parse("{{ env.API_KEY }}");
        let resolved = s
            .resolve(lookup_from(&[("API_KEY", "secret-123")]))
            .unwrap();
        assert_eq!(resolved.value, "secret-123");
        assert_eq!(resolved.provenance, Provenance::EnvSourced {
            names: vec!["API_KEY".into()],
        });
    }

    #[test]
    fn resolve_substring() {
        let s = InterpString::parse("Bearer {{ env.TOKEN }}");
        let resolved = s.resolve(lookup_from(&[("TOKEN", "abc")])).unwrap();
        assert_eq!(resolved.value, "Bearer abc");
    }

    #[test]
    fn resolve_multiple_tokens() {
        let s = InterpString::parse("{{ env.USER }}@{{ env.HOST }}");
        let resolved = s
            .resolve(lookup_from(&[("USER", "root"), ("HOST", "example.com")]))
            .unwrap();
        assert_eq!(resolved.value, "root@example.com");
        assert_eq!(resolved.provenance, Provenance::EnvSourced {
            names: vec!["USER".into(), "HOST".into()],
        });
    }

    #[test]
    fn resolve_missing_env_fails_with_name() {
        let s = InterpString::parse("{{ env.MISSING }}");
        let err = s.resolve(lookup_from(&[])).unwrap_err();
        assert_eq!(err.name, "MISSING");
    }

    #[test]
    fn unterminated_token_treated_as_literal() {
        let s = InterpString::parse("{{ env.OPEN");
        let resolved = s.resolve(lookup_from(&[])).unwrap();
        assert_eq!(resolved.value, "{{ env.OPEN");
        assert_eq!(resolved.provenance, Provenance::Literal);
    }

    #[test]
    fn serde_round_trip_preserves_token_form() {
        #[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq)]
        struct Wrap {
            s: InterpString,
        }

        let input = r#"{"s":"Bearer {{ env.TOKEN }}"}"#;
        let parsed: Wrap = serde_json::from_str(input).unwrap();
        assert_eq!(parsed.s.as_source(), "Bearer {{ env.TOKEN }}");
        let rendered = serde_json::to_string(&parsed).unwrap();
        assert_eq!(rendered, input);
    }

    #[test]
    fn vars_reference_round_trips_source() {
        let s = InterpString::parse("{{ vars.RUNTIME_TOKEN }}");

        assert_eq!(s.var_names(), vec!["RUNTIME_TOKEN"]);
        assert_eq!(s.as_source(), "{{ vars.RUNTIME_TOKEN }}");
    }

    #[test]
    fn resolve_with_variables_substitutes_env_and_var_tokens() {
        let s = InterpString::parse("https://{{ env.REGION }}.{{ vars.DOMAIN }}");

        let resolved = s
            .resolve_with_variables(
                lookup_from(&[("REGION", "us-east-1")]),
                lookup_from(&[("DOMAIN", "example.com")]),
            )
            .unwrap();

        assert_eq!(resolved.value, "https://us-east-1.example.com");
        assert_eq!(resolved.provenance, Provenance::EnvSourced {
            names: vec!["REGION".into()],
        });
    }

    #[test]
    fn resolve_with_variables_reports_missing_variable() {
        let s = InterpString::parse("{{ vars.MISSING }}");

        let err = s
            .resolve_with_variables(lookup_from(&[]), lookup_from(&[]))
            .unwrap_err();

        assert_eq!(err.name, "MISSING");
        assert_eq!(err.kind, ResolveEnvErrorKind::MissingVariable);
    }

    #[test]
    fn env_only_resolution_rejects_vars_reference() {
        let s = InterpString::parse("{{ vars.RUNTIME_TOKEN }}");

        let err = s.resolve(lookup_from(&[])).unwrap_err();

        assert_eq!(err.name, "RUNTIME_TOKEN");
        assert_eq!(err.kind, ResolveEnvErrorKind::UnsupportedVariable);
    }
}
