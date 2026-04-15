//! Workflow / run config loading helpers.
//!
//! Thin wrappers around `parse_settings_layer` / `load_settings_path` plus
//! path resolution for the `[workflow] graph` override. Runtime types
//! that used to be re-exported from here live under
//! `fabro_types::settings::run` now.

use std::path::{Path, PathBuf};

use fabro_types::settings::SettingsLayer;
use fabro_types::settings::run::{ResolvedGoalSource, ResolvedRunGoal, RunGoalLayer};

use crate::load::{load_settings_path, resolve_goal_file_path};
use crate::parse::parse_settings_layer;
use crate::{Error, Result};

/// Load and parse a run config from a TOML file.
pub fn parse_run_config(contents: &str) -> Result<SettingsLayer> {
    parse_settings_layer(contents)
        .map_err(|err| Error::parse("Failed to parse run config TOML", err))
}

/// Load and parse a run config from a TOML file.
///
/// Goes through [`load_settings_path`] so that relative `run.goal.file`
/// paths are anchored at the directory of `path` at load time.
pub fn load_run_config(path: &Path) -> Result<SettingsLayer> {
    load_settings_path(path)
}

/// Resolve a graph path relative to a workflow.toml.
#[must_use]
pub fn resolve_graph_path(workflow_toml: &Path, graph_relative: &str) -> PathBuf {
    workflow_toml
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(graph_relative)
}

#[derive(Debug)]
pub enum ResolveRunGoalError {
    EnvLookup {
        var: String,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for ResolveRunGoalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvLookup { var } => write!(
                f,
                "run.goal.file references env var `{var}` which is not set"
            ),
            Self::Io { path, source } => {
                write!(f, "failed to read goal file {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ResolveRunGoalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EnvLookup { .. } => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

pub fn resolve_run_goal(
    settings: &SettingsLayer,
    base_dir: &Path,
) -> std::result::Result<Option<ResolvedRunGoal>, ResolveRunGoalError> {
    let Some(goal) = settings.run.as_ref().and_then(|run| run.goal.as_ref()) else {
        return Ok(None);
    };

    match goal {
        RunGoalLayer::Inline(text) => Ok(Some(ResolvedRunGoal {
            text: text.as_source(),
            source: ResolvedGoalSource::Inline,
        })),
        RunGoalLayer::File { file } => {
            let resolved = file
                .resolve(|name| std::env::var(name).ok())
                .map_err(|err| ResolveRunGoalError::EnvLookup { var: err.name })?;
            let path = resolve_goal_file_path(&resolved.value, base_dir);
            let text =
                std::fs::read_to_string(&path).map_err(|source| ResolveRunGoalError::Io {
                    path: path.clone(),
                    source,
                })?;
            Ok(Some(ResolvedRunGoal {
                text,
                source: ResolvedGoalSource::File { path },
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::run::RunGoalLayer;

    use super::*;

    #[test]
    fn load_run_config_rewrites_relative_goal_file_path() {
        let tmp = tempfile::tempdir().unwrap();
        let workflow_dir = tmp.path().join("fabro").join("workflows").join("demo");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        let workflow_toml = workflow_dir.join("workflow.toml");
        std::fs::write(
            &workflow_toml,
            r#"_version = 1

[run.goal]
file = "prompts/goal.md"
"#,
        )
        .unwrap();

        let config = load_run_config(&workflow_toml).unwrap();
        let Some(RunGoalLayer::File { file }) =
            config.run.as_ref().and_then(|run| run.goal.as_ref())
        else {
            panic!("expected file variant");
        };
        let expected = workflow_dir.join("prompts").join("goal.md");
        assert_eq!(file.as_source(), expected.to_string_lossy());
    }

    #[test]
    fn load_run_config_leaves_absolute_goal_file_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let workflow_toml = tmp.path().join("workflow.toml");
        std::fs::write(
            &workflow_toml,
            r#"_version = 1

[run.goal]
file = "/etc/fabro/goal.md"
"#,
        )
        .unwrap();

        let config = load_run_config(&workflow_toml).unwrap();
        let Some(RunGoalLayer::File { file }) =
            config.run.as_ref().and_then(|run| run.goal.as_ref())
        else {
            panic!("expected file variant");
        };
        assert_eq!(file.as_source(), "/etc/fabro/goal.md");
    }
}
