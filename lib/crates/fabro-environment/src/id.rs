use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::EnvironmentValidationError;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnvironmentId(String);

impl EnvironmentId {
    pub fn new(value: impl Into<String>) -> Result<Self, EnvironmentValidationError> {
        let value = value.into();
        if is_valid_environment_id(&value) {
            Ok(Self(value))
        } else {
            Err(EnvironmentValidationError::InvalidEnvironmentId { value })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvironmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EnvironmentId {
    type Err = EnvironmentValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for EnvironmentId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EnvironmentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnvironmentRevision(String);

impl EnvironmentRevision {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvironmentRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EnvironmentRevision {
    type Err = EnvironmentRevisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_string()))
        } else {
            Err(EnvironmentRevisionParseError(value.to_string()))
        }
    }
}

impl Serialize for EnvironmentRevision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EnvironmentRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentRevisionParseError(String);

impl fmt::Display for EnvironmentRevisionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid environment revision: {:?}", self.0)
    }
}

impl std::error::Error for EnvironmentRevisionParseError {}

fn is_valid_environment_id(value: &str) -> bool {
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
    bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::{EnvironmentId, EnvironmentRevision};

    #[test]
    fn environment_id_validation_matches_contract() {
        assert!("a".parse::<EnvironmentId>().is_ok());
        assert!("a-1".parse::<EnvironmentId>().is_ok());
        assert!("0".parse::<EnvironmentId>().is_ok());
        assert!("A".parse::<EnvironmentId>().is_err());
        assert!("a_1".parse::<EnvironmentId>().is_err());
        assert!("-a".parse::<EnvironmentId>().is_err());
        assert!("a".repeat(64).parse::<EnvironmentId>().is_err());
    }

    #[test]
    fn revision_requires_lowercase_sha256_hex() {
        assert!("a".repeat(64).parse::<EnvironmentRevision>().is_ok());
        assert!("A".repeat(64).parse::<EnvironmentRevision>().is_err());
        assert!("a".repeat(63).parse::<EnvironmentRevision>().is_err());
    }
}
