use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cli::{ExecConfig, ExecutionMode, ServerConfig};
use crate::combine::Combine;
use crate::hook::{HookConfig, HookDefinition};
use crate::mcp::McpServerEntry;
use crate::project::ProjectFabroConfig;
use crate::run::{
    AssetsConfig, CheckpointConfig, GitHubConfig, LlmConfig, PullRequestConfig, SetupConfig,
};
use crate::sandbox::SandboxConfig;
use crate::server::{ApiConfig, Features, GitConfig, LogConfig, WebConfig};
use crate::settings::FabroSettings;

fn is_default_checkpoint(c: &CheckpointConfig) -> bool {
    c.exclude_globs.is_empty()
}

/// Unified sparse configuration type for all Fabro config sources.
///
/// Loading functions (`load_cli_config`, `load_server_config`, `load_run_config`,
/// `parse_project_config`) all return this type. Fields irrelevant to a
/// particular source are left unset (`None` / empty).
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
    pub server: Option<ServerConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecConfig>,

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

impl Combine for FabroConfig {
    fn combine(self, other: Self) -> Self {
        let hooks = if self.hooks.is_empty() {
            other.hooks
        } else if other.hooks.is_empty() {
            self.hooks
        } else {
            HookConfig { hooks: other.hooks }
                .merge(HookConfig { hooks: self.hooks })
                .hooks
        };

        Self {
            version: self.version.combine(other.version),
            goal: self.goal.combine(other.goal),
            goal_file: self.goal_file.combine(other.goal_file),
            graph: self.graph.combine(other.graph),
            labels: self.labels.combine(other.labels),
            work_dir: self.work_dir.combine(other.work_dir),
            llm: self.llm.combine(other.llm),
            setup: self.setup.combine(other.setup),
            sandbox: self.sandbox.combine(other.sandbox),
            vars: self.vars.combine(other.vars),
            checkpoint: self.checkpoint.combine(other.checkpoint),
            pull_request: self.pull_request.combine(other.pull_request),
            assets: self.assets.combine(other.assets),
            hooks,
            mcp_servers: self.mcp_servers.combine(other.mcp_servers),
            github: self.github.combine(other.github),
            mode: self.mode.combine(other.mode),
            server: self.server.combine(other.server),
            exec: self.exec.combine(other.exec),
            prevent_idle_sleep: self.prevent_idle_sleep.combine(other.prevent_idle_sleep),
            verbose: self.verbose.combine(other.verbose),
            upgrade_check: self.upgrade_check.combine(other.upgrade_check),
            dry_run: self.dry_run.combine(other.dry_run),
            auto_approve: self.auto_approve.combine(other.auto_approve),
            no_retro: self.no_retro.combine(other.no_retro),
            storage_dir: self.storage_dir.combine(other.storage_dir),
            max_concurrent_runs: self.max_concurrent_runs.combine(other.max_concurrent_runs),
            web: self.web.combine(other.web),
            api: self.api.combine(other.api),
            features: self.features.combine(other.features),
            log: self.log.combine(other.log),
            git: self.git.combine(other.git),
            fabro: self.fabro.combine(other.fabro),
        }
    }
}

impl FabroConfig {
    pub fn combine(self, other: Self) -> Self {
        Combine::combine(self, other)
    }

    pub fn try_into_settings(self) -> anyhow::Result<FabroSettings> {
        self.try_into()
    }
}
