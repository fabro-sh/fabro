//! Workflow / run config loading helpers.
//!
//! Thin wrappers around `ConfigLayer::parse` / `ConfigLayer::load` plus
//! path resolution for the `[workflow] graph` override. Runtime types
//! that used to be re-exported from here live under
//! `fabro_types::settings::run` now.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::config::ConfigLayer;

/// Load and parse a run config from a TOML file.
pub fn parse_run_config(contents: &str) -> anyhow::Result<ConfigLayer> {
    ConfigLayer::parse(contents).context("Failed to parse run config TOML")
}

/// Load and parse a run config from a TOML file.
///
/// Goes through [`ConfigLayer::load`] so that relative `run.goal.file`
/// paths are anchored at the directory of `path` at load time.
pub fn load_run_config(path: &Path) -> anyhow::Result<ConfigLayer> {
    ConfigLayer::load(path)
        .with_context(|| format!("Failed to parse workflow config at {}", path.display()))
}

/// Resolve a graph path relative to a workflow.toml.
#[must_use]
pub fn resolve_graph_path(workflow_toml: &Path, graph_relative: &str) -> PathBuf {
    workflow_toml
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(graph_relative)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_types::settings::run::RunGoalLayer;

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
        let Some(RunGoalLayer::File { file }) = config.as_v2().run_goal_layer() else {
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
        let Some(RunGoalLayer::File { file }) = config.as_v2().run_goal_layer() else {
            panic!("expected file variant");
        };
        assert_eq!(file.as_source(), "/etc/fabro/goal.md");
    }
}
