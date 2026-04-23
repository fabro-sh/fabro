//! CLI domain.
//!
//! `[cli]` is owner-first: the CLI process reads its settings from
//! `~/.fabro/settings.toml` plus process-local overrides. `cli.*` stanzas in
//! `.fabro/project.toml` and `workflow.toml` remain schema-valid but
//! runtime-inert.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::interp::InterpString;
use super::run::{AgentPermissions, McpServerSettings};

/// A structurally resolved `[cli]` view for consumers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CliNamespace {
    pub target:  Option<CliTargetSettings>,
    pub auth:    CliAuthSettings,
    pub exec:    CliExecSettings,
    pub output:  CliOutputSettings,
    pub updates: CliUpdatesSettings,
    pub logging: CliLoggingSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CliTargetSettings {
    Http { url: InterpString },
    Unix { path: InterpString },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CliAuthSettings {
    pub strategy: Option<CliAuthStrategy>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CliExecSettings {
    pub prevent_idle_sleep: bool,
    pub model:              CliExecModelSettings,
    pub agent:              CliExecAgentSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CliExecModelSettings {
    pub provider: Option<InterpString>,
    pub name:     Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CliExecAgentSettings {
    pub permissions: Option<AgentPermissions>,
    pub mcps:        HashMap<String, McpServerSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CliOutputSettings {
    pub format:    OutputFormat,
    pub verbosity: OutputVerbosity,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CliUpdatesSettings {
    pub check: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CliLoggingSettings {
    pub level: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliAuthStrategy {
    None,
    Jwt,
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
