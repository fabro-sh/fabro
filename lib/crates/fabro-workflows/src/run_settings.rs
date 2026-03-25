use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use fabro_config::config::FabroConfig;
use fabro_config::run::PullRequestConfig;

use crate::git::GitAuthor;

/// Git checkpoint settings for a workflow run.
#[derive(Clone)]
pub struct GitCheckpointSettings {
    pub base_sha: Option<String>,
    pub run_branch: Option<String>,
    pub meta_branch: Option<String>,
}

/// Configuration for a workflow run.
#[derive(Clone)]
pub struct RunSettings {
    pub config: FabroConfig,
    pub run_dir: PathBuf,
    pub cancel_token: Option<Arc<AtomicBool>>,
    pub dry_run: bool,
    /// Unique identifier for this workflow run.
    pub run_id: String,
    /// User-defined key-value labels for this run.
    pub labels: HashMap<String, String>,
    /// Git author identity for checkpoint commits.
    pub git_author: GitAuthor,
    /// Workflow directory slug (e.g. "smoke" from `fabro/workflows/smoke/`).
    pub workflow_slug: Option<String>,
    /// GitHub App credentials for pushing metadata branches to origin.
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    /// Host repo path for MetadataStore (shadow commits) and host-side pushes.
    pub host_repo_path: Option<PathBuf>,
    /// Name of the branch the run was started from (for PR base).
    pub base_branch: Option<String>,
    /// Git checkpoint settings; `None` means checkpointing disabled.
    pub git: Option<GitCheckpointSettings>,
}

impl RunSettings {
    pub fn checkpoint_exclude_globs(&self) -> &[String] {
        &self.config.checkpoint.exclude_globs
    }

    /// PR config (already normalized — disabled entries stripped at construction).
    pub fn pull_request(&self) -> Option<&PullRequestConfig> {
        self.config.pull_request.as_ref()
    }

    pub fn asset_globs(&self) -> &[String] {
        self.config
            .assets
            .as_ref()
            .map(|a| a.include.as_slice())
            .unwrap_or(&[])
    }
}

/// Configuration for sandbox lifecycle management within the engine.
pub struct LifecycleConfig {
    /// Setup commands to run inside the sandbox after initialization.
    pub setup_commands: Vec<String>,
    /// Timeout in milliseconds for each setup command.
    pub setup_command_timeout_ms: u64,
    /// Devcontainer lifecycle phases and their commands.
    pub devcontainer_phases: Vec<(String, Vec<fabro_devcontainer::Command>)>,
}
