//! Shared types and utilities for SSH-based sandbox implementations (exe, ssh).

use std::time::Instant;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use tokio::sync::OnceCell;

use crate::{shell_quote, SandboxEvent};

/// Output from an SSH command execution.
pub struct SshOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
}

/// Trait abstracting SSH operations for testability.
#[async_trait]
pub trait SshRunner: Send + Sync {
    async fn run_command(&self, command: &str) -> Result<SshOutput, String>;

    async fn run_command_with_timeout(
        &self,
        command: &str,
        timeout: std::time::Duration,
    ) -> Result<SshOutput, String>;

    async fn upload_file(&self, path: &str, content: &[u8]) -> Result<(), String>;

    async fn download_file(&self, path: &str) -> Result<Vec<u8>, String>;
}

/// Parameters for cloning a git repo into the sandbox during initialization.
#[derive(Clone, Debug)]
pub struct GitCloneParams {
    /// Clean HTTPS URL (no embedded credentials).
    pub url: String,
    /// Branch to clone. If None, uses the remote's default.
    pub branch: Option<String>,
}

/// Wrap a shell command in base64 encoding to avoid escaping issues.
pub(crate) fn wrap_bash_command(command: &str) -> String {
    let encoded = STANDARD.encode(command);
    format!("echo '{encoded}' | base64 -d | sh")
}

/// Resolve an authenticated clone URL using GitHub App credentials, falling back to the
/// original URL if authentication fails or no credentials are provided.
pub(crate) async fn resolve_clone_url(
    url: &str,
    github_app: Option<&fabro_github::GitHubAppCredentials>,
) -> Result<String, String> {
    match github_app {
        Some(creds) => fabro_github::resolve_authenticated_url(creds, url)
            .await
            .or_else(|_| Ok(url.to_string())),
        None => Ok(url.to_string()),
    }
}

/// Clone a git repo into a sandbox working directory over SSH.
///
/// Handles the common clone logic including fallback to init+fetch when the
/// directory is not empty, event emission, and origin URL tracking.
pub(crate) async fn clone_repo(
    ssh: &dyn SshRunner,
    working_dir: &str,
    params: &GitCloneParams,
    github_app: Option<&fabro_github::GitHubAppCredentials>,
    origin_url: &OnceCell<String>,
    emit: &(dyn Fn(SandboxEvent) + Send + Sync),
) -> Result<(), String> {
    emit(SandboxEvent::GitCloneStarted {
        url: params.url.clone(),
        branch: params.branch.clone(),
    });
    let clone_start = Instant::now();

    let clone_url = resolve_clone_url(&params.url, github_app).await?;

    let branch_flag = params
        .branch
        .as_deref()
        .map(|b| format!(" --branch {}", shell_quote(b)))
        .unwrap_or_default();

    let clone_script = format!(
        "git clone{branch_flag} {} {}",
        shell_quote(&clone_url),
        shell_quote(working_dir),
    );
    let clone_cmd = wrap_bash_command(&clone_script);
    let clone_timeout = std::time::Duration::from_secs(300);
    let clone_output = ssh
        .run_command_with_timeout(&clone_cmd, clone_timeout)
        .await
        .map_err(|e| {
            let err = format!("git clone failed: {e}");
            emit(SandboxEvent::GitCloneFailed {
                url: params.url.clone(),
                error: err.clone(),
            });
            err
        })?;

    if clone_output.exit_code != 0 {
        let stderr = String::from_utf8_lossy(&clone_output.stderr);

        // Fall back to init + fetch + checkout if directory is not empty
        if stderr.contains("not an empty directory")
            || stderr.contains("already exists and is not an empty")
        {
            let branch = params.branch.as_deref().unwrap_or("main");
            let fallback_script = format!(
                "cd {} && git init && git remote add origin {} && git fetch origin && git checkout {}",
                shell_quote(working_dir),
                shell_quote(&clone_url),
                shell_quote(branch),
            );
            let fallback_cmd = wrap_bash_command(&fallback_script);
            let fallback_output = ssh
                .run_command_with_timeout(&fallback_cmd, clone_timeout)
                .await
                .map_err(|e| {
                    let err = format!("git fallback clone failed: {e}");
                    emit(SandboxEvent::GitCloneFailed {
                        url: params.url.clone(),
                        error: err.clone(),
                    });
                    err
                })?;

            if fallback_output.exit_code != 0 {
                let fallback_stderr = String::from_utf8_lossy(&fallback_output.stderr);
                let err = format!(
                    "git fallback clone failed (exit {}): {fallback_stderr}",
                    fallback_output.exit_code,
                );
                emit(SandboxEvent::GitCloneFailed {
                    url: params.url.clone(),
                    error: err.clone(),
                });
                return Err(err);
            }
        } else {
            let err = format!(
                "git clone failed (exit {}): {stderr}",
                clone_output.exit_code,
            );
            emit(SandboxEvent::GitCloneFailed {
                url: params.url.clone(),
                error: err.clone(),
            });
            return Err(err);
        }
    }

    // Store the clean URL as origin_url for credential refresh
    let _ = origin_url.set(params.url.clone());

    let duration_ms = u64::try_from(clone_start.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit(SandboxEvent::GitCloneCompleted {
        url: params.url.clone(),
        duration_ms,
    });

    Ok(())
}
