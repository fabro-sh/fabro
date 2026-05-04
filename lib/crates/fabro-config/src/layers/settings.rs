//! The top-level sparse settings layer.
//!
//! This struct models a single settings file (`~/.fabro/settings.toml`,
//! `.fabro/project.toml`, or `workflow.toml`) after deserialization. Fields
//! unset in the source stay `None`/empty and are layered later by
//! `fabro-config`.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::cli::CliLayer;
use super::features::FeaturesLayer;
use super::llm::LlmLayer;
use super::project::ProjectLayer;
use super::run::RunLayer;
use super::server::ServerLayer;
use super::workflow::WorkflowLayer;
use crate::parse::{ParseError, parse_settings};

/// A sparse settings layer before merge/resolve.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
pub(crate) struct SettingsLayer {
    #[serde(default, rename = "_version", skip_serializing_if = "Option::is_none")]
    pub version:  Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project:  Option<ProjectLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run:      Option<RunLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli:      Option<CliLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server:   Option<ServerLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm:      Option<LlmLayer>,
}

impl FromStr for SettingsLayer {
    type Err = ParseError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        parse_settings(source)
    }
}

impl From<CliLayer> for SettingsLayer {
    fn from(cli: CliLayer) -> Self {
        Self {
            cli: Some(cli),
            ..Self::default()
        }
    }
}

impl From<FeaturesLayer> for SettingsLayer {
    fn from(features: FeaturesLayer) -> Self {
        Self {
            features: Some(features),
            ..Self::default()
        }
    }
}

impl From<ProjectLayer> for SettingsLayer {
    fn from(project: ProjectLayer) -> Self {
        Self {
            project: Some(project),
            ..Self::default()
        }
    }
}

impl From<RunLayer> for SettingsLayer {
    fn from(run: RunLayer) -> Self {
        Self {
            run: Some(run),
            ..Self::default()
        }
    }
}

impl From<ServerLayer> for SettingsLayer {
    fn from(server: ServerLayer) -> Self {
        Self {
            server: Some(server),
            ..Self::default()
        }
    }
}

impl From<WorkflowLayer> for SettingsLayer {
    fn from(workflow: WorkflowLayer) -> Self {
        Self {
            workflow: Some(workflow),
            ..Self::default()
        }
    }
}

#[cfg(test)]
impl SettingsLayer {
    /// A default layer that resolves cleanly: populates `server.auth.methods`
    /// with `["dev-token"]`. Use anywhere a test needs a starter
    /// `SettingsLayer` that the strict resolver will accept.
    #[must_use]
    pub(crate) fn test_default() -> Self {
        let mut layer = Self::default();
        layer.ensure_test_auth_methods();
        layer
    }

    /// If `server.auth.methods` is unset, populate it with `["dev-token"]`.
    /// Existing methods (set by a fixture) are preserved. Use to make a
    /// parsed-from-TOML layer resolve cleanly without overriding test intent.
    pub(crate) fn ensure_test_auth_methods(&mut self) {
        use fabro_types::settings::ServerAuthMethod;

        use super::server::{ServerAuthLayer, ServerLayer as ServerLayerTy};

        if self
            .server
            .as_ref()
            .and_then(|server| server.auth.as_ref())
            .and_then(|auth| auth.methods.as_ref())
            .is_some()
        {
            return;
        }
        let server = self.server.get_or_insert_with(ServerLayerTy::default);
        let auth = server.auth.get_or_insert_with(ServerAuthLayer::default);
        auth.methods = Some(vec![ServerAuthMethod::DevToken]);
    }
}