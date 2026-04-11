use std::path::PathBuf;
use std::sync::Arc;

#[cfg(any(feature = "docker", feature = "daytona"))]
use anyhow::anyhow;
#[cfg(feature = "daytona")]
use fabro_github::GitHubAppCredentials;
use fabro_types::RunId;

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
        config: DockerSandboxOptions,
    },
    #[cfg(feature = "daytona")]
    Daytona {
        config:       DaytonaConfig,
        github_app:   Option<GitHubAppCredentials>,
        run_id:       Option<RunId>,
        clone_branch: Option<String>,
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
    /// Only Local and Docker have one.
    pub fn host_repo_path(&self) -> Option<PathBuf> {
        match self {
            Self::Local { working_directory } => Some(working_directory.clone()),
            #[cfg(feature = "docker")]
            Self::Docker { config } => Some(PathBuf::from(&config.host_working_directory)),
            #[allow(unreachable_patterns)]
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
            Self::Docker { config } => SandboxRecord {
                provider: self.provider_name().to_string(),
                working_directory: working_directory.clone(),
                identifier,
                host_working_directory: Some(config.host_working_directory.clone()),
                container_mount_point: Some(working_directory),
            },
            _ => SandboxRecord {
                provider: self.provider_name().to_string(),
                working_directory,
                identifier,
                host_working_directory: None,
                container_mount_point: None,
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
                #[cfg(feature = "docker")]
                Self::Docker { .. } => WorkdirStrategy::LocalDirectory,
                #[allow(unreachable_patterns)]
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
            #[cfg(feature = "docker")]
            Self::Docker { .. } => WorkdirStrategy::LocalDirectory,
            #[allow(unreachable_patterns)]
            _ => WorkdirStrategy::Cloud,
        }
    }

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
            Self::Docker { config } => {
                let mut sandbox = DockerSandbox::new(DockerSandboxOptions {
                    image:                  config.image.clone(),
                    host_working_directory: config.host_working_directory.clone(),
                    container_mount_point:  config.container_mount_point.clone(),
                    network_mode:           config.network_mode.clone(),
                    extra_mounts:           config.extra_mounts.clone(),
                    memory_limit:           config.memory_limit,
                    cpu_quota:              config.cpu_quota,
                    auto_pull:              config.auto_pull,
                    env_vars:               config.env_vars.clone(),
                })
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
                clone_branch,
            } => {
                let mut sandbox = DaytonaSandbox::new(
                    config.clone(),
                    github_app.clone(),
                    *run_id,
                    clone_branch.clone(),
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
