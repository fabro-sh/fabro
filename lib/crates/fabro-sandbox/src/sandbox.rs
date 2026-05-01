use std::collections::HashMap;
use std::fmt::Write;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fabro_types::{CommandOutputStream, CommandTermination};
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

            async fn exec_command_streaming(
                &self,
                command: &str,
                timeout_ms: u64,
                working_dir: Option<&str>,
                env_vars: Option<&std::collections::HashMap<String, String>>,
                cancel_token: Option<tokio_util::sync::CancellationToken>,
                output_callback: $crate::CommandOutputCallback,
            ) -> $crate::Result<$crate::ExecStreamingResult> {
                self.$field
                    .exec_command_streaming(
                        command,
                        timeout_ms,
                        working_dir,
                        env_vars,
                        cancel_token,
                        output_callback,
                    )
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

            async fn git_push_ref(&self, refspec: &str) -> $crate::Result<()> {
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
                info!(provider, "Sandbox cleanup started");
            }
            Self::CleanupCompleted {
                provider,
                duration_ms,
            } => {
                info!(provider, duration_ms, "Sandbox cleanup completed");
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
    pub exit_code:   Option<i32>,
    pub termination: CommandTermination,
    pub duration_ms: u64,
}

impl ExecResult {
    pub fn is_success(&self) -> bool {
        self.exit_code == Some(0) && self.termination == CommandTermination::Exited
    }

    pub fn is_timed_out(&self) -> bool {
        self.termination == CommandTermination::TimedOut
    }

    pub fn is_cancelled(&self) -> bool {
        self.termination == CommandTermination::Cancelled
    }

    pub fn display_exit_code(&self) -> i32 {
        self.exit_code.unwrap_or(-1)
    }

    pub fn into_exec_error(self, label: impl Into<String>) -> crate::Error {
        crate::Error::exec(
            label,
            self.exit_code,
            self.termination,
            self.duration_ms,
            self.stderr,
            self.stdout,
        )
    }

    pub fn into_exec_error_with_redactor(
        self,
        label: impl Into<String>,
        redactor: impl Fn(&str) -> String,
    ) -> crate::Error {
        let stderr = redactor(&self.stderr);
        let stdout = redactor(&self.stdout);
        crate::Error::exec(
            label,
            self.exit_code,
            self.termination,
            self.duration_ms,
            stderr,
            stdout,
        )
    }

    pub fn into_result(self, label: impl Into<String>) -> crate::Result<Self> {
        if self.is_success() {
            Ok(self)
        } else {
            Err(self.into_exec_error(label))
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecStreamingResult {
    pub result:            ExecResult,
    pub streams_separated: bool,
    pub live_streaming:    bool,
}

pub type CommandOutputCallback = Arc<
    dyn Fn(CommandOutputStream, Vec<u8>) -> Pin<Box<dyn Future<Output = crate::Result<()>> + Send>>
        + Send
        + Sync,
>;

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
    /// Stream a command's output as it runs.
    ///
    /// **Production sandboxes must override this.** The default falls back to
    /// the non-streaming [`exec_command`](Self::exec_command) and replays its
    /// output through `output_callback` at the end, marking
    /// `live_streaming: false`. That's the right behavior for test mocks but
    /// silently drops live output for any real sandbox that wraps another —
    /// decorators in particular must forward to the inner sandbox's streaming
    /// implementation rather than relying on this default.
    async fn exec_command_streaming(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&std::collections::HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
        output_callback: CommandOutputCallback,
    ) -> crate::Result<ExecStreamingResult> {
        let result = self
            .exec_command(command, timeout_ms, working_dir, env_vars, cancel_token)
            .await?;
        if !result.stdout.is_empty() {
            output_callback(
                CommandOutputStream::Stdout,
                result.stdout.as_bytes().to_vec(),
            )
            .await?;
        }
        if !result.stderr.is_empty() {
            output_callback(
                CommandOutputStream::Stderr,
                result.stderr.as_bytes().to_vec(),
            )
            .await?;
        }
        Ok(ExecStreamingResult {
            result,
            streams_separated: true,
            live_streaming: false,
        })
    }
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
    async fn git_push_ref(&self, _refspec: &str) -> crate::Result<()> {
        Err(crate::Error::message(
            "git_push_ref not implemented for this sandbox",
        ))
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
    let base_branch = if branch_result.is_success() {
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
                .map_err(|e| crate::Error::context("git rev-parse HEAD", e))?
                .into_result("git rev-parse HEAD")?;
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
    sandbox
        .exec_command(&checkout_cmd, 10_000, None, None, None)
        .await
        .map_err(|e| crate::Error::context("git checkout -B", e))?
        .into_result("git checkout -B")?;

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
        if fetch.is_success() {
            let check = sandbox
                .exec_command(&check_cmd, 10_000, None, None, None)
                .await?;
            if check.is_success() {
                return Ok(());
            }
            last_error = check
                .into_exec_error(format!(
                    "checkpoint {checkpoint_sha} is not reachable from {remote_ref}"
                ))
                .to_string();
        } else {
            last_error = fetch
                .into_exec_error("git fetch source run ref")
                .to_string();
        }
        time::sleep(Duration::from_millis(500)).await;
    }

    Err(crate::Error::message(last_error))
}

/// Helper for sandbox implementations that manage git internally.
/// Pushes a refspec to origin via exec_command inside the sandbox.
pub async fn git_push_via_exec(sandbox: &dyn Sandbox, refspec: &str) -> crate::Result<()> {
    if let Err(e) = sandbox.refresh_push_credentials().await {
        tracing::warn!(
            refspec = %refspec,
            error = %e,
            "Failed to refresh push credentials before git push"
        );
    }
    let cmd = format!("{GIT} push origin {}", shell_quote(refspec));
    let label = format!("git push origin {refspec}");
    sandbox
        .exec_command(&cmd, 60_000, None, None, None)
        .await
        .map_err(|e| crate::Error::context(label.clone(), e))?
        .into_result(&label)?;
    tracing::info!(refspec = %refspec, "Pushed git ref to origin");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_result_fields() {
        let result = ExecResult {
            stdout:      "out".into(),
            stderr:      "err".into(),
            exit_code:   Some(1),
            termination: CommandTermination::Exited,
            duration_ms: 5000,
        };
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.termination, CommandTermination::Exited);
        assert_eq!(result.duration_ms, 5000);
    }

    #[test]
    fn exec_result_helpers_convert_failure_to_exec_error() {
        let result = ExecResult {
            stdout:      "out".into(),
            stderr:      "fatal: could not read Username".into(),
            exit_code:   Some(128),
            termination: CommandTermination::Exited,
            duration_ms: 42,
        };
        let error = result.into_result("git push").unwrap_err();
        let crate::Error::Exec {
            label, exit_code, ..
        } = &error
        else {
            panic!("expected Error::Exec, got {error:?}");
        };
        assert_eq!(label, "git push");
        assert_eq!(*exit_code, Some(128));
        assert!(error.to_string().contains("no credentials in origin URL"));
    }

    #[test]
    fn exec_result_success_honors_timeouts() {
        let success = ExecResult {
            stdout:      String::new(),
            stderr:      String::new(),
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 1,
        };
        assert!(success.is_success());

        let timeout = ExecResult {
            exit_code: None,
            termination: CommandTermination::TimedOut,
            ..success
        };
        assert!(!timeout.is_success());
    }

    #[test]
    fn exec_result_redactor_applies_to_stderr_and_stdout() {
        let result = ExecResult {
            stdout:      "stdout https://token@example.com".into(),
            stderr:      "stderr https://token@example.com".into(),
            exit_code:   Some(1),
            termination: CommandTermination::Exited,
            duration_ms: 1,
        };
        let error = result.into_exec_error_with_redactor("git set-url", |s| {
            s.replace("https://token@example.com", "https://****@example.com")
        });

        let crate::Error::Exec { stderr, stdout, .. } = &error else {
            panic!("expected Error::Exec, got {error:?}");
        };
        assert_eq!(stderr, "stderr https://****@example.com");
        assert_eq!(stdout, "stdout https://****@example.com");
    }

    #[test]
    fn sandbox_tracing_events_do_not_log_raw_command_fields() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut failures = Vec::new();
        scan_for_command_tracing(&root, &mut failures);
        assert!(
            failures.is_empty(),
            "raw command/cmd tracing fields found:\n{}",
            failures.join("\n")
        );
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

    #[expect(
        clippy::disallowed_methods,
        reason = "unit test performs a small synchronous source scan of local Rust files"
    )]
    fn scan_for_command_tracing(path: &std::path::Path, failures: &mut Vec<String>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                scan_for_command_tracing(&path, failures);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for macro_name in [
                "tracing::trace!",
                "tracing::debug!",
                "tracing::info!",
                "tracing::warn!",
                "tracing::error!",
                "trace!",
                "debug!",
                "info!",
                "warn!",
                "error!",
            ] {
                let mut rest = source.as_str();
                while let Some(idx) = rest.find(macro_name) {
                    let start = source.len() - rest.len() + idx;
                    if start > 0 && source.as_bytes()[start - 1] == b'"' {
                        rest = &source[start + macro_name.len()..];
                        continue;
                    }
                    let Some(call) = tracing_call(&source[start..]) else {
                        break;
                    };
                    if call.contains("command,")
                        || call.contains("command =")
                        || call.contains("cmd,")
                        || call.contains("cmd =")
                    {
                        failures.push(format!(
                            "{}: {}",
                            path.display(),
                            call.lines().next().unwrap_or(call)
                        ));
                    }
                    rest = &source[start + call.len()..];
                }
            }
        }
    }

    fn tracing_call(source: &str) -> Option<&str> {
        let open = source.find('(')?;
        let mut depth = 0usize;
        for (idx, ch) in source.char_indices().skip(open) {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(&source[..=idx]);
                    }
                }
                _ => {}
            }
        }
        None
    }
}
