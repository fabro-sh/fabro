use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cli::{ExecDefaults, ExecutionMode, ServerDefaults};
use crate::hook::{HookConfig, HookDefinition};
use crate::mcp::McpServerEntry;
use crate::project::ProjectFabroConfig;
use crate::run::{
    AssetsConfig, CheckpointConfig, GitHubConfig, LlmConfig, PullRequestConfig, SetupConfig,
};
use crate::sandbox::SandboxConfig;
use crate::server::{ApiConfig, Features, GitConfig, LogConfig, WebConfig};

fn is_default_checkpoint(c: &CheckpointConfig) -> bool {
    c.exclude_globs.is_empty()
}

/// Unified configuration type for all Fabro config sources.
///
/// Loading functions (`load_cli_config`, `load_server_config`, `load_run_config`,
/// `parse_project_config`) all return this type. Fields irrelevant to a
/// particular source are left at their defaults (None / empty).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct FabroConfig {
    // --- Workflow run config fields ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_file: Option<PathBuf>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,

    // --- Run defaults fields (inlined) ---
    #[serde(default, alias = "directory", skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<SetupConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vars: Option<HashMap<String, String>>,

    #[serde(default, skip_serializing_if = "is_default_checkpoint")]
    pub checkpoint: CheckpointConfig,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullRequestConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<AssetsConfig>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookDefinition>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub mcp_servers: HashMap<String, McpServerEntry>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubConfig>,

    // --- CLI config fields ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ExecutionMode>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerDefaults>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecDefaults>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prevent_idle_sleep: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbose: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade_check: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_approve: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_retro: Option<bool>,

    // --- Server config fields ---
    #[serde(default, alias = "data_dir", skip_serializing_if = "Option::is_none")]
    pub storage_dir: Option<PathBuf>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_runs: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Features>,

    // --- Shared fields ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<LogConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitConfig>,

    // --- Project config fields ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fabro: Option<ProjectFabroConfig>,
}

impl FabroConfig {
    // --- Convenience methods (ported from CliConfig) ---

    /// Resolve the storage directory: config value > default `~/.fabro`.
    pub fn storage_dir(&self) -> PathBuf {
        self.storage_dir.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .expect("could not determine home directory")
                .join(".fabro")
        })
    }

    pub fn app_id(&self) -> Option<&str> {
        self.git.as_ref().and_then(|g| g.app_id.as_deref())
    }

    pub fn slug(&self) -> Option<&str> {
        self.git.as_ref().and_then(|g| g.slug.as_deref())
    }

    pub fn client_id(&self) -> Option<&str> {
        self.git.as_ref().and_then(|g| g.client_id.as_deref())
    }

    pub fn git_author(&self) -> Option<&crate::server::GitAuthorConfig> {
        self.git.as_ref().map(|g| &g.author)
    }

    pub fn verbose_enabled(&self) -> bool {
        self.verbose.unwrap_or(false)
    }

    pub fn prevent_idle_sleep_enabled(&self) -> bool {
        self.prevent_idle_sleep.unwrap_or(false)
    }

    pub fn upgrade_check_enabled(&self) -> bool {
        self.upgrade_check.unwrap_or(true)
    }

    pub fn dry_run_enabled(&self) -> bool {
        self.dry_run.unwrap_or(false)
    }

    pub fn auto_approve_enabled(&self) -> bool {
        self.auto_approve.unwrap_or(false)
    }

    pub fn no_retro_enabled(&self) -> bool {
        self.no_retro.unwrap_or(false)
    }

    /// Merge an overlay on top of this base. The overlay takes precedence
    /// for simple fields; compound fields (vars, hooks, mcp_servers) are
    /// deep-merged with the overlay winning on collision.
    pub fn merge_overlay(&mut self, overlay: FabroConfig) {
        // --- Workflow run config fields ---
        if overlay.version.is_some() {
            self.version = overlay.version;
        }
        if overlay.goal.is_some() {
            self.goal = overlay.goal;
        }
        if overlay.goal_file.is_some() {
            self.goal_file = overlay.goal_file;
        }
        if overlay.graph.is_some() {
            self.graph = overlay.graph;
        }
        if !overlay.labels.is_empty() {
            let mut merged = std::mem::take(&mut self.labels);
            merged.extend(overlay.labels);
            self.labels = merged;
        }

        // --- Run defaults fields ---
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
                .extend(overlay.checkpoint.exclude_globs);
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

        // --- CLI config fields ---
        if overlay.mode.is_some() {
            self.mode = overlay.mode;
        }
        if overlay.server.is_some() {
            self.server = overlay.server;
        }
        if overlay.exec.is_some() {
            self.exec = overlay.exec;
        }
        if overlay.prevent_idle_sleep.is_some() {
            self.prevent_idle_sleep = overlay.prevent_idle_sleep;
        }
        if overlay.verbose.is_some() {
            self.verbose = overlay.verbose;
        }
        if overlay.upgrade_check.is_some() {
            self.upgrade_check = overlay.upgrade_check;
        }
        if overlay.dry_run.is_some() {
            self.dry_run = overlay.dry_run;
        }
        if overlay.auto_approve.is_some() {
            self.auto_approve = overlay.auto_approve;
        }
        if overlay.no_retro.is_some() {
            self.no_retro = overlay.no_retro;
        }

        // --- Server config fields ---
        if overlay.storage_dir.is_some() {
            self.storage_dir = overlay.storage_dir;
        }
        if overlay.max_concurrent_runs.is_some() {
            self.max_concurrent_runs = overlay.max_concurrent_runs;
        }
        if overlay.web.is_some() {
            self.web = overlay.web;
        }
        if overlay.api.is_some() {
            self.api = overlay.api;
        }
        if overlay.features.is_some() {
            self.features = overlay.features;
        }

        // --- Shared fields ---
        if overlay.log.is_some() {
            self.log = overlay.log;
        }
        if overlay.git.is_some() {
            self.git = overlay.git;
        }

        // --- Project config fields ---
        if overlay.fabro.is_some() {
            self.fabro = overlay.fabro;
        }
    }
}
