use std::collections::HashMap;
use std::time::Instant;

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, LogOutput, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions, UploadToContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::HostConfig;
use futures::StreamExt;
use tokio::sync::OnceCell;
use tokio::{fs, time};
use tokio_util::sync::CancellationToken;

use crate::{
    DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback,
    format_lines_numbered,
};

/// Configuration for a Docker-based sandbox.
pub struct DockerSandboxOptions {
    /// Docker image to use. Default: `"fabro-agent:latest"`.
    pub image:                  String,
    /// Host directory to bind-mount into the container.
    pub host_working_directory: String,
    /// Mount point inside the container. Default: `"/workspace"`.
    pub container_mount_point:  String,
    /// Docker network mode. Default: `Some("bridge")`.
    pub network_mode:           Option<String>,
    /// Additional `"host_path:container_path"` bind mounts.
    pub extra_mounts:           Vec<String>,
    /// Memory limit in bytes. `None` = unlimited.
    pub memory_limit:           Option<i64>,
    /// CPU quota (microseconds per 100ms period). `None` = unlimited.
    pub cpu_quota:              Option<i64>,
    /// Whether to pull the image if not found locally. Default: `true`.
    pub auto_pull:              bool,
    /// Additional `KEY=VALUE` environment variables for the container.
    pub env_vars:               Vec<String>,
}

impl Default for DockerSandboxOptions {
    fn default() -> Self {
        Self {
            image:                  "fabro-agent:latest".to_string(),
            host_working_directory: String::new(),
            container_mount_point:  "/workspace".to_string(),
            network_mode:           Some("bridge".to_string()),
            extra_mounts:           Vec::new(),
            memory_limit:           None,
            cpu_quota:              None,
            auto_pull:              true,
            env_vars:               Vec::new(),
        }
    }
}

/// Sandbox that runs all operations inside a Docker container.
///
/// The host working directory is bind-mounted at `container_mount_point`. All
/// file operations, commands, grep, and glob execute inside the container via
/// `docker exec`.
pub struct DockerSandbox {
    docker:            Docker,
    config:            DockerSandboxOptions,
    container_id:      OnceCell<String>,
    cached_platform:   std::sync::OnceLock<String>,
    cached_os_version: std::sync::OnceLock<String>,
    rg_available:      OnceCell<bool>,
    event_callback:    Option<SandboxEventCallback>,
}

impl DockerSandbox {
    /// Creates a new `DockerSandbox`.
    ///
    /// Validates Docker daemon connectivity but does NOT create a container.
    /// Call `initialize()` to create and start the container.
    pub fn new(config: DockerSandboxOptions) -> Result<Self, String> {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| format!("Failed to connect to Docker daemon: {e}"))?;
        Ok(Self {
            docker,
            config,
            container_id: OnceCell::new(),
            cached_platform: std::sync::OnceLock::new(),
            cached_os_version: std::sync::OnceLock::new(),
            rg_available: OnceCell::const_new(),
            event_callback: None,
        })
    }

    pub fn set_event_callback(&mut self, cb: SandboxEventCallback) {
        self.event_callback = Some(cb);
    }

    fn emit(&self, event: SandboxEvent) {
        event.trace();
        if let Some(ref cb) = self.event_callback {
            cb(event);
        }
    }

    fn container_id(&self) -> Result<&str, String> {
        self.container_id
            .get()
            .map(String::as_str)
            .ok_or_else(|| "Container not initialized — call initialize() first".to_string())
    }

    /// Resolves a path for use inside the container.
    /// Absolute paths are used as-is; relative paths are prepended with the
    /// mount point.
    fn resolve_container_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{path}", self.config.container_mount_point)
        }
    }

    /// Maps a container-space path to the corresponding host-space path
    /// using the bind-mount configuration.
    fn container_to_host_path(&self, remote_path: &str) -> Result<std::path::PathBuf, String> {
        let container_path = self.resolve_container_path(remote_path);
        if container_path.starts_with(&self.config.container_mount_point) {
            let relative = &container_path[self.config.container_mount_point.len()..];
            let relative = relative.strip_prefix('/').unwrap_or(relative);
            Ok(std::path::PathBuf::from(&self.config.host_working_directory).join(relative))
        } else {
            Err(format!(
                "Path {container_path} is outside the bind-mounted directory {}",
                self.config.container_mount_point
            ))
        }
    }

    /// Executes a command inside the container, returning `(stdout, stderr,
    /// exit_code)`.
    async fn docker_exec(
        &self,
        cmd: Vec<String>,
        working_dir: Option<&str>,
        env: Option<Vec<String>>,
    ) -> Result<(String, String, i32), String> {
        let container_id = self.container_id()?;

        let exec_opts = CreateExecOptions {
            cmd: Some(cmd),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            working_dir: working_dir.map(ToString::to_string),
            env: env.map(|e| e.into_iter().collect()),
            ..Default::default()
        };

        let exec_instance = self
            .docker
            .create_exec(container_id, exec_opts)
            .await
            .map_err(|e| format!("Failed to create exec: {e}"))?;

        let start_result = self
            .docker
            .start_exec(&exec_instance.id, None)
            .await
            .map_err(|e| format!("Failed to start exec: {e}"))?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let StartExecResults::Attached { mut output, .. } = start_result {
            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(LogOutput::StdOut { message }) => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(LogOutput::StdErr { message }) => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(_) => {}
                    Err(e) => return Err(format!("Error reading exec output: {e}")),
                }
            }
        }

        let inspect = self
            .docker
            .inspect_exec(&exec_instance.id)
            .await
            .map_err(|e| format!("Failed to inspect exec: {e}"))?;

        let exit_code = i32::try_from(inspect.exit_code.unwrap_or(-1)).unwrap();
        Ok((stdout, stderr, exit_code))
    }

    /// Runs a shell command inside the container with timeout and cancellation
    /// support.
    async fn docker_exec_shell(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        let start = Instant::now();

        let effective_dir = working_dir.map_or_else(
            || self.config.container_mount_point.clone(),
            ToString::to_string,
        );

        let env: Option<Vec<String>> =
            env_vars.map(|vars| vars.iter().map(|(k, v)| format!("{k}={v}")).collect());

        let cmd = vec![
            "/bin/bash".to_string(),
            "-c".to_string(),
            command.to_string(),
        ];

        let timeout_duration = std::time::Duration::from_millis(timeout_ms);
        let token = cancel_token.unwrap_or_default();

        tokio::select! {
            result = self.docker_exec(cmd, Some(&effective_dir), env) => {
                let (stdout, stderr, exit_code) = result?;
                let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                Ok(ExecResult {
                    stdout,
                    stderr,
                    exit_code,
                    timed_out: false,
                    duration_ms,
                })
            }
            () = time::sleep(timeout_duration) => {
                let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command timed out".to_string(),
                    exit_code: -1,
                    timed_out: true,
                    duration_ms,
                })
            }
            () = token.cancelled() => {
                let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command cancelled".to_string(),
                    exit_code: -1,
                    timed_out: true,
                    duration_ms,
                })
            }
        }
    }

    /// Pulls the configured image if `auto_pull` is enabled and the image is
    /// not found locally.
    async fn ensure_image(&self) -> Result<(), String> {
        if !self.config.auto_pull {
            return Ok(());
        }

        // Check if image exists locally
        if self.docker.inspect_image(&self.config.image).await.is_ok() {
            return Ok(());
        }

        // Parse image into repo and tag
        let (repo, tag) = if let Some((r, t)) = self.config.image.rsplit_once(':') {
            (r.to_string(), t.to_string())
        } else {
            (self.config.image.clone(), "latest".to_string())
        };

        let opts = CreateImageOptions {
            from_image: repo,
            tag,
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(opts), None, None);
        while let Some(result) = stream.next().await {
            result.map_err(|e| format!("Failed to pull image {}: {e}", self.config.image))?;
        }

        Ok(())
    }
}

#[async_trait]
impl Sandbox for DockerSandbox {
    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &std::path::Path,
    ) -> Result<(), String> {
        let host_path = self.container_to_host_path(remote_path)?;

        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent dirs: {e}"))?;
        }
        fs::copy(&host_path, local_path).await.map_err(|e| {
            format!(
                "Failed to copy {} to {}: {e}",
                host_path.display(),
                local_path.display()
            )
        })?;
        Ok(())
    }

    async fn upload_file_from_local(
        &self,
        local_path: &std::path::Path,
        remote_path: &str,
    ) -> Result<(), String> {
        let host_path = self.container_to_host_path(remote_path)?;

        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent dirs: {e}"))?;
        }
        fs::copy(local_path, &host_path).await.map_err(|e| {
            format!(
                "Failed to copy {} to {}: {e}",
                local_path.display(),
                host_path.display()
            )
        })?;
        Ok(())
    }

    async fn initialize(&self) -> Result<(), String> {
        self.emit(SandboxEvent::Initializing {
            provider: "docker".into(),
        });
        let init_start = Instant::now();

        self.emit(SandboxEvent::SnapshotPulling {
            name: self.config.image.clone(),
        });
        let pull_start = Instant::now();
        if let Err(e) = self.ensure_image().await {
            let duration_ms = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::InitializeFailed {
                provider: "docker".into(),
                error: e.clone(),
                duration_ms,
            });
            return Err(e);
        }
        let pull_duration = u64::try_from(pull_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::SnapshotPulled {
            name:        self.config.image.clone(),
            duration_ms: pull_duration,
        });

        let mut binds = vec![format!(
            "{}:{}",
            self.config.host_working_directory, self.config.container_mount_point
        )];
        for extra in &self.config.extra_mounts {
            binds.push(extra.clone());
        }

        let host_config = HostConfig {
            binds: Some(binds),
            network_mode: self.config.network_mode.clone(),
            memory: self.config.memory_limit,
            cpu_quota: self.config.cpu_quota,
            ..Default::default()
        };

        let container_config = Config {
            image: Some(self.config.image.clone()),
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            working_dir: Some(self.config.container_mount_point.clone()),
            env: if self.config.env_vars.is_empty() {
                None
            } else {
                Some(self.config.env_vars.clone())
            },
            host_config: Some(host_config),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(None::<CreateContainerOptions<String>>, container_config)
            .await
            .map_err(|e| format!("Failed to create container: {e}"))?;

        let id = container.id.clone();

        self.docker
            .start_container(&id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| format!("Failed to start container: {e}"))?;

        self.container_id
            .set(id)
            .map_err(|_| "Container already initialized".to_string())?;

        // Verify container is running
        let (stdout, _, exit_code) = self
            .docker_exec(vec!["echo".to_string(), "ready".to_string()], None, None)
            .await?;

        if exit_code != 0 || !stdout.contains("ready") {
            return Err("Container health check failed".to_string());
        }

        // Cache platform info
        let (uname_output, _, _) = self
            .docker_exec(vec!["uname".to_string(), "-r".to_string()], None, None)
            .await?;

        let _ = self.cached_platform.set("linux".to_string());
        let _ = self
            .cached_os_version
            .set(format!("linux {}", uname_output.trim()));

        let init_duration = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::Ready {
            provider:    "docker".into(),
            duration_ms: init_duration,
            name:        None,
            cpu:         None,
            memory:      None,
            url:         None,
        });

        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        self.emit(SandboxEvent::CleanupStarted {
            provider: "docker".into(),
        });
        let start = Instant::now();

        let container_id = if let Some(id) = self.container_id.get() {
            id.clone()
        } else {
            let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::CleanupCompleted {
                provider: "docker".into(),
                duration_ms,
            });
            return Ok(());
        };

        // Stop with 1-second grace period; ignore "not running" errors
        let stop_opts = StopContainerOptions { t: 1 };
        let _ = self
            .docker
            .stop_container(&container_id, Some(stop_opts))
            .await;

        // Force-remove; ignore "no such container" errors
        let remove_opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        let _ = self
            .docker
            .remove_container(&container_id, Some(remove_opts))
            .await;

        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::CleanupCompleted {
            provider: "docker".into(),
            duration_ms,
        });

        Ok(())
    }

    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        let dir = working_dir.map(|d| self.resolve_container_path(d));
        self.docker_exec_shell(command, timeout_ms, dir.as_deref(), env_vars, cancel_token)
            .await
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        let container_path = self.resolve_container_path(path);
        let (stdout, stderr, exit_code) = self
            .docker_exec(vec!["cat".to_string(), container_path.clone()], None, None)
            .await?;

        if exit_code != 0 {
            return Err(format!("Failed to read {container_path}: {stderr}"));
        }

        Ok(format_lines_numbered(&stdout, offset, limit))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        let container_path = self.resolve_container_path(path);
        let container_id = self.container_id()?;

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(&container_path).parent() {
            let parent_str = parent.to_string_lossy();
            let (_, stderr, exit_code) = self
                .docker_exec(
                    vec![
                        "mkdir".to_string(),
                        "-p".to_string(),
                        parent_str.to_string(),
                    ],
                    None,
                    None,
                )
                .await?;
            if exit_code != 0 {
                return Err(format!(
                    "Failed to create parent dirs for {container_path}: {stderr}"
                ));
            }
        }

        // Build an in-memory tar archive to upload via bollard API.
        // This avoids shell escaping issues with special characters in content.
        let mut tar_builder = tar::Builder::new(Vec::new());
        let file_name = std::path::Path::new(&container_path)
            .file_name()
            .ok_or_else(|| format!("Invalid path: {container_path}"))?
            .to_string_lossy()
            .to_string();

        let content_bytes = content.as_bytes();
        let mut header = tar::Header::new_gnu();
        header
            .set_path(&file_name)
            .map_err(|e| format!("Failed to set tar path: {e}"))?;
        header.set_size(content_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        tar_builder
            .append(&header, content_bytes)
            .map_err(|e| format!("Failed to build tar archive: {e}"))?;

        let tar_bytes = tar_builder
            .into_inner()
            .map_err(|e| format!("Failed to finalize tar archive: {e}"))?;

        let parent_dir = std::path::Path::new(&container_path)
            .parent()
            .map_or_else(|| "/".to_string(), |p| p.to_string_lossy().to_string());

        let upload_opts = UploadToContainerOptions {
            path: parent_dir,
            ..Default::default()
        };

        self.docker
            .upload_to_container(container_id, Some(upload_opts), tar_bytes.into())
            .await
            .map_err(|e| format!("Failed to upload file to container: {e}"))
    }

    async fn delete_file(&self, path: &str) -> Result<(), String> {
        let container_path = self.resolve_container_path(path);
        let (_, stderr, exit_code) = self
            .docker_exec(
                vec!["rm".to_string(), "-f".to_string(), container_path.clone()],
                None,
                None,
            )
            .await?;

        if exit_code != 0 {
            return Err(format!("Failed to delete {container_path}: {stderr}"));
        }
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        let container_path = self.resolve_container_path(path);
        let (_, _, exit_code) = self
            .docker_exec(
                vec!["test".to_string(), "-e".to_string(), container_path],
                None,
                None,
            )
            .await?;

        Ok(exit_code == 0)
    }

    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        let container_path = self.resolve_container_path(path);
        let max_depth = depth.unwrap_or(1);

        // Use find with -printf for structured output: type, size, relative path
        let (stdout, stderr, exit_code) = self
            .docker_exec(
                vec![
                    "find".to_string(),
                    container_path.clone(),
                    "-mindepth".to_string(),
                    "1".to_string(),
                    "-maxdepth".to_string(),
                    max_depth.to_string(),
                    "-printf".to_string(),
                    "%y\t%s\t%P\n".to_string(),
                ],
                None,
                None,
            )
            .await?;

        if exit_code != 0 {
            return Err(format!(
                "Failed to list directory {container_path}: {stderr}"
            ));
        }

        let mut entries: Vec<DirEntry> = stdout
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(3, '\t').collect();
                if parts.len() < 3 {
                    return None;
                }
                let file_type = parts[0];
                let size: Option<u64> = parts[1].parse().ok();
                let name = parts[2].to_string();
                let is_dir = file_type == "d";
                Some(DirEntry {
                    name,
                    is_dir,
                    size: if is_dir { None } else { size },
                })
            })
            .collect();

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> Result<Vec<String>, String> {
        let container_path = self.resolve_container_path(path);

        // Detect ripgrep availability (cached)
        let use_rg = *self
            .rg_available
            .get_or_init(|| async {
                let result = self
                    .docker_exec(vec!["which".to_string(), "rg".to_string()], None, None)
                    .await;
                matches!(result, Ok((_, _, 0)))
            })
            .await;

        let command = if use_rg {
            let mut args = vec!["rg".to_string(), "-n".to_string()];
            if options.case_insensitive {
                args.push("-i".to_string());
            }
            if let Some(ref glob_filter) = options.glob_filter {
                args.push("--glob".to_string());
                args.push(glob_filter.clone());
            }
            if let Some(max) = options.max_results {
                args.push("-m".to_string());
                args.push(max.to_string());
            }
            args.push(pattern.to_string());
            args.push(container_path);
            args.join(" ")
        } else {
            let mut args = vec!["grep".to_string(), "-rn".to_string()];
            if options.case_insensitive {
                args.push("-i".to_string());
            }
            if let Some(ref glob_filter) = options.glob_filter {
                args.push("--include".to_string());
                args.push(glob_filter.clone());
            }
            if let Some(max) = options.max_results {
                args.push("-m".to_string());
                args.push(max.to_string());
            }
            args.push(format!("'{pattern}'"));
            args.push(container_path);
            args.join(" ")
        };

        // Run through shell so that quoting works correctly
        let result = self
            .docker_exec_shell(&command, 30_000, None, None, None)
            .await?;

        let results: Vec<String> = result
            .stdout
            .lines()
            .map(String::from)
            .filter(|l| !l.is_empty())
            .collect();

        Ok(results)
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String> {
        let base_dir = path.map_or_else(
            || self.config.container_mount_point.clone(),
            |p| self.resolve_container_path(p),
        );

        let full_pattern = if pattern.starts_with('/') {
            pattern.to_string()
        } else {
            format!("{base_dir}/{pattern}")
        };

        // Use bash globbing with stat for mtime-descending sort
        let script = format!(
            "shopt -s nullglob globstar; for f in {full_pattern}; do stat --format='%Y %n' \"$f\" 2>/dev/null; done | sort -rn | cut -d' ' -f2-"
        );

        let result = self
            .docker_exec_shell(&script, 30_000, None, None, None)
            .await?;

        let results: Vec<String> = result
            .stdout
            .lines()
            .map(String::from)
            .filter(|l| !l.is_empty())
            .collect();

        Ok(results)
    }

    fn working_directory(&self) -> &str {
        &self.config.container_mount_point
    }

    fn platform(&self) -> &str {
        self.cached_platform.get().map_or("linux", String::as_str)
    }

    fn os_version(&self) -> String {
        self.cached_os_version
            .get()
            .cloned()
            .unwrap_or_else(|| "linux".to_string())
    }

    fn sandbox_info(&self) -> String {
        self.container_id.get().cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn require_docker() -> Docker {
        Docker::connect_with_local_defaults().expect("Docker not available — skipping")
    }

    fn test_config(host_dir: &str) -> DockerSandboxOptions {
        DockerSandboxOptions {
            host_working_directory: host_dir.to_string(),
            auto_pull: false,
            ..Default::default()
        }
    }

    #[tokio::test]
    #[ignore = "requires local Docker daemon"]
    async fn full_lifecycle() {
        let _docker = require_docker();
        let host_dir =
            std::env::temp_dir().join(format!("docker_env_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&host_dir).unwrap();

        let config = test_config(host_dir.to_str().unwrap());
        let env: Arc<dyn Sandbox> = Arc::new(DockerSandbox::new(config).unwrap());

        // Initialize
        env.initialize().await.unwrap();

        // Platform and OS version
        assert_eq!(env.platform(), "linux");
        assert!(env.os_version().starts_with("linux "));

        // exec_command
        let result = env
            .exec_command("echo hello", 5000, None, None, None)
            .await
            .unwrap();
        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);

        // write_file + read_file
        env.write_file("test.txt", "line1\nline2\nline3")
            .await
            .unwrap();
        let content = env.read_file("test.txt", None, None).await.unwrap();
        assert!(content.contains("1 | line1"));
        assert!(content.contains("2 | line2"));
        assert!(content.contains("3 | line3"));

        // file_exists
        assert!(env.file_exists("test.txt").await.unwrap());
        assert!(!env.file_exists("nonexistent.txt").await.unwrap());

        // list_directory
        let entries = env.list_directory(".", None).await.unwrap();
        assert!(entries.iter().any(|e| e.name == "test.txt"));

        // grep
        let grep_results = env
            .grep("line2", "test.txt", &GrepOptions::default())
            .await
            .unwrap();
        assert_eq!(grep_results.len(), 1);
        assert!(grep_results[0].contains("line2"));

        // glob
        let glob_results = env.glob("*.txt", None).await.unwrap();
        assert!(glob_results.iter().any(|p| p.contains("test.txt")));

        // delete_file
        env.delete_file("test.txt").await.unwrap();
        assert!(!env.file_exists("test.txt").await.unwrap());

        // Cleanup
        env.cleanup().await.unwrap();
        std::fs::remove_dir_all(&host_dir).ok();
    }

    #[tokio::test]
    #[ignore = "requires local Docker daemon"]
    async fn timeout_handling() {
        let _docker = require_docker();
        let host_dir =
            std::env::temp_dir().join(format!("docker_timeout_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&host_dir).unwrap();

        let config = test_config(host_dir.to_str().unwrap());
        let env = DockerSandbox::new(config).unwrap();
        env.initialize().await.unwrap();

        let result = env
            .exec_command("sleep 2", 1000, None, None, None)
            .await
            .unwrap();
        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);

        env.cleanup().await.unwrap();
        std::fs::remove_dir_all(&host_dir).ok();
    }

    #[tokio::test]
    #[ignore = "requires local Docker daemon"]
    async fn special_characters_in_write() {
        let _docker = require_docker();
        let host_dir =
            std::env::temp_dir().join(format!("docker_special_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&host_dir).unwrap();

        let config = test_config(host_dir.to_str().unwrap());
        let env = DockerSandbox::new(config).unwrap();
        env.initialize().await.unwrap();

        let content = "hello \"world\"\nit's a `test`\nprice: $100\nbackslash: \\\nnewline above";
        env.write_file("special.txt", content).await.unwrap();

        // Read raw content back via cat to verify exact match
        let result = env
            .exec_command("cat /workspace/special.txt", 5000, None, None, None)
            .await
            .unwrap();
        assert_eq!(result.stdout, content);

        env.cleanup().await.unwrap();
        std::fs::remove_dir_all(&host_dir).ok();
    }

    #[tokio::test]
    #[ignore = "requires local Docker daemon"]
    async fn path_resolution() {
        let _docker = require_docker();
        let host_dir =
            std::env::temp_dir().join(format!("docker_path_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&host_dir).unwrap();

        let config = test_config(host_dir.to_str().unwrap());
        let env = DockerSandbox::new(config).unwrap();
        env.initialize().await.unwrap();

        // Relative path resolves to container_mount_point
        env.write_file("relative.txt", "relative").await.unwrap();
        assert!(env.file_exists("relative.txt").await.unwrap());
        assert!(env.file_exists("/workspace/relative.txt").await.unwrap());

        // Absolute path used as-is
        env.write_file("/tmp/absolute.txt", "absolute")
            .await
            .unwrap();
        assert!(env.file_exists("/tmp/absolute.txt").await.unwrap());

        env.cleanup().await.unwrap();
        std::fs::remove_dir_all(&host_dir).ok();
    }

    #[tokio::test]
    #[ignore = "requires local Docker daemon"]
    async fn cleanup_idempotent() {
        let _docker = require_docker();
        let host_dir =
            std::env::temp_dir().join(format!("docker_cleanup_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&host_dir).unwrap();

        let config = test_config(host_dir.to_str().unwrap());
        let env = DockerSandbox::new(config).unwrap();
        env.initialize().await.unwrap();

        // First cleanup
        env.cleanup().await.unwrap();
        // Second cleanup should not error
        env.cleanup().await.unwrap();

        std::fs::remove_dir_all(&host_dir).ok();
    }
}
