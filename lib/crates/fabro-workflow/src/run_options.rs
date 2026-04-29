use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use fabro_types::settings::run::RunMode;
use fabro_types::{ForkSourceRef, GitContext, RunId, WorkflowSettings};

use crate::git::{GitAuthor, git_author_from_settings};

/// Git checkpoint options for a workflow run.
#[derive(Clone)]
pub struct GitCheckpointOptions {
    pub base_sha:    Option<String>,
    pub run_branch:  Option<String>,
    pub meta_branch: Option<String>,
}

/// Options for a workflow run.
#[derive(Clone)]
pub struct RunOptions {
    pub settings:         WorkflowSettings,
    pub run_dir:          PathBuf,
    pub cancel_token:     Option<Arc<AtomicBool>>,
    /// Unique identifier for this workflow run.
    pub run_id:           RunId,
    /// User-defined key-value labels for this run.
    pub labels:           HashMap<String, String>,
    /// Workflow directory slug (e.g. "smoke" from `.fabro/workflows/smoke/`).
    pub workflow_slug:    Option<String>,
    /// GitHub credentials for pushing metadata branches to origin.
    pub github_app:       Option<fabro_github::GitHubCredentials>,
    /// Submitter-side git context captured before the run was created.
    pub pre_run_git:      Option<GitContext>,
    /// Source checkpoint ref used by fork/rewind-created runs.
    pub fork_source_ref:  Option<ForkSourceRef>,
    /// Name of the branch the run was started from (for PR base).
    pub base_branch:      Option<String>,
    /// Base commit SHA to display in lifecycle events/UI even when
    /// checkpointing is disabled.
    pub display_base_sha: Option<String>,
    /// Git checkpoint options; `None` means checkpointing disabled.
    pub git:              Option<GitCheckpointOptions>,
}

impl RunOptions {
    pub fn dry_run_enabled(&self) -> bool {
        self.settings.run.execution.mode == RunMode::DryRun
    }

    pub fn checkpoint_exclude_globs(&self) -> Vec<String> {
        self.settings.run.checkpoint.exclude_globs.clone()
    }

    pub fn git_author(&self) -> GitAuthor {
        git_author_from_settings(&self.settings)
    }

    pub fn artifact_globs(&self) -> Vec<String> {
        self.settings.run.artifacts.include.clone()
    }

    /// Run branch name from git checkpoint options, if set.
    pub fn run_branch(&self) -> Option<&str> {
        self.git.as_ref().and_then(|g| g.run_branch.as_deref())
    }
}

/// Options for sandbox lifecycle management within the engine.
pub struct LifecycleOptions {
    /// Setup commands to run inside the sandbox after initialization.
    pub setup_commands:           Vec<String>,
    /// Timeout in milliseconds for each setup command.
    pub setup_command_timeout_ms: u64,
    /// Devcontainer lifecycle phases and their commands.
    pub devcontainer_phases:      Vec<(String, Vec<fabro_devcontainer::Command>)>,
}
