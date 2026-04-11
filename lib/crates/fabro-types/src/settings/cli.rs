//! CLI domain.
//!
//! `[cli]` is owner-first: the CLI process reads its settings from
//! `~/.fabro/settings.toml` plus process-local overrides. `cli.*` stanzas in
//! `.fabro/project.toml` and `workflow.toml` remain schema-valid but
//! runtime-inert.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::interp::InterpString;
use super::run::{AgentPermissions, McpEntryLayer, McpServerSettings};

/// A structurally resolved `[cli]` view for consumers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliSettings {
    pub target: Option<CliTargetSettings>,
    pub auth: CliAuthSettings,
    pub exec: CliExecSettings,
    pub output: CliOutputSettings,
    pub updates: CliUpdatesSettings,
    pub logging: CliLoggingSettings,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CliTargetSettings {
    Http {
        url: InterpString,
        tls: Option<CliTargetTlsSettings>,
    },
    Unix {
        path: InterpString,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CliTargetTlsSettings {
    pub cert: InterpString,
    pub key: InterpString,
    pub ca: InterpString,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliAuthSettings {
    pub strategy: Option<CliAuthStrategy>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliExecSettings {
    pub prevent_idle_sleep: bool,
    pub model: CliExecModelSettings,
    pub agent: CliExecAgentSettings,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliExecModelSettings {
    pub provider: Option<InterpString>,
    pub name: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliExecAgentSettings {
    pub permissions: Option<AgentPermissions>,
    pub mcps: HashMap<String, McpServerSettings>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliOutputSettings {
    pub format: OutputFormat,
    pub verbosity: OutputVerbosity,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliUpdatesSettings {
    pub check: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliLoggingSettings {
    pub level: Option<String>,
}

/// A sparse `[cli]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<CliTargetLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<CliAuthLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<CliExecLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<CliOutputLayer>,
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
        #[serde(default)]
        tls: Option<CliTargetTlsLayer>,
    },
    Unix {
        #[serde(default)]
        path: Option<InterpString>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliTargetTlsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<InterpString>,
}

/// `[cli.auth]` — explicit auth strategy selection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliAuthLayer {
    /// `none` explicitly disables inherited auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<CliAuthStrategy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliAuthStrategy {
    None,
    Jwt,
    Mtls,
}

/// `[cli.exec]` — `fabro exec` defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliExecLayer {
    /// Prevent idle sleep on macOS while an exec run is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prevent_idle_sleep: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<CliExecModelLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<CliExecAgentLayer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliExecModelLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliExecAgentLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<AgentPermissions>,
    /// Agent-scoped MCP entries for `fabro exec`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub mcps: HashMap<String, McpEntryLayer>,
}

/// `[cli.output]` — generic CLI output defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliOutputLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<OutputFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<OutputVerbosity>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputVerbosity {
    Quiet,
    #[default]
    Normal,
    Verbose,
}

/// `[cli.updates]` — upgrade check toggle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
