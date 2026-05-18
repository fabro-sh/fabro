use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::Display;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SecretType {
    /// Opaque API-key/PAT-style token value.
    #[default]
    Token,
    /// JSON-encoded OAuth credential. Refreshable; never projected into env.
    Oauth,
    /// Path-shaped secret materialized to the filesystem.
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMetadata {
    pub name:        String,
    #[serde(rename = "type")]
    pub secret_type: SecretType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at:  DateTime<Utc>,
    pub updated_at:  DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_type_serializes_to_snake_case() {
        assert_eq!(
            serde_json::to_string(&SecretType::Token).unwrap(),
            "\"token\""
        );
        assert_eq!(
            serde_json::to_string(&SecretType::Oauth).unwrap(),
            "\"oauth\""
        );
        assert_eq!(
            serde_json::to_string(&SecretType::File).unwrap(),
            "\"file\""
        );
    }

    #[test]
    fn secret_type_default_is_token() {
        assert_eq!(SecretType::default(), SecretType::Token);
    }
}
