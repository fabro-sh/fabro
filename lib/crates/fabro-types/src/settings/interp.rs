//! Env var interpolation for config strings.
//!
//! Any string field may use `${env.NAME}` tokens, either as a whole value or
//! as one or more substrings inside a larger string. Resolution happens only
//! when the field is consumed, and provenance tracking lets outward-facing
//! renderers redact env-sourced values uniformly.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A config string that may contain `${env.NAME}` tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterpString {
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    EnvVar(String),
}

impl InterpString {
    /// Parse a raw string into its literal/env-var segments.
    ///
    /// Parsing is infallible: the token grammar is intentionally permissive so
    /// that validation happens at consumption time along with env lookup.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        let mut segments: Vec<Segment> = Vec::new();
        let mut rest = input;

        while let Some(start) = rest.find("${env.") {
            if start > 0 {
                segments.push(Segment::Literal(rest[..start].to_owned()));
            }
            // rest[start..] begins with "${env."
            let after_prefix = &rest[start + "${env.".len()..];
            if let Some(close) = after_prefix.find('}') {
                let name = after_prefix[..close].to_owned();
                segments.push(Segment::EnvVar(name));
                rest = &after_prefix[close + 1..];
            } else {
                // Unterminated token — treat the remainder as literal text.
                segments.push(Segment::Literal(rest[start..].to_owned()));
                rest = "";
                break;
            }
        }

        if !rest.is_empty() {
            segments.push(Segment::Literal(rest.to_owned()));
        }

        if segments.is_empty() {
            segments.push(Segment::Literal(String::new()));
        }

        Self { segments }
    }

    /// True when this string contains no env var tokens.
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

    /// The env var names referenced by this string, in source order.
    #[must_use]
    pub fn env_var_names(&self) -> Vec<&str> {
        self.segments
            .iter()
            .filter_map(|seg| match seg {
                Segment::EnvVar(name) => Some(name.as_str()),
                Segment::Literal(_) => None,
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
                    out.push_str("${env.");
                    out.push_str(name);
                    out.push('}');
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
                        return Err(ResolveEnvError { name: name.clone() });
                    };
                    value.push_str(&resolved);
                    used.push(name.clone());
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
    pub value: String,
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
}

impl fmt::Display for ResolveEnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "environment variable {:?} referenced by ${{env.{}}} is not set",
            self.name, self.name
        )
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
                f.write_str("a string, optionally containing ${env.NAME} interpolation tokens")
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
        let s = InterpString::parse("${env.API_KEY}");
        assert!(!s.is_literal());
        assert_eq!(s.env_var_names(), vec!["API_KEY"]);
        assert_eq!(s.as_source(), "${env.API_KEY}");
    }

    #[test]
    fn substring_env_reference() {
        let s = InterpString::parse("Bearer ${env.TOKEN}");
        assert_eq!(s.env_var_names(), vec!["TOKEN"]);
    }

    #[test]
    fn multi_token_env_reference() {
        let s = InterpString::parse("${env.USER}@${env.HOST}:${env.PORT}");
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
        let s = InterpString::parse("${env.API_KEY}");
        let resolved = s
            .resolve(lookup_from(&[("API_KEY", "secret-123")]))
            .unwrap();
        assert_eq!(resolved.value, "secret-123");
        assert_eq!(
            resolved.provenance,
            Provenance::EnvSourced {
                names: vec!["API_KEY".into()]
            }
        );
    }

    #[test]
    fn resolve_substring() {
        let s = InterpString::parse("Bearer ${env.TOKEN}");
        let resolved = s.resolve(lookup_from(&[("TOKEN", "abc")])).unwrap();
        assert_eq!(resolved.value, "Bearer abc");
    }

    #[test]
    fn resolve_multiple_tokens() {
        let s = InterpString::parse("${env.USER}@${env.HOST}");
        let resolved = s
            .resolve(lookup_from(&[("USER", "root"), ("HOST", "example.com")]))
            .unwrap();
        assert_eq!(resolved.value, "root@example.com");
        assert_eq!(
            resolved.provenance,
            Provenance::EnvSourced {
                names: vec!["USER".into(), "HOST".into()]
            }
        );
    }

    #[test]
    fn resolve_missing_env_fails_with_name() {
        let s = InterpString::parse("${env.MISSING}");
        let err = s.resolve(lookup_from(&[])).unwrap_err();
        assert_eq!(err.name, "MISSING");
    }

    #[test]
    fn unterminated_token_treated_as_literal() {
        let s = InterpString::parse("${env.OPEN");
        let resolved = s.resolve(lookup_from(&[])).unwrap();
        assert_eq!(resolved.value, "${env.OPEN");
        assert_eq!(resolved.provenance, Provenance::Literal);
    }

    #[test]
    fn serde_round_trip_preserves_token_form() {
        #[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq)]
        struct Wrap {
            s: InterpString,
        }

        let input = r#"{"s":"Bearer ${env.TOKEN}"}"#;
        let parsed: Wrap = serde_json::from_str(input).unwrap();
        assert_eq!(parsed.s.as_source(), "Bearer ${env.TOKEN}");
        let rendered = serde_json::to_string(&parsed).unwrap();
        assert_eq!(rendered, input);
    }
}
