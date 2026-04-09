//! Legacy flat `Settings` shape plus the v2 namespaced schema.
//!
//! The authoritative config schema lives in [`v2`] — it is the namespaced
//! parse tree that `_version = 1` TOML files decode into. Value-language
//! helpers, the merge matrix, and strict unknown-key validation all live
//! there.
//!
//! The flat [`Settings`] type and its submodules (`hook`, `mcp`, `project`,
//! `run`, `sandbox`, `server`, `user`) are the **runtime shapes** that
//! downstream crates (fabro-workflow, fabro-sandbox, fabro-mcp,
//! fabro-hooks) still consume at execution time. Stage 6.1 deleted the
//! `Settings` parse path; Stage 6.2 deleted the `bridge_to_old`
//! catch-all converter. Narrow v2→runtime helpers live in
//! [`v2::to_runtime`] and build these runtime shapes from specific v2
//! subtrees on demand.
//!
//! Stage 6.3 deletes these runtime types entirely in favor of v2-native
//! replacements, at which point this module and the helper modules
//! around it go away too.

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
pub mod v2;

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
    ApiAuthStrategy, ApiSettings, ArtifactStorageBackend, ArtifactStorageSettings, AuthProvider,
    AuthSettings, FeaturesSettings, GitAuthorSettings, GitProvider, GitSettings, LogSettings,
    SlackSettings, TlsSettings, WebSettings, WebhookSettings, WebhookStrategy,
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
    pub artifact_storage: Option<ArtifactStorageSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<SlackSettings>,
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

// All inherent helpers on `Settings` are gone -- the v2 `SettingsFile`
// accessors in `settings::v2::accessors` are the single source of truth
// for reading merged configuration. The flat `Settings` struct itself
// lingers for the OpenAPI legacy `ServerSettings` response shape and a
// handful of demo-route payloads; Stage 6.6 finishes the deletion once
// the OpenAPI spec is rewritten to return v2 DTOs.
