//! v2-backed configuration layer.
//!
//! `ConfigLayer` is now a newtype over [`SettingsFile`] — the v2 namespaced
//! parse tree in `fabro_types::settings::v2`. Loading functions return the
//! same `ConfigLayer` type they always have; internally they call
//! `parse_settings_file`, which hard-fails on any legacy top-level key with a
//! targeted rename hint.
//!
//! `ConfigLayer::combine` walks the v2 merge matrix from `crate::merge`.
//! `ConfigLayer::resolve` uses the temporary bridge in
//! `fabro_types::settings::v2::bridge` to produce the legacy flat [`Settings`]
//! shape until Stage 4 migrates consumers off it.

use std::path::Path;

use anyhow::Context;
use fabro_types::Settings;
use fabro_types::settings::v2::{
    SettingsFile, bridge_to_old, parse_settings_file as parse_v2_settings_file,
};
use serde::{Deserialize, Serialize};

use crate::merge::combine_files;
use crate::project::{self};
use crate::user;

/// A parsed settings file layer.
///
/// Currently a thin newtype around the v2 [`SettingsFile`] parse tree. The
/// newtype exists so fabro-config can attach helper methods and evolve the
/// internal representation without forcing every caller to import v2 types.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConfigLayer {
    pub file: SettingsFile,
}

impl From<SettingsFile> for ConfigLayer {
    fn from(file: SettingsFile) -> Self {
        Self { file }
    }
}

impl From<ConfigLayer> for SettingsFile {
    fn from(layer: ConfigLayer) -> Self {
        layer.file
    }
}

impl TryFrom<ConfigLayer> for Settings {
    type Error = anyhow::Error;

    fn try_from(value: ConfigLayer) -> Result<Self, Self::Error> {
        Ok(value.resolve())
    }
}

impl TryFrom<&ConfigLayer> for Settings {
    type Error = anyhow::Error;

    fn try_from(value: &ConfigLayer) -> Result<Self, Self::Error> {
        Ok(value.clone().resolve())
    }
}

impl ConfigLayer {
    /// Combine two layers using the v2 merge matrix.
    #[must_use]
    pub fn combine(self, other: Self) -> Self {
        // In the legacy contract `self.combine(other)` means `self` is the
        // higher-precedence layer and `other` is the lower-precedence one.
        // The merge matrix walker takes (lower, higher).
        Self {
            file: combine_files(other.file, self.file),
        }
    }

    /// Parse a v2 TOML settings file into a layer.
    pub fn parse(content: &str) -> anyhow::Result<Self> {
        let file = parse_v2_settings_file(content)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("Failed to parse settings file")?;
        Ok(Self { file })
    }

    /// Load a v2 TOML settings file from disk.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        Self::parse(&content)
    }

    /// Load workflow config + project config for a workflow path.
    ///
    /// Resolves the workflow path, loads its config, discovers project config
    /// (`fabro.toml`) from the resolved workflow's parent directory, and
    /// combines them (workflow takes precedence over project).
    pub fn for_workflow(path: &Path, cwd: &Path) -> anyhow::Result<Self> {
        let resolution = project::resolve_workflow_path(path, cwd)?;
        if resolution.workflow_config.is_none() && !resolution.resolved_workflow_path.is_file() {
            anyhow::bail!(
                "Workflow not found: {}",
                resolution.resolved_workflow_path.display()
            );
        }

        let workflow_config = resolution.workflow_config.unwrap_or_default();
        let project_config = project::discover_project_config(
            resolution
                .resolved_workflow_path
                .parent()
                .unwrap_or_else(|| Path::new(".")),
        )?
        .map(|(_, config)| config)
        .unwrap_or_default();

        Ok(workflow_config.combine(project_config))
    }

    /// Discover project config (`fabro.toml`) by walking ancestors from `start`.
    pub fn project(start: &Path) -> anyhow::Result<Self> {
        Ok(project::discover_project_config(start)?
            .map(|(_, config)| config)
            .unwrap_or_default())
    }

    /// Load machine-level defaults from `~/.fabro/settings.toml`.
    pub fn settings() -> anyhow::Result<Self> {
        user::load_settings_config(None)
    }

    /// Convert this layer into the legacy flat [`Settings`] shape via the
    /// temporary bridge. This path is removed in Stage 6.
    #[must_use]
    pub fn resolve(self) -> Settings {
        bridge_to_old(&self.file)
    }

    /// Borrow the inner v2 settings file for direct access.
    #[must_use]
    pub fn as_v2(&self) -> &SettingsFile {
        &self.file
    }

    /// Mutably borrow the inner v2 settings file.
    pub fn as_v2_mut(&mut self) -> &mut SettingsFile {
        &mut self.file
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_legacy_flat_keys() {
        let err = ConfigLayer::parse("[llm]\nprovider = \"openai\"").unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("run.model") || text.contains("llm"),
            "expected rename hint in error: {text}"
        );
    }

    #[test]
    fn parse_accepts_minimal_v2_file() {
        let layer = ConfigLayer::parse(
            r#"
_version = 1
[run]
goal = "Do things"
"#,
        )
        .unwrap();
        assert_eq!(
            layer
                .file
                .run
                .as_ref()
                .and_then(|r| r.goal.as_ref())
                .map(fabro_types::settings::v2::InterpString::as_source)
                .as_deref(),
            Some("Do things")
        );
    }

    #[test]
    fn combine_prefers_higher_precedence_self() {
        let higher = ConfigLayer::parse(
            r#"
_version = 1
[run]
goal = "higher goal"
"#,
        )
        .unwrap();
        let lower = ConfigLayer::parse(
            r#"
_version = 1
[run]
goal = "lower goal"
"#,
        )
        .unwrap();
        let merged = higher.combine(lower);
        assert_eq!(
            merged
                .file
                .run
                .as_ref()
                .and_then(|r| r.goal.as_ref())
                .map(fabro_types::settings::v2::InterpString::as_source)
                .as_deref(),
            Some("higher goal")
        );
    }
}
