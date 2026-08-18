use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Selected ACP harness profile for external coding agents.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ExternalAgentHarness {
    Codex,
    Omp,
}

impl ExternalAgentHarness {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// Server-level process specification for an external agent harness profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalAgentProfile {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args:    Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env:     BTreeMap<String, String>,
}

/// Server-level external agent profiles allowlist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalAgentsSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<ExternalAgentProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omp:   Option<ExternalAgentProfile>,
}
