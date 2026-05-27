use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::AutomationValidationError;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AutomationId(String);

impl AutomationId {
    pub fn new(value: impl Into<String>) -> Result<Self, AutomationValidationError> {
        let value = value.into();
        if is_valid_automation_id(&value) {
            Ok(Self(value))
        } else {
            Err(AutomationValidationError::InvalidAutomationId { value })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AutomationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AutomationId {
    type Err = AutomationValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for AutomationId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AutomationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AutomationTriggerId(String);

impl AutomationTriggerId {
    pub fn new(value: impl Into<String>) -> Result<Self, AutomationValidationError> {
        let value = value.into();
        if is_valid_automation_trigger_id(&value) {
            Ok(Self(value))
        } else {
            Err(AutomationValidationError::InvalidAutomationTriggerId { value })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AutomationTriggerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AutomationTriggerId {
    type Err = AutomationValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for AutomationTriggerId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AutomationTriggerId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AutomationRevision(String);

impl AutomationRevision {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AutomationRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AutomationRevision {
    type Err = AutomationRevisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_string()))
        } else {
            Err(AutomationRevisionParseError(value.to_string()))
        }
    }
}

impl Serialize for AutomationRevision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AutomationRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationRevisionParseError(String);

impl fmt::Display for AutomationRevisionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid automation revision: {:?}", self.0)
    }
}

impl std::error::Error for AutomationRevisionParseError {}

fn is_valid_automation_id(value: &str) -> bool {
    is_valid_id(value, false)
}

fn is_valid_automation_trigger_id(value: &str) -> bool {
    is_valid_id(value, true)
}

fn is_valid_id(value: &str, allow_underscore: bool) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    if value.len() > 63 {
        return false;
    }
    bytes.all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || byte == b'-'
            || (allow_underscore && byte == b'_')
    })
}

#[cfg(test)]
mod tests {
    use super::{AutomationId, AutomationRevision, AutomationTriggerId};

    #[test]
    fn automation_id_validation_matches_contract() {
        assert!("a".parse::<AutomationId>().is_ok());
        assert!("a-1".parse::<AutomationId>().is_ok());
        assert!("0".parse::<AutomationId>().is_ok());
        assert!("A".parse::<AutomationId>().is_err());
        assert!("a_1".parse::<AutomationId>().is_err());
        assert!("-a".parse::<AutomationId>().is_err());
        assert!("a".repeat(64).parse::<AutomationId>().is_err());
    }

    #[test]
    fn trigger_id_allows_underscore() {
        assert!("api_trigger".parse::<AutomationTriggerId>().is_ok());
        assert!("api.trigger".parse::<AutomationTriggerId>().is_err());
    }

    #[test]
    fn revision_is_lowercase_sha256_hex() {
        let revision = AutomationRevision::from_bytes(b"hello");
        assert_eq!(
            revision.to_string(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(revision.to_string().parse::<AutomationRevision>().is_ok());
        assert!("ABC".parse::<AutomationRevision>().is_err());
    }
}
