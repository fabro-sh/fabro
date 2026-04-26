use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecord {
    pub provider:               String,
    pub working_directory:      String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier:             Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_mount_point:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_cloned:            Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_origin_url:       Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_branch:           Option<String>,
}
