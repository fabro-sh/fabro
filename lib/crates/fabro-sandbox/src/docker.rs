use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Cursor;
use std::time::Instant;

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, DownloadFromContainerOptions, InspectContainerOptions,
    LogOutput, RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
    UploadToContainerOptions,
};
use bollard::errors::Error as DockerError;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::HostConfig;
use fabro_github::GitHubCredentials;
use fabro_types::RunId;
use futures::StreamExt;
use tokio::sync::OnceCell;
use tokio::{fs, time};
use tokio_util::sync::CancellationToken;

use crate::clone_source::{self, CloneDecision, EmptyWorkspaceReason};
use crate::redact::{classify_credential_refresh_failure, redact_auth_url};
use crate::sandbox::resolve_path;
use crate::{
    DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback,
    format_lines_numbered, shell_quote,
};

const WORKING_DIRECTORY: &str = "/workspace";

const MANAGED_LABEL: &str = "sh.fabro.managed";
const RUN_ID_LABEL: &str = "sh.fabro.run_id";

#[derive(Clone, Debug, PartialEq)]
pub struct DockerSandboxOptions {
    /// Docker image to use.
    pub image:        String,
    /// Docker network mode. Default: `Some("bridge")`.
    pub network_mode: Option<String>,
    /// Memory limit in bytes. `None` = unlimited.
    pub memory_limit: Option<i64>,
    /// CPU quota (microseconds per 100ms period). `None` = unlimited.
    pub cpu_quota:    Option<i64>,
    /// Whether to pull the image if not found locally. Default: `true`.
    pub auto_pull:    bool,
    /// Additional `KEY=VALUE` environment variables for the container.
    pub env_vars:     Vec<String>,
    /// Create an empty workspace instead of cloning even when an origin exists.
    pub skip_clone:   bool,
}

impl Default for DockerSandboxOptions {
    fn default() -> Self {
        Self {
            image:        "buildpack-deps:noble".to_string(),
            network_mode: Some("bridge".to_string()),
            memory_limit: None,
            cpu_quota:    None,
            auto_pull:    true,
            env_vars:     Vec::new(),
            skip_clone:   false,
        }
    }
}

pub struct DockerSandbox {
    docker:            Docker,
    config:            DockerSandboxOptions,
    github_app:        Option<GitHubCredentials>,
    run_id:            Option<RunId>,
    clone_origin_url:  Option<String>,
    clone_branch:      Option<String>,
    container_id:      OnceCell<String>,
    repo_cloned:       OnceCell<bool>,
    origin_url:        OnceCell<String>,
    cached_platform:   std::sync::OnceLock<String>,
    cached_os_version: std::sync::OnceLock<String>,
    rg_available:      OnceCell<bool>,
    event_callback:    Option<SandboxEventCallback>,
}

impl DockerSandbox {
    pub fn new(
        config: DockerSandboxOptions,
        github_app: Option<GitHubCredentials>,
        run_id: Option<RunId>,
        clone_origin_url: Option<String>,
        clone_branch: Option<String>,
    ) -> crate::Result<Self> {
        let docker = Docker::connect_with_local_defaults().map_err(crate::Error::docker_connect)?;
        Ok(Self {
            docker,
            config,
            github_app,
            run_id,
            clone_origin_url,
            clone_branch,
            container_id: OnceCell::new(),
            repo_cloned: OnceCell::new(),
            origin_url: OnceCell::new(),
            cached_platform: std::sync::OnceLock::new(),
            cached_os_version: std::sync::OnceLock::new(),
            rg_available: OnceCell::const_new(),
            event_callback: None,
        })
    }

    pub async fn reconnect(
        container_id: &str,
        repo_cloned: bool,
        clone_origin_url: Option<String>,
        clone_branch: Option<String>,
    ) -> crate::Result<Self> {
        let sandbox = Self::new(
            DockerSandboxOptions::default(),
            None,
            None,
            clone_origin_url.clone(),
            clone_branch,
        )?;
        sandbox.validate_managed_container(container_id).await?;
        sandbox
            .container_id
            .set(container_id.to_string())
            .map_err(|_| "Container already initialized".to_string())?;
        sandbox
            .repo_cloned
            .set(repo_cloned)
            .map_err(|_| "Clone state already initialized".to_string())?;
        if repo_cloned {
            if let Some(origin) = clone_origin_url {
                let _ = sandbox.origin_url.set(origin);
            }
        }
        Ok(sandbox)
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

    fn container_id(&self) -> crate::Result<&str> {
        self.container_id.get().map(String::as_str).ok_or_else(|| {
            crate::Error::message("Container not initialized — call initialize() first")
        })
    }

    fn resolve_container_path(path: &str) -> String {
        resolve_path(path, WORKING_DIRECTORY)
    }

    fn repo_cloned(&self) -> bool {
        self.repo_cloned.get().copied().unwrap_or(false)
    }

    async fn docker_exec(
        &self,
        cmd: Vec<String>,
        working_dir: Option<&str>,
        env: Option<Vec<String>>,
    ) -> crate::Result<(String, String, i32)> {
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
            .map_err(|e| crate::Error::context("Failed to create exec", e))?;

        let start_result = self
            .docker
            .start_exec(&exec_instance.id, None)
            .await
            .map_err(|e| crate::Error::context("Failed to start exec", e))?;

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
                    Err(e) => {
                        return Err(crate::Error::context("Error reading exec output", e));
                    }
                }
            }
        }

        let inspect = self
            .docker
            .inspect_exec(&exec_instance.id)
            .await
            .map_err(|e| crate::Error::context("Failed to inspect exec", e))?;

        let exit_code = inspect
            .exit_code
            .and_then(|code| i32::try_from(code).ok())
            .unwrap_or(-1);
        Ok((stdout, stderr, exit_code))
    }

    async fn docker_exec_shell(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> crate::Result<ExecResult> {
        let start = Instant::now();
        let effective_dir = working_dir.unwrap_or(WORKING_DIRECTORY).to_string();
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

    async fn ensure_image(&self) -> crate::Result<()> {
        if !self.config.auto_pull {
            return Ok(());
        }

        match self.docker.inspect_image(&self.config.image).await {
            Ok(_) => return Ok(()),
            Err(e) if docker_not_found(&e) => {}
            Err(e) => {
                return Err(crate::Error::docker_image_inspect(
                    self.config.image.clone(),
                    e,
                ));
            }
        }

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
            result.map_err(|e| crate::Error::docker_image_pull(self.config.image.clone(), e))?;
        }

        Ok(())
    }

    async fn create_workspace(&self) -> crate::Result<()> {
        let result = self
            .docker_exec_shell(
                &format!("mkdir -p {}", shell_quote(WORKING_DIRECTORY)),
                10_000,
                Some("/"),
                None,
                None,
            )
            .await?;
        if result.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "Failed to create Docker workspace (exit {}): {}",
                result.exit_code, result.stderr
            )));
        }
        Ok(())
    }

    async fn verify_git_available(&self) -> crate::Result<()> {
        let result = self
            .docker_exec_shell("git --version", 10_000, Some("/"), None, None)
            .await?;
        if result.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "Docker image '{}' must include git for repository clone and git lifecycle operations. Use an image with bash and git, such as buildpack-deps:noble.",
                self.config.image
            )));
        }
        Ok(())
    }

    async fn clone_github_repo(
        &self,
        origin_url: String,
        branch: Option<String>,
    ) -> crate::Result<()> {
        self.verify_git_available().await?;

        self.emit(SandboxEvent::GitCloneStarted {
            url:    origin_url.clone(),
            branch: branch.clone(),
        });
        let clone_start = Instant::now();

        let auth_url = match &self.github_app {
            Some(creds) => Some(
                fabro_github::resolve_authenticated_url(
                    &fabro_github::GitHubContext::new(creds, &fabro_github::github_api_base_url()),
                    &origin_url,
                )
                .await
                .map_err(|e| {
                    crate::Error::message(format!(
                        "Failed to get GitHub App credentials for clone: {e}"
                    ))
                })?,
            ),
            None => None,
        };
        let clone_url = auth_url
            .as_ref()
            .map_or(origin_url.as_str(), |url| url.as_raw_url().as_str());

        let mut command = "git -c maintenance.auto=0 -c gc.auto=0 clone".to_string();
        if let Some(branch) = branch.as_ref() {
            command.push_str(" --branch ");
            command.push_str(&shell_quote(branch));
            command.push_str(" --single-branch");
        }
        command.push_str(" -- ");
        command.push_str(&shell_quote(clone_url));
        command.push(' ');
        command.push_str(&shell_quote(WORKING_DIRECTORY));

        let result = self
            .docker_exec_shell(&command, 300_000, Some("/"), None, None)
            .await?;
        if result.exit_code != 0 {
            let stderr = redact_auth_url(&result.stderr, auth_url.as_ref());
            let err = crate::Error::message(if self.github_app.is_none() {
                format!(
                    "Git clone failed: {stderr}. If this is a private repository, configure a GitHub App with `fabro install` and install it for your organization."
                )
            } else {
                format!("Failed to clone repo into Docker sandbox: {stderr}")
            });
            self.emit(SandboxEvent::GitCloneFailed {
                url:    origin_url,
                error:  err.to_string(),
                causes: err.causes(),
            });
            return Err(err);
        }

        let _ = self.repo_cloned.set(true);
        let _ = self.origin_url.set(origin_url.clone());

        if let Some(auth_url) = auth_url.as_ref() {
            let command = format!(
                "git -c maintenance.auto=0 remote set-url origin {}",
                shell_quote(auth_url.as_raw_url().as_str())
            );
            let result = self
                .docker_exec_shell(&command, 10_000, Some(WORKING_DIRECTORY), None, None)
                .await?;
            if result.exit_code != 0 {
                let err = result
                    .into_exec_error_with_redactor("git remote set-url origin (post-clone)", |s| {
                        redact_auth_url(s, Some(auth_url))
                    });
                tracing::warn!(
                    error = %err,
                    "Failed to set Docker sandbox push credentials on origin — \
                     subsequent git push from this sandbox will fail"
                );
            }
        }

        let clone_duration = u64::try_from(clone_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::GitCloneCompleted {
            url:         origin_url,
            duration_ms: clone_duration,
        });
        Ok(())
    }

    async fn validate_managed_container(&self, container_id: &str) -> crate::Result<()> {
        let labels = self.inspect_labels(container_id).await?;
        verify_managed_labels(container_id, &labels, self.run_id.as_ref())
    }

    async fn inspect_labels(&self, container_id: &str) -> crate::Result<HashMap<String, String>> {
        let inspect = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| {
                if docker_not_found(&e) {
                    crate::Error::message(format!("Docker container '{container_id}' is gone"))
                } else {
                    crate::Error::message(format!(
                        "Failed to inspect Docker container '{container_id}': {e}"
                    ))
                }
            })?;
        Ok(inspect
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default())
    }

    async fn ensure_name_available(&self) -> crate::Result<Option<String>> {
        let Some(run_id) = self.run_id.as_ref() else {
            return Ok(None);
        };
        let name = container_name(run_id);
        match self
            .docker
            .inspect_container(&name, None::<InspectContainerOptions>)
            .await
        {
            Ok(_) => Err(crate::Error::message(format!(
                "Docker container name '{name}' already exists for run {run_id}. Remove the stale container manually before retrying."
            ))),
            Err(e) if docker_not_found(&e) => Ok(Some(name)),
            Err(e) => Err(crate::Error::message(format!(
                "Failed to check Docker container name '{name}' before creation: {e}"
            ))),
        }
    }

    async fn upload_bytes_to_container(&self, path: &str, bytes: &[u8]) -> crate::Result<()> {
        let container_path = Self::resolve_container_path(path);
        let container_id = self.container_id()?;
        let parent_dir = std::path::Path::new(&container_path)
            .parent()
            .map_or_else(|| "/".to_string(), |p| p.to_string_lossy().to_string());
        let file_name = std::path::Path::new(&container_path)
            .file_name()
            .ok_or_else(|| crate::Error::message(format!("Invalid path: {container_path}")))?
            .to_string_lossy()
            .to_string();

        let result = self
            .docker_exec_shell(
                &format!("mkdir -p {}", shell_quote(&parent_dir)),
                10_000,
                Some("/"),
                None,
                None,
            )
            .await?;
        if result.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "Failed to create parent dirs for {container_path}: {}",
                result.stderr
            )));
        }

        let tar_bytes = build_single_file_tar(&file_name, bytes)?;
        let upload_opts = UploadToContainerOptions {
            path:                     parent_dir,
            no_overwrite_dir_non_dir: "false".to_string(),
        };

        self.docker
            .upload_to_container(container_id, Some(upload_opts), tar_bytes.into())
            .await
            .map_err(|e| crate::Error::context("Failed to upload file to container", e))
    }

    fn cleanup_error(&self, error: crate::Error) -> crate::Result<()> {
        self.emit(SandboxEvent::CleanupFailed {
            provider: "docker".into(),
            error:    error.to_string(),
            causes:   error.causes(),
        });
        Err(error)
    }

    fn fail_init(&self, init_start: Instant, err: crate::Error) -> crate::Error {
        let duration_ms = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::InitializeFailed {
            provider: "docker".into(),
            error: err.to_string(),
            causes: err.causes(),
            duration_ms,
        });
        err
    }
}

fn container_name(run_id: &RunId) -> String {
    format!("fabro-run-{run_id}")
}

fn container_labels(run_id: Option<&RunId>) -> HashMap<String, String> {
    let mut labels = HashMap::from([(MANAGED_LABEL.to_string(), "true".to_string())]);
    if let Some(run_id) = run_id {
        labels.insert(RUN_ID_LABEL.to_string(), run_id.to_string());
    }
    labels
}

fn host_config(config: &DockerSandboxOptions) -> HostConfig {
    HostConfig {
        binds: None,
        network_mode: config.network_mode.clone(),
        memory: config.memory_limit,
        cpu_quota: config.cpu_quota,
        ..Default::default()
    }
}

fn container_config(config: &DockerSandboxOptions, run_id: Option<&RunId>) -> Config<String> {
    Config {
        image: Some(config.image.clone()),
        cmd: Some(vec![
            "/bin/bash".to_string(),
            "-lc".to_string(),
            format!(
                "mkdir -p {} && sleep infinity",
                shell_quote(WORKING_DIRECTORY)
            ),
        ]),
        working_dir: Some(WORKING_DIRECTORY.to_string()),
        env: if config.env_vars.is_empty() {
            None
        } else {
            Some(config.env_vars.clone())
        },
        labels: Some(container_labels(run_id)),
        host_config: Some(host_config(config)),
        ..Default::default()
    }
}

fn verify_managed_labels(
    container_id: &str,
    labels: &HashMap<String, String>,
    run_id: Option<&RunId>,
) -> crate::Result<()> {
    if labels.get(MANAGED_LABEL).map(String::as_str) != Some("true") {
        return Err(crate::Error::message(format!(
            "Refusing to operate on Docker container '{container_id}' because it is missing label {MANAGED_LABEL}=true"
        )));
    }
    if let Some(run_id) = run_id {
        let actual = labels.get(RUN_ID_LABEL).map(String::as_str);
        let expected = run_id.to_string();
        if actual != Some(expected.as_str()) {
            return Err(crate::Error::message(format!(
                "Refusing to operate on Docker container '{container_id}' because label {RUN_ID_LABEL}={actual:?} does not match run {run_id}"
            )));
        }
    }
    Ok(())
}

fn docker_not_found(error: &DockerError) -> bool {
    matches!(error, DockerError::DockerResponseServerError {
        status_code: 404,
        ..
    })
}

fn docker_already_stopped(error: &DockerError) -> bool {
    matches!(error, DockerError::DockerResponseServerError {
        status_code: 304,
        ..
    })
}

fn bash_remediation(error: &DockerError, image: &str) -> String {
    format!(
        "Failed to start Docker container from image '{image}': {error}. Docker sandboxes require /bin/bash for internal commands; use an image with bash and git, such as buildpack-deps:noble."
    )
}

fn build_single_file_tar(file_name: &str, bytes: &[u8]) -> crate::Result<Vec<u8>> {
    let mut tar_builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header
        .set_path(file_name)
        .map_err(|e| crate::Error::context("Failed to set tar path", e))?;
    header.set_size(
        u64::try_from(bytes.len())
            .map_err(|_| crate::Error::message("file is too large for tar header"))?,
    );
    header.set_mode(0o644);
    header.set_cksum();
    tar_builder
        .append(&header, bytes)
        .map_err(|e| crate::Error::context("Failed to build tar archive", e))?;
    tar_builder
        .into_inner()
        .map_err(|e| crate::Error::context("Failed to finalize tar archive", e))
}

#[async_trait]
impl Sandbox for DockerSandbox {
    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &std::path::Path,
    ) -> crate::Result<()> {
        let container_id = self.container_id()?;
        let container_path = Self::resolve_container_path(remote_path);
        let opts = DownloadFromContainerOptions {
            path: container_path.clone(),
        };
        let mut stream = self
            .docker
            .download_from_container(container_id, Some(opts));
        let mut archive_bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                crate::Error::context(
                    format!("Failed to download {container_path} from container"),
                    e,
                )
            })?;
            archive_bytes.extend_from_slice(&chunk);
        }

        let bytes = {
            #[expect(
                clippy::disallowed_types,
                reason = "tar entries are synchronous in-memory readers; bytes are collected before any await"
            )]
            use std::io::Read as _;

            let mut archive = tar::Archive::new(Cursor::new(archive_bytes));
            let entries = archive.entries().map_err(|e| {
                crate::Error::context(
                    format!("Failed to read Docker archive for {container_path}"),
                    e,
                )
            })?;
            let mut file_bytes = None;
            for entry in entries {
                let mut entry = entry.map_err(|e| {
                    crate::Error::context(
                        format!("Failed to read Docker archive entry for {container_path}"),
                        e,
                    )
                })?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).map_err(|e| {
                    crate::Error::context(
                        format!("Failed to read Docker archive file for {container_path}"),
                        e,
                    )
                })?;
                file_bytes = Some(bytes);
                break;
            }
            file_bytes.ok_or_else(|| {
                crate::Error::message(format!(
                    "Docker archive for {container_path} did not contain a file"
                ))
            })?
        };

        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| crate::Error::context("Failed to create parent dirs", e))?;
        }
        fs::write(local_path, bytes).await.map_err(|e| {
            crate::Error::context(format!("Failed to write {}", local_path.display()), e)
        })
    }

    async fn upload_file_from_local(
        &self,
        local_path: &std::path::Path,
        remote_path: &str,
    ) -> crate::Result<()> {
        let bytes = fs::read(local_path).await.map_err(|e| {
            crate::Error::context(format!("Failed to read {}", local_path.display()), e)
        })?;
        self.upload_bytes_to_container(remote_path, &bytes).await
    }

    async fn initialize(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::Initializing {
            provider: "docker".into(),
        });
        let init_start = Instant::now();

        self.emit(SandboxEvent::SnapshotPulling {
            name: self.config.image.clone(),
        });
        let pull_start = Instant::now();
        if let Err(e) = self.ensure_image().await {
            return Err(self.fail_init(init_start, e));
        }
        let pull_duration = u64::try_from(pull_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::SnapshotPulled {
            name:        self.config.image.clone(),
            duration_ms: pull_duration,
        });

        let container_name = self
            .ensure_name_available()
            .await
            .map_err(|e| self.fail_init(init_start, e))?;
        let create_options = container_name.map(|name| CreateContainerOptions {
            name,
            platform: None,
        });
        let container = self
            .docker
            .create_container(create_options, container_config(&self.config, self.run_id.as_ref()))
            .await
            .map_err(|e| {
                let message = if matches!(
                    e,
                    DockerError::DockerResponseServerError {
                        status_code: 409,
                        ..
                    }
                ) {
                    "Docker container for run already exists. Remove the stale fabro-run container manually before retrying.".to_string()
                } else {
                    "Failed to create Docker container".to_string()
                };
                self.fail_init(init_start, crate::Error::context(message, e))
            })?;

        let id = container.id.clone();
        self.container_id
            .set(id.clone())
            .map_err(|_| crate::Error::message("Container already initialized"))?;

        self.docker
            .start_container(&id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| {
                let err = crate::Error::context(bash_remediation(&e, &self.config.image), e);
                self.fail_init(init_start, err)
            })?;

        let (stdout, stderr, exit_code) = self
            .docker_exec(
                vec![
                    "/bin/bash".to_string(),
                    "-lc".to_string(),
                    "echo ready".to_string(),
                ],
                Some(WORKING_DIRECTORY),
                None,
            )
            .await?;
        if exit_code != 0 || !stdout.contains("ready") {
            let err = crate::Error::message(format!(
                "Docker container health check failed. Docker sandboxes require /bin/bash; use an image with bash and git, such as buildpack-deps:noble. {stderr}"
            ));
            return Err(self.fail_init(init_start, err));
        }

        let (uname_output, _, _) = self
            .docker_exec(vec!["uname".to_string(), "-r".to_string()], None, None)
            .await?;
        let _ = self.cached_platform.set("linux".to_string());
        let _ = self
            .cached_os_version
            .set(format!("linux {}", uname_output.trim()));

        let clone_decision = clone_source::decide_clone(
            self.config.skip_clone,
            self.clone_origin_url.as_deref(),
            self.clone_branch.as_deref(),
        )
        .map_err(|e| self.fail_init(init_start, e))?;

        match clone_decision {
            CloneDecision::EmptyWorkspace { reason } => {
                if matches!(reason, EmptyWorkspaceReason::MissingOrigin) {
                    tracing::warn!(
                        provider = "docker",
                        reason = reason.message(),
                        "Clone source missing for clone-based sandbox"
                    );
                }
                if let Err(e) = self.create_workspace().await {
                    return Err(self.fail_init(init_start, e));
                }
                let _ = self.repo_cloned.set(false);
            }
            CloneDecision::GitHub { origin_url, branch } => {
                if let Err(e) = self.clone_github_repo(origin_url, branch).await {
                    return Err(self.fail_init(init_start, e));
                }
            }
        }

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

    async fn cleanup(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::CleanupStarted {
            provider: "docker".into(),
        });
        let start = Instant::now();

        let Some(container_id) = self.container_id.get().cloned() else {
            let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::CleanupCompleted {
                provider: "docker".into(),
                duration_ms,
            });
            return Ok(());
        };

        let labels = match self.inspect_labels(&container_id).await {
            Ok(labels) => labels,
            Err(e) => return self.cleanup_error(e),
        };
        if let Err(e) = verify_managed_labels(&container_id, &labels, self.run_id.as_ref()) {
            return self.cleanup_error(e);
        }

        let stop_opts = StopContainerOptions { t: 1 };
        if let Err(e) = self
            .docker
            .stop_container(&container_id, Some(stop_opts))
            .await
        {
            if !docker_not_found(&e) && !docker_already_stopped(&e) {
                return self.cleanup_error(crate::Error::context(
                    format!(
                        "Failed to stop Docker container '{container_id}' with labels {labels:?}"
                    ),
                    e,
                ));
            }
        }

        let remove_opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        if let Err(e) = self
            .docker
            .remove_container(&container_id, Some(remove_opts))
            .await
        {
            if !docker_not_found(&e) {
                return self.cleanup_error(crate::Error::context(
                    format!(
                        "Failed to remove Docker container '{container_id}' with labels {labels:?}"
                    ),
                    e,
                ));
            }
        }

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
    ) -> crate::Result<ExecResult> {
        let dir = working_dir.map(Self::resolve_container_path);
        self.docker_exec_shell(command, timeout_ms, dir.as_deref(), env_vars, cancel_token)
            .await
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> crate::Result<String> {
        let container_path = Self::resolve_container_path(path);
        let (stdout, stderr, exit_code) = self
            .docker_exec(vec!["cat".to_string(), container_path.clone()], None, None)
            .await?;

        if exit_code != 0 {
            return Err(crate::Error::message(format!(
                "Failed to read {container_path}: {stderr}"
            )));
        }

        Ok(format_lines_numbered(&stdout, offset, limit))
    }

    async fn write_file(&self, path: &str, content: &str) -> crate::Result<()> {
        self.upload_bytes_to_container(path, content.as_bytes())
            .await
    }

    async fn delete_file(&self, path: &str) -> crate::Result<()> {
        let container_path = Self::resolve_container_path(path);
        let (_, stderr, exit_code) = self
            .docker_exec(
                vec!["rm".to_string(), "-f".to_string(), container_path.clone()],
                None,
                None,
            )
            .await?;

        if exit_code != 0 {
            return Err(crate::Error::message(format!(
                "Failed to delete {container_path}: {stderr}"
            )));
        }
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> crate::Result<bool> {
        let container_path = Self::resolve_container_path(path);
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
    ) -> crate::Result<Vec<DirEntry>> {
        let container_path = Self::resolve_container_path(path);
        let max_depth = depth.unwrap_or(1);
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
            return Err(crate::Error::message(format!(
                "Failed to list directory {container_path}: {stderr}"
            )));
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
    ) -> crate::Result<Vec<String>> {
        let container_path = Self::resolve_container_path(path);
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
            let mut command = "rg -n".to_string();
            if options.case_insensitive {
                command.push_str(" -i");
            }
            if let Some(ref glob_filter) = options.glob_filter {
                command.push_str(" --glob ");
                command.push_str(&shell_quote(glob_filter));
            }
            if let Some(max) = options.max_results {
                let _ = write!(&mut command, " -m {max}");
            }
            command.push_str(" -- ");
            command.push_str(&shell_quote(pattern));
            command.push(' ');
            command.push_str(&shell_quote(&container_path));
            command
        } else {
            let mut command = "grep -rn".to_string();
            if options.case_insensitive {
                command.push_str(" -i");
            }
            if let Some(ref glob_filter) = options.glob_filter {
                command.push_str(" --include ");
                command.push_str(&shell_quote(glob_filter));
            }
            if let Some(max) = options.max_results {
                let _ = write!(&mut command, " -m {max}");
            }
            command.push_str(" -- ");
            command.push_str(&shell_quote(pattern));
            command.push(' ');
            command.push_str(&shell_quote(&container_path));
            command
        };

        let result = self
            .docker_exec_shell(&command, 30_000, None, None, None)
            .await?;
        if result.exit_code == 1 {
            return Ok(Vec::new());
        }
        if result.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "grep failed (exit {}): {}",
                result.exit_code, result.stderr
            )));
        }

        Ok(result
            .stdout
            .lines()
            .map(String::from)
            .filter(|line| !line.is_empty())
            .collect())
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> crate::Result<Vec<String>> {
        let base_dir = path.map_or_else(
            || WORKING_DIRECTORY.to_string(),
            Self::resolve_container_path,
        );
        let command = format!(
            "find {} -name {} -type f | sort",
            shell_quote(&base_dir),
            shell_quote(pattern)
        );
        let result = self
            .docker_exec_shell(&command, 30_000, None, None, None)
            .await?;
        if result.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "glob failed (exit {}): {}",
                result.exit_code, result.stderr
            )));
        }

        Ok(result
            .stdout
            .lines()
            .map(String::from)
            .filter(|line| !line.is_empty())
            .collect())
    }

    fn working_directory(&self) -> &str {
        WORKING_DIRECTORY
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

    async fn setup_git(
        &self,
        intent: &crate::GitSetupIntent,
    ) -> crate::Result<Option<crate::GitRunInfo>> {
        if !self.repo_cloned() {
            return Ok(None);
        }
        crate::setup_git_via_exec(self, intent).await.map(Some)
    }

    fn resume_setup_commands(&self, run_branch: &str) -> Vec<String> {
        if !self.repo_cloned() {
            return Vec::new();
        }
        vec![format!(
            "git fetch origin {} && git checkout {}",
            shell_quote(run_branch),
            shell_quote(run_branch)
        )]
    }

    async fn git_push_ref(&self, refspec: &str) -> crate::Result<()> {
        if !self.repo_cloned() {
            return Ok(());
        }
        crate::git_push_via_exec(self, refspec).await
    }

    fn parallel_worktree_path(
        &self,
        _run_dir: &std::path::Path,
        run_id: &str,
        node_id: &str,
        key: &str,
    ) -> String {
        format!(
            "{}/.fabro/scratch/{}/parallel/{}/{}",
            self.working_directory(),
            run_id,
            node_id,
            key
        )
    }

    fn origin_url(&self) -> Option<&str> {
        if !self.repo_cloned() {
            return None;
        }
        self.origin_url.get().map(String::as_str)
    }

    async fn refresh_push_credentials(&self) -> crate::Result<()> {
        if !self.repo_cloned() {
            return Ok(());
        }
        let Some(origin_url) = self.origin_url.get() else {
            return Ok(());
        };
        let Some(creds) = &self.github_app else {
            return Ok(());
        };

        let auth_url = fabro_github::resolve_authenticated_url(
            &fabro_github::GitHubContext::new(creds, &fabro_github::github_api_base_url()),
            origin_url,
        )
        .await
        .map_err(|e| {
            let class = classify_credential_refresh_failure(&format!("token_mint_failed: {e}"));
            crate::Error::message(format!("Failed to refresh push credentials: {class}"))
        })?;

        let command = format!(
            "git -c maintenance.auto=0 remote set-url origin {}",
            shell_quote(auth_url.as_raw_url().as_str())
        );
        let result = self
            .docker_exec_shell(&command, 10_000, Some(WORKING_DIRECTORY), None, None)
            .await?;
        if result.exit_code != 0 {
            let class = classify_credential_refresh_failure("set_url_nonzero");
            return Err(crate::Error::message(format!(
                "Failed to refresh push credentials: {class}"
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[expect(
        clippy::disallowed_types,
        reason = "unit test reads an in-memory tar entry synchronously"
    )]
    use std::io::Read as _;

    use super::*;

    #[test]
    fn default_options_are_clone_based() {
        let options = DockerSandboxOptions::default();
        assert_eq!(options.image, "buildpack-deps:noble");
        assert_eq!(options.network_mode.as_deref(), Some("bridge"));
        assert!(!options.skip_clone);
    }

    #[test]
    fn container_config_has_no_bind_mounts_or_socket() {
        let options = DockerSandboxOptions {
            env_vars: vec!["FOO=bar".to_string()],
            memory_limit: Some(4_000_000_000),
            cpu_quota: Some(200_000),
            ..DockerSandboxOptions::default()
        };
        let config = container_config(&options, None);
        let host_config = config.host_config.expect("host config");
        assert!(host_config.binds.is_none());
        assert_eq!(host_config.memory, Some(4_000_000_000));
        assert_eq!(host_config.cpu_quota, Some(200_000));
        assert_eq!(config.working_dir.as_deref(), Some(WORKING_DIRECTORY));
        assert_eq!(config.env, Some(vec!["FOO=bar".to_string()]));
        assert!(
            config
                .env
                .unwrap()
                .iter()
                .all(|value| !value.starts_with("DOCKER_HOST="))
        );
    }

    #[test]
    fn real_run_container_gets_name_and_labels() {
        let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
        assert_eq!(
            container_name(&run_id),
            "fabro-run-01HY0000000000000000000000"
        );
        let labels = container_labels(Some(&run_id));
        assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
        assert_eq!(
            labels.get(RUN_ID_LABEL).map(String::as_str),
            Some("01HY0000000000000000000000")
        );
    }

    #[test]
    fn label_validation_rejects_unmanaged_container() {
        let labels = HashMap::new();
        let error = verify_managed_labels("abc", &labels, None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("missing label sh.fabro.managed=true")
        );
    }

    #[test]
    fn single_file_tar_contains_named_file() {
        let bytes = build_single_file_tar("nested.txt", b"hello").unwrap();
        let mut archive = tar::Archive::new(Cursor::new(bytes));
        let mut entries = archive.entries().unwrap();
        let mut entry = entries.next().unwrap().unwrap();
        assert_eq!(entry.path().unwrap().to_string_lossy(), "nested.txt");
        let mut content = String::new();
        entry.read_to_string(&mut content).unwrap();
        assert_eq!(content, "hello");
    }
}
