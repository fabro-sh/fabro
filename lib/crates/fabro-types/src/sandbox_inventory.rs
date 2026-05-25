use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    SandboxNetwork, SandboxProviderKind, SandboxResources, SandboxState, SandboxTimestamps,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxInfo {
    pub provider:          SandboxProviderKind,
    pub id:                String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name:      Option<String>,
    pub state:             SandboxState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_state:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image:             Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot:          Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region:            Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url:           Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    pub resources:         SandboxResources,
    #[serde(default)]
    pub network:           SandboxNetwork,
    #[serde(default)]
    pub labels:            BTreeMap<String, String>,
    pub timestamps:        SandboxTimestamps,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxProviderLookupError {
    pub provider: SandboxProviderKind,
    pub message:  String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SandboxListMeta {
    #[serde(default)]
    pub provider_errors: Vec<SandboxProviderLookupError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxListResponse {
    pub data: Vec<SandboxInfo>,
    pub meta: SandboxListMeta,
}
