//! Sparse `[cli]` settings layer definitions.

use fabro_types::settings::InterpString;
use fabro_types::settings::cli::{CliAuthStrategy, OutputFormat, OutputVerbosity};
use fabro_types::settings::run::AgentPermissions;
use serde::{Deserialize, Serialize};

use super::maps::StickyMap;
use super::run::McpEntryLayer;

/// A sparse `[cli]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct CliLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target:  Option<CliTargetLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth:    Option<CliAuthLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec:    Option<CliExecLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output:  Option<CliOutputLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updates: Option<CliUpdatesLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<CliLoggingLayer>,
}

/// `[cli.target]` — explicit transport selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "lowercase")]
pub enum CliTargetLayer {
    Http {
        #[serde(default)]
        url: Option<InterpString>,
    },
    Unix {
        #[serde(default)]
        path: Option<InterpString>,
    },
}

/// `[cli.auth]` — explicit auth strategy selection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliAuthLayer {
    /// `none` explicitly disables inherited auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<CliAuthStrategy>,
}

/// `[cli.exec]` — `fabro exec` defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct CliExecLayer {
    /// Prevent idle sleep on macOS while an exec run is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prevent_idle_sleep: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:              Option<CliExecModelLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent:              Option<CliExecAgentLayer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct CliExecModelLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name:     Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct CliExecAgentLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<AgentPermissions>,
    /// Agent-scoped MCP entries for `fabro exec`.
    #[serde(default, skip_serializing_if = "StickyMap::is_empty")]
    pub mcps:        StickyMap<McpEntryLayer>,
}

/// `[cli.output]` — generic CLI output defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct CliOutputLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format:    Option<OutputFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<OutputVerbosity>,
}

/// `[cli.updates]` — upgrade check toggle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct CliUpdatesLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<bool>,
}

/// `[cli.logging]` — process-owned logging configuration for the CLI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliLoggingLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}
