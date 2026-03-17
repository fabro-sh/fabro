use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use tracing::debug;

use fabro_mcp::config::{McpServerConfig, McpTransport};

use crate::daytona_sandbox::{DaytonaConfig, DockerfileSource};

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
    pub hooks: Vec<crate::hook::HookDefinition>,
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
    pub pull_request: Option<PullRequestConfig>,
    pub assets: Option<AssetsConfig>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerEntry>,
    pub github: Option<GitHubConfig>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpServerEntry {
    #[serde(flatten)]
    pub transport: McpTransport,
    #[serde(default = "fabro_mcp::config::default_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
    #[serde(default = "fabro_mcp::config::default_tool_timeout_secs")]
    pub tool_timeout_secs: u64,
}

impl McpServerEntry {
    pub fn into_config(self, name: String) -> McpServerConfig {
        McpServerConfig {
            name,
            transport: self.transport,
            startup_timeout_secs: self.startup_timeout_secs,
            tool_timeout_secs: self.tool_timeout_secs,
        }
    }
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

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeMode {
    Always,
    #[default]
    Clean,
    Dirty,
    Never,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LocalSandboxConfig {
    #[serde(default)]
    pub worktree_mode: WorktreeMode,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SandboxConfig {
    pub provider: Option<String>,
    pub preserve: Option<bool>,
    #[serde(default)]
    pub devcontainer: Option<bool>,
    pub local: Option<LocalSandboxConfig>,
    pub daytona: Option<DaytonaConfig>,
    #[cfg(feature = "exedev")]
    pub exe: Option<fabro_exe::ExeConfig>,
    pub ssh: Option<fabro_ssh::SshConfig>,
    pub env: Option<HashMap<String, String>>,
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
    pub hooks: Vec<crate::hook::HookDefinition>,
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
        if self.work_dir.is_none() {
            self.work_dir = defaults.work_dir.clone();
        }

        match (&mut self.llm, &defaults.llm) {
            (Some(task), Some(default)) => {
                if task.model.is_none() {
                    task.model = default.model.clone();
                }
                if task.provider.is_none() {
                    task.provider = default.provider.clone();
                }
                if task.fallbacks.is_none() {
                    task.fallbacks = default.fallbacks.clone();
                }
            }
            (None, Some(_)) => self.llm = defaults.llm.clone(),
            _ => {}
        }

        match (&mut self.setup, &defaults.setup) {
            (Some(task), Some(default)) => {
                if task.timeout_ms.is_none() {
                    task.timeout_ms = default.timeout_ms;
                }
            }
            (None, Some(_)) => self.setup = defaults.setup.clone(),
            _ => {}
        }

        match (&mut self.sandbox, &defaults.sandbox) {
            (Some(task), Some(default)) => {
                if task.provider.is_none() {
                    task.provider = default.provider.clone();
                }
                if task.preserve.is_none() {
                    task.preserve = default.preserve;
                }
                if task.devcontainer.is_none() {
                    task.devcontainer = default.devcontainer;
                }
                if task.local.is_none() {
                    task.local = default.local.clone();
                }
                match (&mut task.daytona, &default.daytona) {
                    (Some(task_d), Some(default_d)) => {
                        if task_d.auto_stop_interval.is_none() {
                            task_d.auto_stop_interval = default_d.auto_stop_interval;
                        }
                        if task_d.snapshot.is_none() {
                            task_d.snapshot = default_d.snapshot.clone();
                        }
                        if let Some(ref default_labels) = default_d.labels {
                            let mut merged = default_labels.clone();
                            if let Some(ref task_labels) = task_d.labels {
                                merged.extend(task_labels.clone());
                            }
                            task_d.labels = Some(merged);
                        }
                        if task_d.network.is_none() {
                            task_d.network = default_d.network.clone();
                        }
                    }
                    (None, Some(_)) => task.daytona = default.daytona.clone(),
                    _ => {}
                }
                #[cfg(feature = "exedev")]
                match (&mut task.exe, &default.exe) {
                    (Some(task_e), Some(default_e)) => {
                        if task_e.image.is_none() {
                            task_e.image = default_e.image.clone();
                        }
                    }
                    (None, Some(_)) => task.exe = default.exe.clone(),
                    _ => {}
                }
                if let Some(ref default_env) = default.env {
                    let mut merged = default_env.clone();
                    if let Some(ref task_env) = task.env {
                        merged.extend(task_env.clone());
                    }
                    task.env = Some(merged);
                }
            }
            (None, Some(_)) => self.sandbox = defaults.sandbox.clone(),
            _ => {}
        }

        if let Some(ref default_vars) = defaults.vars {
            let mut merged = default_vars.clone();
            if let Some(ref task_vars) = self.vars {
                merged.extend(task_vars.clone());
            }
            self.vars = Some(merged);
        }

        // Union checkpoint exclude globs from defaults and task config, dedup
        if !defaults.checkpoint.exclude_globs.is_empty() {
            let mut merged = defaults.checkpoint.exclude_globs.clone();
            merged.append(&mut self.checkpoint.exclude_globs);
            merged.sort();
            merged.dedup();
            self.checkpoint.exclude_globs = merged;
        }

        if self.pull_request.is_none() {
            self.pull_request = defaults.pull_request.clone();
        }

        if self.assets.is_none() {
            self.assets = defaults.assets.clone();
        }

        // Merge hooks: defaults as base, workflow overrides by name
        if !defaults.hooks.is_empty() {
            let base = crate::hook::HookConfig {
                hooks: defaults.hooks.clone(),
            };
            let overlay = crate::hook::HookConfig {
                hooks: std::mem::take(&mut self.hooks),
            };
            self.hooks = base.merge(overlay).hooks;
        }

        // Merge mcp_servers: defaults as base, workflow overrides by key
        if !defaults.mcp_servers.is_empty() {
            let mut merged = defaults.mcp_servers.clone();
            merged.extend(std::mem::take(&mut self.mcp_servers));
            self.mcp_servers = merged;
        }

        if self.github.is_none() {
            self.github = defaults.github.clone();
        }
    }
}

impl RunDefaults {
    /// Merge an overlay on top of this base. The overlay takes precedence
    /// for simple fields; compound fields (vars, hooks, mcp_servers) are
    /// deep-merged with the overlay winning on collision.
    ///
    /// Uses the same deep-merge semantics as `WorkflowRunConfig::apply_defaults`.
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
            let base = crate::hook::HookConfig {
                hooks: std::mem::take(&mut self.hooks),
            };
            let over = crate::hook::HookConfig {
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
pub fn load_run_config(path: &Path) -> anyhow::Result<WorkflowRunConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut config = parse_run_config(&contents)?;

    let config_dir = path.parent().unwrap_or(Path::new("."));
    resolve_dockerfile(&mut config, config_dir)?;
    resolve_sandbox_env(&mut config)?;

    Ok(config)
}

/// Resolve `${env.VARNAME}` references in `[sandbox.env]` values.
///
/// Only whole-value references are supported (no partial interpolation).
/// Missing host env vars produce a hard error.
fn resolve_sandbox_env(config: &mut WorkflowRunConfig) -> anyhow::Result<()> {
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

fn parse_run_config(contents: &str) -> anyhow::Result<WorkflowRunConfig> {
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

/// Expand `$name` placeholders in `source` using the given variable map.
///
/// Identifiers match `[a-zA-Z_][a-zA-Z0-9_]*`. A `$` not followed by an
/// identifier character is left as-is. Undefined variables produce an error.
pub fn expand_vars(source: &str, vars: &HashMap<String, String>) -> anyhow::Result<String> {
    let mut result = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'$' {
            let start = i + 1;
            if start < len && bytes[start] == b'$' {
                result.push('$');
                i = start + 1;
            } else if start < len && (bytes[start].is_ascii_alphabetic() || bytes[start] == b'_') {
                let mut end = start + 1;
                while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                    end += 1;
                }
                let name = &source[start..end];
                match vars.get(name) {
                    Some(value) => result.push_str(value),
                    None => bail!("Undefined variable: ${name}"),
                }
                i = end;
            } else {
                result.push('$');
                i = start;
            }
        } else {
            result.push(source[i..].chars().next().unwrap());
            i += source[i..].chars().next().unwrap().len_utf8();
        }
    }

    Ok(result)
}

/// Run setup commands sequentially in the given directory.
///
/// Each command gets the full `timeout_ms` budget. Commands are executed
/// via `sh -c` so shell features (pipes, redirects, etc.) work.
pub async fn run_setup(setup: &SetupConfig, directory: &Path) -> anyhow::Result<()> {
    let timeout = std::time::Duration::from_millis(setup.timeout_ms.unwrap_or(300_000));

    for cmd in &setup.commands {
        let fut = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(directory)
            .output();

        let output = tokio::time::timeout(timeout, fut)
            .await
            .with_context(|| {
                format!(
                    "Setup command timed out after {}ms: {cmd}",
                    timeout.as_millis()
                )
            })?
            .with_context(|| format!("Failed to execute setup command: {cmd}"))?;

        if !output.status.success() {
            let code = output
                .status
                .code()
                .map_or("unknown".to_string(), |c| c.to_string());
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Setup command failed (exit code {code}): {cmd}\n{stderr}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daytona_sandbox::{DaytonaSnapshotConfig, DockerfileSource};

    #[test]
    fn parse_toml_with_vars() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[vars]
repo_url = "https://github.com/org/repo"
language = "python"
"#;
        let config = parse_run_config(toml).unwrap();
        let vars = config.vars.unwrap();
        assert_eq!(vars["repo_url"], "https://github.com/org/repo");
        assert_eq!(vars["language"], "python");
    }

    #[test]
    fn expand_single_var() {
        let vars = HashMap::from([("name".to_string(), "world".to_string())]);
        assert_eq!(expand_vars("Hello $name", &vars).unwrap(), "Hello world");
    }

    #[test]
    fn expand_multiple_vars() {
        let vars = HashMap::from([
            ("greeting".to_string(), "Hello".to_string()),
            ("name".to_string(), "world".to_string()),
        ]);
        assert_eq!(
            expand_vars("$greeting $name!", &vars).unwrap(),
            "Hello world!"
        );
    }

    #[test]
    fn expand_undefined_var_errors() {
        let vars = HashMap::new();
        let err = expand_vars("Hello $missing", &vars).unwrap_err();
        assert!(
            err.to_string().contains("Undefined variable: $missing"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn expand_no_vars_passthrough() {
        let vars = HashMap::new();
        assert_eq!(
            expand_vars("no variables here", &vars).unwrap(),
            "no variables here"
        );
    }

    #[test]
    fn expand_dollar_not_followed_by_ident() {
        let vars = HashMap::new();
        assert_eq!(expand_vars("costs $5", &vars).unwrap(), "costs $5");
    }

    #[test]
    fn expand_escaped_dollar() {
        let vars = HashMap::from([("name".to_string(), "world".to_string())]);
        assert_eq!(
            expand_vars("literal $$name here", &vars).unwrap(),
            "literal $name here"
        );
    }

    #[test]
    fn expand_escaped_dollar_at_end() {
        let vars = HashMap::new();
        assert_eq!(expand_vars("trailing $$", &vars).unwrap(), "trailing $");
    }

    #[test]
    fn expand_escaped_dollar_before_non_ident() {
        let vars = HashMap::new();
        assert_eq!(expand_vars("price is $$5", &vars).unwrap(), "price is $5");
    }

    #[test]
    fn parse_toml_with_devcontainer_enabled() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox]
provider = "daytona"
devcontainer = true
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.devcontainer, Some(true));
    }

    #[test]
    fn parse_toml_without_devcontainer() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox]
provider = "daytona"
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.devcontainer, None);
    }

    #[test]
    fn parse_toml_with_sandbox() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox]
provider = "daytona"
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));
        assert!(sandbox.daytona.is_none());
    }

    #[test]
    fn parse_toml_with_daytona_config() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox]
provider = "daytona"

[sandbox.daytona]
auto_stop_interval = 60

[sandbox.daytona.labels]
project = "fabro"

[sandbox.daytona.snapshot]
name = "my-snapshot"
cpu = 4
memory = 8
disk = 10
dockerfile = "FROM rust:1.85-slim-bookworm\nRUN apt-get update"
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));

        let daytona = sandbox.daytona.unwrap();
        assert_eq!(daytona.auto_stop_interval, Some(60));
        let labels = daytona.labels.unwrap();
        assert_eq!(labels["project"], "fabro");

        let snapshot = daytona.snapshot.unwrap();
        assert_eq!(snapshot.name, "my-snapshot");
        assert_eq!(snapshot.cpu, Some(4));
        assert_eq!(snapshot.memory, Some(8));
        assert_eq!(snapshot.disk, Some(10));
        assert_eq!(
            snapshot.dockerfile,
            Some(DockerfileSource::Inline(
                "FROM rust:1.85-slim-bookworm\nRUN apt-get update".into()
            ))
        );
    }

    #[test]
    fn parse_toml_with_daytona_no_snapshot() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox]
provider = "daytona"

[sandbox.daytona]
auto_stop_interval = 30
"#;
        let config = parse_run_config(toml).unwrap();
        let daytona = config.sandbox.unwrap().daytona.unwrap();
        assert_eq!(daytona.auto_stop_interval, Some(30));
        assert!(daytona.snapshot.is_none());
    }

    #[test]
    fn parse_minimal_toml() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"
"#;
        let config = parse_run_config(toml).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.goal.as_deref(), Some("Run tests"));
        assert_eq!(config.graph, "workflow.fabro");
        assert!(config.work_dir.is_none());
        assert!(config.llm.is_none());
        assert!(config.setup.is_none());
    }

    #[test]
    fn parse_toml_without_goal() {
        let toml = r#"
version = 1
graph = "workflow.fabro"
"#;
        let config = parse_run_config(toml).unwrap();
        assert!(config.goal.is_none());
    }

    #[test]
    fn parse_full_toml() {
        let toml = r#"
version = 1
goal = "Full workflow"
graph = "workflow.fabro"
directory = "/tmp/repo"

[llm]
model = "claude-haiku"
provider = "anthropic"

[setup]
commands = ["pip install -r requirements.txt", "npm install"]
timeout_ms = 60000
"#;
        let config = parse_run_config(toml).unwrap();
        assert_eq!(config.goal.as_deref(), Some("Full workflow"));
        assert_eq!(config.work_dir.as_deref(), Some("/tmp/repo"));

        let llm = config.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("claude-haiku"));
        assert_eq!(llm.provider.as_deref(), Some("anthropic"));

        let setup = config.setup.unwrap();
        assert_eq!(setup.commands.len(), 2);
        assert_eq!(setup.timeout_ms, Some(60000));
    }

    #[test]
    fn parse_toml_with_work_dir() {
        let toml = r#"
version = 1
graph = "workflow.fabro"
work_dir = "/workspace"
"#;
        let config = parse_run_config(toml).unwrap();
        assert_eq!(config.work_dir.as_deref(), Some("/workspace"));
    }

    #[test]
    fn parse_run_defaults_directory_alias() {
        let toml = r#"directory = "/old""#;
        let defaults: RunDefaults = toml::from_str(toml).unwrap();
        assert_eq!(defaults.work_dir.as_deref(), Some("/old"));
    }

    #[test]
    fn unsupported_version_rejected() {
        let toml = r#"
version = 2
goal = "x"
graph = "p.fabro"
"#;
        let err = parse_run_config(toml).unwrap_err();
        assert!(
            err.to_string().contains("Unsupported run config version 2"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn graph_path_resolved_relative_to_toml() {
        let toml_path = Path::new("/tmp/sub/task.toml");
        let resolved = resolve_graph_path(toml_path, "p.fabro");
        assert_eq!(resolved, PathBuf::from("/tmp/sub/p.fabro"));
    }

    #[test]
    fn graph_path_absolute_unchanged() {
        let toml_path = Path::new("/tmp/sub/task.toml");
        let resolved = resolve_graph_path(toml_path, "/other/workflow.fabro");
        assert_eq!(resolved, PathBuf::from("/other/workflow.fabro"));
    }

    #[test]
    fn goal_is_optional() {
        let no_goal = r#"
version = 1
graph = "p.fabro"
"#;
        let config = parse_run_config(no_goal).unwrap();
        assert!(config.goal.is_none());
    }

    #[test]
    fn graph_defaults_when_omitted() {
        let no_graph = r#"
version = 1
goal = "x"
"#;
        let config = parse_run_config(no_graph).unwrap();
        assert_eq!(config.graph, "workflow.fabro");
    }

    #[test]
    fn parse_toml_with_dockerfile_path() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox.daytona.snapshot]
name = "my-snapshot"
dockerfile = { path = "./Dockerfile" }
"#;
        let config = parse_run_config(toml).unwrap();
        let snapshot = config.sandbox.unwrap().daytona.unwrap().snapshot.unwrap();
        assert_eq!(
            snapshot.dockerfile,
            Some(DockerfileSource::Path {
                path: "./Dockerfile".into()
            })
        );
    }

    #[test]
    fn load_run_config_resolves_dockerfile_path() {
        let dir = tempfile::tempdir().unwrap();
        let dockerfile_path = dir.path().join("Dockerfile");
        std::fs::write(
            &dockerfile_path,
            "FROM rust:1.85-slim-bookworm\nRUN apt-get update",
        )
        .unwrap();

        let toml_path = dir.path().join("run.toml");
        std::fs::write(
            &toml_path,
            r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox.daytona.snapshot]
name = "my-snapshot"
dockerfile = { path = "Dockerfile" }
"#,
        )
        .unwrap();

        let config = load_run_config(&toml_path).unwrap();
        let snapshot = config.sandbox.unwrap().daytona.unwrap().snapshot.unwrap();
        assert_eq!(
            snapshot.dockerfile,
            Some(DockerfileSource::Inline(
                "FROM rust:1.85-slim-bookworm\nRUN apt-get update".into()
            ))
        );
    }

    #[test]
    fn load_run_config_dockerfile_path_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("run.toml");
        std::fs::write(
            &toml_path,
            r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox.daytona.snapshot]
name = "my-snapshot"
dockerfile = { path = "nonexistent" }
"#,
        )
        .unwrap();

        let err = load_run_config(&toml_path).unwrap_err();
        assert!(
            err.to_string().contains("Failed to read dockerfile"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_run_defaults_with_llm() {
        let toml = r#"
[llm]
model = "claude-haiku"
provider = "anthropic"
"#;
        let defaults: RunDefaults = toml::from_str(toml).unwrap();
        let llm = defaults.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("claude-haiku"));
        assert_eq!(llm.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn parse_run_defaults_empty() {
        let defaults: RunDefaults = toml::from_str("").unwrap();
        assert!(defaults.work_dir.is_none());
        assert!(defaults.llm.is_none());
        assert!(defaults.setup.is_none());
        assert!(defaults.sandbox.is_none());
        assert!(defaults.vars.is_none());
    }

    #[test]
    fn parse_run_defaults_full() {
        let toml = r#"
directory = "/work"

[llm]
model = "gpt-4"
provider = "openai"

[setup]
commands = ["make build"]
timeout_ms = 5000

[sandbox]
provider = "daytona"

[vars]
key = "value"
"#;
        let defaults: RunDefaults = toml::from_str(toml).unwrap();
        assert_eq!(defaults.work_dir.as_deref(), Some("/work"));
        assert!(defaults.llm.is_some());
        assert!(defaults.setup.is_some());
        assert!(defaults.sandbox.is_some());
        assert_eq!(defaults.vars.as_ref().unwrap()["key"], "value");
    }

    #[test]
    fn apply_defaults_fills_missing_llm() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: Some("default-model".into()),
                provider: Some("anthropic".into()),
                fallbacks: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("default-model"));
    }

    #[test]
    fn apply_defaults_task_config_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[llm]
model = "task-model"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: Some("default-model".into()),
                provider: None,
                fallbacks: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("task-model"));
    }

    #[test]
    fn apply_defaults_merges_vars() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[vars]
task_key = "task_val"
shared = "from_task"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            vars: Some(HashMap::from([
                ("default_key".into(), "default_val".into()),
                ("shared".into(), "from_default".into()),
            ])),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let vars = cfg.vars.unwrap();
        assert_eq!(vars["default_key"], "default_val");
        assert_eq!(vars["task_key"], "task_val");
        // Task config wins on collision
        assert_eq!(vars["shared"], "from_task");
    }

    #[test]
    fn apply_defaults_vars_default_only() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            vars: Some(HashMap::from([("key".into(), "val".into())])),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let vars = cfg.vars.unwrap();
        assert_eq!(vars["key"], "val");
    }

    #[test]
    fn apply_defaults_merges_llm_fields() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[llm]
model = "haiku"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: None,
                provider: Some("anthropic".into()),
                fallbacks: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("haiku"));
        assert_eq!(llm.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn apply_defaults_merges_setup_timeout() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[setup]
commands = ["make test"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            setup: Some(SetupConfig {
                commands: vec!["make build".into()],
                timeout_ms: Some(60000),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let setup = cfg.setup.unwrap();
        assert_eq!(setup.commands, vec!["make test"]);
        assert_eq!(setup.timeout_ms, Some(60000));
    }

    #[test]
    fn parse_toml_with_sandbox_preserve() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox]
provider = "docker"
preserve = true
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.preserve, Some(true));
    }

    #[test]
    fn parse_toml_sandbox_preserve_defaults_to_none() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[sandbox]
provider = "docker"
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.preserve, None);
    }

    #[test]
    fn parse_toml_worktree_mode_always() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.local]
worktree_mode = "always"
"#;
        let config = parse_run_config(toml).unwrap();
        let local = config.sandbox.unwrap().local.unwrap();
        assert_eq!(local.worktree_mode, WorktreeMode::Always);
    }

    #[test]
    fn parse_toml_worktree_mode_defaults_to_clean() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox]
provider = "local"
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.local, None);
    }

    #[test]
    fn parse_toml_worktree_mode_empty_local_defaults_to_clean() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.local]
"#;
        let config = parse_run_config(toml).unwrap();
        let local = config.sandbox.unwrap().local.unwrap();
        assert_eq!(local.worktree_mode, WorktreeMode::Clean);
    }

    #[test]
    fn apply_defaults_merges_sandbox_preserve_task_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox]
preserve = true
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: Some(false),
                devcontainer: None,
                local: None,
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.sandbox.unwrap().preserve, Some(true));
    }

    #[test]
    fn apply_defaults_merges_sandbox_preserve_from_default() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox]
provider = "docker"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: Some(true),
                devcontainer: None,
                local: None,
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.sandbox.unwrap().preserve, Some(true));
    }

    #[test]
    fn apply_defaults_merges_sandbox_fields() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox]
provider = "daytona"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: Some(DaytonaConfig {
                    auto_stop_interval: Some(30),
                    ..DaytonaConfig::default()
                }),
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let sandbox = cfg.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));
        let daytona = sandbox.daytona.unwrap();
        assert_eq!(daytona.auto_stop_interval, Some(30));
    }

    #[test]
    fn apply_defaults_merges_daytona_fields() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox]
provider = "daytona"

[sandbox.daytona]
auto_stop_interval = 60
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: Some("daytona".into()),
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: Some(DaytonaConfig {
                    auto_stop_interval: Some(30),
                    labels: Some(HashMap::from([("env".into(), "prod".into())])),
                    ..DaytonaConfig::default()
                }),
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let daytona = cfg.sandbox.unwrap().daytona.unwrap();
        assert_eq!(daytona.auto_stop_interval, Some(60));
        assert_eq!(daytona.labels.as_ref().unwrap()["env"], "prod");
    }

    #[test]
    fn apply_defaults_merges_daytona_labels() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.daytona.labels]
project = "fabro"
env = "from_task"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: Some(DaytonaConfig {
                    labels: Some(HashMap::from([
                        ("env".into(), "from_default".into()),
                        ("team".into(), "platform".into()),
                    ])),
                    ..DaytonaConfig::default()
                }),
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let labels = cfg.sandbox.unwrap().daytona.unwrap().labels.unwrap();
        assert_eq!(labels["project"], "fabro");
        assert_eq!(labels["team"], "platform");
        assert_eq!(labels["env"], "from_task");
    }

    #[test]
    fn apply_defaults_daytona_snapshot_whole_struct() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.daytona.snapshot]
name = "task-snap"
cpu = 2
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: Some(DaytonaConfig {
                    snapshot: Some(DaytonaSnapshotConfig {
                        name: "default-snap".into(),
                        cpu: Some(8),
                        memory: Some(16),
                        disk: Some(100),
                        dockerfile: Some(DockerfileSource::Inline("FROM ubuntu".into())),
                    }),
                    ..DaytonaConfig::default()
                }),
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let snapshot = cfg.sandbox.unwrap().daytona.unwrap().snapshot.unwrap();
        assert_eq!(snapshot.name, "task-snap");
        assert_eq!(snapshot.cpu, Some(2));
        assert!(snapshot.memory.is_none());
    }

    #[test]
    fn apply_defaults_daytona_snapshot_from_default() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.daytona]
auto_stop_interval = 60
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: Some(DaytonaConfig {
                    snapshot: Some(DaytonaSnapshotConfig {
                        name: "default-snap".into(),
                        cpu: Some(4),
                        memory: Some(8),
                        disk: None,
                        dockerfile: None,
                    }),
                    ..DaytonaConfig::default()
                }),
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let snapshot = cfg.sandbox.unwrap().daytona.unwrap().snapshot.unwrap();
        assert_eq!(snapshot.name, "default-snap");
        assert_eq!(snapshot.cpu, Some(4));
        assert_eq!(snapshot.memory, Some(8));
    }

    #[test]
    fn parse_toml_with_fallbacks() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[llm]
model = "claude-opus-4-6"
provider = "anthropic"

[llm.fallbacks]
anthropic = ["gemini", "openai"]
gemini = ["anthropic", "openai"]
"#;
        let config = parse_run_config(toml).unwrap();
        let llm = config.llm.unwrap();
        let fallbacks = llm.fallbacks.unwrap();
        assert_eq!(fallbacks["anthropic"], vec!["gemini", "openai"]);
        assert_eq!(fallbacks["gemini"], vec!["anthropic", "openai"]);
    }

    #[test]
    fn parse_toml_without_fallbacks() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[llm]
model = "claude-opus-4-6"
provider = "anthropic"
"#;
        let config = parse_run_config(toml).unwrap();
        let llm = config.llm.unwrap();
        assert!(llm.fallbacks.is_none());
    }

    #[test]
    fn apply_defaults_fallbacks_task_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[llm]
model = "opus"

[llm.fallbacks]
anthropic = ["gemini"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: None,
                provider: Some("anthropic".into()),
                fallbacks: Some(HashMap::from([("anthropic".into(), vec!["openai".into()])])),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.fallbacks.unwrap()["anthropic"], vec!["gemini"]);
    }

    #[test]
    fn apply_defaults_fallbacks_inherited() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[llm]
model = "opus"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: None,
                provider: Some("anthropic".into()),
                fallbacks: Some(HashMap::from([("anthropic".into(), vec!["openai".into()])])),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.fallbacks.unwrap()["anthropic"], vec!["openai"]);
    }

    #[tokio::test]
    async fn run_setup_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let setup = SetupConfig {
            commands: vec!["echo hello".to_string()],
            timeout_ms: None,
        };
        run_setup(&setup, dir.path()).await.unwrap();
    }

    #[tokio::test]
    async fn run_setup_fails_on_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let setup = SetupConfig {
            commands: vec!["false".to_string()],
            timeout_ms: None,
        };
        let err = run_setup(&setup, dir.path()).await.unwrap_err();
        assert!(
            err.to_string().contains("exit code"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn run_setup_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let setup = SetupConfig {
            commands: vec!["sleep 10".to_string()],
            timeout_ms: Some(100),
        };
        let err = run_setup(&setup, dir.path()).await.unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_toml_with_hooks() {
        let toml = r#"
version = 1
goal = "Test hooks"
graph = "test.fabro"

[[hooks]]
event = "stage_start"
command = "./scripts/pre-check.sh"
blocking = true
sandbox = false

[[hooks]]
event = "run_complete"
command = "echo done"
"#;
        let cfg: WorkflowRunConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.hooks.len(), 2);
        assert_eq!(cfg.hooks[0].event, crate::hook::HookEvent::StageStart);
        assert_eq!(
            cfg.hooks[0].command.as_deref(),
            Some("./scripts/pre-check.sh")
        );
        assert_eq!(cfg.hooks[0].blocking, Some(true));
        assert_eq!(cfg.hooks[0].sandbox, Some(false));
        assert_eq!(cfg.hooks[1].event, crate::hook::HookEvent::RunComplete);
    }

    #[test]
    fn parse_toml_without_hooks_defaults_empty() {
        let toml = r#"
version = 1
goal = "No hooks"
graph = "test.fabro"
"#;
        let cfg: WorkflowRunConfig = toml::from_str(toml).unwrap();
        assert!(cfg.hooks.is_empty());
    }

    #[test]
    fn parse_toml_with_daytona_network_block() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.daytona]
network = "block"
"#;
        let config = parse_run_config(toml).unwrap();
        let daytona = config.sandbox.unwrap().daytona.unwrap();
        assert_eq!(
            daytona.network,
            Some(crate::daytona_sandbox::DaytonaNetwork::Block)
        );
    }

    #[test]
    fn apply_defaults_network_task_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.daytona]
network = "block"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: Some(DaytonaConfig {
                    network: Some(crate::daytona_sandbox::DaytonaNetwork::AllowAll),
                    ..DaytonaConfig::default()
                }),
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(
            cfg.sandbox.unwrap().daytona.unwrap().network,
            Some(crate::daytona_sandbox::DaytonaNetwork::Block)
        );
    }

    #[test]
    fn apply_defaults_network_inherited() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.daytona]
auto_stop_interval = 60
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: Some(DaytonaConfig {
                    network: Some(crate::daytona_sandbox::DaytonaNetwork::AllowList(vec![
                        "10.0.0.0/8".into(),
                    ])),
                    ..DaytonaConfig::default()
                }),
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(
            cfg.sandbox.unwrap().daytona.unwrap().network,
            Some(crate::daytona_sandbox::DaytonaNetwork::AllowList(vec![
                "10.0.0.0/8".into(),
            ]))
        );
    }

    #[test]
    fn parse_toml_with_checkpoint_exclude_globs() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[checkpoint]
exclude_globs = ["**/node_modules/**", "**/.cache/**"]
"#;
        let config = parse_run_config(toml).unwrap();
        assert_eq!(
            config.checkpoint.exclude_globs,
            vec!["**/node_modules/**", "**/.cache/**"]
        );
    }

    #[test]
    fn parse_toml_without_checkpoint_defaults_empty() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"
"#;
        let config = parse_run_config(toml).unwrap();
        assert!(config.checkpoint.exclude_globs.is_empty());
    }

    #[test]
    fn apply_defaults_unions_checkpoint_exclude_globs() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[checkpoint]
exclude_globs = ["**/dist/**", "**/.cache/**"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            checkpoint: CheckpointConfig {
                exclude_globs: vec!["**/.cache/**".into(), "**/node_modules/**".into()],
            },
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(
            cfg.checkpoint.exclude_globs,
            vec!["**/.cache/**", "**/dist/**", "**/node_modules/**"]
        );
    }

    #[test]
    fn apply_defaults_checkpoint_from_defaults_only() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            checkpoint: CheckpointConfig {
                exclude_globs: vec!["**/node_modules/**".into()],
            },
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.checkpoint.exclude_globs, vec!["**/node_modules/**"]);
    }

    #[test]
    fn apply_defaults_checkpoint_from_task_only() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[checkpoint]
exclude_globs = ["**/dist/**"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults::default();
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.checkpoint.exclude_globs, vec!["**/dist/**"]);
    }

    #[test]
    fn parse_run_defaults_with_checkpoint() {
        let toml = r#"
[checkpoint]
exclude_globs = ["**/node_modules/**"]
"#;
        let defaults: RunDefaults = toml::from_str(toml).unwrap();
        assert_eq!(
            defaults.checkpoint.exclude_globs,
            vec!["**/node_modules/**"]
        );
    }

    #[test]
    fn parse_toml_with_sandbox_env() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.env]
FOO = "bar"
BAZ = "${env.HOME}"
"#;
        let config = parse_run_config(toml).unwrap();
        let env = config.sandbox.unwrap().env.unwrap();
        assert_eq!(env["FOO"], "bar");
        // Not yet resolved (parse_run_config doesn't resolve env refs)
        assert_eq!(env["BAZ"], "${env.HOME}");
    }

    #[test]
    fn parse_toml_without_sandbox_env() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox]
provider = "daytona"
"#;
        let config = parse_run_config(toml).unwrap();
        assert!(config.sandbox.unwrap().env.is_none());
    }

    #[test]
    fn resolve_env_refs_literal_passthrough() {
        let mut env = HashMap::from([("FOO".into(), "bar".into())]);
        resolve_env_refs(&mut env).unwrap();
        assert_eq!(env["FOO"], "bar");
    }

    #[test]
    fn resolve_env_refs_host_var() {
        std::env::set_var("FABRO_TEST_RESOLVE_VAR", "secret123");
        let mut env = HashMap::from([("MY_KEY".into(), "${env.FABRO_TEST_RESOLVE_VAR}".into())]);
        resolve_env_refs(&mut env).unwrap();
        assert_eq!(env["MY_KEY"], "secret123");
        std::env::remove_var("FABRO_TEST_RESOLVE_VAR");
    }

    #[test]
    fn resolve_env_refs_missing_var_errors() {
        let mut env = HashMap::from([(
            "MY_KEY".into(),
            "${env.FABRO_TEST_NONEXISTENT_VAR_12345}".into(),
        )]);
        let err = resolve_env_refs(&mut env).unwrap_err();
        assert!(
            err.to_string().contains("FABRO_TEST_NONEXISTENT_VAR_12345"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_env_refs_partial_not_interpolated() {
        let mut env = HashMap::from([("MIXED".into(), "prefix_${env.HOME}_suffix".into())]);
        // Partial interpolation is not supported — value is left as-is
        resolve_env_refs(&mut env).unwrap();
        assert_eq!(env["MIXED"], "prefix_${env.HOME}_suffix");
    }

    #[test]
    fn apply_defaults_merges_sandbox_env() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.env]
TASK_KEY = "task_val"
SHARED = "from_task"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: Some(HashMap::from([
                    ("DEFAULT_KEY".into(), "default_val".into()),
                    ("SHARED".into(), "from_default".into()),
                ])),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let env = cfg.sandbox.unwrap().env.unwrap();
        assert_eq!(env["DEFAULT_KEY"], "default_val");
        assert_eq!(env["TASK_KEY"], "task_val");
        assert_eq!(env["SHARED"], "from_task");
    }

    #[test]
    fn apply_defaults_sandbox_env_from_default_only() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox]
provider = "daytona"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: Some(HashMap::from([("KEY".into(), "val".into())])),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let env = cfg.sandbox.unwrap().env.unwrap();
        assert_eq!(env["KEY"], "val");
    }

    #[test]
    fn load_run_config_resolves_env_refs() {
        std::env::set_var("FABRO_TEST_LOAD_ENV", "resolved_value");
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("run.toml");
        std::fs::write(
            &toml_path,
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.env]
LITERAL = "hello"
FROM_HOST = "${env.FABRO_TEST_LOAD_ENV}"
"#,
        )
        .unwrap();

        let config = load_run_config(&toml_path).unwrap();
        let env = config.sandbox.unwrap().env.unwrap();
        assert_eq!(env["LITERAL"], "hello");
        assert_eq!(env["FROM_HOST"], "resolved_value");
        std::env::remove_var("FABRO_TEST_LOAD_ENV");
    }

    #[test]
    fn load_run_config_missing_env_var_errors() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("run.toml");
        std::fs::write(
            &toml_path,
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[sandbox.env]
MISSING = "${env.FABRO_TEST_DEFINITELY_NOT_SET_67890}"
"#,
        )
        .unwrap();

        let err = load_run_config(&toml_path).unwrap_err();
        assert!(
            err.to_string()
                .contains("FABRO_TEST_DEFINITELY_NOT_SET_67890"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_toml_with_pull_request() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[pull_request]
enabled = true
"#;
        let config = parse_run_config(toml).unwrap();
        let pr = config.pull_request.unwrap();
        assert!(pr.enabled);
    }

    #[test]
    fn parse_toml_without_pull_request_defaults_none() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"
"#;
        let config = parse_run_config(toml).unwrap();
        assert!(config.pull_request.is_none());
    }

    #[test]
    fn apply_defaults_pull_request_task_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[pull_request]
enabled = true
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            pull_request: Some(PullRequestConfig {
                enabled: false,
                draft: false,
                auto_merge: false,
                merge_strategy: MergeStrategy::Squash,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert!(cfg.pull_request.unwrap().enabled);
    }

    #[test]
    fn apply_defaults_pull_request_inherited() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            pull_request: Some(PullRequestConfig {
                enabled: true,
                draft: false,
                auto_merge: false,
                merge_strategy: MergeStrategy::Squash,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert!(cfg.pull_request.unwrap().enabled);
    }

    #[test]
    fn parse_run_defaults_with_pull_request() {
        let toml = r#"
[pull_request]
enabled = true
"#;
        let defaults: RunDefaults = toml::from_str(toml).unwrap();
        assert!(defaults.pull_request.unwrap().enabled);
    }

    #[test]
    fn parse_toml_with_assets() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.fabro"

[assets]
include = ["test-results/**", "playwright-report/**", "*.trace.zip"]
"#;
        let config = parse_run_config(toml).unwrap();
        let assets = config.assets.unwrap();
        assert_eq!(
            assets.include,
            vec!["test-results/**", "playwright-report/**", "*.trace.zip"]
        );
    }

    #[test]
    fn parse_toml_without_assets() {
        let toml = r#"
version = 1
graph = "workflow.fabro"
"#;
        let config = parse_run_config(toml).unwrap();
        assert!(config.assets.is_none());
    }

    #[test]
    fn apply_defaults_inherits_assets() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            assets: Some(AssetsConfig {
                include: vec!["test-results/**".into()],
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let assets = cfg.assets.unwrap();
        assert_eq!(assets.include, vec!["test-results/**"]);
    }

    #[test]
    fn apply_defaults_task_assets_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[assets]
include = ["playwright-report/**"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            assets: Some(AssetsConfig {
                include: vec!["test-results/**".into()],
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let assets = cfg.assets.unwrap();
        assert_eq!(assets.include, vec!["playwright-report/**"]);
    }

    #[test]
    fn parse_toml_with_pull_request_draft() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[pull_request]
enabled = true
draft = true
"#;
        let config = parse_run_config(toml).unwrap();
        let pr = config.pull_request.unwrap();
        assert!(pr.enabled);
        assert!(pr.draft);
    }

    #[test]
    fn parse_toml_pull_request_draft_defaults_true() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[pull_request]
enabled = true
"#;
        let config = parse_run_config(toml).unwrap();
        let pr = config.pull_request.unwrap();
        assert!(pr.enabled);
        assert!(pr.draft);
    }

    #[test]
    fn apply_defaults_merges_mcp_servers() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[mcp_servers.workflow_server]
type = "stdio"
command = ["npx", "wf-server"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            mcp_servers: HashMap::from([(
                "default_server".into(),
                McpServerEntry {
                    transport: McpTransport::Stdio {
                        command: vec!["npx".into(), "default-server".into()],
                        env: HashMap::new(),
                    },
                    startup_timeout_secs: 10,
                    tool_timeout_secs: 60,
                },
            )]),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.mcp_servers.len(), 2);
        assert!(cfg.mcp_servers.contains_key("default_server"));
        assert!(cfg.mcp_servers.contains_key("workflow_server"));
    }

    #[test]
    fn apply_defaults_mcp_servers_workflow_wins_on_collision() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[mcp_servers.shared]
type = "stdio"
command = ["npx", "from-workflow"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            mcp_servers: HashMap::from([(
                "shared".into(),
                McpServerEntry {
                    transport: McpTransport::Stdio {
                        command: vec!["npx".into(), "from-default".into()],
                        env: HashMap::new(),
                    },
                    startup_timeout_secs: 10,
                    tool_timeout_secs: 60,
                },
            )]),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.mcp_servers.len(), 1);
        match &cfg.mcp_servers["shared"].transport {
            McpTransport::Stdio { command, .. } => {
                assert_eq!(command, &["npx", "from-workflow"]);
            }
            _ => panic!("expected Stdio transport"),
        }
    }

    #[test]
    fn apply_defaults_merges_hooks() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[[hooks]]
name = "wf-hook"
event = "run_complete"
command = "echo done"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            hooks: vec![crate::hook::HookDefinition {
                name: Some("default-hook".into()),
                event: crate::hook::HookEvent::RunStart,
                command: Some("echo start".into()),
                hook_type: None,
                matcher: None,
                blocking: None,
                timeout_ms: None,
                sandbox: None,
            }],
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.hooks.len(), 2);
        assert_eq!(cfg.hooks[0].name.as_deref(), Some("default-hook"));
        assert_eq!(cfg.hooks[1].name.as_deref(), Some("wf-hook"));
    }

    #[test]
    fn apply_defaults_hooks_workflow_wins_on_collision() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[[hooks]]
name = "shared"
event = "run_complete"
command = "echo from-workflow"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            hooks: vec![crate::hook::HookDefinition {
                name: Some("shared".into()),
                event: crate::hook::HookEvent::RunStart,
                command: Some("echo from-default".into()),
                hook_type: None,
                matcher: None,
                blocking: None,
                timeout_ms: None,
                sandbox: None,
            }],
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.hooks.len(), 1);
        assert_eq!(cfg.hooks[0].event, crate::hook::HookEvent::RunComplete);
    }

    #[test]
    fn graph_defaults_to_workflow_fabro() {
        let toml = "version = 1\n";
        let config = parse_run_config(toml).unwrap();
        assert_eq!(config.graph, "workflow.fabro");
    }

    #[test]
    fn parse_toml_with_github_permissions() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"

[github]
permissions = { contents = "write", pull_requests = "read" }
"#;
        let config = parse_run_config(toml).unwrap();
        let github = config.github.unwrap();
        assert_eq!(github.permissions["contents"], "write");
        assert_eq!(github.permissions["pull_requests"], "read");
    }

    #[test]
    fn parse_toml_without_github_defaults_none() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.fabro"
"#;
        let config = parse_run_config(toml).unwrap();
        assert!(config.github.is_none());
    }

    #[test]
    fn apply_defaults_github_inherited() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            github: Some(GitHubConfig {
                permissions: HashMap::from([("contents".into(), "read".into())]),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let github = cfg.github.unwrap();
        assert_eq!(github.permissions["contents"], "read");
    }

    #[test]
    fn apply_defaults_github_task_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.fabro"

[github]
permissions = { contents = "write" }
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            github: Some(GitHubConfig {
                permissions: HashMap::from([("contents".into(), "read".into())]),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let github = cfg.github.unwrap();
        assert_eq!(github.permissions["contents"], "write");
    }

    #[test]
    fn merge_overlay_github_replaces() {
        let mut base = RunDefaults {
            github: Some(GitHubConfig {
                permissions: HashMap::from([("contents".into(), "read".into())]),
            }),
            ..RunDefaults::default()
        };
        let overlay = RunDefaults {
            github: Some(GitHubConfig {
                permissions: HashMap::from([("issues".into(), "write".into())]),
            }),
            ..RunDefaults::default()
        };
        base.merge_overlay(overlay);
        let github = base.github.unwrap();
        assert!(!github.permissions.contains_key("contents"));
        assert_eq!(github.permissions["issues"], "write");
    }

    #[test]
    fn merge_overlay_github_none_keeps_base() {
        let mut base = RunDefaults {
            github: Some(GitHubConfig {
                permissions: HashMap::from([("contents".into(), "read".into())]),
            }),
            ..RunDefaults::default()
        };
        let overlay = RunDefaults::default();
        base.merge_overlay(overlay);
        let github = base.github.unwrap();
        assert_eq!(github.permissions["contents"], "read");
    }
}
