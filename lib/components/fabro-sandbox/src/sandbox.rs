use std::collections::HashMap;
use std::fmt::Write;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fabro_github::token_source::{InstallationTokenSource, TokenSnapshot};
pub use fabro_types::run_event::GitCredentialAction as RemoteCredentialAction;
use fabro_types::{CommandOutputStream, CommandTermination};
use fabro_util::shell;
use fabro_util::workspace_glob::WorkspaceGlob;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::git_retry::{self, CredentialContext, GitRetryReason, RetryPlan};
use crate::push_credentials::{CredentialLease, PushCredentialState, RefreshErrorKind};

/// Git command prefix that disables background maintenance.
pub(crate) const GIT: &str = "git -c maintenance.auto=0 -c gc.auto=0";

pub const DEFAULT_EXEC_OUTPUT_TAIL_BYTES: usize = 8 * 1024;

/// Maximum time a sandbox lifecycle check may spend proving Bash is usable.
pub(crate) const BASH_PROBE_TIMEOUT_MS: u64 = 10_000;

/// Bash path required by Linux-backed remote sandbox providers.
#[cfg(any(feature = "docker", feature = "daytona"))]
pub(crate) const REMOTE_BASH: &str = "/bin/bash";

/// Timeout for provider-neutral remote file traversal.
#[cfg(any(feature = "docker", feature = "daytona"))]
pub(crate) const REMOTE_WALK_TIMEOUT_MS: u64 = 30_000;

/// Environment variable Bash consults for non-interactive startup source.
///
/// Sandbox providers must remove or blank this before invoking `bash -c`;
/// otherwise ambient worker or image configuration can execute code before the
/// requested command.
pub(crate) const BASH_ENV_VAR: &str = "BASH_ENV";

/// Marker a successful [`BASH_PROBE_SCRIPT`] run prints on stdout.
///
/// Providers validate the marker rather than trusting a zero exit: a non-Bash
/// shell can exit zero for simple scripts without satisfying the contract.
pub(crate) const BASH_PROBE_MARKER: &str = "fabro-bash-ready";

/// Deterministic probe proving a sandbox's interpreter is non-login Bash.
///
/// Run as the argument to `bash -c` during fresh initialization and on
/// resume/start, before the sandbox is reported usable. It fails when the
/// interpreter has an ambient `BASH_ENV` startup source, is not Bash, was
/// started as a login shell, or is in POSIX mode. Bash invoked under the name
/// `sh` still sets `BASH_VERSION` while enabling POSIX behavior, so the full
/// interpreter contract is checked rather than assumed.
pub(crate) const BASH_PROBE_SCRIPT: &str = r#"if [ -n "${BASH_ENV:-}" ]; then
  echo 'sandbox interpreter has BASH_ENV startup source configured' >&2
  exit 1
fi
if [ -z "${BASH_VERSION:-}" ]; then
  echo 'sandbox interpreter is not bash' >&2
  exit 1
fi
if shopt -q login_shell; then
  echo 'sandbox interpreter is a login shell' >&2
  exit 1
fi
if shopt -qo posix; then
  echo 'sandbox interpreter is bash in posix mode' >&2
  exit 1
fi
printf '%s\n' 'fabro-bash-ready'"#;

/// Whether a [`BASH_PROBE_SCRIPT`] run succeeded.
///
/// A zero exit without exactly the marker is not a successful probe.
pub(crate) fn bash_probe_passed(exit_code: Option<i32>, stdout: &str) -> bool {
    exit_code == Some(0) && stdout.trim() == BASH_PROBE_MARKER
}

/// Validate a completed Bash probe without flattening its raw output into an
/// error message.
///
/// [`Error::Exec`](crate::Error::Exec) retains stdout/stderr for the existing
/// redacted-tail diagnostics while its display form exposes only bounded,
/// classified metadata safe for lifecycle events and tracing.
pub(crate) fn validate_bash_probe(
    result: ExecResult,
    remediation: impl Into<String>,
) -> crate::Result<()> {
    if result.is_success() && bash_probe_passed(result.exit_code, &result.stdout) {
        return Ok(());
    }

    Err(crate::Error::context(
        remediation,
        result.into_exec_error("Sandbox Bash probe"),
    ))
}

/// Sleep for `timeout_ms` if `Some`, otherwise never resolves. Used by
/// streaming `exec_command` impls to model "no timeout" without scheduling a
/// `Duration::from_millis(u64::MAX)` sleep.
pub(crate) async fn optional_timeout(timeout_ms: Option<u64>) {
    match timeout_ms {
        Some(ms) => time::sleep(Duration::from_millis(ms)).await,
        None => std::future::pending::<()>().await,
    }
}

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
///         async fn read_file_bytes(&self, path: &str) -> $crate::Result<Vec<u8>> {
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
                request: $crate::ExecStreamingRequest<'_>,
            ) -> $crate::Result<$crate::ExecStreamingResult> {
                self.$field.exec_command_streaming(request).await
            }

            async fn spawn_stdio_process(
                &self,
                command: &str,
                working_dir: Option<&str>,
                env_vars: Option<&std::collections::HashMap<String, String>>,
                cancel_token: Option<tokio_util::sync::CancellationToken>,
            ) -> $crate::Result<$crate::StdioProcess> {
                self.$field
                    .spawn_stdio_process(command, working_dir, env_vars, cancel_token)
                    .await
            }

            async fn glob(&self, pattern: &str, path: Option<&str>) -> $crate::Result<Vec<String>> {
                self.$field.glob(pattern, path).await
            }

            async fn walk_files(
                &self,
                base: &str,
                relative_start: &str,
                options: &$crate::WalkOptions,
            ) -> $crate::Result<Vec<$crate::SandboxFile>> {
                self.$field
                    .walk_files(base, relative_start, options)
                    .await
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

            async fn activate(&self) -> $crate::Result<$crate::SandboxActivation> {
                self.$field.activate().await
            }

            async fn start(&self) -> $crate::Result<()> {
                self.$field.start().await
            }

            async fn stop(&self) -> $crate::Result<()> {
                self.$field.stop().await
            }

            async fn delete(&self) -> $crate::Result<()> {
                self.$field.delete().await
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

            fn snapshot_info(&self) -> Option<String> {
                self.$field.snapshot_info()
            }

            async fn refresh_push_credentials(&self) -> $crate::Result<$crate::RefreshOutcome> {
                self.$field.refresh_push_credentials().await
            }

            fn push_token_source(
                &self,
            ) -> Option<std::sync::Arc<$crate::InstallationTokenSource>> {
                self.$field.push_token_source()
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

            async fn git_push_ref(
                &self,
                refspec: &str,
                plan: &$crate::RetryPlan,
            ) -> Result<$crate::PushReport, $crate::PushError> {
                self.$field.git_push_ref(refspec, plan).await
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

            async fn read_file_bytes(&self, path: &str) -> $crate::Result<Vec<u8>> {
                self.$field.read_file_bytes(path).await
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
    StartStarted {
        provider: String,
    },
    StartCompleted {
        provider:    String,
        duration_ms: u64,
    },
    StartFailed {
        provider: String,
        error:    String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        causes:   Vec<String>,
    },
    StopStarted {
        provider: String,
    },
    StopCompleted {
        provider:    String,
        duration_ms: u64,
    },
    StopFailed {
        provider: String,
        error:    String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        causes:   Vec<String>,
    },
    DeleteStarted {
        provider: String,
    },
    DeleteCompleted {
        provider:    String,
        duration_ms: u64,
    },
    DeleteFailed {
        provider: String,
        error:    String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        causes:   Vec<String>,
    },

    // -- Snapshot lifecycle --
    SnapshotPulling {
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
            Self::StartStarted { provider } => {
                info!(provider, "Sandbox start started");
            }
            Self::StartCompleted {
                provider,
                duration_ms,
            } => {
                info!(provider, duration_ms, "Sandbox start completed");
            }
            Self::StartFailed {
                provider,
                error,
                causes,
            } => {
                warn!(provider, error, causes = ?causes, "Sandbox start failed");
            }
            Self::StopStarted { provider } => {
                info!(provider, "Sandbox stop started");
            }
            Self::StopCompleted {
                provider,
                duration_ms,
            } => {
                info!(provider, duration_ms, "Sandbox stop completed");
            }
            Self::StopFailed {
                provider,
                error,
                causes,
            } => {
                warn!(provider, error, causes = ?causes, "Sandbox stop failed");
            }
            Self::DeleteStarted { provider } => {
                info!(provider, "Sandbox delete started");
            }
            Self::DeleteCompleted {
                provider,
                duration_ms,
            } => {
                info!(provider, duration_ms, "Sandbox delete completed");
            }
            Self::DeleteFailed {
                provider,
                error,
                causes,
            } => {
                warn!(provider, error, causes = ?causes, "Sandbox delete failed");
            }
            Self::SnapshotPulling { name } => {
                debug!(name, "Snapshot pulling");
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
/// Applies optional offset (1-based starting line number) and limit (max lines
/// to return). Line numbers are 1-based and right-aligned.
#[must_use]
pub fn format_lines_numbered(content: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    let all_lines: Vec<&str> = content.lines().collect();
    let skip = offset.unwrap_or(1).saturating_sub(1);
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
        crate::Error::exec(label, self)
    }

    pub fn into_exec_error_with_redactor(
        self,
        label: impl Into<String>,
        redactor: impl Fn(&str) -> String,
    ) -> crate::Error {
        crate::Error::exec(label, Self {
            stdout: redactor(&self.stdout),
            stderr: redactor(&self.stderr),
            ..self
        })
    }

    pub fn into_result(self, label: impl Into<String>) -> crate::Result<Self> {
        if self.is_success() {
            Ok(self)
        } else {
            Err(self.into_exec_error(label))
        }
    }

    pub fn redacted_output_tail(
        &self,
        max_bytes_per_stream: usize,
    ) -> Option<fabro_types::ExecOutputTail> {
        redacted_output_tail(&self.stdout, &self.stderr, max_bytes_per_stream)
    }

    pub fn default_redacted_output_tail(&self) -> Option<fabro_types::ExecOutputTail> {
        self.redacted_output_tail(DEFAULT_EXEC_OUTPUT_TAIL_BYTES)
    }

    /// Converts host process output into the canonical full exec result.
    ///
    /// This stores raw stdout/stderr. Callers must not log these fields
    /// directly; use `default_redacted_output_tail()` for events and
    /// `display_for_log()` for tracing.
    #[cfg(test)]
    pub fn from_process_output(output: std::process::Output, duration_ms: u64) -> Self {
        let std::process::Output {
            status,
            stdout,
            stderr,
        } = output;
        Self {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            exit_code: Some(status.code().unwrap_or(-1)),
            termination: CommandTermination::Exited,
            duration_ms,
        }
    }
}

/// Build a redacted `ExecOutputTail` from raw stdout/stderr without
/// fabricating a synthetic `ExecResult`. Pass `""` for either stream that
/// isn't relevant. Returns `None` when both streams are empty.
#[must_use]
pub fn redacted_output_tail(
    stdout: &str,
    stderr: &str,
    max_bytes_per_stream: usize,
) -> Option<fabro_types::ExecOutputTail> {
    let (stdout, stdout_truncated) = redacted_tail(stdout, max_bytes_per_stream);
    let (stderr, stderr_truncated) = redacted_tail(stderr, max_bytes_per_stream);
    let tail = fabro_types::ExecOutputTail {
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    };
    (!tail.is_empty()).then_some(tail)
}

fn redacted_tail(text: &str, max_bytes: usize) -> (Option<String>, bool) {
    if text.is_empty() || max_bytes == 0 {
        return (None, !text.is_empty());
    }

    let redacted = fabro_redact::redact_string(text);
    let sanitized = sanitize_exec_output(&redacted);
    let truncated = sanitized.len() > max_bytes;
    let start = if truncated {
        sanitized.floor_char_boundary(sanitized.len() - max_bytes)
    } else {
        0
    };
    let tail = sanitized[start..].to_string();
    ((!tail.is_empty()).then_some(tail), truncated)
}

fn sanitize_exec_output(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    let mut saw_esc = false;
                    for next in chars.by_ref() {
                        if next == '\u{7}' || (saw_esc && next == '\\') {
                            break;
                        }
                        saw_esc = next == '\u{1b}';
                    }
                }
                Some('(' | ')' | '*' | '+' | '-' | '.' | '/') => {
                    chars.next();
                    chars.next();
                }
                Some('@'..='_') => {
                    chars.next();
                }
                _ => {}
            }
            continue;
        }
        if ch == '\n' || ch == '\r' || ch == '\t' || !ch.is_control() {
            sanitized.push(ch);
        }
    }
    sanitized
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

/// Inputs for a streaming command execution.
///
/// Construct with a struct literal over [`ExecStreamingRequest::new`]:
/// `ExecStreamingRequest { stdin, ..ExecStreamingRequest::new(command) }`.
/// Providers should destructure exhaustively so a new field is a compile
/// error rather than silently ignored input.
///
/// Standard input is owned so providers can move it into a writer task. This
/// type does not implement `Debug` because standard input can contain
/// sensitive workflow data.
pub struct ExecStreamingRequest<'a> {
    pub command:         &'a str,
    pub timeout_ms:      Option<u64>,
    pub working_dir:     Option<&'a str>,
    pub env_vars:        Option<&'a HashMap<String, String>>,
    pub cancel_token:    Option<CancellationToken>,
    pub stdin:           Option<Vec<u8>>,
    pub output_callback: Option<CommandOutputCallback>,
}

impl<'a> ExecStreamingRequest<'a> {
    #[must_use]
    pub fn new(command: &'a str) -> Self {
        Self {
            command,
            timeout_ms: None,
            working_dir: None,
            env_vars: None,
            cancel_token: None,
            stdin: None,
            output_callback: None,
        }
    }
}

pub(crate) async fn write_process_stdin<W>(mut writer: W, stdin: &[u8]) -> crate::Result<()>
where
    W: AsyncWrite + Unpin,
{
    // A command that stops reading its input (`head -1`, an early exit) is
    // not an error; its exit code is the authoritative result. Local pipes
    // surface that as `BrokenPipe`, remote transports (a TCP Docker daemon)
    // as `ConnectionReset`/`ConnectionAborted`.
    fn command_stopped_reading(err: &std::io::Error) -> bool {
        matches!(
            err.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
        )
    }

    if let Err(err) = writer.write_all(stdin).await {
        if !command_stopped_reading(&err) {
            return Err(crate::Error::context(
                "Failed to write command standard input",
                err,
            ));
        }
    }
    if let Err(err) = writer.shutdown().await {
        if !command_stopped_reading(&err) {
            return Err(crate::Error::context(
                "Failed to close command standard input",
                err,
            ));
        }
    }
    Ok(())
}

pub(crate) async fn replay_exec_result(
    result: ExecResult,
    streams_separated: bool,
    output_callback: Option<&CommandOutputCallback>,
) -> crate::Result<ExecStreamingResult> {
    if let Some(output_callback) = output_callback {
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
    }
    Ok(ExecStreamingResult {
        result,
        streams_separated,
        live_streaming: false,
    })
}

pub struct StdioProcess {
    pub stdin:  Pin<Box<dyn AsyncWrite + Send>>,
    pub stdout: Pin<Box<dyn AsyncRead + Send>>,
    pub stderr: StderrCollector,
    pub handle: StdioProcessHandle,
}

#[derive(Debug, Clone)]
pub struct StderrCollector {
    inner:     Arc<TokioMutex<Vec<u8>>>,
    max_bytes: usize,
}

impl StderrCollector {
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(TokioMutex::new(Vec::new())),
            max_bytes,
        }
    }

    pub async fn push(&self, bytes: &[u8]) {
        let mut tail = self.inner.lock().await;
        tail.extend_from_slice(bytes);
        if tail.len() > self.max_bytes {
            let excess = tail.len() - self.max_bytes;
            tail.drain(..excess);
        }
    }

    pub async fn tail_string(&self) -> String {
        let tail = self.inner.lock().await;
        String::from_utf8_lossy(&tail).into_owned()
    }

    pub fn spawn_reader<R>(&self, mut reader: R) -> JoinHandle<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let collector = self.clone();
        tokio::spawn(async move {
            let mut buf = [0_u8; 8192];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => return,
                    Ok(read) => collector.push(&buf[..read]).await,
                    Err(err) => {
                        tracing::warn!(error = %err, "Failed to read stdio process stderr");
                        return;
                    }
                }
            }
        })
    }
}

#[derive(Clone)]
pub struct StdioProcessHandle {
    control: Arc<dyn StdioProcessControl>,
}

impl StdioProcessHandle {
    pub(crate) fn new(control: impl StdioProcessControl + 'static) -> Self {
        Self {
            control: Arc::new(control),
        }
    }

    pub async fn terminate(&self) -> crate::Result<()> {
        self.control.terminate().await
    }

    pub async fn wait(&self) -> crate::Result<StdioProcessTermination> {
        self.control.wait().await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdioProcessTermination {
    pub termination: CommandTermination,
    pub exit_code:   Option<i32>,
}

impl StdioProcessTermination {
    #[must_use]
    pub fn exited(exit_code: Option<i32>) -> Self {
        Self {
            termination: CommandTermination::Exited,
            exit_code,
        }
    }

    #[must_use]
    pub fn cancelled() -> Self {
        Self {
            termination: CommandTermination::Cancelled,
            exit_code:   None,
        }
    }
}

#[async_trait]
pub(crate) trait StdioProcessControl: Send + Sync {
    async fn terminate(&self) -> crate::Result<()>;
    async fn wait(&self) -> crate::Result<StdioProcessTermination>;
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name:   String,
    pub is_dir: bool,
    pub size:   Option<u64>,
}

/// A regular file discovered inside a sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxFile {
    /// Provider-resolved path accepted by sandbox filesystem operations.
    pub path:          String,
    /// `/`-separated path relative to the requested traversal base.
    pub relative_path: String,
    pub size:          u64,
}

/// Provider-neutral controls for recursive file traversal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkOptions {
    /// Directory basenames that providers must prune at every depth.
    pub excluded_directory_names: Vec<String>,
}

impl WalkOptions {
    #[must_use]
    pub fn excludes_name(&self, name: &str) -> bool {
        self.excluded_directory_names
            .iter()
            .any(|excluded| excluded == name)
    }

    #[must_use]
    pub fn excludes_relative_path(&self, relative_path: &str) -> bool {
        relative_path
            .split('/')
            .any(|segment| self.excludes_name(segment))
    }
}

#[derive(Debug, Clone, Default)]
pub struct GrepOptions {
    pub glob_filter:      Option<String>,
    pub case_insensitive: bool,
    pub max_results:      Option<usize>,
}

/// Outcome of [`Sandbox::refresh_push_credentials`]: what this call did to the
/// remote, and the non-secret description of the token embedded in it.
/// `token` is `None` only when `action` is [`RemoteCredentialAction::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// No managed credentials exist for this sandbox.
    None,
    /// The remote already carried this token generation.
    Unchanged(TokenSnapshot),
    /// The remote was updated to carry this token generation.
    Embedded(TokenSnapshot),
}

impl RefreshOutcome {
    /// No managed credentials to refresh.
    #[must_use]
    pub const fn none() -> Self {
        Self::None
    }

    #[must_use]
    pub const fn unchanged(token: TokenSnapshot) -> Self {
        Self::Unchanged(token)
    }

    #[must_use]
    pub const fn embedded(token: TokenSnapshot) -> Self {
        Self::Embedded(token)
    }

    #[must_use]
    pub const fn action(self) -> RemoteCredentialAction {
        match self {
            Self::None => RemoteCredentialAction::None,
            Self::Unchanged(_) => RemoteCredentialAction::Unchanged,
            Self::Embedded(_) => RemoteCredentialAction::Embedded,
        }
    }

    #[must_use]
    pub const fn token(self) -> Option<TokenSnapshot> {
        match self {
            Self::None => None,
            Self::Unchanged(token) | Self::Embedded(token) => Some(token),
        }
    }
}

/// Outcome of [`Sandbox::activate`], telling callers whether the call
/// changed the sandbox lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxActivation {
    /// The sandbox was already running (or only needed unpausing); the caller
    /// is not responsible for stopping it afterwards.
    AlreadyActive,
    /// The sandbox transitioned from stopped to running because of this call.
    /// Read-only inspections that borrow the sandbox transiently should stop
    /// it again when they are finished, or it stays running forever.
    Started,
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn read_file_bytes(&self, path: &str) -> crate::Result<Vec<u8>>;

    async fn read_file_text(&self, path: &str) -> crate::Result<String> {
        String::from_utf8(self.read_file_bytes(path).await?)
            .map_err(|err| crate::Error::context("File is not valid UTF-8", err))
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> crate::Result<String> {
        Ok(format_lines_numbered(
            &self.read_file_text(path).await?,
            offset,
            limit,
        ))
    }

    async fn write_file(&self, path: &str, content: &str) -> crate::Result<()>;

    /// Write a file that the caller has already confirmed exists.
    ///
    /// Providers can override this method to skip setup that is only needed
    /// when creating a new path. The default preserves the behavior of
    /// [`Sandbox::write_file`].
    async fn write_existing_file(&self, path: &str, content: &str) -> crate::Result<()> {
        self.write_file(path, content).await
    }

    async fn delete_file(&self, path: &str) -> crate::Result<()>;
    async fn file_exists(&self, path: &str) -> crate::Result<bool>;
    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> crate::Result<Vec<DirEntry>>;
    /// Run `command` to completion and return its captured output.
    ///
    /// On Unix production sandboxes `command` is **Bash source**: it is
    /// evaluated as a non-login Bash program, equivalent to `bash -c
    /// <command>`. Implementations select the interpreter, not its options —
    /// they must not add login mode, `errexit`, `pipefail`, or any other
    /// implicit shell option, and must never fall back to `sh` or delegate
    /// evaluation to a provider's ambient shell. A caller that wants different
    /// semantics writes them into the command itself (`sh -c ...`, a
    /// `#!/bin/sh` script, an explicit `set -o pipefail`), which then runs
    /// beneath this Bash boundary.
    ///
    /// Providers resolve the Bash executable differently: the local sandbox
    /// resolves `bash` through the worker's `PATH` (NixOS has no `/bin/bash`),
    /// while the Linux remote providers require `/bin/bash`.
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
    /// `command` carries exactly the same interpreter semantics as
    /// [`exec_command`](Self::exec_command) — the two paths must not differ in
    /// interpreter or shell options, so Bash-only syntax behaves identically
    /// through both.
    ///
    /// When `request.stdin` is set, providers must write those exact bytes to
    /// the process's standard input and then close it to deliver EOF. The bytes
    /// must remain separate from command source and diagnostics.
    ///
    /// **Production sandboxes must override this.** The default falls back to
    /// the non-streaming [`exec_command`](Self::exec_command) and replays its
    /// output through `output_callback` at the end when one is supplied,
    /// marking `live_streaming: false`. Passing `None` captures the final
    /// result without paying per-chunk callback costs. That's the right
    /// behavior for test mocks but silently drops live output for any real
    /// sandbox that wraps another — decorators in particular must forward to
    /// the inner sandbox's streaming implementation rather than relying on
    /// this default. The fallback rejects `request.stdin` because
    /// [`exec_command`](Self::exec_command) has no stdin channel.
    async fn exec_command_streaming(
        &self,
        request: ExecStreamingRequest<'_>,
    ) -> crate::Result<ExecStreamingResult> {
        if request.stdin.is_some() {
            return Err(crate::Error::message(
                "This sandbox does not support standard input for streaming commands",
            ));
        }
        let fallback_timeout_ms = request.timeout_ms.unwrap_or(u64::MAX);
        let result = self
            .exec_command(
                request.command,
                fallback_timeout_ms,
                request.working_dir,
                request.env_vars,
                request.cancel_token,
            )
            .await?;
        replay_exec_result(result, true, request.output_callback.as_ref()).await
    }

    /// Launch a long-lived process with bidirectional stdio attached.
    ///
    /// Where supported, `_command` is evaluated under the same non-login Bash
    /// contract as [`exec_command`](Self::exec_command) before the shell
    /// replaces itself with the requested process. Providers without
    /// bidirectional stdio keep this default and report the capability as
    /// unsupported rather than substituting another interpreter.
    async fn spawn_stdio_process(
        &self,
        _command: &str,
        _working_dir: Option<&str>,
        _env_vars: Option<&HashMap<String, String>>,
        _cancel_token: Option<CancellationToken>,
    ) -> crate::Result<StdioProcess> {
        Err(crate::Error::message(
            "ACP backend requires bidirectional stdio; this sandbox provider does not support it",
        ))
    }

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> crate::Result<Vec<String>>;

    /// Recursively enumerate regular files below a caller-declared base.
    ///
    /// `relative_start` is a normalized literal directory path relative to
    /// `base`; it is an optimization boundary, not a matching expression.
    /// Every returned `relative_path` remains relative to `base`.
    /// Implementations resolve `base` itself but must not recurse through
    /// symlinks encountered in `relative_start` or below it.
    ///
    /// Production providers that support filesystem search must override this.
    async fn walk_files(
        &self,
        _base: &str,
        _relative_start: &str,
        _options: &WalkOptions,
    ) -> crate::Result<Vec<SandboxFile>> {
        Err(crate::Error::message(
            "recursive file traversal is not supported by this sandbox",
        ))
    }

    /// Match a workspace-relative glob using provider-independent semantics.
    async fn glob(&self, pattern: &str, path: Option<&str>) -> crate::Result<Vec<String>> {
        let glob = WorkspaceGlob::try_new(pattern)
            .map_err(|error| crate::Error::context("Invalid glob pattern", error))?;
        let base = path.unwrap_or_else(|| self.working_directory());
        let mut files = self
            .walk_files(base, glob.traversal_root(), &WalkOptions::default())
            .await?
            .into_iter()
            .filter(|file| glob.is_match(&file.relative_path))
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok(files.into_iter().map(|file| file.path).collect())
    }
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
    /// Ensure the provider resource is running and not paused before access.
    ///
    /// This access-time operation must be idempotent. Providers that can stop
    /// independently should avoid restarting an already-active sandbox. This
    /// lightweight check does not require the full health verification done by
    /// [`Sandbox::start`], and it does not keep a sandbox active between calls:
    /// when the call reports [`SandboxActivation::Started`], transient
    /// read-only callers must stop the sandbox again when they are done.
    async fn activate(&self) -> crate::Result<SandboxActivation> {
        self.start().await.map(|()| SandboxActivation::Started)
    }
    async fn start(&self) -> crate::Result<()> {
        Ok(())
    }
    async fn stop(&self) -> crate::Result<()> {
        Ok(())
    }
    async fn delete(&self) -> crate::Result<()> {
        self.cleanup().await
    }
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

    /// Return the provider snapshot used by an initialized sandbox, when the
    /// provider has a snapshot concept.
    fn snapshot_info(&self) -> Option<String> {
        None
    }

    /// Refresh git push credentials (e.g. rotate an expiring GitHub App token).
    /// Default is a no-op; Docker/Daytona override to resolve a token through
    /// the shared source and update the remote URL when the embedded
    /// generation is stale. Returns [`RefreshOutcome`] so callers can tell
    /// what happened to the remote and which token it carries.
    async fn refresh_push_credentials(&self) -> crate::Result<RefreshOutcome> {
        Ok(RefreshOutcome::none())
    }

    /// The shared installation-token source feeding this sandbox's push
    /// credentials, when the provider manages GitHub credentials.
    ///
    /// Consumers outside the sandbox (e.g. the run-metadata writer) share
    /// this source so every GitHub-token consumer for the origin repository
    /// reuses one cached token instead of minting its own.
    fn push_token_source(&self) -> Option<Arc<InstallationTokenSource>> {
        None
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

    /// Push a full refspec to origin from inside the sandbox, retrying per
    /// `plan` with a pinned credential generation. Failures keep their
    /// attempt history in the returned [`PushError`].
    async fn git_push_ref(
        &self,
        _refspec: &str,
        _plan: &RetryPlan,
    ) -> Result<PushReport, PushError> {
        Err(PushError {
            report: PushReport::default(),
            error:  crate::Error::message("git_push_ref not implemented for this sandbox"),
        })
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
}

/// Resolve a path: relative paths are prepended with the working directory.
/// Used by the Daytona sandbox implementation.
#[cfg(any(feature = "docker", feature = "daytona"))]
pub(crate) fn resolve_path(path: &str, working_dir: &str) -> String {
    if std::path::Path::new(path).is_absolute() {
        path.to_string()
    } else {
        join_sandbox_path(working_dir, path)
    }
}

#[cfg(any(feature = "docker", feature = "daytona"))]
pub(crate) fn join_sandbox_path(base: &str, relative_path: &str) -> String {
    if relative_path.is_empty() {
        return base.to_string();
    }
    if base.is_empty() {
        return relative_path.to_string();
    }
    if base == "/" {
        return format!("/{relative_path}");
    }
    format!("{}/{relative_path}", base.trim_end_matches('/'))
}

#[cfg(any(feature = "docker", feature = "daytona"))]
pub(crate) fn build_remote_walk_command(
    base: &str,
    relative_start: &str,
    options: &WalkOptions,
) -> String {
    let traversal_root = join_sandbox_path(base, relative_start);
    let quoted_root = shell_quote(&traversal_root);
    let mut command = format!("if [ -e {quoted_root} ]");
    let mut component_path = base.to_string();
    for segment in relative_start
        .split('/')
        .filter(|segment| !segment.is_empty())
    {
        component_path = join_sandbox_path(&component_path, segment);
        let _ = write!(command, " && [ ! -L {} ]", shell_quote(&component_path));
    }
    let _ = write!(command, "; then find -H {quoted_root}");

    if !options.excluded_directory_names.is_empty() {
        command.push_str(" \\( -type d \\(");
        for (index, directory_name) in options.excluded_directory_names.iter().enumerate() {
            if index > 0 {
                command.push_str(" -o");
            }
            let _ = write!(command, " -name {}", shell_quote(directory_name));
        }
        command.push_str(" \\) -prune \\) -o");
    }

    command.push_str(" -not -type l -type f -printf '%s\\0%P\\0'; fi");
    command
}

#[cfg(any(feature = "docker", feature = "daytona"))]
pub(crate) fn parse_remote_walk_output(
    base: &str,
    relative_start: &str,
    output: &str,
) -> crate::Result<Vec<SandboxFile>> {
    let mut fields = output.split('\0');
    let mut files = Vec::new();

    while let Some(size) = fields.next() {
        if size.is_empty() {
            break;
        }
        let relative_to_start = fields.next().ok_or_else(|| {
            crate::Error::message("Malformed recursive file traversal output: missing path")
        })?;
        let size = size.parse::<u64>().map_err(|error| {
            crate::Error::context(
                format!("Malformed recursive file traversal size {size:?}"),
                error,
            )
        })?;
        let relative_path = if relative_to_start.is_empty() {
            relative_start.to_string()
        } else {
            join_sandbox_path(relative_start, relative_to_start)
        };
        files.push(SandboxFile {
            path: join_sandbox_path(base, &relative_path),
            relative_path,
            size,
        });
    }

    Ok(files)
}

/// Shell-quote a string using `shlex::try_quote`, with a fallback for edge
/// cases. Re-exported from [`fabro_util::shell::shell_quote`] so sandbox code
/// and the config resolve layer share one audited implementation.
pub fn shell_quote(s: &str) -> String {
    shell::shell_quote(s)
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

#[tracing::instrument(name = "git_op", skip_all, fields(op = "fetch"))]
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

/// One push attempt inside a retried push operation. Runtime detail only —
/// the durable serialized shape lives in `fabro-types` and the workflow layer
/// owns the conversion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushAttempt {
    /// 1-based attempt number within this operation.
    pub attempt:           u32,
    pub started_at:        chrono::DateTime<chrono::Utc>,
    pub success:           bool,
    /// The classifier's verdict for a failed attempt — recorded on the
    /// terminal attempt too; whether a retry actually followed is positional
    /// (every entry except the last).
    pub retry_reason:      Option<GitRetryReason>,
    /// Redacted, bounded output tail; failed attempts only.
    pub exec_output_tail:  Option<fabro_types::ExecOutputTail>,
    /// The token embedded in the remote during this attempt.
    pub token:             Option<TokenSnapshot>,
    /// What `ensure_embedded` did to the remote this attempt.
    pub credential_action: Option<RemoteCredentialAction>,
    /// A mint or `set-url` failure this attempt pushed through.
    pub refresh_error:     Option<RefreshErrorKind>,
}

/// The attempt history of one push operation.
#[derive(Debug, Clone, Default)]
pub struct PushReport {
    pub attempts: Vec<PushAttempt>,
}

/// A failed push operation: the final typed error plus the attempt history.
/// The error type stays the safety boundary for output tails.
#[derive(Debug, thiserror::Error)]
#[error("git push failed")]
pub struct PushError {
    pub report: PushReport,
    #[source]
    pub error:  crate::Error,
}

/// Classify a failed push attempt by the failure's rendered output.
fn classify_push_error(error: &crate::Error, cred: CredentialContext) -> Option<GitRetryReason> {
    let class = match error {
        crate::Error::Exec { result, .. } if result.termination != CommandTermination::Exited => {
            return None;
        }
        crate::Error::Exec { result, .. } => {
            git_retry::classify_output(&result.stderr, &result.stdout, cred)
        }
        other => git_retry::classify_message(&crate::display_for_log(other), cred),
    };
    class.retry_reason()
}

/// Whether a failed push attempt has the 404/auth-failure shape that a
/// drifted or missing embedded token also produces.
fn push_failure_looks_auth_shaped(error: &crate::Error) -> bool {
    match error {
        crate::Error::Exec { result, .. } => {
            git_retry::output_matches_auth_failure_hints(&result.stderr, &result.stdout)
        }
        other => git_retry::matches_auth_failure_hints(&crate::display_for_log(other)),
    }
}

/// Helper for sandbox implementations that manage git internally.
///
/// Pushes a refspec to origin via `exec_command` inside the sandbox,
/// retrying per `plan` with one pinned credential generation for the whole
/// operation. `credentials` is the provider's push-credential state plus the
/// origin URL; `None` pushes with whatever the remote already carries (the
/// local sandbox, or a workspace without managed credentials).
#[tracing::instrument(name = "git_op", skip_all, fields(op = "push"))]
pub(crate) async fn git_push_via_exec(
    sandbox: &dyn Sandbox,
    credentials: Option<(&PushCredentialState, &str)>,
    refspec: &str,
    plan: &RetryPlan,
) -> Result<PushReport, PushError> {
    use CredentialContext;
    use CredentialLease;

    let start = time::Instant::now();
    let deadline = plan.effective_deadline(start);

    // The lease pins one token generation and owns the embed mutex for the
    // whole operation; no concurrent refresh can re-embed mid-operation, and
    // no attempt can cross the refresh margin and restart the replication
    // clock.
    let mut lease: Option<(CredentialLease<'_>, &str)> = match credentials {
        Some((state, origin_url)) => match match deadline {
            Some(deadline) => match time::timeout_at(deadline, state.lease()).await {
                Ok(result) => result,
                Err(_) => {
                    return Err(push_deadline_error(
                        Vec::new(),
                        "while acquiring credentials",
                    ));
                }
            },
            None => state.lease().await,
        } {
            Ok(lease) => Some((lease, origin_url)),
            Err(error) => {
                return Err(PushError {
                    report: PushReport::default(),
                    error,
                });
            }
        },
        None => None,
    };

    let mut attempts: Vec<PushAttempt> = Vec::new();
    let mut force_reembed = false;
    let mut drift_repaired = false;
    let cmd = format!("{GIT} push origin {}", shell_quote(refspec));
    let label = format!("git push origin {refspec}");

    loop {
        let attempt_number = u32::try_from(attempts.len()).unwrap_or(u32::MAX) + 1;
        let started_at = chrono::Utc::now();
        let attempt_timeout = plan
            .attempt_timeout(deadline)
            .unwrap_or(Duration::from_mins(1));
        if attempt_timeout.is_zero() {
            return Err(push_deadline_error(attempts, "before the next attempt"));
        }
        let attempt_deadline = time::Instant::now() + attempt_timeout;
        let (token, credential_action, refresh_error) = match lease.as_mut() {
            Some((lease, origin_url)) => {
                let ensured = match time::timeout_at(
                    attempt_deadline,
                    lease.ensure_embedded(sandbox, origin_url, force_reembed),
                )
                .await
                {
                    Ok(Ok(ensured)) => ensured,
                    Ok(Err(error)) => {
                        return Err(PushError {
                            report: PushReport { attempts },
                            error,
                        });
                    }
                    Err(_) => {
                        return Err(push_deadline_error(
                            attempts,
                            "while refreshing credentials",
                        ));
                    }
                };
                force_reembed = false;
                (ensured.token, Some(ensured.action), ensured.refresh_error)
            }
            None => (None, None, None),
        };

        let remaining = attempt_deadline.saturating_duration_since(time::Instant::now());
        let timeout_ms = u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX);
        if timeout_ms == 0 {
            return Err(push_deadline_error(attempts, "before running git push"));
        }
        let push_result = match sandbox
            .exec_command(&cmd, timeout_ms, None, None, None)
            .await
        {
            Ok(result) => result.into_result(&label).map(|_| ()),
            Err(err) => Err(crate::Error::context(label.clone(), err)),
        };

        match push_result {
            Ok(()) => {
                attempts.push(PushAttempt {
                    attempt: attempt_number,
                    started_at,
                    success: true,
                    retry_reason: None,
                    exec_output_tail: None,
                    token,
                    credential_action,
                    refresh_error,
                });
                tracing::info!(
                    refspec = %refspec,
                    attempt = attempt_number,
                    token_generation = token.map(|token| token.generation),
                    token_age_ms = token.and_then(|token| token.age_ms()),
                    "Pushed git ref to origin"
                );
                return Ok(PushReport { attempts });
            }
            Err(error) => {
                // Drift recovery: the tracked generation is local belief, and
                // agent code inside the sandbox can rewrite `origin`. The
                // first auth/not-found failure earns one forced re-embed of
                // the pinned token, inside the same retry budget.
                if !drift_repaired && lease.is_some() && push_failure_looks_auth_shaped(&error) {
                    drift_repaired = true;
                    force_reembed = true;
                }
                let cred = CredentialContext::from_snapshot(token.as_ref());
                let retry_reason = classify_push_error(&error, cred);
                attempts.push(PushAttempt {
                    attempt: attempt_number,
                    started_at,
                    success: false,
                    retry_reason,
                    exec_output_tail: error.default_redacted_output_tail(),
                    token,
                    credential_action,
                    refresh_error,
                });

                let exhausted = attempt_number >= plan.max_attempts.max(1);
                let Some(reason) = retry_reason.filter(|_| !exhausted) else {
                    return Err(PushError {
                        report: PushReport { attempts },
                        error,
                    });
                };
                let Some(delay) = plan.retry_delay(attempt_number, deadline) else {
                    return Err(PushError {
                        report: PushReport { attempts },
                        error,
                    });
                };
                // The failure text can carry git stderr, so log the category
                // rather than the message.
                tracing::warn!(
                    refspec = %refspec,
                    attempt = attempt_number,
                    max_attempts = plan.max_attempts,
                    reason = %reason,
                    token_generation = token.map(|token| token.generation),
                    token_age_ms = token.and_then(|token| token.age_ms()),
                    delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    "Git push failed, retrying with the same token"
                );
                time::sleep(delay).await;
            }
        }
    }
}

fn push_deadline_error(attempts: Vec<PushAttempt>, stage: &str) -> PushError {
    PushError {
        report: PushReport { attempts },
        error:  crate::Error::message(format!("Git push retry deadline expired {stage}")),
    }
}

#[cfg(test)]
mod push_tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Utc;
    use fabro_github::InstallationToken;
    use fabro_github::test_support::{InstallationTokenMinter, installation_token_source};
    use fabro_github::token_source::{InstallationTokenSource, REFRESH_MARGIN};
    use tokio::sync::Mutex as AsyncMutex;

    use super::*;
    use crate::git_retry::{GitRetryReason, RetryPlan};
    use crate::push_credentials::{PushCredentialState, RefreshErrorKind};

    const ORIGIN: &str = "https://github.com/fabro-testing/repo";
    const REFSPEC: &str = "refs/heads/fabro/run/01M0DH033P2XSTHAGVBHG6922F";

    fn ok_exec() -> ExecResult {
        ExecResult {
            stdout:      String::new(),
            stderr:      String::new(),
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 5,
        }
    }

    fn failed_exec(stderr: &str) -> ExecResult {
        ExecResult {
            stdout:      String::new(),
            stderr:      stderr.to_string(),
            exit_code:   Some(128),
            termination: CommandTermination::Exited,
            duration_ms: 5,
        }
    }

    fn timed_out_exec() -> ExecResult {
        ExecResult {
            stdout:      String::new(),
            stderr:      "Command timed out".to_string(),
            exit_code:   None,
            termination: CommandTermination::TimedOut,
            duration_ms: 60_000,
        }
    }

    /// Sandbox stub that scripts `git push` results and records the exec
    /// commands the push driver runs. `git remote set-url` execs succeed
    /// unless scripted otherwise.
    struct ScriptedGitSandbox {
        push_results:     Mutex<VecDeque<ExecResult>>,
        set_url_results:  Mutex<VecDeque<ExecResult>>,
        push_commands:    Mutex<Vec<String>>,
        set_url_commands: Mutex<Vec<String>>,
    }

    impl ScriptedGitSandbox {
        fn new(push_results: Vec<ExecResult>) -> Self {
            Self {
                push_results:     Mutex::new(push_results.into()),
                set_url_results:  Mutex::new(VecDeque::new()),
                push_commands:    Mutex::new(Vec::new()),
                set_url_commands: Mutex::new(Vec::new()),
            }
        }

        fn with_set_url_results(self, results: Vec<ExecResult>) -> Self {
            *self.set_url_results.lock().unwrap() = results.into();
            self
        }

        fn push_count(&self) -> usize {
            self.push_commands.lock().unwrap().len()
        }

        fn set_url_commands(&self) -> Vec<String> {
            self.set_url_commands.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Sandbox for ScriptedGitSandbox {
        async fn exec_command(
            &self,
            command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&HashMap<String, String>>,
            _cancel_token: Option<CancellationToken>,
        ) -> crate::Result<ExecResult> {
            if command.contains("remote set-url") {
                self.set_url_commands
                    .lock()
                    .unwrap()
                    .push(command.to_string());
                return Ok(self
                    .set_url_results
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(ok_exec));
            }
            assert!(
                command.contains("push origin"),
                "unexpected exec: {command}"
            );
            self.push_commands.lock().unwrap().push(command.to_string());
            Ok(self
                .push_results
                .lock()
                .unwrap()
                .pop_front()
                .expect("push script exhausted"))
        }

        async fn read_file_bytes(&self, _path: &str) -> crate::Result<Vec<u8>> {
            unimplemented!()
        }

        async fn write_file(&self, _path: &str, _content: &str) -> crate::Result<()> {
            unimplemented!()
        }

        async fn delete_file(&self, _path: &str) -> crate::Result<()> {
            unimplemented!()
        }

        async fn file_exists(&self, _path: &str) -> crate::Result<bool> {
            unimplemented!()
        }

        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> crate::Result<Vec<DirEntry>> {
            unimplemented!()
        }

        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &GrepOptions,
        ) -> crate::Result<Vec<String>> {
            unimplemented!()
        }

        async fn download_file_to_local(
            &self,
            _remote_path: &str,
            _local_path: &Path,
        ) -> crate::Result<()> {
            unimplemented!()
        }

        async fn upload_file_from_local(
            &self,
            _local_path: &Path,
            _remote_path: &str,
        ) -> crate::Result<()> {
            unimplemented!()
        }

        async fn initialize(&self) -> crate::Result<()> {
            Ok(())
        }

        async fn cleanup(&self) -> crate::Result<()> {
            Ok(())
        }

        fn working_directory(&self) -> &'static str {
            "/workspace"
        }

        fn platform(&self) -> &'static str {
            "linux"
        }

        fn os_version(&self) -> String {
            "linux".to_string()
        }
    }

    enum MintAction {
        Token(&'static str, chrono::Duration),
        Error(&'static str),
    }

    struct ScriptedMinter {
        calls:  AtomicUsize,
        script: AsyncMutex<VecDeque<MintAction>>,
    }

    impl ScriptedMinter {
        fn new(script: Vec<MintAction>) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                calls:  AtomicUsize::new(0),
                script: AsyncMutex::new(script.into()),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl InstallationTokenMinter for ScriptedMinter {
        async fn mint(&self) -> anyhow::Result<InstallationToken> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.script.lock().await.pop_front().expect("mint script") {
                MintAction::Token(token, ttl) => Ok(InstallationToken {
                    token:      token.to_string(),
                    expires_at: Utc::now() + ttl,
                }),
                MintAction::Error(message) => Err(anyhow::anyhow!(message)),
            }
        }
    }

    struct SlowMinter;

    #[async_trait]
    impl InstallationTokenMinter for SlowMinter {
        async fn mint(&self) -> anyhow::Result<InstallationToken> {
            time::sleep(Duration::from_secs(2)).await;
            Ok(InstallationToken {
                token:      "ghs_slow".to_string(),
                expires_at: Utc::now() + chrono::Duration::hours(1),
            })
        }
    }

    fn minting_state(
        script: Vec<MintAction>,
    ) -> (PushCredentialState, std::sync::Arc<ScriptedMinter>) {
        let minter = ScriptedMinter::new(script);
        let source = installation_token_source(
            "fabro-testing/repo",
            std::sync::Arc::clone(&minter) as std::sync::Arc<dyn InstallationTokenMinter>,
        );
        (PushCredentialState::new(Some(source)), minter)
    }

    async fn seed_clone_token(state: &PushCredentialState) {
        let clone_token = state
            .source()
            .expect("state has a source")
            .mint_for_clone()
            .await
            .expect("clone mint succeeds");
        state.record_embedded(clone_token).await;
    }

    /// Regression for run `01M0DH033P2XSTHAGVBHG6922F` (the push variant of
    /// `clone_not_found_after_a_successful_mint_is_retried`): GitHub rejected
    /// pushes with 404 "Repository not found" milliseconds after a token
    /// mint. The retry must reuse the same token — replication of a given
    /// token only makes progress — and recover inside the plan's budget.
    #[tokio::test(start_paused = true)]
    async fn push_not_found_after_a_successful_mint_is_retried_with_the_same_token() {
        let (state, minter) = minting_state(vec![MintAction::Token(
            "ghs_gen1",
            chrono::Duration::minutes(60),
        )]);
        let sandbox = ScriptedGitSandbox::new(vec![
            failed_exec("remote: Repository not found."),
            failed_exec("remote: Repository not found."),
            ok_exec(),
        ]);

        let report = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::checkpoint_push(),
        )
        .await
        .expect("push should recover within the checkpoint plan");

        assert_eq!(report.attempts.len(), 3);
        assert_eq!(minter.calls(), 1, "retries must not re-mint");
        for attempt in &report.attempts {
            assert_eq!(attempt.token.expect("token recorded").generation, 1);
        }
        assert_eq!(
            report.attempts[0].retry_reason,
            Some(GitRetryReason::TokenReplication)
        );
        assert!(report.attempts[0].exec_output_tail.is_some());
        assert_eq!(
            report.attempts[0].credential_action,
            Some(RemoteCredentialAction::Embedded),
            "first attempt embeds the resolved token"
        );
        assert!(report.attempts[2].success);
        assert!(report.attempts[2].exec_output_tail.is_none());
        assert_eq!(sandbox.push_count(), 3);
    }

    /// The publish plan gives the terminal push a real budget: four
    /// replication-lag failures still recover on the fifth attempt.
    #[tokio::test(start_paused = true)]
    async fn publish_plan_survives_four_not_found_failures() {
        let (state, minter) = minting_state(vec![MintAction::Token(
            "ghs_gen1",
            chrono::Duration::minutes(60),
        )]);
        let sandbox = ScriptedGitSandbox::new(vec![
            failed_exec("remote: Repository not found."),
            failed_exec("remote: Repository not found."),
            failed_exec("remote: Repository not found."),
            failed_exec("remote: Repository not found."),
            ok_exec(),
        ]);

        let report = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::publish_push(),
        )
        .await
        .expect("push should recover within the publish plan");

        assert_eq!(report.attempts.len(), 5);
        assert_eq!(minter.calls(), 1);
        assert!(report.attempts[4].success);
    }

    /// Margin-boundary pinning: a token resolved just above the refresh
    /// margin stays pinned through a full retry sequence — the operation
    /// never re-resolves mid-flight, so no fresh mint can restart the
    /// replication clock.
    #[tokio::test(start_paused = true)]
    async fn token_resolved_just_above_the_margin_stays_pinned_through_retries() {
        let ttl = REFRESH_MARGIN + Duration::from_secs(5);
        let (state, minter) = minting_state(vec![MintAction::Token(
            "ghs_gen1",
            chrono::Duration::from_std(ttl).unwrap(),
        )]);
        let sandbox = ScriptedGitSandbox::new(vec![
            failed_exec("remote: Repository not found."),
            failed_exec("remote: Repository not found."),
            ok_exec(),
        ]);

        let report = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::checkpoint_push(),
        )
        .await
        .expect("push should recover");

        assert_eq!(minter.calls(), 1, "no mid-operation mint");
        let generations: Vec<u64> = report
            .attempts
            .iter()
            .map(|attempt| attempt.token.expect("token recorded").generation)
            .collect();
        assert_eq!(generations, vec![1, 1, 1]);
    }

    #[tokio::test(start_paused = true)]
    async fn static_credential_auth_failure_fails_fast() {
        let source = InstallationTokenSource::for_origin(
            &fabro_github::GitHubCredentials::Pat("ghp_pat".to_string()),
            ORIGIN,
            serde_json::json!({ "contents": "write" }),
        )
        .unwrap();
        let state = PushCredentialState::new(Some(source));
        seed_clone_token(&state).await;
        let sandbox = ScriptedGitSandbox::new(vec![failed_exec(
            "fatal: Authentication failed for 'https://github.com/fabro-testing/repo'",
        )]);

        let push_error = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::publish_push(),
        )
        .await
        .expect_err("static credentials cannot become valid by waiting");

        assert_eq!(push_error.report.attempts.len(), 1);
        assert_eq!(push_error.report.attempts[0].retry_reason, None);
        assert!(push_error.report.attempts[0].token.unwrap().is_static());
    }

    /// Clone seeding closes the "nothing was ever embedded" hole: when the
    /// first refresh mint fails, the push falls back to the clone token
    /// recorded as last-embedded instead of aborting.
    #[tokio::test(start_paused = true)]
    async fn mint_failure_falls_back_to_the_clone_token() {
        let (state, minter) = minting_state(vec![
            MintAction::Token("ghs_clone", chrono::Duration::minutes(5)),
            // The clone token is inside the margin, so lease acquisition
            // re-mints and fails.
            MintAction::Error("mint failed"),
        ]);
        seed_clone_token(&state).await;
        let sandbox = ScriptedGitSandbox::new(vec![ok_exec()]);

        let report = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::checkpoint_push(),
        )
        .await
        .expect("push proceeds with the still-valid clone token");

        assert_eq!(minter.calls(), 2);
        let attempt = &report.attempts[0];
        assert!(attempt.success);
        assert_eq!(attempt.refresh_error, Some(RefreshErrorKind::Mint));
        assert_eq!(
            attempt.token.expect("fallback token recorded").generation,
            1,
            "attempts classify against the embedded clone token, never None"
        );
        assert_eq!(
            attempt.credential_action,
            Some(RemoteCredentialAction::Unchanged)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn acquisition_fails_when_mint_fails_and_nothing_was_embedded() {
        let (state, _minter) = minting_state(vec![MintAction::Error("mint failed")]);
        let sandbox = ScriptedGitSandbox::new(vec![]);

        let push_error = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::checkpoint_push(),
        )
        .await
        .expect_err("there is nothing to push with");

        assert!(push_error.report.attempts.is_empty());
        assert!(push_error.error.to_string().contains("token_mint_failed"));
        assert_eq!(sandbox.push_count(), 0);
    }

    /// Late-mint recovery: the fallback push fails on the expired-ish old
    /// token, a later attempt's resolve retry succeeds, the target embeds,
    /// and the push recovers — all inside one operation's budget.
    #[tokio::test(start_paused = true)]
    async fn late_mint_recovery_lands_the_target_inside_the_operation() {
        let (state, minter) = minting_state(vec![
            MintAction::Token("ghs_gen1", chrono::Duration::minutes(5)),
            MintAction::Error("mint failed"),
            MintAction::Token("ghs_gen2", chrono::Duration::minutes(60)),
        ]);
        seed_clone_token(&state).await;
        let sandbox = ScriptedGitSandbox::new(vec![
            failed_exec("fatal: Authentication failed for 'https://github.com'"),
            ok_exec(),
        ]);

        let report = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::checkpoint_push(),
        )
        .await
        .expect("late mint should recover the push");

        assert_eq!(minter.calls(), 3);
        let first = &report.attempts[0];
        assert_eq!(first.refresh_error, Some(RefreshErrorKind::Mint));
        assert_eq!(first.token.unwrap().generation, 1);
        let second = &report.attempts[1];
        assert!(second.success);
        assert_eq!(second.refresh_error, None);
        assert_eq!(second.token.unwrap().generation, 2);
        assert_eq!(
            second.credential_action,
            Some(RemoteCredentialAction::Embedded),
            "the report shows the single generation transition"
        );
    }

    /// A failed `set-url` defers the embed: attempt 1 records the old
    /// generation with the refresh error, attempt 2 lands the target, and the
    /// report shows the one generation transition via `credential_action`.
    #[tokio::test(start_paused = true)]
    async fn set_url_failure_defers_the_embed_until_the_next_attempt() {
        let (state, minter) = minting_state(vec![
            MintAction::Token("ghs_gen1", chrono::Duration::minutes(5)),
            MintAction::Token("ghs_gen2", chrono::Duration::minutes(60)),
        ]);
        seed_clone_token(&state).await;
        let sandbox = ScriptedGitSandbox::new(vec![
            failed_exec("error: RPC failed; connection reset by peer"),
            ok_exec(),
        ])
        .with_set_url_results(vec![failed_exec("error: could not lock config file")]);

        let report = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::checkpoint_push(),
        )
        .await
        .expect("deferred embed should land on the retry");

        assert_eq!(
            minter.calls(),
            2,
            "the successful resolve is never repeated"
        );
        let first = &report.attempts[0];
        assert_eq!(first.refresh_error, Some(RefreshErrorKind::SetUrl));
        assert_eq!(
            first.token.unwrap().generation,
            1,
            "pin stays on the old token"
        );
        assert_eq!(
            first.credential_action,
            Some(RemoteCredentialAction::Unchanged)
        );
        let second = &report.attempts[1];
        assert_eq!(second.token.unwrap().generation, 2);
        assert_eq!(
            second.credential_action,
            Some(RemoteCredentialAction::Embedded)
        );
        assert!(second.success);
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_set_url_stops_before_push_while_it_may_still_run() {
        let (state, minter) = minting_state(vec![
            MintAction::Token("ghs_gen1", chrono::Duration::minutes(5)),
            MintAction::Token("ghs_gen2", chrono::Duration::minutes(60)),
        ]);
        seed_clone_token(&state).await;
        let sandbox = ScriptedGitSandbox::new(vec![]).with_set_url_results(vec![timed_out_exec()]);

        let push_error = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::checkpoint_push(),
        )
        .await
        .expect_err("a timed-out set-url can still rewrite origin later");

        assert_eq!(minter.calls(), 2);
        assert!(push_error.report.attempts.is_empty());
        assert_eq!(sandbox.push_count(), 0);
    }

    /// Remote drift: agent code rewrote `origin`, so the push fails on auth
    /// even though the tracked generation looks current. The first
    /// auth-shaped failure earns one forced re-embed of the pinned token.
    #[tokio::test(start_paused = true)]
    async fn remote_drift_gets_one_forced_reembed_of_the_pinned_token() {
        let (state, minter) = minting_state(vec![MintAction::Token(
            "ghs_gen1",
            chrono::Duration::minutes(60),
        )]);
        seed_clone_token(&state).await;
        let sandbox = ScriptedGitSandbox::new(vec![
            failed_exec(
                "fatal: could not read Username for 'https://github.com': No such device or address\nremote: Repository not found.",
            ),
            ok_exec(),
        ]);

        let report = git_push_via_exec(
            &sandbox,
            Some((&state, ORIGIN)),
            REFSPEC,
            &RetryPlan::checkpoint_push(),
        )
        .await
        .expect("drift repair should restore the pinned credentials");

        assert_eq!(minter.calls(), 1, "drift repair re-embeds, never re-mints");
        assert_eq!(
            report.attempts[0].credential_action,
            Some(RemoteCredentialAction::Unchanged),
            "before the failure the tracked generation matched"
        );
        assert_eq!(
            report.attempts[1].credential_action,
            Some(RemoteCredentialAction::Embedded),
            "the retry force-re-embeds the pinned token"
        );
        let set_urls = sandbox.set_url_commands();
        assert_eq!(set_urls.len(), 1);
        assert!(set_urls[0].contains("ghs_gen1"));
    }

    #[tokio::test(start_paused = true)]
    async fn push_without_managed_credentials_reports_no_token() {
        let sandbox = ScriptedGitSandbox::new(vec![ok_exec()]);

        let report = git_push_via_exec(&sandbox, None, REFSPEC, &RetryPlan::checkpoint_push())
            .await
            .expect("push succeeds");

        assert_eq!(report.attempts.len(), 1);
        assert_eq!(report.attempts[0].token, None);
        assert_eq!(report.attempts[0].credential_action, None);
    }

    #[tokio::test(start_paused = true)]
    async fn unauthenticated_auth_failure_is_permanent() {
        let sandbox = ScriptedGitSandbox::new(vec![failed_exec(
            "fatal: Authentication failed for 'https://github.com/fabro-testing/repo'",
        )]);

        let push_error = git_push_via_exec(&sandbox, None, REFSPEC, &RetryPlan::publish_push())
            .await
            .expect_err("no credentials to wait on");

        assert_eq!(push_error.report.attempts.len(), 1);
        assert_eq!(push_error.report.attempts[0].retry_reason, None);
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_push_is_not_retried_while_the_remote_process_may_still_run() {
        let sandbox = ScriptedGitSandbox::new(vec![timed_out_exec()]);

        let push_error = git_push_via_exec(&sandbox, None, REFSPEC, &RetryPlan::publish_push())
            .await
            .expect_err("an unconfirmed timeout must fail without another push");

        assert_eq!(sandbox.push_count(), 1);
        assert_eq!(push_error.report.attempts.len(), 1);
        assert_eq!(push_error.report.attempts[0].retry_reason, None);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_deadline_includes_credential_lease_acquisition() {
        let source = installation_token_source("fabro-testing/repo", Arc::new(SlowMinter));
        let state = PushCredentialState::new(Some(source));
        let sandbox = ScriptedGitSandbox::new(vec![]);
        let mut plan = RetryPlan::checkpoint_push();
        plan.max_elapsed = Some(Duration::from_secs(1));

        let push_error = git_push_via_exec(&sandbox, Some((&state, ORIGIN)), REFSPEC, &plan)
            .await
            .expect_err("credential acquisition must stop at the operation deadline");

        assert!(push_error.report.attempts.is_empty());
        assert_eq!(sandbox.push_count(), 0);
        assert!(push_error.error.to_string().contains("deadline expired"));
    }

    #[tokio::test(start_paused = true)]
    async fn expired_retry_deadline_does_not_launch_a_zero_timeout_push() {
        let sandbox = ScriptedGitSandbox::new(vec![]);
        let mut plan = RetryPlan::checkpoint_push();
        plan.max_elapsed = Some(Duration::ZERO);

        let push_error = git_push_via_exec(&sandbox, None, REFSPEC, &plan)
            .await
            .expect_err("an expired operation must stop before exec");

        assert!(push_error.report.attempts.is_empty());
        assert_eq!(sandbox.push_count(), 0);
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
        let crate::Error::Exec { label, result, .. } = &error else {
            panic!("expected Error::Exec, got {error:?}");
        };
        assert_eq!(label, "git push");
        assert_eq!(result.exit_code, Some(128));
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

        let crate::Error::Exec { result, .. } = &error else {
            panic!("expected Error::Exec, got {error:?}");
        };
        assert_eq!(result.stderr, "stderr https://****@example.com");
        assert_eq!(result.stdout, "stdout https://****@example.com");
    }

    #[test]
    fn exec_result_redacts_before_taking_tail() {
        let secret = "sk-ant-api03-xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA";
        let result = ExecResult {
            stdout:      format!("{} {secret} done", "context ".repeat(20)),
            stderr:      String::new(),
            exit_code:   Some(1),
            termination: CommandTermination::Exited,
            duration_ms: 1,
        };

        let tail = result
            .redacted_output_tail(32)
            .expect("redacted output tail");
        let stdout = tail.stdout.expect("stdout tail");
        assert!(stdout.contains("REDACTED"), "{stdout}");
        assert!(!stdout.contains("F0gH3jE6pA"), "{stdout}");
        assert!(tail.stdout_truncated);
    }

    #[test]
    fn exec_result_tail_sanitizes_terminal_control_sequences() {
        let result = ExecResult {
            stdout:      "\u{1b}[31mred\u{1b}[0m \u{1b}]0;window-title\u{7}shown \
                          \u{1b}(Bset \u{1b}Mtwo-byte \u{8}backspace"
                .to_string(),
            stderr:      String::new(),
            exit_code:   Some(1),
            termination: CommandTermination::Exited,
            duration_ms: 1,
        };

        let tail = result
            .redacted_output_tail(1024)
            .expect("redacted output tail");
        let stdout = tail.stdout.expect("stdout tail");
        assert_eq!(stdout, "red shown set two-byte backspace");
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test intentionally creates host process output for conversion coverage"
    )]
    fn from_process_output_uses_minus_one_for_signal_exit_without_code() {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("printf out; printf err >&2; kill -9 $$")
            .output()
            .expect("signal-killed process output");

        let result = ExecResult::from_process_output(output, 12);

        assert_eq!(result.stdout, "out");
        assert_eq!(result.stderr, "err");
        assert_eq!(result.exit_code, Some(-1));
        assert_eq!(result.termination, CommandTermination::Exited);
        assert_eq!(result.duration_ms, 12);
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test intentionally creates host process output for conversion coverage"
    )]
    fn from_process_output_handles_lossy_non_utf8_output() {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("printf '\\377'; printf '\\376' >&2")
            .output()
            .expect("non-utf8 process output");

        let result = ExecResult::from_process_output(output, 3);
        let tail = result
            .redacted_output_tail(16)
            .expect("redacted output tail");

        assert!(tail.stdout.expect("stdout tail").len() <= 16);
        assert!(tail.stderr.expect("stderr tail").len() <= 16);
    }

    #[test]
    fn default_exec_output_tail_serialized_budget_stays_below_40_kib() {
        let result = ExecResult {
            stdout:      "o".repeat(DEFAULT_EXEC_OUTPUT_TAIL_BYTES + 128),
            stderr:      "e".repeat(DEFAULT_EXEC_OUTPUT_TAIL_BYTES + 128),
            exit_code:   Some(1),
            termination: CommandTermination::Exited,
            duration_ms: 1,
        };

        let tail = result.default_redacted_output_tail().expect("tail present");
        assert_eq!(
            tail.stdout.as_deref().map(str::len),
            Some(DEFAULT_EXEC_OUTPUT_TAIL_BYTES)
        );
        assert_eq!(
            tail.stderr.as_deref().map(str::len),
            Some(DEFAULT_EXEC_OUTPUT_TAIL_BYTES)
        );
        assert!(tail.stdout_truncated);
        assert!(tail.stderr_truncated);
        let serialized = serde_json::to_vec(&tail).expect("serialize tail");
        assert!(
            serialized.len() < 40 * 1024,
            "tail JSON was {} bytes",
            serialized.len()
        );
    }

    #[test]
    fn sandbox_tracing_events_do_not_log_raw_command_or_stdin_fields() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut failures = Vec::new();
        scan_for_command_tracing(&root, &mut failures);
        assert!(
            failures.is_empty(),
            "raw command/cmd/stdin tracing fields found:\n{}",
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

        assert_eq!(events.len(), 13, "should test all 13 variants");

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
        let result = format_lines_numbered("a\nb\nc\nd\ne", Some(2), Some(2));
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

    #[test]
    fn bash_probe_script_prints_the_marker_callers_validate() {
        assert!(
            BASH_PROBE_SCRIPT.contains(BASH_PROBE_MARKER),
            "the probe must print the marker providers check for"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_probe_accepts_only_clean_non_login_bash() {
        use tokio::process::Command;

        async fn run(program: &str, args: &[&str]) -> (Option<i32>, String) {
            let output = Command::new(program)
                .args(args)
                .arg(BASH_PROBE_SCRIPT)
                .env_remove("BASH_ENV")
                .output()
                .await
                .expect("probe should run");
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
            )
        }

        let (code, stdout) = run("bash", &["-c"]).await;
        assert!(bash_probe_passed(code, &stdout), "non-login bash: {stdout}");

        let (code, stdout) = run("bash", &["--noprofile", "-lc"]).await;
        assert!(
            !bash_probe_passed(code, &stdout),
            "a login shell must fail the probe: {stdout}"
        );

        let output = Command::new("bash")
            .args(["-c", BASH_PROBE_SCRIPT])
            .env(BASH_ENV_VAR, "/dev/null")
            .output()
            .await
            .expect("probe with BASH_ENV should run");
        assert!(
            !bash_probe_passed(
                output.status.code(),
                &String::from_utf8_lossy(&output.stdout)
            ),
            "a shell with BASH_ENV must fail the probe"
        );

        // Where `/bin/sh` is really Bash (macOS), Bash enters POSIX mode and
        // changes behavior; where it is dash (most Linux images),
        // `BASH_VERSION` is unset. The probe rejects both.
        let (code, stdout) = run("sh", &["-c"]).await;
        assert!(
            !bash_probe_passed(code, &stdout),
            "sh must fail the probe: {stdout}"
        );
    }

    #[test]
    fn bash_probe_requires_the_exact_marker_output() {
        assert!(bash_probe_passed(
            Some(0),
            &format!("  {BASH_PROBE_MARKER}\n")
        ));
        assert!(!bash_probe_passed(
            Some(0),
            &format!("prefix-{BASH_PROBE_MARKER}-suffix")
        ));
        assert!(!bash_probe_passed(
            Some(0),
            &format!("{BASH_PROBE_MARKER}\nunexpected output")
        ));
    }

    #[test]
    fn bash_probe_failure_keeps_raw_output_out_of_the_error_chain() {
        let err = validate_bash_probe(
            ExecResult {
                stdout:      String::new(),
                stderr:      "raw-probe-output".to_string(),
                exit_code:   Some(1),
                termination: CommandTermination::Exited,
                duration_ms: 1,
            },
            "Install Bash",
        )
        .expect_err("failed probe should return remediation");

        assert!(!err.display_with_causes().contains("raw-probe-output"));
        assert_eq!(
            err.default_redacted_output_tail()
                .and_then(|tail| tail.stderr),
            Some("raw-probe-output".to_string())
        );
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
                        || call.contains("stdin,")
                        || call.contains("stdin =")
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
