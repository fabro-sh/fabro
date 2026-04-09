//! v2-backed configuration layer.
//!
//! `ConfigLayer` is a newtype over [`SettingsFile`] — the v2 namespaced
//! parse tree in `fabro_types::settings::v2`. Loading functions (`parse`,
//! `load`, `for_workflow`, `project`, `settings`) all hard-fail on legacy
//! top-level keys with targeted rename hints. `ConfigLayer::combine` walks
//! the v2 merge matrix from [`crate::merge`].
//!
//! Consumers that need the inner tree call [`ConfigLayer::as_v2`] (borrow)
//! or `.into()` to move out an owned `SettingsFile`. The legacy flat
//! `Settings` shape is no longer reachable from this layer.

use std::path::Path;

use anyhow::Context;
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::run::RunGoalLayer;
use fabro_types::settings::{SettingsFile, parse_settings_file as parse_v2_settings_file};
use serde::{Deserialize, Serialize};

use crate::merge::combine_files;
use crate::project::{self};
use crate::user;

/// Rewrite any relative `run.goal = { file = "..." }` path in `file` to an
/// absolute path anchored at `base_dir`.
///
/// Called from `ConfigLayer::load` so that layers coming from different
/// config files can be merged without losing the "relative to my source
/// file" context. Paths that contain `${env.NAME}` interpolation are left
/// alone (they get resolved against the run's working directory at consume
/// time via [`SettingsFile::resolve_run_goal`]).
fn resolve_goal_file_paths(file: &mut SettingsFile, base_dir: &Path) {
    let Some(run) = file.run.as_mut() else {
        return;
    };
    let Some(RunGoalLayer::File { file: goal_file }) = run.goal.as_mut() else {
        return;
    };
    if !goal_file.is_literal() {
        // Env-tokenized paths stay unresolved until consume time.
        return;
    }
    let literal = goal_file.as_source();
    let path = Path::new(&literal);
    if path.is_absolute() {
        return;
    }
    let absolute = base_dir.join(path);
    *goal_file = InterpString::parse(&absolute.to_string_lossy());
}

/// A parsed settings file layer.
///
/// Thin newtype around the v2 [`SettingsFile`] parse tree. The newtype
/// exists so fabro-config can attach helper methods and evolve the
/// internal representation without forcing every caller to import v2
/// types.
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
    ///
    /// Relative `run.goal = { file = "..." }` paths are resolved against
    /// the directory of `path` at load time. Subsequent merging with other
    /// layers can then safely treat the path as self-contained.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let mut layer = Self::parse(&content)?;
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        resolve_goal_file_paths(&mut layer.file, base_dir);
        Ok(layer)
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
    use fabro_types::settings::run::RunGoalLayer;

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
    fn parse_accepts_inline_goal() {
        let layer = ConfigLayer::parse(
            r#"
_version = 1
[run]
goal = "Do things"
"#,
        )
        .unwrap();
        assert_eq!(
            layer.file.run_goal_inline_str().as_deref(),
            Some("Do things")
        );
    }

    #[test]
    fn parse_accepts_file_variant() {
        let layer = ConfigLayer::parse(
            r#"
_version = 1
[run.goal]
file = "prompts/goal.md"
"#,
        )
        .unwrap();
        let Some(RunGoalLayer::File { file }) = layer.file.run_goal_layer() else {
            panic!("expected run.goal.file variant");
        };
        assert_eq!(file.as_source(), "prompts/goal.md");
    }

    #[test]
    fn parse_rejects_goal_with_unknown_sibling_fields() {
        // The untagged enum should reject any `{ file = ..., extra = ... }`
        // shape because neither the inline nor the file variant matches.
        let err = ConfigLayer::parse(
            r#"
_version = 1
[run.goal]
file = "prompts/goal.md"
extra = "boom"
"#,
        )
        .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.to_lowercase().contains("run.goal") || text.contains("extra"));
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
            merged.file.run_goal_inline_str().as_deref(),
            Some("higher goal")
        );
    }

    #[test]
    fn combine_replaces_file_goal_with_inline_from_higher_layer() {
        // A higher-precedence `run.goal = "inline"` must fully override a
        // lower layer's `run.goal = { file = "..." }` — the scalar merge
        // treats `goal` as one field regardless of which variant each
        // layer picked.
        let higher = ConfigLayer::parse(
            r#"
_version = 1
[run]
goal = "inline override"
"#,
        )
        .unwrap();
        let lower = ConfigLayer::parse(
            r#"
_version = 1
[run.goal]
file = "/tmp/goal.md"
"#,
        )
        .unwrap();
        let merged = higher.combine(lower);
        assert_eq!(
            merged.file.run_goal_inline_str().as_deref(),
            Some("inline override")
        );
    }

    #[test]
    fn combine_replaces_inline_goal_with_file_from_higher_layer() {
        let higher = ConfigLayer::parse(
            r#"
_version = 1
[run.goal]
file = "/tmp/goal.md"
"#,
        )
        .unwrap();
        let lower = ConfigLayer::parse(
            r#"
_version = 1
[run]
goal = "inline loser"
"#,
        )
        .unwrap();
        let merged = higher.combine(lower);
        assert!(matches!(
            merged.file.run_goal_layer(),
            Some(RunGoalLayer::File { .. })
        ));
    }

    #[test]
    fn load_rewrites_relative_goal_file_to_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("fabro.toml");
        std::fs::write(
            &config_path,
            r#"
_version = 1
[run.goal]
file = "prompts/goal.md"
"#,
        )
        .unwrap();

        let layer = ConfigLayer::load(&config_path).unwrap();
        let Some(RunGoalLayer::File { file }) = layer.file.run_goal_layer() else {
            panic!("expected file variant");
        };
        let resolved = file.as_source();
        let expected = tmp.path().join("prompts").join("goal.md");
        assert_eq!(resolved, expected.to_string_lossy());
    }

    #[test]
    fn load_leaves_absolute_goal_file_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("fabro.toml");
        let abs_goal = "/etc/fabro/goal.md";
        std::fs::write(
            &config_path,
            format!(
                r#"
_version = 1
[run.goal]
file = "{abs_goal}"
"#
            ),
        )
        .unwrap();

        let layer = ConfigLayer::load(&config_path).unwrap();
        let Some(RunGoalLayer::File { file }) = layer.file.run_goal_layer() else {
            panic!("expected file variant");
        };
        assert_eq!(file.as_source(), abs_goal);
    }

    #[test]
    fn load_leaves_env_interpolated_goal_file_untouched() {
        // InterpString paths aren't resolved at load time because env
        // lookups happen at consume time. The loader should leave them
        // alone.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("fabro.toml");
        std::fs::write(
            &config_path,
            r#"
_version = 1
[run.goal]
file = "${env.GOALS_DIR}/goal.md"
"#,
        )
        .unwrap();

        let layer = ConfigLayer::load(&config_path).unwrap();
        let Some(RunGoalLayer::File { file }) = layer.file.run_goal_layer() else {
            panic!("expected file variant");
        };
        assert_eq!(file.as_source(), "${env.GOALS_DIR}/goal.md");
    }
}
