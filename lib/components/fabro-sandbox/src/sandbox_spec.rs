use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use fabro_github::GitHubCredentials;
use fabro_types::{RunId, RunSandboxInstance, RunSandboxRuntime, SandboxProviderKind};
use sandbox_driver::{EventContext, SandboxSource, SandboxSpec as DriverSpec};

use crate::driver::ProviderAccess;
use crate::driver_sandbox::{LayoutSource, RunSandbox};
use crate::environment::CloneRequest;
use crate::{clone_source, provider_sandbox};

/// A run's sandbox on any provider fabro can name: a bundled kind in
/// process or a sandbox-driver plugin. What the environment asked for, and
/// how the repository is cloned into it.
#[derive(Clone, Debug)]
pub struct SandboxSpec {
    pub kind:       SandboxProviderKind,
    /// The provider settings and vault credentials the kind needs.
    pub access:     ProviderAccess,
    /// The environment's request, as the driver spec every provider
    /// starts from.
    pub spec:       DriverSpec,
    pub clone:      CloneRequest,
    pub github_app: Option<GitHubCredentials>,
    pub run_id:     Option<RunId>,
}

impl SandboxSpec {
    /// A sandbox on this host at `working_directory`, the fabro `local`
    /// kind. The directory is designated: the sandbox uses it in place,
    /// never removes it, and clones nothing into it. The Host provider has
    /// no image, labels, or lifecycle timers, so the spec names only the
    /// directory.
    #[must_use]
    pub fn local(working_directory: impl Into<PathBuf>, access: ProviderAccess) -> Self {
        Self {
            kind: SandboxProviderKind::LOCAL,
            access,
            spec: DriverSpec::new(SandboxSource::HostDirectory)
                .working_directory(working_directory.into().display().to_string()),
            clone: CloneRequest::none(),
            github_app: None,
            run_id: None,
        }
    }

    pub fn provider(&self) -> SandboxProviderKind {
        self.kind.clone()
    }

    pub fn provider_name(&self) -> String {
        self.kind.to_string()
    }

    /// The directory the spec designates on the provider, when it names one.
    #[must_use]
    pub fn working_directory(&self) -> Option<&str> {
        self.spec.working_directory.as_deref()
    }

    /// The image the run record names for this sandbox: the environment's,
    /// or the provider's default when the environment names none.
    pub fn image(&self) -> Option<String> {
        provider_sandbox::recorded_image(&self.kind, &self.spec)
    }

    /// Build initialized sandbox metadata for persistence.
    pub fn to_run_sandbox_instance(&self, sandbox: &RunSandbox) -> RunSandboxInstance {
        let working_directory = sandbox.working_directory().to_string();
        let id = sandbox.sandbox_info();
        let clone_origin_url = &self.clone.origin_url;
        let repo_cloned =
            clone_source::repo_cloned_for_record(self.clone.skip, clone_origin_url.as_deref());
        // A fixed layout is known before the sandbox exists; a
        // provider-chosen one only from the sandbox.
        let layout = match provider_sandbox::layout_source(&self.kind) {
            LayoutSource::Fixed(fixed) => {
                let repo = runtime_layout_metadata(
                    repo_cloned,
                    clone_origin_url.as_deref(),
                    &fixed.workspace_root,
                    &fixed.repos_root,
                );
                Some(crate::SandboxWorkspaceLayout {
                    workspace_root:    fixed.workspace_root,
                    repos_root:        fixed.repos_root,
                    primary_repo_path: repo.as_ref().map(|layout| layout.primary_repo_path.clone()),
                    primary_repo_link: repo.as_ref().map(|layout| layout.primary_repo_link.clone()),
                })
            }
            LayoutSource::ProviderWorkingDirectory => sandbox.workspace_layout(),
        };
        RunSandboxInstance {
            provider:          self.kind.clone(),
            image:             self.image(),
            snapshot:          sandbox.snapshot_info(),
            runtime:           RunSandboxRuntime {
                id,
                working_directory,
                repo_cloned,
                clone_origin_url: clone_source::clean_clone_origin_for_record(
                    clone_origin_url.as_deref(),
                ),
                clone_branch: self.clone.branch.clone(),
                workspace_root: layout.as_ref().map(|layout| layout.workspace_root.clone()),
                repos_root: layout.as_ref().map(|layout| layout.repos_root.clone()),
                primary_repo_path: layout
                    .as_ref()
                    .and_then(|layout| layout.primary_repo_path.clone()),
                primary_repo_link: layout
                    .as_ref()
                    .and_then(|layout| layout.primary_repo_link.clone()),
            },
            ready_duration_ms: None,
            retained:          None,
        }
    }

    /// Builds the sandbox; `initialize` creates it on the provider. The
    /// driver reports its lifecycle through `events` from then on.
    pub async fn build(
        &self,
        events: Option<EventContext>,
    ) -> Result<Arc<RunSandbox>, anyhow::Error> {
        let mut sandbox = provider_sandbox::provider_sandbox(
            self.kind.clone(),
            &self.access,
            self.spec.clone(),
            &self.clone,
            self.github_app.as_ref(),
            self.run_id,
        )
        .await
        .with_context(|| format!("Failed to create {} sandbox", self.kind))?;
        if let Some(events) = events {
            sandbox.set_events(events);
        }
        Ok(Arc::new(sandbox))
    }
}

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
    use sandbox_driver_testing::ScriptedSandbox;

    use super::*;

    fn docker_spec(clone: CloneRequest) -> SandboxSpec {
        SandboxSpec {
            kind: SandboxProviderKind::DOCKER,
            access: ProviderAccess::default(),
            spec: DriverSpec::new(SandboxSource::HostDirectory),
            clone,
            github_app: None,
            run_id: None,
        }
    }

    fn sandbox_at(kind: SandboxProviderKind, working_dir: &str) -> RunSandbox {
        RunSandbox::new(
            kind,
            Arc::new(ScriptedSandbox::with_id_and_working_dir(
                "scripted-1",
                working_dir,
            )),
        )
    }

    #[test]
    fn docker_run_sandbox_persists_layout_metadata_for_cloned_repo() {
        let spec = docker_spec(CloneRequest {
            origin_url: Some("git@github.com:brynary/rack-test.git".to_string()),
            branch: Some("main".to_string()),
            ..CloneRequest::default()
        });
        let sandbox = sandbox_at(SandboxProviderKind::DOCKER, "/workspace/rack-test");

        let record = spec.to_run_sandbox_instance(&sandbox);
        let runtime = record.runtime;

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
        let runtime_json = serde_json::to_value(&runtime).expect("runtime should serialize");
        assert!(runtime_json.get("clone_commit_sha").is_none());
    }

    #[tokio::test]
    async fn invalid_exact_checkout_spec_fails_before_provider_connection() {
        let spec = docker_spec(CloneRequest {
            origin_url: Some("https://github.com/acme/widgets".to_string()),
            branch: Some("main".to_string()),
            commit_sha: Some("not-a-sha".to_string()),
            ..CloneRequest::default()
        });

        let error = spec
            .build(None)
            .await
            .err()
            .expect("spec validation should run before Docker connection");
        assert!(
            error
                .to_string()
                .contains("Failed to create docker sandbox")
        );
        assert!(format!("{error:#}").contains("40 ASCII hexadecimal"));
        assert!(!format!("{error:#}").contains("Docker daemon"));
    }

    #[test]
    fn docker_run_sandbox_omits_primary_repo_metadata_for_empty_workspace() {
        let spec = docker_spec(CloneRequest {
            origin_url: Some("https://gitlab.com/acme/widgets".to_string()),
            ..CloneRequest::none()
        });
        let sandbox = sandbox_at(SandboxProviderKind::DOCKER, "/workspace");

        let record = spec.to_run_sandbox_instance(&sandbox);
        let runtime = record.runtime;

        assert_eq!(runtime.working_directory, "/workspace");
        assert_eq!(runtime.repo_cloned, Some(false));
        assert_eq!(runtime.workspace_root.as_deref(), Some("/workspace"));
        assert_eq!(runtime.repos_root.as_deref(), Some("/repos"));
        assert!(runtime.primary_repo_path.is_none());
        assert!(runtime.primary_repo_link.is_none());
    }

    #[test]
    fn local_spec_designates_the_directory_and_clones_nothing() {
        let spec = SandboxSpec::local("/home/dev/project", ProviderAccess::default());

        assert_eq!(spec.kind, SandboxProviderKind::LOCAL);
        assert_eq!(spec.working_directory(), Some("/home/dev/project"));
        assert!(spec.clone.skip);
        assert_eq!(spec.clone.origin_url, None);
        assert_eq!(spec.image(), None);
        assert!(matches!(spec.spec.source, SandboxSource::HostDirectory));

        let sandbox = sandbox_at(SandboxProviderKind::LOCAL, "/home/dev/project");
        let record = spec.to_run_sandbox_instance(&sandbox);

        assert_eq!(record.provider, SandboxProviderKind::LOCAL);
        assert_eq!(record.image, None);
        assert_eq!(record.snapshot, None);
        assert_eq!(record.runtime.id, "scripted-1");
        assert_eq!(record.runtime.working_directory, "/home/dev/project");
        assert_eq!(record.runtime.repo_cloned, Some(false));
        assert_eq!(record.runtime.clone_origin_url, None);
        assert_eq!(record.runtime.clone_branch, None);
        assert_eq!(
            record.runtime.workspace_root.as_deref(),
            Some("/home/dev/project")
        );
        assert!(record.runtime.primary_repo_path.is_none());
        assert!(record.runtime.primary_repo_link.is_none());
    }
}
