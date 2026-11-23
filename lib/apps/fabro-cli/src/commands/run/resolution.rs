use std::path::Path;

use anyhow::{Context as _, bail};
use fabro_manifest::{CollectedWorkflowClosure, ResolvedLocalWorkflowPackage};
use fabro_types::{RunTarget, SandboxProviderKind};
use tokio::task;

use super::remote_workflow::{Interruption, NativeGit};
use super::selection::{TargetSelection, WorkflowSelection};
use crate::user_config;

/// Owns the canonical collector result without copying its contents. Local
/// location metadata remains available for settings warnings and target
/// inference.
pub(super) enum ResolvedWorkflow {
    Local(ResolvedLocalWorkflowPackage),
    Git(CollectedWorkflowClosure),
}

impl ResolvedWorkflow {
    pub(super) fn closure(&self) -> &CollectedWorkflowClosure {
        match self {
            Self::Local(package) => package.closure(),
            Self::Git(closure) => closure,
        }
    }
}

pub(super) async fn workflow(
    selection: &WorkflowSelection,
    cwd: &Path,
    user_workflows: Option<&Path>,
    interruption: &Interruption,
) -> anyhow::Result<ResolvedWorkflow> {
    match selection {
        WorkflowSelection::Local(path) => {
            let (path, cwd, user_workflows) = (
                path.clone(),
                cwd.to_path_buf(),
                user_workflows.map(Path::to_path_buf),
            );
            let package = task::spawn_blocking(move || {
                fabro_manifest::resolve_local_workflow_package(
                    &path,
                    &cwd,
                    user_workflows.as_deref(),
                )
                .map_err(anyhow::Error::new)
            })
            .await
            .context("local workflow collection task failed")??;
            Ok(ResolvedWorkflow::Local(package))
        }
        WorkflowSelection::Git {
            repository,
            selector,
            revision,
        } => {
            let git = NativeGit::new(user_config::github_host()?);
            let (repository, selector, revision) =
                (repository.clone(), selector.clone(), revision.clone());
            let closure = interruption
                .owned(move |cancel| async move {
                    git.collect(repository, selector, revision, cancel).await
                })
                .await?;
            Ok(ResolvedWorkflow::Git(closure))
        }
    }
}

pub(super) async fn target(
    selection: &TargetSelection,
    provider: &SandboxProviderKind,
    cwd: &Path,
    configured_repo_origin_url: Option<&str>,
    interruption: &Interruption,
) -> anyhow::Result<(RunTarget, bool)> {
    let path = match selection {
        TargetSelection::Path(path) => cwd
            .join(path)
            .canonicalize()
            .context("failed to canonicalize target directory")?,
        TargetSelection::Git { repository, branch } => {
            if !provider.clones_workspace() {
                bail!("Git targets require a clone-enabled environment");
            }
            let git = NativeGit::new(user_config::github_host()?);
            let (repository, branch) = (repository.clone(), branch.clone());
            let target = interruption
                .owned(move |cancel| async move {
                    git.resolve_target(repository, branch, &cancel).await
                })
                .await?;
            // Canonical admission retains ownership of provider capabilities.
            return Ok((RunTarget::Git(target), false));
        }
    };
    if !path.is_dir() {
        bail!("target path must be a directory");
    }
    // The existing observer can push/query Git synchronously. Preserve its
    // behavior without blocking a Tokio worker or promising a new timeout.
    let provider = provider.clone();
    let host = user_config::github_host()?;
    let configured_repo_origin_url = configured_repo_origin_url.map(str::to_owned);
    let derived = task::spawn_blocking(move || {
        fabro_manifest::derive_run_target_for_provider(
            &host,
            &provider,
            &path,
            configured_repo_origin_url.as_deref(),
        )
    })
    .await
    .context("target observation task failed")??;
    Ok((derived.target, derived.dirty_worktree))
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "resolver tests construct small local workflow fixtures"
)]
mod tests {
    use super::super::test_support::write_workflow;
    use super::*;

    #[tokio::test]
    async fn run_selection_target_resolution_by_provider() {
        let caller = tempfile::tempdir().unwrap();
        let root = caller.path().canonicalize().unwrap();
        write_workflow(&root, ".fabro/workflows/review");
        std::fs::create_dir(root.join("target")).unwrap();
        let selected = TargetSelection::Path("target".into());
        let interruption = Interruption::new(false);
        assert_eq!(
            target(
                &selected,
                &SandboxProviderKind::LOCAL,
                &root,
                None,
                &interruption
            )
            .await
            .unwrap()
            .0,
            RunTarget::Folder {
                path: root.join("target").to_str().unwrap().into(),
            }
        );
        for provider in [
            SandboxProviderKind::DOCKER,
            SandboxProviderKind::DAYTONA,
            SandboxProviderKind::try_new("host").unwrap(),
        ] {
            assert_eq!(
                target(&selected, &provider, &root, None, &interruption)
                    .await
                    .unwrap()
                    .0,
                RunTarget::None {}
            );
        }
        assert_eq!(
            target(
                &TargetSelection::Path(".".into()),
                &SandboxProviderKind::LOCAL,
                &root,
                None,
                &interruption
            )
            .await
            .unwrap()
            .0,
            RunTarget::Folder {
                path: root.to_str().unwrap().into(),
            }
        );
        assert!(
            target(
                &TargetSelection::Git {
                    repository: "acme/app".parse().unwrap(),
                    branch:     None,
                },
                &SandboxProviderKind::LOCAL,
                &root,
                None,
                &interruption
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("clone-enabled environment")
        );
        for path in ["missing", ".fabro/workflows/review/workflow.toml"] {
            assert!(
                target(
                    &TargetSelection::Path(path.into()),
                    &SandboxProviderKind::LOCAL,
                    &root,
                    None,
                    &interruption
                )
                .await
                .is_err(),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn run_selection_local_lookup_preserves_precedence_and_explicit_failure() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let project = root.path().join("project");
        let checkout = root.path().join("project/checkout");
        write_workflow(&user, "review");
        write_workflow(&project, ".fabro/workflows/review");
        write_workflow(&checkout, ".fabro/workflows/review");
        std::fs::write(project.join(".fabro/project.toml"), "_version = 1\n").unwrap();
        git2::Repository::init(&checkout).unwrap();
        let selected = WorkflowSelection::Local("review".into());
        let interruption = Interruption::new(false);
        for (cwd, expected_root) in [
            (checkout.as_path(), checkout.as_path()),
            (project.as_path(), project.as_path()),
            (root.path(), user.as_path()),
        ] {
            let ResolvedWorkflow::Local(package) =
                workflow(&selected, cwd, Some(&user), &interruption)
                    .await
                    .unwrap()
            else {
                panic!("local package");
            };
            assert_eq!(package.source_root(), expected_root.canonicalize().unwrap());
        }
        assert!(
            workflow(
                &WorkflowSelection::Local("missing.toml".into()),
                &checkout,
                Some(&user),
                &interruption
            )
            .await
            .is_err()
        );
    }
}
