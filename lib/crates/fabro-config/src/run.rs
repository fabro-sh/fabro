//! Re-export shim for run-side settings types.
//!
//! Stage 3 replaced the parse-time types previously defined here with the
//! v2 parse tree in `fabro_types::settings::v2`. This module stays alive as
//! a pass-through for crates that still import resolved run types via the
//! legacy `fabro_config::run` path; Stage 6 deletes it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::config::ConfigLayer;

pub use fabro_types::settings::run::{
    ArtifactsSettings, CheckpointSettings, GitHubSettings, LlmSettings, MergeStrategy,
    PullRequestSettings, SetupSettings,
};

/// Expand `${env.NAME}` whole-value references inside a string map.
///
/// Leaves entries that don't match the whole-value form untouched. Missing
/// host variables produce an error. This is the minimal resolver legacy
/// consumers still call while they are being migrated off `Settings`; the
/// full v2 interpolation pass lives in `fabro_types::settings::v2::interp`.
pub fn resolve_env_refs(env: &mut HashMap<String, String>) -> anyhow::Result<()> {
    for (key, value) in env.iter_mut() {
        if let Some(var_name) = value
            .strip_prefix("${env.")
            .and_then(|s| s.strip_suffix('}'))
        {
            *value = std::env::var(var_name).with_context(|| {
                format!("sandbox.env.{key}: host environment variable {var_name:?} is not set")
            })?;
        }
    }
    Ok(())
}

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
