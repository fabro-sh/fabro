use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod hook;
pub mod mcp;
pub mod project;
pub mod run;
pub mod sandbox;
pub mod server;
pub mod user;

pub use hook::{HookDefinition, HookEvent, HookSettings, HookType, TlsMode};
pub use mcp::{
    McpServerEntry, McpServerSettings, McpTransport, default_startup_timeout_secs,
    default_tool_timeout_secs,
};
pub use project::ProjectSettings;
pub use run::{
    ArtifactsSettings, CheckpointSettings, GitHubSettings, LlmSettings, MergeStrategy,
    PullRequestSettings, SetupSettings,
};
pub use sandbox::{
    DaytonaNetwork, DaytonaSettings, DaytonaSnapshotSettings, DockerfileSource,
    LocalSandboxSettings, SandboxSettings, WorktreeMode,
};
pub use server::{
    ApiAuthStrategy, ApiSettings, AuthProvider, AuthSettings, FeaturesSettings, GitAuthorSettings,
    GitProvider, GitSettings, LogSettings, TlsSettings, WebSettings, WebhookSettings,
    WebhookStrategy,
};
pub use user::{ClientTlsSettings, ExecSettings, OutputFormat, PermissionLevel, ServerSettings};

fn is_default_checkpoint(c: &CheckpointSettings) -> bool {
    c.exclude_globs.is_empty()
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Settings {
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
    pub artifacts: Option<ArtifactsSettings>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookDefinition>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub mcp_servers: HashMap<String, McpServerEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubSettings>,
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
    pub fabro: Option<ProjectSettings>,
}

impl Settings {
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

    pub fn sandbox_settings(&self) -> Option<&SandboxSettings> {
        self.sandbox.as_ref()
    }

    pub fn setup_settings(&self) -> Option<&SetupSettings> {
        self.setup.as_ref()
    }

    pub fn setup_commands(&self) -> &[String] {
        self.setup
            .as_ref()
            .map_or(&[], |setup| setup.commands.as_slice())
    }

    pub fn setup_timeout_ms(&self) -> Option<u64> {
        self.setup.as_ref().and_then(|setup| setup.timeout_ms)
    }

    pub fn preserve_sandbox_enabled(&self) -> bool {
        self.sandbox
            .as_ref()
            .and_then(|sandbox| sandbox.preserve)
            .unwrap_or(false)
    }

    pub fn github_permissions(&self) -> Option<&HashMap<String, String>> {
        self.github
            .as_ref()
            .and_then(|github| (!github.permissions.is_empty()).then_some(&github.permissions))
    }

    pub fn mcp_server_entries(&self) -> &HashMap<String, McpServerEntry> {
        &self.mcp_servers
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

    pub fn storage_dir(&self) -> PathBuf {
        self.storage_dir.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .expect("could not determine home directory")
                .join(".fabro")
        })
    }
}
