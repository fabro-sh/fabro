use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::combine::Combine;
use crate::config::FabroConfig;
pub use fabro_types::settings::run::{
    AssetsSettings, CheckpointSettings, GitHubSettings, LlmSettings, MergeStrategy,
    PullRequestSettings, SetupSettings,
};

const SUPPORTED_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CheckpointConfig {
    #[serde(default)]
    pub exclude_globs: Vec<String>,
}

impl Combine for CheckpointConfig {
    fn combine(mut self, other: Self) -> Self {
        self.exclude_globs.extend(other.exclude_globs);
        self.exclude_globs.sort();
        self.exclude_globs.dedup();
        self
    }
}

impl From<CheckpointConfig> for CheckpointSettings {
    fn from(value: CheckpointConfig) -> Self {
        let mut exclude_globs = value.exclude_globs;
        exclude_globs.sort();
        exclude_globs.dedup();
        Self { exclude_globs }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct PullRequestConfig {
    pub enabled: Option<bool>,
    pub draft: Option<bool>,
    pub auto_merge: Option<bool>,
    pub merge_strategy: Option<MergeStrategy>,
}

impl From<PullRequestConfig> for PullRequestSettings {
    fn from(value: PullRequestConfig) -> Self {
        Self {
            enabled: value.enabled.unwrap_or(false),
            draft: value.draft.unwrap_or(true),
            auto_merge: value.auto_merge.unwrap_or(false),
            merge_strategy: value.merge_strategy.unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct AssetsConfig {
    #[serde(default)]
    pub include: Vec<String>,
}

impl From<AssetsConfig> for AssetsSettings {
    fn from(value: AssetsConfig) -> Self {
        Self {
            include: value.include,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct GitHubConfig {
    #[serde(default)]
    pub permissions: HashMap<String, String>,
}

impl From<GitHubConfig> for GitHubSettings {
    fn from(value: GitHubConfig) -> Self {
        Self {
            permissions: value.permissions,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct LlmConfig {
    pub model: Option<String>,
    pub provider: Option<String>,
    #[serde(default)]
    pub fallbacks: Option<HashMap<String, Vec<String>>>,
}

impl From<LlmConfig> for LlmSettings {
    fn from(value: LlmConfig) -> Self {
        Self {
            model: value.model,
            provider: value.provider,
            fallbacks: value.fallbacks,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct SetupConfig {
    #[serde(default)]
    pub commands: Vec<String>,
    pub timeout_ms: Option<u64>,
}

impl From<SetupConfig> for SetupSettings {
    fn from(value: SetupConfig) -> Self {
        Self {
            commands: value.commands,
            timeout_ms: value.timeout_ms,
        }
    }
}

/// Load and validate a run config from a TOML file.
///
/// The `graph` path in the returned config is resolved relative to the
/// TOML file's parent directory. Any `dockerfile = { path = "..." }` is
/// resolved to inline content.
///
/// `${env.VARNAME}` references in `[sandbox.env]` are NOT resolved here —
/// call [`resolve_sandbox_env`] separately after snapshotting, so that
/// plaintext secrets are never written to disk.
pub fn load_run_config(path: &Path) -> anyhow::Result<FabroConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut config = parse_run_config(&contents)?;

    let config_dir = path.parent().unwrap_or(Path::new("."));
    resolve_dockerfile(&mut config, config_dir)?;

    Ok(config)
}

/// Resolve `${env.VARNAME}` references in `[sandbox.env]` values.
///
/// Only whole-value references are supported (no partial interpolation).
/// Missing host env vars produce a hard error.
pub fn resolve_sandbox_env(config: &mut FabroConfig) -> anyhow::Result<()> {
    if let Some(env) = config.sandbox.as_mut().and_then(|s| s.env.as_mut()) {
        resolve_env_refs(env)?;
    }
    Ok(())
}

/// Resolve `${env.VARNAME}` patterns in a map of env vars.
///
/// If the entire value is `${env.VARNAME}`, it is replaced with the host
/// environment variable. Any other value is left as-is. Missing host
/// variables produce an error.
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

/// If the config contains a `dockerfile = { path = "..." }`, read the file
/// and replace it with `DockerfileSource::Inline(contents)`.
fn resolve_dockerfile(config: &mut FabroConfig, config_dir: &Path) -> anyhow::Result<()> {
    let source = config
        .sandbox
        .as_mut()
        .and_then(|s| s.daytona.as_mut())
        .and_then(|d| d.snapshot.as_mut())
        .and_then(|snap| snap.dockerfile.as_mut());

    if let Some(crate::sandbox::DockerfileSource::Path { path: ref rel }) = source {
        let path = config_dir.join(rel);
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read dockerfile at {}", path.display()))?;
        debug!(path = %path.display(), "Resolved dockerfile from path");
        *source.unwrap() = crate::sandbox::DockerfileSource::Inline(contents);
    }

    Ok(())
}

/// Resolve the graph path relative to the TOML file's parent directory.
pub fn resolve_graph_path(toml_path: &Path, graph: &str) -> PathBuf {
    let graph_path = Path::new(graph);
    if graph_path.is_absolute() {
        graph_path.to_path_buf()
    } else {
        toml_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(graph_path)
    }
}

pub fn parse_run_config(contents: &str) -> anyhow::Result<FabroConfig> {
    let mut config: FabroConfig =
        toml::from_str(contents).context("Failed to parse run config TOML")?;

    if config.graph.is_none() {
        config.graph = Some("workflow.fabro".to_string());
    }

    let version = config.version.unwrap_or(0);
    if version != SUPPORTED_VERSION {
        bail!(
            "Unsupported run config version {version}. Only version {SUPPORTED_VERSION} is supported.",
        );
    }

    Ok(config)
}
