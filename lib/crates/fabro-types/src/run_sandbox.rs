use serde::{Deserialize, Serialize};

use crate::SandboxProviderKind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSandbox {
    pub provider: SandboxProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime:  Option<RunSandboxRuntime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSandboxRuntime {
    pub id:                String,
    pub working_directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_cloned:       Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_origin_url:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_branch:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos_root:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_repo_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_repo_link: Option<String>,
}
