use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use fabro_types::RunId;
use fabro_types::settings::SettingsFile;

use crate::git::{GitAuthor, git_author_from_settings};

/// Git checkpoint options for a workflow run.
#[derive(Clone)]
pub struct GitCheckpointOptions {
    pub base_sha: Option<String>,
    pub run_branch: Option<String>,
    pub meta_branch: Option<String>,
}

/// Options for a workflow run.
#[derive(Clone)]
pub struct RunOptions {
    pub settings: SettingsFile,
    pub run_dir: PathBuf,
    pub cancel_token: Option<Arc<AtomicBool>>,
    /// Unique identifier for this workflow run.
    pub run_id: RunId,
    /// User-defined key-value labels for this run.
    pub labels: HashMap<String, String>,
    /// Workflow directory slug (e.g. "smoke" from `fabro/workflows/smoke/`).
    pub workflow_slug: Option<String>,
    /// GitHub App credentials for pushing metadata branches to origin.
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    /// Host repo path for MetadataStore (shadow commits) and host-side pushes.
    pub host_repo_path: Option<PathBuf>,
    /// Name of the branch the run was started from (for PR base).
    pub base_branch: Option<String>,
    /// Base commit SHA to display in lifecycle events/UI even when checkpointing is disabled.
    pub display_base_sha: Option<String>,
    /// Git checkpoint options; `None` means checkpointing disabled.
    pub git: Option<GitCheckpointOptions>,
}

impl RunOptions {
    pub fn dry_run_enabled(&self) -> bool {
        fabro_config::resolve_run_from_file(&self.settings)
            .map(|settings| settings.execution.mode == fabro_types::settings::run::RunMode::DryRun)
            .unwrap_or(false)
    }

    pub fn checkpoint_exclude_globs(&self) -> Vec<String> {
        fabro_config::resolve_run_from_file(&self.settings)
            .map(|settings| settings.checkpoint.exclude_globs)
            .unwrap_or_default()
    }

    pub fn git_author(&self) -> GitAuthor {
        git_author_from_settings(&self.settings)
    }

    pub fn artifact_globs(&self) -> Vec<String> {
        fabro_config::resolve_run_from_file(&self.settings)
            .map(|settings| settings.artifacts.include)
            .unwrap_or_default()
    }
}

/// Options for sandbox lifecycle management within the engine.
pub struct LifecycleOptions {
    /// Setup commands to run inside the sandbox after initialization.
    pub setup_commands: Vec<String>,
    /// Timeout in milliseconds for each setup command.
    pub setup_command_timeout_ms: u64,
    /// Devcontainer lifecycle phases and their commands.
    pub devcontainer_phases: Vec<(String, Vec<fabro_devcontainer::Command>)>,
}
