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
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    fabro_macros::Combine,
    fabro_macros::OptionsMetadata,
)]
#[serde(deny_unknown_fields)]
pub struct CliExecLayer {
    /// Prevent idle sleep on macOS while an exec run is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[option(name = "prevent_idle_sleep", default = "false", value_type = "boolean")]
    pub prevent_idle_sleep: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:              Option<CliExecModelLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent:              Option<CliExecAgentLayer>,
}

#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    fabro_macros::Combine,
    fabro_macros::OptionsMetadata,
)]
#[serde(deny_unknown_fields)]
pub struct CliExecModelLayer {
    /// LLM provider for `fabro exec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[option(value_type = "string")]
    pub provider: Option<InterpString>,
    /// Model name for `fabro exec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[option(value_type = "string")]
    pub name:     Option<InterpString>,
}

#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    fabro_macros::Combine,
    fabro_macros::OptionsMetadata,
)]
#[serde(deny_unknown_fields)]
pub struct CliExecAgentLayer {
    /// Tool permission level for `fabro exec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[option(
        default = "\"read-write\"",
        value_type = "\"read-only\" | \"read-write\" | \"full\""
    )]
    pub permissions: Option<AgentPermissions>,
    /// Agent-scoped MCP entries for `fabro exec`.
    #[serde(default, skip_serializing_if = "StickyMap::is_empty")]
    #[option(value_type = "table")]
    pub mcps:        StickyMap<McpEntryLayer>,
}

/// `[cli.output]` — generic CLI output defaults.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    fabro_macros::Combine,
    fabro_macros::OptionsMetadata,
)]
#[serde(deny_unknown_fields)]
pub struct CliOutputLayer {
    /// Output format for commands that support machine-readable output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[option(default = "\"text\"", value_type = "\"text\" | \"json\"")]
    pub format:    Option<OutputFormat>,
    /// Default output verbosity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[option(
        default = "\"normal\"",
        value_type = "\"quiet\" | \"normal\" | \"verbose\""
    )]
    pub verbosity: Option<OutputVerbosity>,
}

/// `[cli.updates]` — upgrade check toggle.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    fabro_macros::Combine,
    fabro_macros::OptionsMetadata,
)]
#[serde(deny_unknown_fields)]
pub struct CliUpdatesLayer {
    /// Check for new Fabro releases during supported CLI commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[option(default = "true", value_type = "boolean")]
    pub check: Option<bool>,
}

/// `[cli.logging]` — process-owned logging configuration for the CLI.
#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::OptionsMetadata,
)]
#[serde(deny_unknown_fields)]
pub struct CliLoggingLayer {
    /// Default CLI log level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[option(
        default = "\"info\"",
        value_type = "\"error\" | \"warn\" | \"info\" | \"debug\" | \"trace\""
    )]
    pub level: Option<String>,
}
