use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A string-backed provider identifier.
///
/// Unlike the closed `Provider` enum, `ProviderId` can represent any
/// provider — built-in or user-defined through settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    /// Create a new provider ID from a string.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The string value of this provider ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ProviderId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
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

impl PartialEq<str> for ProviderId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ProviderId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Convert from the legacy `Provider` enum for migration compatibility.
impl From<crate::Provider> for ProviderId {
    fn from(p: crate::Provider) -> Self {
        Self(p.to_string())
    }
}

/// A string-backed model identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    /// Create a new model ID from a string.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The string value of this model ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ModelId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
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

impl PartialEq<str> for ModelId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ModelId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_from_str() {
        let id: ProviderId = "anthropic".parse().unwrap();
        assert_eq!(id.as_str(), "anthropic");
    }

    #[test]
    fn provider_id_display() {
        let id = ProviderId::new("openai");
        assert_eq!(id.to_string(), "openai");
    }

    #[test]
    fn provider_id_serde_roundtrip() {
        let id = ProviderId::new("kimi");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"kimi\"");
        let parsed: ProviderId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn provider_id_from_legacy_provider() {
        let id = ProviderId::from(crate::Provider::Anthropic);
        assert_eq!(id.as_str(), "anthropic");
    }

    #[test]
    fn provider_id_eq_str() {
        let id = ProviderId::new("anthropic");
        assert_eq!(id, "anthropic");
        assert_eq!(id, *"anthropic");
    }

    #[test]
    fn model_id_from_str() {
        let id: ModelId = "claude-opus-4-6".parse().unwrap();
        assert_eq!(id.as_str(), "claude-opus-4-6");
    }

    #[test]
    fn model_id_display() {
        let id = ModelId::new("gpt-5.4");
        assert_eq!(id.to_string(), "gpt-5.4");
    }

    #[test]
    fn model_id_serde_roundtrip() {
        let id = ModelId::new("gemini-3.1-pro-preview");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"gemini-3.1-pro-preview\"");
        let parsed: ModelId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }
}
