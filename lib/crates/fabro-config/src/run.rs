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
/// Returns the v2-backed `ConfigLayer`.
pub fn load_run_config(path: &Path) -> anyhow::Result<ConfigLayer> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    ConfigLayer::parse(&content)
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
