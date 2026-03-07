use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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
        impl $crate::sandbox::Sandbox for $type {
            $($custom)*

            async fn file_exists(&self, path: &str) -> Result<bool, String> {
                self.$field.file_exists(path).await
            }

            async fn list_directory(
                &self,
                path: &str,
                depth: Option<usize>,
            ) -> Result<Vec<$crate::sandbox::DirEntry>, String> {
                self.$field.list_directory(path, depth).await
            }

            async fn exec_command(
                &self,
                command: &str,
                timeout_ms: u64,
                working_dir: Option<&str>,
                env_vars: Option<&std::collections::HashMap<String, String>>,
                cancel_token: Option<tokio_util::sync::CancellationToken>,
            ) -> Result<$crate::sandbox::ExecResult, String> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockSandbox;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn mock_env_read_file() {
        let mut files = HashMap::new();
        files.insert("test.rs".into(), "hello".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let result = env.read_file("test.rs", None, None).await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn mock_env_exec_command() {
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let result = env
            .exec_command("echo", 5000, None, None, None)
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn mock_env_list_directory() {
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let entries = env.list_directory("/tmp", None).await.unwrap();
        assert_eq!(entries.len(), 0);
    }

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
    fn mock_env_platform() {
        let env = MockSandbox::default();
        assert_eq!(env.platform(), "darwin");
        assert_eq!(env.working_directory(), "/tmp/test");
        assert_eq!(env.os_version(), "Darwin 24.0.0");
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
}
