use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cli::{ExecSettings, ExecutionMode, ServerSettings};
use crate::config::FabroConfig;
use crate::hook::HookDefinition;
use crate::mcp::McpServerEntry;
use crate::project::ProjectFabroSettings;
use crate::run::{
    AssetsSettings, CheckpointSettings, GitHubSettings, LlmSettings, PullRequestSettings,
    SetupSettings,
};
use crate::sandbox::SandboxSettings;
use crate::server::{
    ApiSettings, FeaturesSettings, GitAuthorSettings, GitSettings, LogSettings, WebSettings,
};

fn is_default_checkpoint(c: &CheckpointSettings) -> bool {
    c.exclude_globs.is_empty()
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct FabroSettings {
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

    #[serde(default, alias = "directory", skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<SetupSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vars: Option<HashMap<String, String>>,

    #[serde(default, skip_serializing_if = "is_default_checkpoint")]
    pub checkpoint: CheckpointSettings,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullRequestSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<AssetsSettings>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookDefinition>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub mcp_servers: HashMap<String, McpServerEntry>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ExecutionMode>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecSettings>,

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

    #[serde(default, alias = "data_dir", skip_serializing_if = "Option::is_none")]
    pub storage_dir: Option<PathBuf>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_runs: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<LogSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fabro: Option<ProjectFabroSettings>,
}

impl TryFrom<FabroConfig> for FabroSettings {
    type Error = anyhow::Error;

    fn try_from(value: FabroConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            version: value.version,
            goal: value.goal,
            goal_file: value.goal_file,
            graph: value.graph,
            labels: value.labels,
            work_dir: value.work_dir,
            llm: value.llm.map(Into::into),
            setup: value.setup.map(Into::into),
            sandbox: value.sandbox.map(TryInto::try_into).transpose()?,
            vars: value.vars,
            checkpoint: value.checkpoint.into(),
            pull_request: value.pull_request.map(Into::into),
            assets: value.assets.map(Into::into),
            hooks: value.hooks,
            mcp_servers: value.mcp_servers,
            github: value.github.map(Into::into),
            mode: value.mode,
            server: value.server.map(TryInto::try_into).transpose()?,
            exec: value.exec.map(Into::into),
            prevent_idle_sleep: value.prevent_idle_sleep,
            verbose: value.verbose,
            upgrade_check: value.upgrade_check,
            dry_run: value.dry_run,
            auto_approve: value.auto_approve,
            no_retro: value.no_retro,
            storage_dir: value.storage_dir,
            max_concurrent_runs: value.max_concurrent_runs,
            web: value.web.map(Into::into),
            api: value.api.map(TryInto::try_into).transpose()?,
            features: value.features.map(Into::into),
            log: value.log.map(Into::into),
            git: value.git.map(TryInto::try_into).transpose()?,
            fabro: value.fabro.map(Into::into),
        })
    }
}

impl TryFrom<&FabroConfig> for FabroSettings {
    type Error = anyhow::Error;

    fn try_from(value: &FabroConfig) -> Result<Self, Self::Error> {
        value.clone().try_into()
    }
}

impl FabroSettings {
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

    pub fn git_author(&self) -> Option<&GitAuthorSettings> {
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
}
