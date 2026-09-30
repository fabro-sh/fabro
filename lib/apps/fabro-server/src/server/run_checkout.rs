//! Acquire a Git target on the server; Petri owns delivery to the workspace.
//!
//! The source is an independent repository under the run's scratch directory,
//! pinned before admission. Keep it for delayed starts and recovery, and remove
//! it with the run. A guard removes it if compilation or persistence fails.

use fabro_config::Storage;
use fabro_types::settings::run::RunMode;
use fabro_types::{DirtyStatus, GitContext, GitCoordinateValidationError, RunId, RunTarget};
use tempfile::TempDir;
use tokio::fs;

use crate::git_checkout::{self, GitCheckoutError};
use crate::run_compiler::PreparedRun;
use crate::server::AppState;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RunCheckoutError {
    #[error("invalid Git target")]
    Target(#[source] GitCoordinateValidationError),
    #[error("could not resolve GitHub checkout credentials; check the server's GitHub integration")]
    Credentials(#[source] anyhow::Error),
    #[error("could not create the run's source directory")]
    Directory(#[source] std::io::Error),
    #[error("could not check out {repo}; check repository access and the requested revision")]
    Checkout {
        repo:   String,
        #[source]
        source: GitCheckoutError,
    },
}

/// Acquire and pin the target, returning its cleanup guard. Folder targets
/// already have a repository; empty, disabled-clone and simulated runs need
/// no remote access. The caller retains the guard only after persisting the
/// run.
pub(crate) async fn prepare(
    state: &AppState,
    prepared: PreparedRun,
    run_id: RunId,
) -> Result<(PreparedRun, Option<TempDir>), RunCheckoutError> {
    let settings = &prepared.settings().run;
    let Some(RunTarget::Git(target)) = prepared.target() else {
        return Ok((prepared, None));
    };
    if !settings.clone.enabled || settings.execution.mode == RunMode::DryRun {
        return Ok((prepared, None));
    }
    let validated = target
        .clone()
        .validate()
        .map_err(RunCheckoutError::Target)?;
    let repo = validated.repository();
    let server_settings = state.server_settings();
    let credentials = state
        .github_credentials(&server_settings.server.integrations.github)
        .await
        .map_err(RunCheckoutError::Credentials)?;
    let auth = git_checkout::resolve_git_read_auth_config(
        credentials.as_ref(),
        repo,
        &state.github_api_base_url,
        state.http_client.clone(),
    )
    .await
    .map_err(RunCheckoutError::Credentials)?;
    let scratch = Storage::new(state.server_storage_dir()).run_scratch(&run_id);
    fs::create_dir_all(scratch.root())
        .await
        .map_err(RunCheckoutError::Directory)?;
    let directory = tempfile::Builder::new()
        .prefix("target-")
        .tempdir_in(scratch.root())
        .map_err(RunCheckoutError::Directory)?;
    let sha = git_checkout::prepare_run_repository(
        validated.target(),
        &git_checkout::github_clone_url(repo),
        auth.as_ref(),
        settings.clone.depth,
        directory.path(),
    )
    .await
    .map_err(|source| RunCheckoutError::Checkout {
        repo: repo.to_string(),
        source,
    })?;
    let origin_url = repo.https_url();
    let mut target = validated.into_target();
    target.sha = Some(sha);
    // Keep the durable target and its Git projection in agreement with the
    // source Petri receives, including when the caller selected only a ref.
    let git = GitContext {
        origin_url,
        branch: target.branch.clone(),
        sha: target.sha.clone(),
        dirty: DirtyStatus::Clean,
    };
    Ok((
        prepared.with_target_and_git(RunTarget::Git(target), Some(git)),
        Some(directory),
    ))
}
