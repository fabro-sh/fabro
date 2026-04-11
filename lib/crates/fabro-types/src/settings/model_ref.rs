//! Model references for `run.model.fallbacks`.
//!
//! Each entry is one of:
//!
//! - a bare token such as `openai` or `gpt-5.4` — the parser cannot tell alone
//!   whether the token is a provider name or a model alias
//! - a qualified reference such as `gemini/gemini-flash`, which names both a
//!   provider and a model
//!
//! The parser produces [`ModelRef`]; ambiguity resolution against a known
//! registry of providers and models happens at consumption time via
//! [`ModelRef::resolve`].

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A parsed model reference. Bare tokens remain ambiguous until resolved
/// against a registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRef {
    /// A bare token. May be a provider name, a model alias, or a model id.
    Bare(String),
    /// A provider-qualified model reference.
    Qualified { provider: String, model: String },
}

/// An error returned when parsing a model reference fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseModelRefError {
    /// The input was empty or whitespace only.
    Empty,
    /// The input contained more than one `/`, which is not a valid qualified
    /// ref.
    TooManySlashes { input: String },
    /// The provider or model side of a qualified reference was empty.
    EmptySide { input: String },
}

impl fmt::Display for ParseModelRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("model reference is empty"),
            Self::TooManySlashes { input } => {
                write!(
                    f,
                    "model reference {input:?}: expected at most one \"/\" separator between provider and model"
                )
            }
            Self::EmptySide { input } => {
                write!(
                    f,
                    "model reference {input:?}: provider and model sides must both be non-empty"
                )
            }
        }
    }
}

impl std::error::Error for ParseModelRefError {}

impl FromStr for ModelRef {
    type Err = ParseModelRefError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(ParseModelRefError::Empty);
        }

        let parts: Vec<&str> = trimmed.split('/').collect();
        match parts.as_slice() {
            [bare] => Ok(Self::Bare((*bare).to_owned())),
            [provider, model] => {
                if provider.is_empty() || model.is_empty() {
                    Err(ParseModelRefError::EmptySide {
                        input: input.to_owned(),
                    })
                } else {
                    Ok(Self::Qualified {
                        provider: (*provider).to_owned(),
                        model:    (*model).to_owned(),
                    })
                }
            }
            _ => Err(ParseModelRefError::TooManySlashes {
                input: input.to_owned(),
            }),
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bare(token) => f.write_str(token),
            Self::Qualified { provider, model } => write!(f, "{provider}/{model}"),
        }
    }
}

/// An error returned when resolving an ambiguous bare model reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousModelRef {
    pub input:     String,
    pub providers: Vec<String>,
    pub models:    Vec<String>,
}

impl fmt::Display for AmbiguousModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "model reference {:?} is ambiguous: matches provider names {:?} and model names {:?}; qualify it as \"provider/model\"",
            self.input, self.providers, self.models
        )
    }
}

impl std::error::Error for AmbiguousModelRef {}

/// The resolved form of a model reference after registry lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedModelRef {
    /// The reference named a provider; the runtime should pick the best model
    /// from that provider.
    Provider(String),
    /// The reference named a model (qualified or unambiguously bare).
    Model {
        provider: Option<String>,
        model:    String,
    },
}

/// A minimal registry view used by [`ModelRef::resolve`].
///
/// Each method reports whether a bare token is a known provider, model, or
/// both. The registry is abstract so unit tests and runtime resolution can
/// share the same logic.
pub trait ModelRegistry {
    fn is_provider(&self, token: &str) -> bool;
    fn is_model(&self, token: &str) -> bool;
    /// Returns the canonical provider for a bare model token, when the registry
    /// knows of a unique provider for that model.
    fn provider_of(&self, token: &str) -> Option<String>;
}

impl ModelRef {
    /// Resolve this reference against a registry.
    ///
    /// - [`ModelRef::Qualified`] always resolves to a model.
    /// - [`ModelRef::Bare`] resolves to a provider if the token is only a
    ///   provider, to a model if the token is only a model, and returns
    ///   [`AmbiguousModelRef`] if the token matches both a provider and a model
    ///   name.
    pub fn resolve(
        &self,
        registry: &dyn ModelRegistry,
    ) -> Result<ResolvedModelRef, AmbiguousModelRef> {
        match self {
            Self::Qualified { provider, model } => Ok(ResolvedModelRef::Model {
                provider: Some(provider.clone()),
                model:    model.clone(),
            }),
            Self::Bare(token) => {
                let is_provider = registry.is_provider(token);
                let is_model = registry.is_model(token);
                match (is_provider, is_model) {
                    (true, false) => Ok(ResolvedModelRef::Provider(token.clone())),
                    (false, true) => Ok(ResolvedModelRef::Model {
                        provider: registry.provider_of(token),
                        model:    token.clone(),
                    }),
                    (true, true) => Err(AmbiguousModelRef {
                        input:     token.clone(),
                        providers: vec![token.clone()],
                        models:    vec![token.clone()],
                    }),
                    // Unknown tokens flow through as bare models — provider TBD at runtime.
                    (false, false) => Ok(ResolvedModelRef::Model {
                        provider: None,
                        model:    token.clone(),
                    }),
                }
            }
        }
    }
}

impl Serialize for ModelRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ModelRefVisitor;

        impl Visitor<'_> for ModelRefVisitor {
            type Value = ModelRef;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    r#"a model reference such as "openai", "gpt-5.4", or "gemini/gemini-flash""#,
                )
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<ModelRef, E> {
                value.parse().map_err(de::Error::custom)
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<ModelRef, E> {
                self.visit_str(&value)
            }
        }

        deserializer.deserialize_str(ModelRefVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRegistry {
        providers: &'static [&'static str],
        models:    &'static [&'static str],
    }

    impl ModelRegistry for TestRegistry {
        fn is_provider(&self, token: &str) -> bool {
            self.providers.contains(&token)
        }
        fn is_model(&self, token: &str) -> bool {
            self.models.contains(&token)
        }
        fn provider_of(&self, token: &str) -> Option<String> {
            if self.models.contains(&token) {
                Some("test".to_owned())
            } else {
                None
            }
        }
    }

    #[test]
    fn parses_bare_token() {
        assert_eq!(
            "openai".parse::<ModelRef>().unwrap(),
            ModelRef::Bare("openai".into())
        );
    }

    #[test]
    fn parses_qualified() {
        assert_eq!(
            "gemini/gemini-flash".parse::<ModelRef>().unwrap(),
            ModelRef::Qualified {
                provider: "gemini".into(),
                model:    "gemini-flash".into(),
            }
        );
    }

    #[test]
    fn rejects_too_many_slashes() {
        let err = "a/b/c".parse::<ModelRef>().unwrap_err();
        assert!(matches!(err, ParseModelRefError::TooManySlashes { .. }));
    }

    #[test]
    fn rejects_empty_side() {
        assert!(matches!(
            "/foo".parse::<ModelRef>().unwrap_err(),
            ParseModelRefError::EmptySide { .. }
        ));
        assert!(matches!(
            "foo/".parse::<ModelRef>().unwrap_err(),
            ParseModelRefError::EmptySide { .. }
        ));
    }

    #[test]
    fn rejects_empty_input() {
        assert!(matches!(
            "".parse::<ModelRef>().unwrap_err(),
            ParseModelRefError::Empty
        ));
    }

    #[test]
    fn resolves_unique_provider_token() {
        let reg = TestRegistry {
            providers: &["openai"],
            models:    &[],
        };
        let resolved = ModelRef::Bare("openai".into()).resolve(&reg).unwrap();
        assert_eq!(resolved, ResolvedModelRef::Provider("openai".into()));
    }

    #[test]
    fn resolves_unique_model_token() {
        let reg = TestRegistry {
            providers: &[],
            models:    &["gpt-5.4"],
        };
        let resolved = ModelRef::Bare("gpt-5.4".into()).resolve(&reg).unwrap();
        assert_eq!(resolved, ResolvedModelRef::Model {
            provider: Some("test".into()),
            model:    "gpt-5.4".into(),
        });
    }

    #[test]
    fn ambiguous_bare_token_errors() {
        let reg = TestRegistry {
            providers: &["ambiguous"],
            models:    &["ambiguous"],
        };
        let err = ModelRef::Bare("ambiguous".into())
            .resolve(&reg)
            .unwrap_err();
        assert_eq!(err.input, "ambiguous");
    }

    #[test]
    fn qualified_never_ambiguous() {
        let reg = TestRegistry {
            providers: &["ambiguous"],
            models:    &["ambiguous"],
        };
        let resolved = ModelRef::Qualified {
            provider: "a".into(),
            model:    "b".into(),
        }
        .resolve(&reg)
        .unwrap();
        assert_eq!(resolved, ResolvedModelRef::Model {
            provider: Some("a".into()),
            model:    "b".into(),
        });
    }

    #[test]
    fn display_round_trip() {
        for input in ["openai", "gpt-5.4", "gemini/gemini-flash"] {
            let parsed: ModelRef = input.parse().unwrap();
            assert_eq!(parsed.to_string(), input);
        }
    }

    #[test]
    fn serde_round_trip_via_json() {
        #[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq)]
        struct Wrap {
            m: ModelRef,
        }

        let input = r#"{"m":"gemini/gemini-flash"}"#;
        let parsed: Wrap = serde_json::from_str(input).unwrap();
        assert!(matches!(
            parsed.m,
            ModelRef::Qualified { ref provider, ref model }
                if provider == "gemini" && model == "gemini-flash"
        ));
        let rendered = serde_json::to_string(&parsed).unwrap();
        assert_eq!(rendered, input);
    }
}
