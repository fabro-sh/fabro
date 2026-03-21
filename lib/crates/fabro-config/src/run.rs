use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use tracing::debug;

use crate::hook::{HookConfig, HookDefinition};
use crate::mcp::McpServerEntry;
use crate::sandbox::{DockerfileSource, SandboxConfig};

const SUPPORTED_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CheckpointConfig {
    #[serde(default)]
    pub exclude_globs: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn default_graph() -> String {
    "workflow.fabro".to_string()
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PullRequestConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub draft: bool,
    #[serde(default)]
    pub auto_merge: bool,
    #[serde(default)]
    pub merge_strategy: MergeStrategy,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeStrategy {
    #[default]
    Squash,
    Merge,
    Rebase,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct AssetsConfig {
    #[serde(default)]
    pub include: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct GitHubConfig {
    #[serde(default)]
    pub permissions: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunConfig {
    pub version: u32,
    pub goal: Option<String>,
    #[serde(default = "default_graph")]
    pub graph: String,
    #[serde(alias = "directory")]
    pub work_dir: Option<String>,
    pub llm: Option<LlmConfig>,
    pub setup: Option<SetupConfig>,
    pub sandbox: Option<SandboxConfig>,
    pub vars: Option<HashMap<String, String>>,
    #[serde(default)]
    pub hooks: Vec<HookDefinition>,
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
    pub pull_request: Option<PullRequestConfig>,
    pub assets: Option<AssetsConfig>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerEntry>,
    pub github: Option<GitHubConfig>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LlmConfig {
    pub model: Option<String>,
    pub provider: Option<String>,
    #[serde(default)]
    pub fallbacks: Option<HashMap<String, Vec<String>>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SetupConfig {
    pub commands: Vec<String>,
    pub timeout_ms: Option<u64>,
}

/// Defaults for workflow runs, loaded from the server config.
///
/// Fields mirror `WorkflowRunConfig` but are all optional.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct RunDefaults {
    #[serde(alias = "directory")]
    pub work_dir: Option<String>,
    pub llm: Option<LlmConfig>,
    pub setup: Option<SetupConfig>,
    pub sandbox: Option<SandboxConfig>,
    pub vars: Option<HashMap<String, String>>,
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
    pub pull_request: Option<PullRequestConfig>,
    pub assets: Option<AssetsConfig>,
    #[serde(default)]
    pub hooks: Vec<HookDefinition>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerEntry>,
    pub github: Option<GitHubConfig>,
}

impl WorkflowRunConfig {
    /// Apply server-level run defaults to this config.
    ///
    /// Each field uses the first non-`None` value (task config wins).
    /// Vars are merged: defaults first, then task config overwrites.
    pub fn apply_defaults(&mut self, defaults: &RunDefaults) {
        // Build a RunDefaults from the task's current fields (the "overlay"),
        // then merge it on top of the server defaults (the "base").
        // This gives "task wins" semantics via merge_overlay's "overlay wins".
        let task_overlay = RunDefaults {
            work_dir: self.work_dir.take(),
            llm: self.llm.take(),
            setup: self.setup.take(),
            sandbox: self.sandbox.take(),
            vars: self.vars.take(),
            checkpoint: std::mem::take(&mut self.checkpoint),
            pull_request: self.pull_request.take(),
            assets: self.assets.take(),
            hooks: std::mem::take(&mut self.hooks),
            mcp_servers: std::mem::take(&mut self.mcp_servers),
            github: self.github.take(),
        };
        let mut merged = defaults.clone();
        merged.merge_overlay(task_overlay);

        self.work_dir = merged.work_dir;
        self.llm = merged.llm;
        self.setup = merged.setup;
        self.sandbox = merged.sandbox;
        self.vars = merged.vars;
        self.checkpoint = merged.checkpoint;
        self.pull_request = merged.pull_request;
        self.assets = merged.assets;
        self.hooks = merged.hooks;
        self.mcp_servers = merged.mcp_servers;
        self.github = merged.github;
    }
}

impl RunDefaults {
    /// Merge an overlay on top of this base. The overlay takes precedence
    /// for simple fields; compound fields (vars, hooks, mcp_servers) are
    /// deep-merged with the overlay winning on collision.
    pub fn merge_overlay(&mut self, overlay: RunDefaults) {
        if overlay.work_dir.is_some() {
            self.work_dir = overlay.work_dir;
        }

        match (&mut self.llm, overlay.llm) {
            (Some(base), Some(over)) => {
                if over.model.is_some() {
                    base.model = over.model;
                }
                if over.provider.is_some() {
                    base.provider = over.provider;
                }
                if over.fallbacks.is_some() {
                    base.fallbacks = over.fallbacks;
                }
            }
            (None, Some(over)) => self.llm = Some(over),
            _ => {}
        }

        match (&mut self.setup, overlay.setup) {
            (Some(base), Some(over)) => {
                if over.timeout_ms.is_some() {
                    base.timeout_ms = over.timeout_ms;
                }
            }
            (None, Some(over)) => self.setup = Some(over),
            _ => {}
        }

        match (&mut self.sandbox, overlay.sandbox) {
            (Some(base), Some(over)) => {
                if over.provider.is_some() {
                    base.provider = over.provider;
                }
                if over.preserve.is_some() {
                    base.preserve = over.preserve;
                }
                if over.devcontainer.is_some() {
                    base.devcontainer = over.devcontainer;
                }
                if over.local.is_some() {
                    base.local = over.local;
                }
                match (&mut base.daytona, over.daytona) {
                    (Some(base_d), Some(over_d)) => {
                        if over_d.auto_stop_interval.is_some() {
                            base_d.auto_stop_interval = over_d.auto_stop_interval;
                        }
                        if over_d.snapshot.is_some() {
                            base_d.snapshot = over_d.snapshot;
                        }
                        if let Some(over_labels) = over_d.labels {
                            let mut merged = base_d.labels.take().unwrap_or_default();
                            merged.extend(over_labels);
                            base_d.labels = Some(merged);
                        }
                        if over_d.network.is_some() {
                            base_d.network = over_d.network;
                        }
                    }
                    (None, Some(over_d)) => base.daytona = Some(over_d),
                    _ => {}
                }
                #[cfg(feature = "exedev")]
                match (&mut base.exe, over.exe) {
                    (Some(base_e), Some(over_e)) => {
                        if over_e.image.is_some() {
                            base_e.image = over_e.image;
                        }
                    }
                    (None, Some(over_e)) => base.exe = Some(over_e),
                    _ => {}
                }
                if over.ssh.is_some() {
                    base.ssh = over.ssh;
                }
                if let Some(over_env) = over.env {
                    let mut merged = base.env.take().unwrap_or_default();
                    merged.extend(over_env);
                    base.env = Some(merged);
                }
            }
            (None, Some(over)) => self.sandbox = Some(over),
            _ => {}
        }

        if let Some(overlay_vars) = overlay.vars {
            let mut merged = self.vars.take().unwrap_or_default();
            merged.extend(overlay_vars);
            self.vars = Some(merged);
        }

        if !overlay.checkpoint.exclude_globs.is_empty() {
            self.checkpoint
                .exclude_globs
                .append(&mut overlay.checkpoint.exclude_globs.clone());
            self.checkpoint.exclude_globs.sort();
            self.checkpoint.exclude_globs.dedup();
        }

        if overlay.pull_request.is_some() {
            self.pull_request = overlay.pull_request;
        }

        if overlay.assets.is_some() {
            self.assets = overlay.assets;
        }

        if !overlay.hooks.is_empty() {
            let base = HookConfig {
                hooks: std::mem::take(&mut self.hooks),
            };
            let over = HookConfig {
                hooks: overlay.hooks,
            };
            self.hooks = base.merge(over).hooks;
        }

        if !overlay.mcp_servers.is_empty() {
            let mut merged = std::mem::take(&mut self.mcp_servers);
            merged.extend(overlay.mcp_servers);
            self.mcp_servers = merged;
        }

        if overlay.github.is_some() {
            self.github = overlay.github;
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
pub fn load_run_config(path: &Path) -> anyhow::Result<WorkflowRunConfig> {
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
pub fn resolve_sandbox_env(config: &mut WorkflowRunConfig) -> anyhow::Result<()> {
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
fn resolve_dockerfile(config: &mut WorkflowRunConfig, config_dir: &Path) -> anyhow::Result<()> {
    let source = config
        .sandbox
        .as_mut()
        .and_then(|s| s.daytona.as_mut())
        .and_then(|d| d.snapshot.as_mut())
        .and_then(|snap| snap.dockerfile.as_mut());

    if let Some(DockerfileSource::Path { path: ref rel }) = source {
        let path = config_dir.join(rel);
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read dockerfile at {}", path.display()))?;
        debug!(path = %path.display(), "Resolved dockerfile from path");
        *source.unwrap() = DockerfileSource::Inline(contents);
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

pub fn parse_run_config(contents: &str) -> anyhow::Result<WorkflowRunConfig> {
    let config: WorkflowRunConfig =
        toml::from_str(contents).context("Failed to parse run config TOML")?;

    if config.version != SUPPORTED_VERSION {
        bail!(
            "Unsupported run config version {}. Only version {SUPPORTED_VERSION} is supported.",
            config.version
        );
    }

    Ok(config)
}
