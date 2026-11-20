use serde::{Deserialize, Serialize};

use crate::{SandboxProviderKind, SandboxStatus};

/// One sandbox of fabro's inventory: the provider fabro connected it
/// through, and the status the sandbox driver reports for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxInfo {
    pub provider: SandboxProviderKind,
    pub status:   SandboxStatus,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxListResponse {
    pub data: Vec<SandboxInfo>,
    pub meta: SandboxListMeta,
}
