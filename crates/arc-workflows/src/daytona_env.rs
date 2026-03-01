use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use arc_agent::execution_env::{
    format_lines_numbered, DirEntry, ExecEnvEventCallback, ExecResult, ExecutionEnvEvent,
    ExecutionEnvironment, GrepOptions,
};
use async_trait::async_trait;
use rand::Rng;
use serde::Deserialize;

const WORKING_DIRECTORY: &str = "/home/daytona/workspace";
const DEFAULT_IMAGE: &str = "ubuntu:22.04";

/// Configuration for a Daytona cloud sandbox execution environment.
///
/// Doubles as the TOML deserialization target for `[execution.daytona]`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaytonaConfig {
    #[serde(default)]
    pub sandbox: DaytonaSandboxConfig,
    pub snapshot: Option<DaytonaSnapshotConfig>,
}

/// Sandbox-level settings (labels, auto-stop).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaytonaSandboxConfig {
    pub auto_stop_interval: Option<i32>,
    pub labels: Option<HashMap<String, String>>,
}

/// Snapshot configuration: when present, the sandbox is created from a snapshot
/// instead of a bare Docker image.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaytonaSnapshotConfig {
    pub name: String,
    pub cpu: Option<i32>,
    pub memory: Option<i32>,
    pub disk: Option<i32>,
    pub dockerfile: Option<String>,
}

/// Execution environment that runs all operations inside a Daytona cloud sandbox.
pub struct DaytonaExecutionEnvironment {
    config: DaytonaConfig,
    client: daytona_sdk::Client,
    sandbox: tokio::sync::OnceCell<daytona_sdk::Sandbox>,
    rg_available: tokio::sync::OnceCell<bool>,
    event_callback: Option<ExecEnvEventCallback>,
}

impl DaytonaExecutionEnvironment {
    #[must_use]
    pub fn new(client: daytona_sdk::Client, config: DaytonaConfig) -> Self {
        Self {
            config,
            client,
            sandbox: tokio::sync::OnceCell::new(),
            rg_available: tokio::sync::OnceCell::const_new(),
            event_callback: None,
        }
    }

    pub fn set_event_callback(&mut self, cb: ExecEnvEventCallback) {
        self.event_callback = Some(cb);
    }

    fn emit(&self, event: ExecutionEnvEvent) {
        if let Some(ref cb) = self.event_callback {
            cb(event);
        }
    }

    /// Resolve a path: relative paths are prepended with the working directory.
    fn resolve_path(&self, path: &str) -> String {
        if Path::new(path).is_absolute() {
            path.to_string()
        } else {
            format!("{WORKING_DIRECTORY}/{path}")
        }
    }

    /// Get the sandbox, returning an error if not yet initialized.
    fn sandbox(&self) -> Result<&daytona_sdk::Sandbox, String> {
        self.sandbox
            .get()
            .ok_or_else(|| "Daytona sandbox not initialized — call initialize() first".to_string())
    }

    /// Build `SandboxBaseParams` from config, generating a unique sandbox name.
    fn base_params(&self) -> daytona_sdk::SandboxBaseParams {
        let name = format!(
            "arc-{}-{:04x}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            rand::thread_rng().gen_range(0..0x10000u32),
        );
        daytona_sdk::SandboxBaseParams {
            name: Some(name),
            auto_stop_interval: self.config.sandbox.auto_stop_interval,
            labels: self.config.sandbox.labels.clone(),
            ephemeral: Some(true),
            ..Default::default()
        }
    }

    /// Ensure the named snapshot exists and is active.
    ///
    /// If the snapshot doesn't exist and a dockerfile is provided, creates it
    /// and polls until it reaches `Active` state. Returns an error if the
    /// snapshot is in a terminal failure state.
    async fn ensure_snapshot(&self, snap_cfg: &DaytonaSnapshotConfig) -> Result<(), String> {
        match self.client.snapshot.get(&snap_cfg.name).await {
            Ok(dto) => {
                use daytona_api_client::models::SnapshotState;
                match dto.state {
                    SnapshotState::Active => return Ok(()),
                    SnapshotState::Error | SnapshotState::BuildFailed => {
                        return Err(format!(
                            "Snapshot '{}' is in state '{}': {}",
                            snap_cfg.name,
                            dto.state,
                            dto.error_reason.unwrap_or_default()
                        ));
                    }
                    _ => {
                        // Building/Pending/Pulling — fall through to poll
                    }
                }
            }
            Err(daytona_sdk::DaytonaError::NotFound { .. }) => {
                let dockerfile = snap_cfg.dockerfile.as_deref().ok_or_else(|| {
                    format!(
                        "Snapshot '{}' does not exist and no dockerfile provided to create it",
                        snap_cfg.name
                    )
                })?;

                let params = daytona_sdk::CreateSnapshotParams {
                    name: snap_cfg.name.clone(),
                    image: daytona_sdk::ImageSource::Custom(
                        daytona_sdk::DockerImage::from_dockerfile(dockerfile),
                    ),
                    resources: Some(daytona_sdk::Resources {
                        cpu: snap_cfg.cpu,
                        memory: snap_cfg.memory,
                        disk: snap_cfg.disk,
                        ..Default::default()
                    }),
                    entrypoint: None,
                };
                self.client
                    .snapshot
                    .create(&params)
                    .await
                    .map_err(|e| format!("Failed to create snapshot '{}': {e}", snap_cfg.name))?;
            }
            Err(e) => {
                return Err(format!("Failed to get snapshot '{}': {e}", snap_cfg.name));
            }
        }

        // Poll until Active (or terminal failure).
        self.poll_snapshot_active(&snap_cfg.name).await
    }

    /// Poll a snapshot until it reaches `Active` state, with exponential back-off.
    async fn poll_snapshot_active(&self, name: &str) -> Result<(), String> {
        use daytona_api_client::models::SnapshotState;
        let mut delay = std::time::Duration::from_secs(2);
        let max_delay = std::time::Duration::from_secs(30);
        let deadline = Instant::now() + std::time::Duration::from_secs(600);

        while Instant::now() < deadline {
            tokio::time::sleep(delay).await;
            let dto = self
                .client
                .snapshot
                .get(name)
                .await
                .map_err(|e| format!("Failed to poll snapshot '{name}': {e}"))?;

            match dto.state {
                SnapshotState::Active => return Ok(()),
                SnapshotState::Error | SnapshotState::BuildFailed => {
                    return Err(format!(
                        "Snapshot '{name}' failed ({}): {}",
                        dto.state,
                        dto.error_reason.unwrap_or_default()
                    ));
                }
                _ => {
                    delay = (delay * 2).min(max_delay);
                }
            }
        }

        Err(format!(
            "Timed out waiting for snapshot '{name}' to become active"
        ))
    }
}

/// Convert a Git SSH URL to HTTPS format for token-based authentication.
///
/// SSH URLs like `git@github.com:owner/repo.git` become
/// `https://github.com/owner/repo.git`. URLs that are already HTTPS
/// (or any other non-SSH format) are returned unchanged.
fn ssh_url_to_https(url: &str) -> String {
    // Match `git@<host>:<path>` (standard SSH URL format)
    if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("https://{host}/{path}");
        }
    }
    // Match `ssh://git@<host>/<path>`
    if let Some(rest) = url.strip_prefix("ssh://git@") {
        return format!("https://{rest}");
    }
    url.to_string()
}

/// Detect the git remote URL and current branch from a local repository.
///
/// Uses `git2` to discover the repo at `path`, reads the `origin` remote URL
/// and the HEAD branch name.
pub fn detect_repo_info(path: &Path) -> Result<(String, Option<String>), String> {
    let repo = git2::Repository::discover(path)
        .map_err(|e| format!("Failed to discover git repo at {}: {e}", path.display()))?;

    let url = repo
        .find_remote("origin")
        .map_err(|e| format!("Failed to find 'origin' remote: {e}"))?
        .url()
        .ok_or_else(|| "origin remote URL is not valid UTF-8".to_string())?
        .to_string();

    let branch = repo
        .head()
        .ok()
        .and_then(|head| head.shorthand().map(String::from));

    Ok((url, branch))
}

/// Get a GitHub authentication token via `gh auth token`.
pub fn get_gh_token() -> Result<String, String> {
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|e| format!("Failed to run 'gh auth token': {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "gh auth token failed (exit code {}): {stderr}",
            output.status.code().unwrap_or(-1)
        ));
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err("gh auth token returned empty string".to_string());
    }
    Ok(token)
}

#[async_trait]
impl ExecutionEnvironment for DaytonaExecutionEnvironment {
    async fn initialize(&self) -> Result<(), String> {
        self.emit(ExecutionEnvEvent::Initializing {
            env_type: "daytona".into(),
        });
        let init_start = Instant::now();

        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        let params = if let Some(ref snap_cfg) = self.config.snapshot {
            self.emit(ExecutionEnvEvent::SnapshotEnsuring {
                name: snap_cfg.name.clone(),
            });
            let snap_start = Instant::now();
            if let Err(e) = self.ensure_snapshot(snap_cfg).await {
                self.emit(ExecutionEnvEvent::SnapshotFailed {
                    name: snap_cfg.name.clone(),
                    error: e.clone(),
                });
                let duration_ms =
                    u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.emit(ExecutionEnvEvent::InitializeFailed {
                    env_type: "daytona".into(),
                    error: e.clone(),
                    duration_ms,
                });
                return Err(e);
            }
            let snap_duration = u64::try_from(snap_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(ExecutionEnvEvent::SnapshotReady {
                name: snap_cfg.name.clone(),
                duration_ms: snap_duration,
            });

            daytona_sdk::CreateParams::Snapshot(daytona_sdk::SnapshotParams {
                base: self.base_params(),
                snapshot: snap_cfg.name.clone(),
            })
        } else {
            daytona_sdk::CreateParams::Image(daytona_sdk::ImageParams {
                base: self.base_params(),
                image: daytona_sdk::ImageSource::Name(DEFAULT_IMAGE.to_string()),
                resources: None,
            })
        };

        let sandbox = self
            .client
            .create(params, daytona_sdk::CreateSandboxOptions::default())
            .await
            .map_err(|e| {
                let err = format!("Failed to create Daytona sandbox: {e}");
                let duration_ms =
                    u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.emit(ExecutionEnvEvent::InitializeFailed {
                    env_type: "daytona".into(),
                    error: err.clone(),
                    duration_ms,
                });
                err
            })?;

        // Clone the repo into the sandbox
        match detect_repo_info(&cwd) {
            Ok((detected_url, branch)) => {
                // Daytona clones over HTTPS with token auth, so rewrite SSH URLs.
                let url = ssh_url_to_https(&detected_url);
                self.emit(ExecutionEnvEvent::GitCloneStarted {
                    url: url.clone(),
                    branch: branch.clone(),
                });
                let clone_start = Instant::now();

                let token = get_gh_token()
                    .map_err(|e| format!("Failed to get GitHub token for Daytona clone: {e}"));
                let token = match token {
                    Ok(t) => t,
                    Err(e) => {
                        self.emit(ExecutionEnvEvent::GitCloneFailed {
                            url: url.clone(),
                            error: e.clone(),
                        });
                        let duration_ms =
                            u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                        self.emit(ExecutionEnvEvent::InitializeFailed {
                            env_type: "daytona".into(),
                            error: e.clone(),
                            duration_ms,
                        });
                        return Err(e);
                    }
                };

                let git_svc = sandbox
                    .git()
                    .await
                    .map_err(|e| format!("Failed to get Daytona git service: {e}"));
                let git_svc = match git_svc {
                    Ok(g) => g,
                    Err(e) => {
                        self.emit(ExecutionEnvEvent::GitCloneFailed {
                            url: url.clone(),
                            error: e.clone(),
                        });
                        let duration_ms =
                            u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                        self.emit(ExecutionEnvEvent::InitializeFailed {
                            env_type: "daytona".into(),
                            error: e.clone(),
                            duration_ms,
                        });
                        return Err(e);
                    }
                };

                match git_svc
                    .clone(
                        &url,
                        WORKING_DIRECTORY,
                        daytona_sdk::GitCloneOptions {
                            branch,
                            username: Some("x-access-token".to_string()),
                            password: Some(token),
                            ..Default::default()
                        },
                    )
                    .await
                {
                    Ok(()) => {
                        let clone_duration =
                            u64::try_from(clone_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                        self.emit(ExecutionEnvEvent::GitCloneCompleted {
                            url,
                            duration_ms: clone_duration,
                        });
                    }
                    Err(e) => {
                        let err = format!("Failed to clone repo into Daytona sandbox: {e}");
                        self.emit(ExecutionEnvEvent::GitCloneFailed {
                            url,
                            error: err.clone(),
                        });
                        let duration_ms =
                            u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                        self.emit(ExecutionEnvEvent::InitializeFailed {
                            env_type: "daytona".into(),
                            error: err.clone(),
                            duration_ms,
                        });
                        return Err(err);
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: could not detect git repo for Daytona clone: {e}");
                // Create working directory even without a repo
                let fs_svc = sandbox
                    .fs()
                    .await
                    .map_err(|e| format!("Failed to get Daytona fs service: {e}"))?;
                fs_svc
                    .create_folder(WORKING_DIRECTORY, None)
                    .await
                    .map_err(|e| format!("Failed to create working directory: {e}"))?;
            }
        }

        self.sandbox
            .set(sandbox)
            .map_err(|_| "Daytona sandbox already initialized".to_string())?;

        let init_duration = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(ExecutionEnvEvent::Ready {
            env_type: "daytona".into(),
            duration_ms: init_duration,
        });

        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        self.emit(ExecutionEnvEvent::CleanupStarted {
            env_type: "daytona".into(),
        });
        let start = Instant::now();
        if let Some(sandbox) = self.sandbox.get() {
            if let Err(e) = sandbox.delete().await {
                let err = format!("Failed to delete Daytona sandbox: {e}");
                self.emit(ExecutionEnvEvent::CleanupFailed {
                    env_type: "daytona".into(),
                    error: err.clone(),
                });
                return Err(err);
            }
        }
        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(ExecutionEnvEvent::CleanupCompleted {
            env_type: "daytona".into(),
            duration_ms,
        });
        Ok(())
    }

    fn working_directory(&self) -> &str {
        WORKING_DIRECTORY
    }

    fn platform(&self) -> &str {
        "linux"
    }

    fn os_version(&self) -> String {
        "Linux (Daytona)".to_string()
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| format!("Failed to get fs service: {e}"))?;

        let bytes = fs_svc
            .download_file(&resolved)
            .await
            .map_err(|e| format!("Failed to read file {resolved}: {e}"))?;

        let content =
            String::from_utf8(bytes).map_err(|e| format!("File is not valid UTF-8: {e}"))?;

        Ok(format_lines_numbered(&content, offset, limit))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        // Ensure parent directory exists
        if let Some(parent) = Path::new(&resolved).parent() {
            let parent_str = parent.to_string_lossy();
            if parent_str != "/" {
                let fs_svc = sandbox
                    .fs()
                    .await
                    .map_err(|e| format!("Failed to get fs service: {e}"))?;
                let _ = fs_svc.create_folder(&parent_str, None).await;
            }
        }

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| format!("Failed to get fs service: {e}"))?;

        fs_svc
            .upload_file_bytes(&resolved, content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write file {resolved}: {e}"))?;

        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<(), String> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| format!("Failed to get fs service: {e}"))?;

        fs_svc
            .delete_file(&resolved, false)
            .await
            .map_err(|e| format!("Failed to delete file {resolved}: {e}"))?;

        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| format!("Failed to get fs service: {e}"))?;

        match fs_svc.get_file_info(&resolved).await {
            Ok(_) => Ok(true),
            Err(daytona_sdk::DaytonaError::NotFound { .. }) => Ok(false),
            Err(e) => Err(format!("Failed to check file existence {resolved}: {e}")),
        }
    }

    async fn list_directory(
        &self,
        path: &str,
        _depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| format!("Failed to get fs service: {e}"))?;

        let files = fs_svc
            .list_files(&resolved)
            .await
            .map_err(|e| format!("Failed to list directory {resolved}: {e}"))?;

        Ok(files
            .into_iter()
            .map(|f| DirEntry {
                name: f.name,
                is_dir: f.is_dir,
                size: if f.size > 0 {
                    Some(f.size as u64)
                } else {
                    None
                },
            })
            .collect())
    }

    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        _env_vars: Option<&HashMap<String, String>>,
        _cancel_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<ExecResult, String> {
        let sandbox = self.sandbox()?;
        let start = Instant::now();

        let cwd = working_dir
            .map(|d| self.resolve_path(d))
            .unwrap_or_else(|| WORKING_DIRECTORY.to_string());

        let process_svc = sandbox
            .process()
            .await
            .map_err(|e| format!("Failed to get process service: {e}"))?;

        let options = daytona_sdk::ExecuteCommandOptions {
            cwd: Some(cwd),
            timeout: Some(std::time::Duration::from_millis(timeout_ms)),
            ..Default::default()
        };

        // Wrap with `bash -c` so pipes, env vars, and shell features work.
        // The Daytona API uses direct exec, not a shell.
        let wrapped = wrap_bash_command(command);
        let result = process_svc
            .execute_command(&wrapped, options)
            .await
            .map_err(|e| format!("Failed to execute command: {e}"))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        // The Daytona SDK returns combined output in `result` field.
        // Separate stderr isn't available in the simple execute_command API.
        Ok(ExecResult {
            stdout: result.result.clone(),
            stderr: String::new(),
            exit_code: result.exit_code,
            timed_out: false,
            duration_ms,
        })
    }

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> Result<Vec<String>, String> {
        let resolved = self.resolve_path(path);

        // Detect ripgrep availability (cached)
        let use_rg = *self
            .rg_available
            .get_or_init(|| async {
                let result = self
                    .exec_command("rg --version", 10_000, None, None, None)
                    .await;
                matches!(result, Ok(r) if r.exit_code == 0)
            })
            .await;

        let cmd = if use_rg {
            let mut cmd = "rg --line-number --no-heading".to_string();
            if options.case_insensitive {
                cmd.push_str(" -i");
            }
            if let Some(ref glob_filter) = options.glob_filter {
                cmd.push_str(&format!(" --glob '{glob_filter}'"));
            }
            if let Some(max) = options.max_results {
                cmd.push_str(&format!(" --max-count {max}"));
            }
            cmd.push_str(&format!(
                " -- '{}' '{}'",
                pattern.replace('\'', "'\\''"),
                resolved
            ));
            cmd
        } else {
            let mut cmd = "grep -rn".to_string();
            if options.case_insensitive {
                cmd.push_str(" -i");
            }
            if let Some(ref glob_filter) = options.glob_filter {
                cmd.push_str(&format!(" --include '{glob_filter}'"));
            }
            if let Some(max) = options.max_results {
                cmd.push_str(&format!(" -m {max}"));
            }
            cmd.push_str(&format!(
                " -- '{}' '{}'",
                pattern.replace('\'', "'\\''"),
                resolved
            ));
            cmd
        };

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;

        if result.exit_code == 1 {
            // Both rg and grep exit 1 for no matches
            return Ok(Vec::new());
        }
        if result.exit_code != 0 {
            return Err(format!(
                "grep failed (exit {}): {}",
                result.exit_code, result.stderr
            ));
        }

        Ok(result.stdout.lines().map(String::from).collect())
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String> {
        let base = path
            .map(|p| self.resolve_path(p))
            .unwrap_or_else(|| WORKING_DIRECTORY.to_string());

        let cmd = format!(
            "find '{}' -name '{}' -type f | sort",
            base.replace('\'', "'\\''"),
            pattern.replace('\'', "'\\''"),
        );

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;

        if result.exit_code != 0 {
            return Err(format!(
                "glob failed (exit {}): {}",
                result.exit_code, result.stderr
            ));
        }

        Ok(result
            .stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }
}

/// Wrap a command string with `bash -c '...'`, escaping single quotes.
///
/// The Daytona API uses direct exec (not a shell), so pipes, env vars,
/// semicolons, etc. won't work without this wrapper.
///
/// Uses base64 encoding (matching the TypeScript/Python/Ruby Daytona SDKs)
/// to avoid shell escaping issues with quotes and special characters.
fn wrap_bash_command(command: &str) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(command);
    format!("sh -c \"echo '{encoded}' | base64 -d | sh\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daytona_config_defaults() {
        let config = DaytonaConfig::default();
        assert!(config.snapshot.is_none());
        assert!(config.sandbox.auto_stop_interval.is_none());
        assert!(config.sandbox.labels.is_none());
    }

    #[test]
    fn wrap_bash_uses_base64_encoding() {
        let wrapped = wrap_bash_command("echo hello");
        // Should use base64 pipe to sh
        assert!(
            wrapped.starts_with("sh -c \"echo '"),
            "should start with sh -c wrapper"
        );
        assert!(
            wrapped.ends_with("' | base64 -d | sh\""),
            "should end with base64 -d | sh"
        );
        // The base64 of "echo hello" is "ZWNobyBoZWxsbw=="
        assert!(
            wrapped.contains("ZWNobyBoZWxsbw=="),
            "should contain base64 of 'echo hello'"
        );
    }

    #[test]
    fn wrap_bash_handles_single_quotes_safely() {
        // Single quotes in the original command are safely encoded in base64
        let wrapped = wrap_bash_command("echo 'hello world'");
        assert!(
            wrapped.starts_with("sh -c \"echo '"),
            "should use sh -c wrapper"
        );
        // No raw single quotes from the original command should appear in the base64
        assert!(
            !wrapped.contains("hello world"),
            "original command should be base64 encoded, not literal"
        );
    }

    #[test]
    fn wrap_bash_handles_pipes() {
        let wrapped = wrap_bash_command("ls | grep foo");
        assert!(
            wrapped.starts_with("sh -c \"echo '"),
            "should use sh -c wrapper"
        );
        assert!(
            wrapped.ends_with("' | base64 -d | sh\""),
            "should end with base64 -d | sh"
        );
    }

    #[test]
    fn ssh_url_to_https_converts_git_at_syntax() {
        assert_eq!(
            ssh_url_to_https("git@github.com:brynary/arc.git"),
            "https://github.com/brynary/arc.git"
        );
    }

    #[test]
    fn ssh_url_to_https_converts_ssh_protocol() {
        assert_eq!(
            ssh_url_to_https("ssh://git@github.com/brynary/arc.git"),
            "https://github.com/brynary/arc.git"
        );
    }

    #[test]
    fn ssh_url_to_https_passes_through_https() {
        assert_eq!(
            ssh_url_to_https("https://github.com/brynary/arc.git"),
            "https://github.com/brynary/arc.git"
        );
    }

    #[test]
    fn detect_git_remote_from_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        repo.remote("origin", "https://github.com/org/repo.git")
            .unwrap();

        let (url, _branch) = detect_repo_info(dir.path()).unwrap();
        assert_eq!(url, "https://github.com/org/repo.git");
    }

    #[test]
    fn detect_git_branch_from_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        // Create an initial commit so HEAD points to a branch
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        repo.remote("origin", "https://github.com/org/repo.git")
            .unwrap();

        let (_, branch) = detect_repo_info(dir.path()).unwrap();
        // git init creates "master" or "main" depending on git config
        assert!(branch.is_some());
    }

    #[test]
    #[ignore] // requires `gh` CLI installed and authenticated
    fn gh_auth_token_returns_nonempty_string() {
        let token = get_gh_token().unwrap();
        assert!(!token.is_empty());
    }
}
