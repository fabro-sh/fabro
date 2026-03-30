use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Git command prefix that disables background maintenance.
const GIT: &str = "git -c maintenance.auto=0 -c gc.auto=0";

/// Information returned when a sandbox sets up git for a workflow run.
pub struct GitRunInfo {
    pub base_sha: String,
    pub run_branch: String,
    pub base_branch: Option<String>,
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
///         async fn read_file(&self, path: &str, offset: Option<usize>, limit: Option<usize>) -> Result<String, String> {
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

            async fn file_exists(&self, path: &str) -> Result<bool, String> {
                self.$field.file_exists(path).await
            }

            async fn list_directory(
                &self,
                path: &str,
                depth: Option<usize>,
            ) -> Result<Vec<$crate::DirEntry>, String> {
                self.$field.list_directory(path, depth).await
            }

            async fn exec_command(
                &self,
                command: &str,
                timeout_ms: u64,
                working_dir: Option<&str>,
                env_vars: Option<&std::collections::HashMap<String, String>>,
                cancel_token: Option<tokio_util::sync::CancellationToken>,
            ) -> Result<$crate::ExecResult, String> {
                self.$field
                    .exec_command(command, timeout_ms, working_dir, env_vars, cancel_token)
                    .await
            }

            async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String> {
                self.$field.glob(pattern, path).await
            }

            async fn download_file_to_local(
                &self,
                remote_path: &str,
                local_path: &std::path::Path,
            ) -> Result<(), String> {
                self.$field.download_file_to_local(remote_path, local_path).await
            }

            async fn upload_file_from_local(
                &self,
                local_path: &std::path::Path,
                remote_path: &str,
            ) -> Result<(), String> {
                self.$field.upload_file_from_local(local_path, remote_path).await
            }

            async fn initialize(&self) -> Result<(), String> {
                self.$field.initialize().await
            }

            async fn cleanup(&self) -> Result<(), String> {
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

            async fn refresh_push_credentials(&self) -> Result<(), String> {
                self.$field.refresh_push_credentials().await
            }

            async fn set_autostop_interval(&self, minutes: i32) -> Result<(), String> {
                self.$field.set_autostop_interval(minutes).await
            }

            async fn setup_git_for_run(&self, run_id: &str) -> Result<Option<$crate::GitRunInfo>, String> {
                self.$field.setup_git_for_run(run_id).await
            }

            fn resume_setup_commands(&self, run_branch: &str) -> Vec<String> {
                self.$field.resume_setup_commands(run_branch)
            }

            async fn git_push_branch(&self, branch: &str) -> bool {
                self.$field.git_push_branch(branch).await
            }

            fn host_git_dir(&self) -> Option<&str> {
                self.$field.host_git_dir()
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

            async fn ssh_access_command(&self) -> Result<Option<String>, String> {
                self.$field.ssh_access_command().await
            }

            fn origin_url(&self) -> Option<&str> {
                self.$field.origin_url()
            }

            async fn get_preview_url(&self, port: u16) -> Result<Option<(String, std::collections::HashMap<String, String>)>, String> {
                self.$field.get_preview_url(port).await
            }

            async fn read_file(
                &self,
                path: &str,
                offset: Option<usize>,
                limit: Option<usize>,
            ) -> Result<String, String> {
                self.$field.read_file(path, offset, limit).await
            }

            async fn grep(
                &self,
                pattern: &str,
                path: &str,
                options: &$crate::GrepOptions,
            ) -> Result<Vec<String>, String> {
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
        provider: String,
        duration_ms: u64,
        name: Option<String>,
        cpu: Option<f64>,
        memory: Option<f64>,
        url: Option<String>,
    },
    InitializeFailed {
        provider: String,
        error: String,
        duration_ms: u64,
    },
    CleanupStarted {
        provider: String,
    },
    CleanupCompleted {
        provider: String,
        duration_ms: u64,
    },
    CleanupFailed {
        provider: String,
        error: String,
    },

    // -- Docker --
    SnapshotPulling {
        name: String,
    },
    SnapshotPulled {
        name: String,
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
        name: String,
        duration_ms: u64,
    },
    SnapshotFailed {
        name: String,
        error: String,
    },

    // -- Daytona git --
    GitCloneStarted {
        url: String,
        branch: Option<String>,
    },
    GitCloneCompleted {
        url: String,
        duration_ms: u64,
    },
    GitCloneFailed {
        url: String,
        error: String,
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
                duration_ms,
            } => {
                error!(provider, error, duration_ms, "Sandbox init failed");
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
            Self::CleanupFailed { provider, error } => {
                warn!(provider, error, "Sandbox cleanup failed");
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
            Self::SnapshotFailed { name, error } => {
                error!(name, error, "Snapshot failed");
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
            Self::GitCloneFailed { url, error } => {
                error!(url, error, "Git clone failed");
            }
        }
    }
}

/// Callback type for sandbox events.
pub type SandboxEventCallback = Arc<dyn Fn(SandboxEvent) + Send + Sync>;

/// Formats file content with line numbers for display.
///
/// Applies optional offset (0-based lines to skip) and limit (max lines to return).
/// Line numbers are 1-based and right-aligned.
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
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct GrepOptions {
    pub glob_filter: Option<String>,
    pub case_insensitive: bool,
    pub max_results: Option<usize>,
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, String>;
    async fn write_file(&self, path: &str, content: &str) -> Result<(), String>;
    async fn delete_file(&self, path: &str) -> Result<(), String>;
    async fn file_exists(&self, path: &str) -> Result<bool, String>;
    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String>;
    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&std::collections::HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String>;
    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> Result<Vec<String>, String>;
    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String>;
    /// Copy a file from the sandbox to a local filesystem path.
    /// Handles binary files correctly across all sandbox types.
    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<(), String>;
    /// Copy a file from the local filesystem into the sandbox.
    /// Handles binary files correctly across all sandbox types.
    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<(), String>;
    async fn initialize(&self) -> Result<(), String>;
    async fn cleanup(&self) -> Result<(), String>;
    fn working_directory(&self) -> &str;
    fn platform(&self) -> &str;
    fn os_version(&self) -> String;
    /// Return a human-readable identifier for the sandbox (e.g. container ID, sandbox name).
    /// Used when `--preserve-sandbox` is active to tell the user how to reconnect.
    fn sandbox_info(&self) -> String {
        String::new()
    }

    /// Refresh git push credentials (e.g. rotate an expiring GitHub App token).
    /// Default is a no-op; Daytona overrides to update the remote URL with a fresh token.
    async fn refresh_push_credentials(&self) -> Result<(), String> {
        Ok(())
    }

    /// Set the auto-stop interval in minutes (0 to disable).
    /// Default is a no-op; Daytona overrides to call the Daytona API.
    async fn set_autostop_interval(&self, _minutes: i32) -> Result<(), String> {
        Ok(())
    }

    /// Set up git state for a new workflow run.
    /// Sandboxes that manage their own git clone (e.g., remote VMs) should create
    /// a run branch and return the git info. Local sandboxes return `None`.
    async fn setup_git_for_run(&self, _run_id: &str) -> Result<Option<GitRunInfo>, String> {
        Ok(None)
    }

    /// Commands to run inside the sandbox when resuming on an existing run branch.
    fn resume_setup_commands(&self, _run_branch: &str) -> Vec<String> {
        Vec::new()
    }

    /// Push a run branch to origin from inside the sandbox.
    /// Returns `true` if the push was handled. When `false`, the engine will
    /// attempt a host-side push instead.
    async fn git_push_branch(&self, _branch: &str) -> bool {
        false
    }

    /// The host-accessible path to this sandbox's git worktree, if applicable.
    /// When `Some`, the engine runs git operations (add, commit) from the host.
    fn host_git_dir(&self) -> Option<&str> {
        None
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

    /// Return an SSH command string for connecting to this sandbox, if supported.
    async fn ssh_access_command(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// The display URL of the cloned origin remote, if known.
    fn origin_url(&self) -> Option<&str> {
        None
    }

    /// Get an authenticated preview URL for a port exposed by this sandbox.
    /// Returns `Ok(None)` when the sandbox does not support port previews.
    /// Used to connect to services (e.g. MCP servers) running inside the sandbox.
    async fn get_preview_url(
        &self,
        _port: u16,
    ) -> Result<Option<(String, HashMap<String, String>)>, String> {
        Ok(None)
    }

    /// Record that the agent has explicitly read (seen) the given file path.
    /// Called by tool executors after agent-visible reads (e.g. `read_file`, `grep`).
    /// Default is a no-op; `ReadBeforeWriteSandbox` overrides to populate its read set.
    fn mark_agent_read(&self, _path: &str) {}
}

/// Resolve a path: relative paths are prepended with the working directory.
/// Used by the Daytona sandbox implementation.
#[cfg(feature = "daytona")]
pub(crate) fn resolve_path(path: &str, working_dir: &str) -> String {
    if std::path::Path::new(path).is_absolute() {
        path.to_string()
    } else {
        format!("{working_dir}/{path}")
    }
}

/// Shell-quote a string using `shlex::try_quote`, with a fallback for edge cases.
pub fn shell_quote(s: &str) -> String {
    shlex::try_quote(s).map_or_else(
        |_| format!("'{}'", s.replace('\'', "'\\''")),
        |q| q.to_string(),
    )
}

/// Helper for sandbox implementations that manage git internally.
/// Executes git commands inside the sandbox to create a run branch.
pub async fn setup_git_via_exec(sandbox: &dyn Sandbox, run_id: &str) -> Result<GitRunInfo, String> {
    // Get current branch name
    let branch_result = sandbox
        .exec_command("git rev-parse --abbrev-ref HEAD", 10_000, None, None, None)
        .await
        .map_err(|e| format!("git rev-parse --abbrev-ref HEAD failed: {e}"))?;
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

    // Get current HEAD as base SHA
    let sha_result = sandbox
        .exec_command("git rev-parse HEAD", 10_000, None, None, None)
        .await
        .map_err(|e| format!("git rev-parse HEAD failed: {e}"))?;
    if sha_result.exit_code != 0 {
        return Err(format!(
            "git rev-parse HEAD failed (exit {}): {}",
            sha_result.exit_code, sha_result.stderr
        ));
    }
    let base_sha = sha_result.stdout.trim().to_string();

    let branch_name = format!("fabro/run/{run_id}");

    // Create and checkout a run branch
    let checkout_cmd = format!("git checkout -b {branch_name}");
    let checkout_result = sandbox
        .exec_command(&checkout_cmd, 10_000, None, None, None)
        .await
        .map_err(|e| format!("git checkout failed: {e}"))?;
    if checkout_result.exit_code != 0 {
        return Err(format!(
            "git checkout -b failed (exit {}): {}",
            checkout_result.exit_code, checkout_result.stderr
        ));
    }

    Ok(GitRunInfo {
        base_sha,
        run_branch: branch_name,
        base_branch,
    })
}

/// Helper for sandbox implementations that manage git internally.
/// Pushes a branch to origin via exec_command inside the sandbox.
pub async fn git_push_via_exec(sandbox: &dyn Sandbox, branch: &str) -> bool {
    if let Err(e) = sandbox.refresh_push_credentials().await {
        tracing::warn!(error = %e, "Failed to refresh push credentials");
    }
    let cmd = format!("{GIT} push origin {branch}");
    match sandbox.exec_command(&cmd, 60_000, None, None, None).await {
        Ok(r) if r.exit_code == 0 => {
            tracing::info!(branch, "Pushed run branch to origin");
            true
        }
        Ok(r) => {
            tracing::warn!(branch, exit_code = r.exit_code, "Failed to push run branch");
            false
        }
        Err(e) => {
            tracing::warn!(branch, error = %e, "Failed to push run branch");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_result_fields() {
        let result = ExecResult {
            stdout: "out".into(),
            stderr: "err".into(),
            exit_code: 1,
            timed_out: true,
            duration_ms: 5000,
        };
        assert_eq!(result.exit_code, 1);
        assert!(result.timed_out);
        assert_eq!(result.duration_ms, 5000);
    }

    #[test]
    fn dir_entry_fields() {
        let entry = DirEntry {
            name: "src".into(),
            is_dir: true,
            size: None,
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
                provider: "local".into(),
                duration_ms: 50,
                name: None,
                cpu: None,
                memory: None,
                url: None,
            },
            SandboxEvent::InitializeFailed {
                provider: "docker".into(),
                error: "no daemon".into(),
                duration_ms: 100,
            },
            SandboxEvent::CleanupStarted {
                provider: "daytona".into(),
            },
            SandboxEvent::CleanupCompleted {
                provider: "daytona".into(),
                duration_ms: 200,
            },
            SandboxEvent::CleanupFailed {
                provider: "docker".into(),
                error: "container gone".into(),
            },
            SandboxEvent::SnapshotPulling {
                name: "ubuntu:22.04".into(),
            },
            SandboxEvent::SnapshotPulled {
                name: "ubuntu:22.04".into(),
                duration_ms: 5000,
            },
            SandboxEvent::SnapshotEnsuring {
                name: "my-snap".into(),
            },
            SandboxEvent::SnapshotCreating {
                name: "my-snap".into(),
            },
            SandboxEvent::SnapshotReady {
                name: "my-snap".into(),
                duration_ms: 30000,
            },
            SandboxEvent::SnapshotFailed {
                name: "my-snap".into(),
                error: "build failed".into(),
            },
            SandboxEvent::GitCloneStarted {
                url: "https://github.com/org/repo.git".into(),
                branch: Some("main".into()),
            },
            SandboxEvent::GitCloneCompleted {
                url: "https://github.com/org/repo.git".into(),
                duration_ms: 8000,
            },
            SandboxEvent::GitCloneFailed {
                url: "https://github.com/org/repo.git".into(),
                error: "auth failed".into(),
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
