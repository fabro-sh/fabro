use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[must_use]
pub fn is_env_style_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }

    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Variable {
    pub name:        String,
    pub value:       String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at:  DateTime<Utc>,
    pub updated_at:  DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VariableListResponse {
    pub data: Vec<Variable>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateVariableRequest {
    pub name:        String,
    pub value:       String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateVariableRequest {
    pub value:       String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::is_env_style_name;

    #[test]
    fn env_style_names_match_variable_store_contract() {
        for valid in ["A", "_A", "A_123"] {
            assert!(is_env_style_name(valid), "{valid} should be accepted");
        }

        for invalid in ["", "1BAD", "bad-name", "BAD.NAME"] {
            assert!(!is_env_style_name(invalid), "{invalid} should be rejected");
        }
    }
}
