use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "docker")]
use anyhow::Context as _;
#[cfg(any(feature = "docker", feature = "daytona"))]
use fabro_github::GitHubCredentials;
#[allow(
    unused_imports,
    reason = "Daytona-enabled builds persist RunId in the sandbox spec."
)]
use fabro_types::{RunId, RunSandbox, RunSandboxRuntime, SandboxProviderKind};

#[cfg(any(feature = "docker", feature = "daytona"))]
use crate::clone_source;
#[cfg(feature = "daytona")]
use crate::daytona::{self, DaytonaConfig, DaytonaSandbox};
#[cfg(feature = "docker")]
use crate::docker::{self, DockerSandbox, DockerSandboxOptions};
use crate::local::LocalSandbox;
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
        config:           Box<DaytonaConfig>,
        github_app:       Option<GitHubCredentials>,
        run_id:           Option<RunId>,
        clone_origin_url: Option<String>,
        clone_branch:     Option<String>,
        api_key:          Option<String>,
    },
}

impl SandboxSpec {
    pub fn provider(&self) -> SandboxProviderKind {
        match self {
            Self::Local { .. } => SandboxProviderKind::Local,
            #[cfg(feature = "docker")]
            Self::Docker { .. } => SandboxProviderKind::Docker,
            #[cfg(feature = "daytona")]
            Self::Daytona { .. } => SandboxProviderKind::Daytona,
        }
    }

    pub fn provider_name(&self) -> &'static str {
        match self.provider() {
            SandboxProviderKind::Local => "local",
            SandboxProviderKind::Docker => "docker",
            SandboxProviderKind::Daytona => "daytona",
        }
    }

    /// Build a RunSandbox for persistence.
    pub fn to_run_sandbox(&self, sandbox: &dyn Sandbox, run_id: RunId) -> RunSandbox {
        let working_directory = sandbox.working_directory().to_string();
        let id = {
            let info = sandbox.sandbox_info();
            if info.is_empty() {
                format!("local:{run_id}")
            } else {
                info
            }
        };

        match self {
            #[cfg(feature = "docker")]
            Self::Docker {
                config,
                clone_origin_url,
                clone_branch,
                ..
            } => {
                let repo_cloned = clone_source::repo_cloned_for_record(
                    config.skip_clone,
                    clone_origin_url.as_deref(),
                );
                let layout = runtime_layout_metadata(
                    repo_cloned,
                    clone_origin_url.as_deref(),
                    docker::WORKING_DIRECTORY,
                    docker::REPOS_ROOT,
                );
                RunSandbox {
                    provider: self.provider(),
                    image:    (!config.image.is_empty()).then(|| config.image.clone()),
                    snapshot: None,
                    runtime:  Some(RunSandboxRuntime {
                        id,
                        working_directory: working_directory.clone(),
                        repo_cloned,
                        clone_origin_url: clone_source::clean_clone_origin_for_record(
                            clone_origin_url.as_deref(),
                        ),
                        clone_branch: clone_branch.clone(),
                        workspace_root: Some(docker::WORKING_DIRECTORY.to_string()),
                        repos_root: Some(docker::REPOS_ROOT.to_string()),
                        primary_repo_path: layout
                            .as_ref()
                            .map(|layout| layout.primary_repo_path.clone()),
                        primary_repo_link: layout
                            .as_ref()
                            .map(|layout| layout.primary_repo_link.clone()),
                    }),
                }
            }
            #[cfg(feature = "daytona")]
            Self::Daytona {
                config,
                clone_origin_url,
                clone_branch,
                ..
            } => {
                let repo_cloned = clone_source::repo_cloned_for_record(
                    config.skip_clone,
                    clone_origin_url.as_deref(),
                );
                let layout = runtime_layout_metadata(
                    repo_cloned,
                    clone_origin_url.as_deref(),
                    daytona::WORKING_DIRECTORY,
                    daytona::REPOS_ROOT,
                );
                RunSandbox {
                    provider: self.provider(),
                    image:    None,
                    snapshot: sandbox.snapshot_info(),
                    runtime:  Some(RunSandboxRuntime {
                        id,
                        working_directory: working_directory.clone(),
                        repo_cloned,
                        clone_origin_url: clone_source::clean_clone_origin_for_record(
                            clone_origin_url.as_deref(),
                        ),
                        clone_branch: clone_branch.clone(),
                        workspace_root: Some(daytona::WORKING_DIRECTORY.to_string()),
                        repos_root: Some(daytona::REPOS_ROOT.to_string()),
                        primary_repo_path: layout
                            .as_ref()
                            .map(|layout| layout.primary_repo_path.clone()),
                        primary_repo_link: layout
                            .as_ref()
                            .map(|layout| layout.primary_repo_link.clone()),
                    }),
                }
            }
            _ => RunSandbox {
                provider: self.provider(),
                image:    None,
                snapshot: None,
                runtime:  Some(RunSandboxRuntime {
                    id,
                    working_directory,
                    repo_cloned: None,
                    clone_origin_url: None,
                    clone_branch: None,
                    workspace_root: None,
                    repos_root: None,
                    primary_repo_path: None,
                    primary_repo_link: None,
                }),
            },
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
                .context("Failed to create Docker sandbox")?;
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
                    config.as_ref().clone(),
                    github_app.clone(),
                    *run_id,
                    clone_origin_url.clone(),
                    clone_branch.clone(),
                    api_key.clone(),
                )
                .await
                .map_err(anyhow::Error::new)?;
                if let Some(callback) = event_callback {
                    sandbox.set_event_callback(callback);
                }
                Ok(Arc::new(sandbox))
            }
        }
    }
}

#[cfg(any(feature = "docker", feature = "daytona"))]
fn runtime_layout_metadata(
    repo_cloned: Option<bool>,
    clone_origin_url: Option<&str>,
    workspace_root: &str,
    repos_root: &str,
) -> Option<clone_source::GitHubRepoLayout> {
    if repo_cloned != Some(true) {
        return None;
    }
    clone_source::github_repo_layout(clone_origin_url?, workspace_root, repos_root).ok()
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "docker")]
    use fabro_types::RunId;

    #[cfg(feature = "docker")]
    use super::*;
    #[cfg(feature = "docker")]
    use crate::test_support::MockSandbox;

    #[cfg(feature = "docker")]
    #[test]
    fn docker_run_sandbox_persists_layout_metadata_for_cloned_repo() {
        let spec = SandboxSpec::Docker {
            config:           DockerSandboxOptions::default(),
            github_app:       None,
            run_id:           None,
            clone_origin_url: Some("git@github.com:brynary/rack-test.git".to_string()),
            clone_branch:     Some("main".to_string()),
        };
        let mut sandbox = MockSandbox::linux();
        sandbox.working_dir = "/workspace/rack-test";

        let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
        let record = spec.to_run_sandbox(&sandbox, run_id);
        let runtime = record.runtime.expect("runtime");

        assert_eq!(runtime.working_directory, "/workspace/rack-test");
        assert_eq!(runtime.repo_cloned, Some(true));
        assert_eq!(
            runtime.clone_origin_url.as_deref(),
            Some("https://github.com/brynary/rack-test")
        );
        assert_eq!(runtime.workspace_root.as_deref(), Some("/workspace"));
        assert_eq!(runtime.repos_root.as_deref(), Some("/repos"));
        assert_eq!(
            runtime.primary_repo_path.as_deref(),
            Some("/repos/brynary/rack-test")
        );
        assert_eq!(
            runtime.primary_repo_link.as_deref(),
            Some("/workspace/rack-test")
        );
    }

    #[cfg(feature = "docker")]
    #[test]
    fn docker_run_sandbox_omits_primary_repo_metadata_for_empty_workspace() {
        let spec = SandboxSpec::Docker {
            config:           DockerSandboxOptions {
                skip_clone: true,
                ..DockerSandboxOptions::default()
            },
            github_app:       None,
            run_id:           None,
            clone_origin_url: Some("https://gitlab.com/acme/widgets".to_string()),
            clone_branch:     None,
        };
        let mut sandbox = MockSandbox::linux();
        sandbox.working_dir = "/workspace";

        let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
        let record = spec.to_run_sandbox(&sandbox, run_id);
        let runtime = record.runtime.expect("runtime");

        assert_eq!(runtime.working_directory, "/workspace");
        assert_eq!(runtime.repo_cloned, Some(false));
        assert_eq!(runtime.workspace_root.as_deref(), Some("/workspace"));
        assert_eq!(runtime.repos_root.as_deref(), Some("/repos"));
        assert!(runtime.primary_repo_path.is_none());
        assert!(runtime.primary_repo_link.is_none());
    }
}
