use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::time;
use tokio_util::sync::CancellationToken;

/// Git command prefix that disables background maintenance.
const GIT: &str = "git -c maintenance.auto=0 -c gc.auto=0";

/// Information returned when a sandbox sets up git for a workflow run.
#[derive(Debug, Clone)]
pub struct GitRunInfo {
    pub base_sha:    String,
    pub run_branch:  String,
    pub base_branch: Option<String>,
}

/// Git setup requested by the workflow layer.
#[derive(Debug, Clone)]
pub enum GitSetupIntent {
    NewRun {
        run_id: String,
    },
    ForkFromCheckpoint {
        new_run_id:     String,
        source_run_id:  String,
        checkpoint_sha: String,
    },
}

/// Generates an `#[async_trait] impl Sandbox` block for a decorator type
/// that wraps an `Arc<dyn Sandbox>`. The caller provides custom method
/// implementations; all remaining trait methods delegate to the inner field.
///
/// # Usage
///
/// ```ignore
/// delegate_sandbox! {
///     MyDecorator => inner {
///         // Only provide methods with custom logic — the rest delegate automatically.
///         async fn read_file(&self, path: &str, offset: Option<usize>, limit: Option<usize>) -> $crate::Result<String> {
///             // custom logic...
///         }
///     }
/// }
/// ```
#[macro_export]
macro_rules! delegate_sandbox {
    (
        $type:ty => $field:ident {
            $($custom:item)*
        }
    ) => {
        #[async_trait::async_trait]
        impl $crate::Sandbox for $type {
            $($custom)*

            async fn file_exists(&self, path: &str) -> $crate::Result<bool> {
                self.$field.file_exists(path).await
            }

            async fn list_directory(
                &self,
                path: &str,
                depth: Option<usize>,
            ) -> $crate::Result<Vec<$crate::DirEntry>> {
                self.$field.list_directory(path, depth).await
            }

            async fn exec_command(
                &self,
                command: &str,
                timeout_ms: u64,
                working_dir: Option<&str>,
                env_vars: Option<&std::collections::HashMap<String, String>>,
                cancel_token: Option<tokio_util::sync::CancellationToken>,
            ) -> $crate::Result<$crate::ExecResult> {
                self.$field
                    .exec_command(command, timeout_ms, working_dir, env_vars, cancel_token)
                    .await
            }

            async fn glob(&self, pattern: &str, path: Option<&str>) -> $crate::Result<Vec<String>> {
                self.$field.glob(pattern, path).await
            }

            async fn download_file_to_local(
                &self,
                remote_path: &str,
                local_path: &std::path::Path,
            ) -> $crate::Result<()> {
                self.$field.download_file_to_local(remote_path, local_path).await
            }

            async fn upload_file_from_local(
                &self,
                local_path: &std::path::Path,
                remote_path: &str,
            ) -> $crate::Result<()> {
                self.$field.upload_file_from_local(local_path, remote_path).await
            }

            async fn initialize(&self) -> $crate::Result<()> {
                self.$field.initialize().await
            }

            async fn cleanup(&self) -> $crate::Result<()> {
                self.$field.cleanup().await
            }

            fn working_directory(&self) -> &str {
                self.$field.working_directory()
            }

            fn platform(&self) -> &str {
                self.$field.platform()
            }

            fn os_version(&self) -> String {
                self.$field.os_version()
            }

            fn sandbox_info(&self) -> String {
                self.$field.sandbox_info()
            }

            async fn refresh_push_credentials(&self) -> $crate::Result<()> {
                self.$field.refresh_push_credentials().await
            }

            async fn set_autostop_interval(&self, minutes: i32) -> $crate::Result<()> {
                self.$field.set_autostop_interval(minutes).await
            }

            async fn setup_git(&self, intent: &$crate::GitSetupIntent) -> $crate::Result<Option<$crate::GitRunInfo>> {
                self.$field.setup_git(intent).await
            }

            fn resume_setup_commands(&self, run_branch: &str) -> Vec<String> {
                self.$field.resume_setup_commands(run_branch)
            }

            async fn git_push_ref(&self, refspec: &str) -> bool {
                self.$field.git_push_ref(refspec).await
            }

            fn parallel_worktree_path(
                &self,
                run_dir: &std::path::Path,
                run_id: &str,
                node_id: &str,
                key: &str,
            ) -> String {
                self.$field.parallel_worktree_path(run_dir, run_id, node_id, key)
            }

            async fn ssh_access_command(&self) -> $crate::Result<Option<String>> {
                self.$field.ssh_access_command().await
            }

            fn origin_url(&self) -> Option<&str> {
                self.$field.origin_url()
            }

            async fn get_preview_url(&self, port: u16) -> $crate::Result<Option<(String, std::collections::HashMap<String, String>)>> {
                self.$field.get_preview_url(port).await
            }

            async fn read_file(
                &self,
                path: &str,
                offset: Option<usize>,
                limit: Option<usize>,
            ) -> $crate::Result<String> {
                self.$field.read_file(path, offset, limit).await
            }

            async fn grep(
                &self,
                pattern: &str,
                path: &str,
                options: &$crate::GrepOptions,
            ) -> $crate::Result<Vec<String>> {
                self.$field.grep(pattern, path, options).await
            }
        }
    };
}

/// Events emitted during sandbox lifecycle operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SandboxEvent {
    // -- Common lifecycle --
    Initializing {
        provider: String,
    },
    Ready {
        provider:    String,
        duration_ms: u64,
        name:        Option<String>,
        cpu:         Option<f64>,
        memory:      Option<f64>,
        url:         Option<String>,
    },
    InitializeFailed {
        provider:    String,
        error:       String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        causes:      Vec<String>,
        duration_ms: u64,
    },
    CleanupStarted {
        provider: String,
    },
    CleanupCompleted {
        provider:    String,
        duration_ms: u64,
    },
    CleanupFailed {
        provider: String,
        error:    String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        causes:   Vec<String>,
    },

    // -- Docker --
    SnapshotPulling {
        name: String,
    },
    SnapshotPulled {
        name:        String,
        duration_ms: u64,
    },

    // -- Daytona snapshots --
    SnapshotEnsuring {
        name: String,
    },
    SnapshotCreating {
        name: String,
    },
    SnapshotReady {
        name:        String,
        duration_ms: u64,
    },
    SnapshotFailed {
        name:   String,
        error:  String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        causes: Vec<String>,
    },

    // -- Daytona git --
    GitCloneStarted {
        url:    String,
        branch: Option<String>,
    },
    GitCloneCompleted {
        url:         String,
        duration_ms: u64,
    },
    GitCloneFailed {
        url:    String,
        error:  String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        causes: Vec<String>,
    },
}

impl SandboxEvent {
    pub fn trace(&self) {
        use tracing::{debug, error, info, warn};
        match self {
            Self::Initializing { provider } => {
                debug!(provider, "Sandbox initializing");
            }
            Self::Ready {
                provider,
                duration_ms,
                ..
            } => {
                info!(provider, duration_ms, "Sandbox ready");
            }
            Self::InitializeFailed {
                provider,
                error,
                causes,
                duration_ms,
            } => {
                error!(provider, error, causes = ?causes, duration_ms, "Sandbox init failed");
            }
            Self::CleanupStarted { provider } => {
                debug!(provider, "Sandbox cleanup started");
            }
            Self::CleanupCompleted {
                provider,
                duration_ms,
            } => {
                debug!(provider, duration_ms, "Sandbox cleanup completed");
            }
            Self::CleanupFailed {
                provider,
                error,
                causes,
            } => {
                warn!(provider, error, causes = ?causes, "Sandbox cleanup failed");
            }
            Self::SnapshotPulling { name } => {
                debug!(name, "Snapshot pulling");
            }
            Self::SnapshotPulled { name, duration_ms } => {
                debug!(name, duration_ms, "Snapshot pulled");
            }
            Self::SnapshotEnsuring { name } => {
                debug!(name, "Snapshot ensuring");
            }
            Self::SnapshotCreating { name } => {
                debug!(name, "Snapshot creating");
            }
            Self::SnapshotReady { name, duration_ms } => {
                info!(name, duration_ms, "Snapshot ready");
            }
            Self::SnapshotFailed {
                name,
                error,
                causes,
            } => {
                error!(name, error, causes = ?causes, "Snapshot failed");
            }
            Self::GitCloneStarted { url, branch } => {
                debug!(
                    url,
                    branch = branch.as_deref().unwrap_or(""),
                    "Git clone started"
                );
            }
            Self::GitCloneCompleted { url, duration_ms } => {
                debug!(url, duration_ms, "Git clone completed");
            }
            Self::GitCloneFailed { url, error, causes } => {
                error!(url, error, causes = ?causes, "Git clone failed");
            }
        }
    }
}

/// Callback type for sandbox events.
pub type SandboxEventCallback = Arc<dyn Fn(SandboxEvent) + Send + Sync>;

/// Formats file content with line numbers for display.
///
/// Applies optional offset (0-based lines to skip) and limit (max lines to
/// return). Line numbers are 1-based and right-aligned.
#[must_use]
pub fn format_lines_numbered(content: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    let all_lines: Vec<&str> = content.lines().collect();
    let skip = offset.unwrap_or(0);
    let take = limit.unwrap_or(all_lines.len());
    let selected: Vec<&str> = all_lines.into_iter().skip(skip).take(take).collect();
    let width = (skip + selected.len()).to_string().len().max(1);
    let mut result = String::new();
    for (i, line) in selected.iter().enumerate() {
        let line_num = skip + i + 1;
        let _ = writeln!(result, "{line_num:>width$} | {line}");
    }
    result
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout:      String,
    pub stderr:      String,
    pub exit_code:   i32,
    pub timed_out:   bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name:   String,
    pub is_dir: bool,
    pub size:   Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct GrepOptions {
    pub glob_filter:      Option<String>,
    pub case_insensitive: bool,
    pub max_results:      Option<usize>,
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> crate::Result<String>;
    async fn write_file(&self, path: &str, content: &str) -> crate::Result<()>;
    async fn delete_file(&self, path: &str) -> crate::Result<()>;
    async fn file_exists(&self, path: &str) -> crate::Result<bool>;
    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> crate::Result<Vec<DirEntry>>;
    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&std::collections::HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> crate::Result<ExecResult>;
    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> crate::Result<Vec<String>>;
    async fn glob(&self, pattern: &str, path: Option<&str>) -> crate::Result<Vec<String>>;
    /// Copy a file from the sandbox to a local filesystem path.
    /// Handles binary files correctly across all sandbox types.
    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> crate::Result<()>;
    /// Copy a file from the local filesystem into the sandbox.
    /// Handles binary files correctly across all sandbox types.
    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> crate::Result<()>;
    async fn initialize(&self) -> crate::Result<()>;
    async fn cleanup(&self) -> crate::Result<()>;
    fn working_directory(&self) -> &str;
    fn platform(&self) -> &str;
    fn os_version(&self) -> String;
    /// Return a human-readable identifier for the sandbox (e.g. container ID,
    /// sandbox name). Used when `--preserve-sandbox` is active to tell the
    /// user how to reconnect.
    fn sandbox_info(&self) -> String {
        String::new()
    }

    /// Refresh git push credentials (e.g. rotate an expiring GitHub App token).
    /// Default is a no-op; Daytona overrides to update the remote URL with a
    /// fresh token.
    async fn refresh_push_credentials(&self) -> crate::Result<()> {
        Ok(())
    }

    /// Set the auto-stop interval in minutes (0 to disable).
    /// Default is a no-op; Daytona overrides to call the Daytona API.
    async fn set_autostop_interval(&self, _minutes: i32) -> crate::Result<()> {
        Ok(())
    }

    /// Set up git state for a workflow run.
    /// Sandboxes that manage their own git clone (e.g., remote VMs) should
    /// create a run branch and return the git info.
    async fn setup_git(&self, _intent: &GitSetupIntent) -> crate::Result<Option<GitRunInfo>> {
        Ok(None)
    }

    /// Commands to run inside the sandbox when resuming on an existing run
    /// branch.
    fn resume_setup_commands(&self, _run_branch: &str) -> Vec<String> {
        Vec::new()
    }

    /// Push a full refspec to origin from inside the sandbox.
    async fn git_push_ref(&self, _refspec: &str) -> bool {
        false
    }

    /// Compute the filesystem path for a parallel branch worktree.
    fn parallel_worktree_path(
        &self,
        run_dir: &std::path::Path,
        _run_id: &str,
        node_id: &str,
        key: &str,
    ) -> String {
        run_dir
            .join("parallel")
            .join(node_id)
            .join(key)
            .join("worktree")
            .to_string_lossy()
            .into_owned()
    }

    /// Return an SSH command string for connecting to this sandbox, if
    /// supported.
    async fn ssh_access_command(&self) -> crate::Result<Option<String>> {
        Ok(None)
    }

    /// The display URL of the cloned origin remote, if known.
    fn origin_url(&self) -> Option<&str> {
        None
    }

    /// Get an authenticated preview URL for a port exposed by this sandbox.
    /// Returns `Ok(None)` when the sandbox does not support port previews.
    /// Used to connect to services (e.g. MCP servers) running inside the
    /// sandbox.
    async fn get_preview_url(
        &self,
        _port: u16,
    ) -> crate::Result<Option<(String, HashMap<String, String>)>> {
        Ok(None)
    }

    /// Record that the agent has explicitly read (seen) the given file path.
    /// Called by tool executors after agent-visible reads (e.g. `read_file`,
    /// `grep`). Default is a no-op; `ReadBeforeWriteSandbox` overrides to
    /// populate its read set.
    fn mark_agent_read(&self, _path: &str) {}
}

/// Resolve a path: relative paths are prepended with the working directory.
/// Used by the Daytona sandbox implementation.
#[cfg(any(feature = "docker", feature = "daytona"))]
pub(crate) fn resolve_path(path: &str, working_dir: &str) -> String {
    if std::path::Path::new(path).is_absolute() {
        path.to_string()
    } else {
        format!("{working_dir}/{path}")
    }
}

/// Shell-quote a string using `shlex::try_quote`, with a fallback for edge
/// cases.
pub fn shell_quote(s: &str) -> String {
    shlex::try_quote(s).map_or_else(
        |_| format!("'{}'", s.replace('\'', "'\\''")),
        |q| q.to_string(),
    )
}

/// Helper for sandbox implementations that manage git internally.
/// Executes git commands inside the sandbox to create a run branch.
pub async fn setup_git_via_exec(
    sandbox: &dyn Sandbox,
    intent: &GitSetupIntent,
) -> crate::Result<GitRunInfo> {
    // Get current branch name
    let branch_result = sandbox
        .exec_command("git rev-parse --abbrev-ref HEAD", 10_000, None, None, None)
        .await
        .map_err(|e| {
            crate::Error::message(format!("git rev-parse --abbrev-ref HEAD failed: {e}"))
        })?;
    let base_branch = if branch_result.exit_code == 0 {
        let name = branch_result.stdout.trim().to_string();
        if name.is_empty() || name == "HEAD" {
            None
        } else {
            Some(name)
        }
    } else {
        None
    };

    let (base_sha, branch_name) = match intent {
        GitSetupIntent::NewRun { run_id } => {
            let sha_result = sandbox
                .exec_command("git rev-parse HEAD", 10_000, None, None, None)
                .await
                .map_err(|e| crate::Error::message(format!("git rev-parse HEAD failed: {e}")))?;
            if sha_result.exit_code != 0 {
                return Err(crate::Error::message(format!(
                    "git rev-parse HEAD failed (exit {}): {}",
                    sha_result.exit_code, sha_result.stderr
                )));
            }
            (
                sha_result.stdout.trim().to_string(),
                format!("fabro/run/{run_id}"),
            )
        }
        GitSetupIntent::ForkFromCheckpoint {
            new_run_id,
            source_run_id,
            checkpoint_sha,
        } => {
            fetch_source_run_ref(sandbox, source_run_id, checkpoint_sha).await?;
            (checkpoint_sha.clone(), format!("fabro/run/{new_run_id}"))
        }
    };

    let checkout_cmd = format!(
        "git checkout -B {} {}",
        shell_quote(&branch_name),
        shell_quote(&base_sha)
    );
    let checkout_result = sandbox
        .exec_command(&checkout_cmd, 10_000, None, None, None)
        .await
        .map_err(|e| crate::Error::message(format!("git checkout failed: {e}")))?;
    if checkout_result.exit_code != 0 {
        return Err(crate::Error::message(format!(
            "git checkout -B failed (exit {}): {}",
            checkout_result.exit_code, checkout_result.stderr
        )));
    }

    Ok(GitRunInfo {
        base_sha,
        run_branch: branch_name,
        base_branch,
    })
}

pub(crate) async fn fetch_source_run_ref(
    sandbox: &dyn Sandbox,
    source_run_id: &str,
    checkpoint_sha: &str,
) -> crate::Result<()> {
    let remote_ref = format!("refs/heads/fabro/run/{source_run_id}");
    let tracking_ref = format!("refs/remotes/origin/fabro/run/{source_run_id}");
    let fetch_cmd = format!(
        "{GIT} fetch origin {}:{}",
        shell_quote(&remote_ref),
        shell_quote(&tracking_ref)
    );
    let check_cmd = format!(
        "{GIT} merge-base --is-ancestor {} {}",
        shell_quote(checkpoint_sha),
        shell_quote(&tracking_ref)
    );

    let mut last_error = String::new();
    for _ in 0..5 {
        let fetch = sandbox
            .exec_command(&fetch_cmd, 30_000, None, None, None)
            .await?;
        if fetch.exit_code != 0 {
            last_error = format!(
                "git fetch source run ref failed (exit {}): {}",
                fetch.exit_code,
                fetch.stderr.trim()
            );
        } else {
            let check = sandbox
                .exec_command(&check_cmd, 10_000, None, None, None)
                .await?;
            if check.exit_code == 0 {
                return Ok(());
            }
            last_error = format!(
                "checkpoint {checkpoint_sha} is not reachable from {remote_ref} (exit {}): {}",
                check.exit_code,
                check.stderr.trim()
            );
        }
        time::sleep(Duration::from_millis(500)).await;
    }

    Err(crate::Error::message(last_error))
}

/// Helper for sandbox implementations that manage git internally.
/// Pushes a refspec to origin via exec_command inside the sandbox.
pub async fn git_push_via_exec(sandbox: &dyn Sandbox, refspec: &str) -> bool {
    if let Err(e) = sandbox.refresh_push_credentials().await {
        tracing::warn!(
            refspec,
            error = %fabro_redact::redact_string(&e.to_string()),
            "Failed to refresh push credentials before git push"
        );
    }
    let cmd = format!("{GIT} push origin {}", shell_quote(refspec));
    match sandbox.exec_command(&cmd, 60_000, None, None, None).await {
        Ok(r) if r.exit_code == 0 => {
            tracing::info!(refspec, "Pushed git ref to origin");
            true
        }
        Ok(r) => {
            tracing::warn!(
                refspec,
                exit_code = r.exit_code,
                timed_out = r.timed_out,
                stderr = %trim_for_log(&r.stderr, GIT_LOG_TAIL_BYTES),
                stdout = %trim_for_log(&r.stdout, GIT_LOG_TAIL_BYTES),
                hint = classify_git_push_failure(&r.stderr).unwrap_or(""),
                "Failed to push git ref"
            );
            false
        }
        Err(e) => {
            tracing::warn!(
                refspec,
                error = %fabro_redact::redact_string(&e.to_string()),
                "Failed to invoke git push in sandbox"
            );
            false
        }
    }
}

/// Maximum bytes of git stdout/stderr to include in a single log line.
/// Long enough to capture the typical 1-3 line `fatal:` / `remote:` output
/// without flooding the log when git emits a large progress dump.
const GIT_LOG_TAIL_BYTES: usize = 2048;

/// Redact secrets from `text`, then keep at most the trailing `limit` bytes.
/// Trailing because git's relevant `fatal:` / `remote: rejected` lines are
/// emitted at the end of the output.
fn trim_for_log(text: &str, limit: usize) -> String {
    let redacted = fabro_redact::redact_string(text);
    let trimmed = redacted.trim_end();
    if trimmed.len() <= limit {
        return trimmed.to_string();
    }
    let start = trimmed.len() - limit;
    let safe_start = (start..=trimmed.len())
        .find(|i| trimmed.is_char_boundary(*i))
        .unwrap_or(trimmed.len());
    format!("…{}", &trimmed[safe_start..])
}

/// Map a git stderr to a short hint pointing at the likely cause. Returns
/// `None` when no known pattern matches; callers should still log the raw
/// (redacted) stderr so unknown failures stay debuggable.
fn classify_git_push_failure(stderr: &str) -> Option<&'static str> {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("could not read username") || lower.contains("terminal prompts disabled") {
        Some(
            "no credentials in origin URL — check that the sandbox forwarded \
             GITHUB_APP_PRIVATE_KEY (or GITHUB_TOKEN) and that refresh_push_credentials succeeded",
        )
    } else if lower.contains("permission to") && lower.contains("denied") {
        Some(
            "github denied the push — installation token lacks contents:write \
             on this repo, or a branch protection / push ruleset is rejecting the ref",
        )
    } else if lower.contains("protected branch")
        || lower.contains("ruleset")
        || lower.contains("rejected")
    {
        Some("github rejected the ref — likely a branch protection rule or push ruleset")
    } else if lower.contains("authentication failed") || lower.contains("invalid username") {
        Some("github authentication failed — installation token may be expired or wrong scope")
    } else if lower.contains("could not resolve host") || lower.contains("network is unreachable") {
        Some("network failure inside sandbox — check DNS / egress from the run container")
    } else if lower.contains("repository not found") {
        Some("github 404 — the App installation may not include this repo")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_result_fields() {
        let result = ExecResult {
            stdout:      "out".into(),
            stderr:      "err".into(),
            exit_code:   1,
            timed_out:   true,
            duration_ms: 5000,
        };
        assert_eq!(result.exit_code, 1);
        assert!(result.timed_out);
        assert_eq!(result.duration_ms, 5000);
    }

    #[test]
    fn trim_for_log_keeps_short_output_intact() {
        assert_eq!(trim_for_log("fatal: nope\n", 2048), "fatal: nope");
    }

    #[test]
    fn trim_for_log_keeps_trailing_bytes_when_oversized() {
        let prefix = "x".repeat(3000);
        let suffix = "fatal: rejected";
        let trimmed = trim_for_log(&format!("{prefix}{suffix}"), 64);
        assert!(trimmed.starts_with('…'));
        assert!(trimmed.ends_with(suffix));
        assert!(trimmed.chars().count() <= 64 + 1);
    }

    #[test]
    fn trim_for_log_redacts_high_entropy_secrets() {
        let stderr = "fatal: unable to access \
                      'https://x-access-token:ghs_xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA@github.com/owner/repo/'";
        let trimmed = trim_for_log(stderr, 2048);
        assert!(!trimmed.contains("ghs_xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA"));
        assert!(trimmed.contains("REDACTED"));
    }

    #[test]
    fn classify_git_push_failure_recognises_missing_credentials() {
        let hint = classify_git_push_failure(
            "fatal: could not read Username for 'https://github.com': No such device or address",
        );
        assert!(hint.unwrap().contains("no credentials in origin URL"));
    }

    #[test]
    fn classify_git_push_failure_recognises_permission_denied() {
        let hint = classify_git_push_failure(
            "remote: Permission to owner/repo.git denied to fabro-app[bot].",
        );
        assert!(hint.unwrap().contains("github denied the push"));
    }

    #[test]
    fn classify_git_push_failure_recognises_branch_protection() {
        let hint = classify_git_push_failure(
            "remote: error: GH013: Repository rule violations found for refs/heads/main\n\
             remote: - Cannot create ref 'refs/heads/fabro/run/X' due to ruleset",
        );
        assert!(hint.unwrap().contains("ruleset"));
    }

    #[test]
    fn classify_git_push_failure_returns_none_for_unknown() {
        assert!(classify_git_push_failure("fatal: weird new git error message").is_none());
    }

    #[test]
    fn dir_entry_fields() {
        let entry = DirEntry {
            name:   "src".into(),
            is_dir: true,
            size:   None,
        };
        assert_eq!(entry.name, "src");
        assert!(entry.is_dir);
        assert!(entry.size.is_none());
    }

    #[test]
    fn grep_options_defaults() {
        let opts = GrepOptions::default();
        assert!(opts.glob_filter.is_none());
        assert!(!opts.case_insensitive);
        assert!(opts.max_results.is_none());
    }

    #[test]
    fn sandbox_event_serialization_round_trip() {
        let events = vec![
            SandboxEvent::Initializing {
                provider: "local".into(),
            },
            SandboxEvent::Ready {
                provider:    "local".into(),
                duration_ms: 50,
                name:        None,
                cpu:         None,
                memory:      None,
                url:         None,
            },
            SandboxEvent::InitializeFailed {
                provider:    "docker".into(),
                error:       "no daemon".into(),
                causes:      vec!["connection refused".into()],
                duration_ms: 100,
            },
            SandboxEvent::CleanupStarted {
                provider: "daytona".into(),
            },
            SandboxEvent::CleanupCompleted {
                provider:    "daytona".into(),
                duration_ms: 200,
            },
            SandboxEvent::CleanupFailed {
                provider: "docker".into(),
                error:    "container gone".into(),
                causes:   Vec::new(),
            },
            SandboxEvent::SnapshotPulling {
                name: "ubuntu:22.04".into(),
            },
            SandboxEvent::SnapshotPulled {
                name:        "ubuntu:22.04".into(),
                duration_ms: 5000,
            },
            SandboxEvent::SnapshotEnsuring {
                name: "my-snap".into(),
            },
            SandboxEvent::SnapshotCreating {
                name: "my-snap".into(),
            },
            SandboxEvent::SnapshotReady {
                name:        "my-snap".into(),
                duration_ms: 30000,
            },
            SandboxEvent::SnapshotFailed {
                name:   "my-snap".into(),
                error:  "build failed".into(),
                causes: Vec::new(),
            },
            SandboxEvent::GitCloneStarted {
                url:    "https://github.com/org/repo.git".into(),
                branch: Some("main".into()),
            },
            SandboxEvent::GitCloneCompleted {
                url:         "https://github.com/org/repo.git".into(),
                duration_ms: 8000,
            },
            SandboxEvent::GitCloneFailed {
                url:    "https://github.com/org/repo.git".into(),
                error:  "auth failed".into(),
                causes: Vec::new(),
            },
        ];

        assert_eq!(events.len(), 15, "should test all 15 variants");

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let deserialized: SandboxEvent = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn sandbox_event_callback_type_compiles() {
        let cb: SandboxEventCallback = Arc::new(|_event| {});
        cb(SandboxEvent::Initializing {
            provider: "test".into(),
        });
    }

    #[test]
    fn format_lines_numbered_basic() {
        let result = format_lines_numbered("hello\nworld\nfoo", None, None);
        assert_eq!(result, "1 | hello\n2 | world\n3 | foo\n");
    }

    #[test]
    fn format_lines_numbered_with_offset_limit() {
        let result = format_lines_numbered("a\nb\nc\nd\ne", Some(1), Some(2));
        assert!(result.contains("2 | b"));
        assert!(result.contains("3 | c"));
        assert!(!result.contains("1 | a"));
        assert!(!result.contains("4 | d"));
    }

    #[test]
    fn shell_quote_basic() {
        assert_eq!(shell_quote("hello"), "hello");
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }
}
