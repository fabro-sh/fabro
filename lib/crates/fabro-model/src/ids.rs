//! String-backed provider and model identifiers.
//!
//! Provider and model identity are catalog data, not closed enums. These
//! newtypes give catalog/auth/server seams a single, type-safe wrapper while
//! keeping wire format compatible with plain strings.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable provider identifier referenced from settings, vault, and request
/// routing.
///
/// Wraps a `String` because the set of providers is open-ended and supplied
/// by `[llm.providers]` settings rather than compiled into a Rust enum.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    /// Construct a provider ID from any string-like value without validation.
    /// Catalog construction is responsible for canonicalisation; consumers
    /// only need a wrapper for type clarity.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the inner string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the inner `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ProviderId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for ProviderId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl AsRef<str> for ProviderId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Stable model identifier — either the canonical catalog ID or one of its
/// declared aliases.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ModelId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for ModelId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl AsRef<str> for ModelId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_is_transparent_string_in_json() {
        let id = ProviderId::new("kimi");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"kimi\"");
        let back: ProviderId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn model_id_is_transparent_string_in_json() {
        let id = ModelId::new("kimi-k2.5");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"kimi-k2.5\"");
        let back: ModelId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn display_writes_inner_string() {
        assert_eq!(ProviderId::new("anthropic").to_string(), "anthropic");
        assert_eq!(
            ModelId::new("claude-opus-4-7").to_string(),
            "claude-opus-4-7"
        );
    }

    #[test]
    fn ord_is_lexicographic() {
        let mut v = [ProviderId::new("zai"), ProviderId::new("anthropic")];
        v.sort();
        assert_eq!(v[0].as_str(), "anthropic");
        assert_eq!(v[1].as_str(), "zai");
    }
}
