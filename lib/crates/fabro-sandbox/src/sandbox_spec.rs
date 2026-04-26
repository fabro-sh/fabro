use std::path::PathBuf;
use std::sync::Arc;

#[cfg(any(feature = "docker", feature = "daytona"))]
use anyhow::anyhow;
#[cfg(any(feature = "docker", feature = "daytona"))]
use fabro_github::GitHubCredentials;
#[allow(
    unused_imports,
    reason = "Daytona-enabled builds persist RunId in the sandbox spec."
)]
use fabro_types::RunId;

#[cfg(any(feature = "docker", feature = "daytona"))]
use crate::clone_source;
use crate::config::WorktreeMode;
#[cfg(feature = "daytona")]
use crate::daytona::{DaytonaConfig, DaytonaSandbox, DaytonaSnapshotConfig};
#[cfg(feature = "docker")]
use crate::docker::{DockerSandbox, DockerSandboxOptions};
use crate::local::LocalSandbox;
use crate::sandbox_record::SandboxRecord;
use crate::{Sandbox, SandboxEventCallback};

/// Options for sandbox initialization and construction.
pub enum SandboxSpec {
    Local {
        working_directory: PathBuf,
    },
    #[cfg(feature = "docker")]
    Docker {
        config:           DockerSandboxOptions,
        github_app:       Option<GitHubCredentials>,
        run_id:           Option<RunId>,
        clone_origin_url: Option<String>,
        clone_branch:     Option<String>,
    },
    #[cfg(feature = "daytona")]
    Daytona {
        config:           DaytonaConfig,
        github_app:       Option<GitHubCredentials>,
        run_id:           Option<RunId>,
        clone_origin_url: Option<String>,
        clone_branch:     Option<String>,
        api_key:          Option<String>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WorkdirStrategy {
    LocalDirectory,
    LocalWorktree,
    Cloud,
}

impl SandboxSpec {
    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            #[cfg(feature = "docker")]
            Self::Docker { .. } => "docker",
            #[cfg(feature = "daytona")]
            Self::Daytona { .. } => "daytona",
        }
    }

    /// Host-accessible repo path for git status / worktree decisions.
    /// Only Local has one. Clone-based providers use their persisted clone
    /// metadata instead of the worker process filesystem.
    pub fn host_repo_path(&self) -> Option<PathBuf> {
        match self {
            Self::Local { working_directory } => Some(working_directory.clone()),
            #[allow(
                unreachable_patterns,
                reason = "Feature-gated variants make this fallback arm reachable on some builds."
            )]
            _ => None,
        }
    }

    /// Build a SandboxRecord for persistence.
    pub fn to_sandbox_record(&self, sandbox: &dyn Sandbox) -> SandboxRecord {
        let working_directory = sandbox.working_directory().to_string();
        let identifier = {
            let info = sandbox.sandbox_info();
            if info.is_empty() { None } else { Some(info) }
        };

        match self {
            #[cfg(feature = "docker")]
            Self::Docker {
                config,
                clone_origin_url,
                clone_branch,
                ..
            } => SandboxRecord {
                provider: self.provider_name().to_string(),
                working_directory: working_directory.clone(),
                identifier,
                host_working_directory: None,
                container_mount_point: None,
                repo_cloned: clone_source::repo_cloned_for_record(
                    config.skip_clone,
                    clone_origin_url.as_deref(),
                ),
                clone_origin_url: clone_source::clean_clone_origin_for_record(
                    clone_origin_url.as_deref(),
                ),
                clone_branch: clone_branch.clone(),
            },
            #[cfg(feature = "daytona")]
            Self::Daytona {
                config,
                clone_origin_url,
                clone_branch,
                ..
            } => SandboxRecord {
                provider: self.provider_name().to_string(),
                working_directory: working_directory.clone(),
                identifier,
                host_working_directory: None,
                container_mount_point: None,
                repo_cloned: clone_source::repo_cloned_for_record(
                    config.skip_clone,
                    clone_origin_url.as_deref(),
                ),
                clone_origin_url: clone_source::clean_clone_origin_for_record(
                    clone_origin_url.as_deref(),
                ),
                clone_branch: clone_branch.clone(),
            },
            _ => SandboxRecord {
                provider: self.provider_name().to_string(),
                working_directory,
                identifier,
                host_working_directory: None,
                container_mount_point: None,
                repo_cloned: None,
                clone_origin_url: None,
                clone_branch: None,
            },
        }
    }

    /// Apply devcontainer snapshot config. Only Daytona uses this.
    #[cfg(feature = "daytona")]
    pub fn apply_devcontainer_snapshot(&mut self, snapshot: DaytonaSnapshotConfig) {
        if let Self::Daytona { config, .. } = self {
            config.snapshot = Some(snapshot);
        }
    }

    pub fn workdir_strategy(
        &self,
        worktree_mode: WorktreeMode,
        git_is_clean: bool,
        checkpoint_present: bool,
    ) -> WorkdirStrategy {
        if checkpoint_present {
            return match self {
                Self::Local { .. } => WorkdirStrategy::LocalDirectory,
                #[allow(
                    unreachable_patterns,
                    reason = "Feature-gated variants make this fallback arm reachable on some builds."
                )]
                _ => WorkdirStrategy::Cloud,
            };
        }

        match self {
            Self::Local { .. } => match worktree_mode {
                WorktreeMode::Always => WorkdirStrategy::LocalWorktree,
                WorktreeMode::Clean => {
                    if git_is_clean {
                        WorkdirStrategy::LocalWorktree
                    } else {
                        WorkdirStrategy::LocalDirectory
                    }
                }
                WorktreeMode::Dirty => {
                    if git_is_clean {
                        WorkdirStrategy::LocalDirectory
                    } else {
                        WorkdirStrategy::LocalWorktree
                    }
                }
                WorktreeMode::Never => WorkdirStrategy::LocalDirectory,
            },
            #[allow(
                unreachable_patterns,
                reason = "Feature-gated variants make this fallback arm reachable on some builds."
            )]
            _ => WorkdirStrategy::Cloud,
        }
    }

    #[allow(
        clippy::unused_async,
        reason = "Only Daytona construction awaits; local and Docker builds share the async API."
    )]
    pub async fn build(
        &self,
        event_callback: Option<SandboxEventCallback>,
    ) -> Result<Arc<dyn Sandbox>, anyhow::Error> {
        match self {
            Self::Local { working_directory } => {
                let mut sandbox = LocalSandbox::new(working_directory.clone());
                if let Some(callback) = event_callback {
                    sandbox.set_event_callback(callback);
                }
                Ok(Arc::new(sandbox))
            }
            #[cfg(feature = "docker")]
            Self::Docker {
                config,
                github_app,
                run_id,
                clone_origin_url,
                clone_branch,
            } => {
                let mut sandbox = DockerSandbox::new(
                    config.clone(),
                    github_app.clone(),
                    *run_id,
                    clone_origin_url.clone(),
                    clone_branch.clone(),
                )
                .map_err(|e| anyhow!("Failed to create Docker sandbox: {e}"))?;
                if let Some(callback) = event_callback {
                    sandbox.set_event_callback(callback);
                }
                Ok(Arc::new(sandbox))
            }
            #[cfg(feature = "daytona")]
            Self::Daytona {
                config,
                github_app,
                run_id,
                clone_origin_url,
                clone_branch,
                api_key,
            } => {
                let mut sandbox = DaytonaSandbox::new(
                    config.clone(),
                    github_app.clone(),
                    *run_id,
                    clone_origin_url.clone(),
                    clone_branch.clone(),
                    api_key.clone(),
                )
                .await
                .map_err(|e| anyhow!(e))?;
                if let Some(callback) = event_callback {
                    sandbox.set_event_callback(callback);
                }
                Ok(Arc::new(sandbox))
            }
        }
    }
}
