use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CheckpointSettings {
    #[serde(default)]
    pub exclude_globs: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PullRequestSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub draft: bool,
    #[serde(default)]
    pub auto_merge: bool,
    #[serde(default)]
    pub merge_strategy: MergeStrategy,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
#[serde(rename_all = "lowercase")]
pub enum MergeStrategy {
    #[default]
    Squash,
    Merge,
    Rebase,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ArtifactsSettings {
    #[serde(default)]
    pub include: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct GitHubSettings {
    #[serde(default)]
    pub permissions: HashMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LlmSettings {
    pub model: Option<String>,
    pub provider: Option<String>,
    #[serde(default)]
    pub fallbacks: Option<HashMap<String, Vec<String>>>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SetupSettings {
    #[serde(default)]
    pub commands: Vec<String>,
    pub timeout_ms: Option<u64>,
}
