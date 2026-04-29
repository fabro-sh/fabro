use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::time::Instant;

use async_trait::async_trait;
use daytona_sdk::api_types::SignedPortPreviewUrl;
use fabro_github::GitHubCredentials;
use fabro_types::RunId;
use rand::Rng;
use tokio::sync::OnceCell;
use tokio::{fs, time};
use tokio_util::sync::CancellationToken;

use crate::clone_source::{self, CloneDecision, EmptyWorkspaceReason};
use crate::sandbox::resolve_path;
use crate::{
    DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback,
    format_lines_numbered, shell_quote,
};

const WORKING_DIRECTORY: &str = "/home/daytona/workspace";
const DEFAULT_SNAPSHOT: &str = "daytona-medium";

pub use crate::config::{
    DaytonaNetwork, DaytonaSettings as DaytonaConfig,
    DaytonaSnapshotSettings as DaytonaSnapshotConfig, DockerfileSource,
};

/// Build a [`daytona_sdk::Client`], forwarding an optional API key from the
/// vault so the SDK doesn't have to rely on `DAYTONA_API_KEY` being in the
/// process environment.
async fn build_daytona_client(
    api_key: Option<String>,
) -> Result<daytona_sdk::Client, daytona_sdk::DaytonaError> {
    let sdk_config = daytona_sdk::DaytonaConfig {
        api_key,
        ..Default::default()
    };
    daytona_sdk::Client::new_with_config(sdk_config).await
}

/// Validate a Daytona API key by performing a single authenticated call. Used
/// at install time to confirm the operator-provided credential is accepted by
/// the Daytona control plane before persisting it. The call requests a single
/// sandbox listing entry to keep latency and quota impact minimal.
pub async fn validate_daytona_api_key(api_key: String) -> Result<(), daytona_sdk::DaytonaError> {
    let client = build_daytona_client(Some(api_key)).await?;
    client.list(None, Some(1), Some(1)).await?;
    Ok(())
}

fn elapsed_ms(start: &Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Sandbox that runs all operations inside a Daytona cloud sandbox.
pub struct DaytonaSandbox {
    config:           DaytonaConfig,
    client:           daytona_sdk::Client,
    github_app:       Option<GitHubCredentials>,
    sandbox:          OnceCell<daytona_sdk::Sandbox>,
    rg_available:     OnceCell<bool>,
    event_callback:   Option<SandboxEventCallback>,
    /// HTTPS origin URL stored after clone so we can refresh push credentials
    /// later.
    origin_url:       OnceCell<String>,
    repo_cloned:      OnceCell<bool>,
    run_id:           Option<RunId>,
    clone_origin_url: Option<String>,
    /// Explicit branch to clone. When set, overrides the branch detected by
    /// the submitted run spec.
    clone_branch:     Option<String>,
}

impl DaytonaSandbox {
    /// Create a new `DaytonaSandbox`, creating the Daytona client internally.
    ///
    /// `api_key` is the Daytona API key, typically resolved from the vault.
    /// When `None`, the SDK falls back to the `DAYTONA_API_KEY` env var.
    pub async fn new(
        config: DaytonaConfig,
        github_app: Option<GitHubCredentials>,
        run_id: Option<RunId>,
        clone_origin_url: Option<String>,
        clone_branch: Option<String>,
        api_key: Option<String>,
    ) -> crate::Result<Self> {
        let client = build_daytona_client(api_key)
            .await
            .map_err(|e| crate::Error::context("Failed to create Daytona client", e))?;
        Ok(Self {
            config,
            client,
            github_app,
            sandbox: OnceCell::new(),
            rg_available: OnceCell::const_new(),
            event_callback: None,
            origin_url: OnceCell::new(),
            repo_cloned: OnceCell::new(),
            run_id,
            clone_origin_url,
            clone_branch,
        })
    }

    /// Reconnect to an existing Daytona sandbox by name.
    ///
    /// Creates the client internally and fetches the sandbox, replacing the old
    /// `from_existing()` + manual client/get boilerplate at call sites.
    pub async fn reconnect(
        sandbox_name: &str,
        api_key: Option<String>,
        repo_cloned: bool,
        clone_origin_url: Option<String>,
        clone_branch: Option<String>,
    ) -> crate::Result<Self> {
        let client = build_daytona_client(api_key)
            .await
            .map_err(|e| crate::Error::context("Failed to create Daytona client", e))?;
        let sdk_sandbox = client.get(sandbox_name).await.map_err(|e| {
            crate::Error::context(
                format!("Failed to reconnect to Daytona sandbox '{sandbox_name}'"),
                e,
            )
        })?;
        let sandbox_cell = OnceCell::new();
        let _ = sandbox_cell.set(sdk_sandbox);
        let origin_url = OnceCell::new();
        if repo_cloned {
            if let Some(origin) = clone_origin_url.as_ref() {
                let _ = origin_url.set(origin.clone());
            }
        }
        let repo_cloned_cell = OnceCell::new();
        let _ = repo_cloned_cell.set(repo_cloned);
        Ok(Self {
            config: DaytonaConfig::default(),
            client,
            github_app: None,
            sandbox: sandbox_cell,
            rg_available: OnceCell::const_new(),
            event_callback: None,
            origin_url,
            repo_cloned: repo_cloned_cell,
            run_id: None,
            clone_origin_url,
            clone_branch,
        })
    }

    pub fn set_event_callback(&mut self, cb: SandboxEventCallback) {
        self.event_callback = Some(cb);
    }

    /// Get the `ComputerUseService` for this sandbox.
    ///
    /// Requires the sandbox to be initialized first.
    pub async fn computer_use(&self) -> crate::Result<daytona_sdk::ComputerUseService> {
        let sandbox = self.sandbox()?;
        sandbox
            .computer_use()
            .await
            .map_err(|e| crate::Error::context("Failed to get computer use service", e))
    }

    /// Create SSH access and return the connection command string.
    pub async fn create_ssh_access(&self, ttl_minutes: Option<f64>) -> crate::Result<String> {
        let sandbox = self.sandbox()?;
        let dto = sandbox
            .create_ssh_access(ttl_minutes)
            .await
            .map_err(|e| crate::Error::context("Failed to create SSH access", e))?;
        Ok(dto.ssh_command)
    }

    /// Get a preview link (URL + token) for a port on this sandbox.
    pub async fn get_preview_link(&self, port: u16) -> crate::Result<daytona_sdk::PreviewLink> {
        let sandbox = self.sandbox()?;
        sandbox.get_preview_link(port).await.map_err(|e| {
            crate::Error::context(format!("Failed to get preview link for port {port}"), e)
        })
    }

    /// Get a signed preview URL for a port on this sandbox.
    pub async fn get_signed_preview_url(
        &self,
        port: u16,
        expires_in_seconds: Option<i32>,
    ) -> crate::Result<SignedPortPreviewUrl> {
        let sandbox = self.sandbox()?;
        sandbox
            .get_signed_preview_url(i32::from(port), expires_in_seconds)
            .await
            .map_err(|e| {
                crate::Error::context(
                    format!("Failed to get signed preview URL for port {port}"),
                    e,
                )
            })
    }

    fn emit(&self, event: SandboxEvent) {
        event.trace();
        if let Some(ref cb) = self.event_callback {
            cb(event);
        }
    }

    fn fail_init(&self, init_start: Instant, err: crate::Error) -> crate::Error {
        let duration_ms = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::InitializeFailed {
            provider: "daytona".into(),
            error: err.to_string(),
            causes: err.causes(),
            duration_ms,
        });
        err
    }

    #[allow(
        clippy::unused_self,
        reason = "Path resolution stays on the sandbox type even though it only uses constants."
    )]
    fn resolve_path(&self, path: &str) -> String {
        resolve_path(path, WORKING_DIRECTORY)
    }

    /// Get the sandbox, returning an error if not yet initialized.
    fn sandbox(&self) -> crate::Result<&daytona_sdk::Sandbox> {
        self.sandbox.get().ok_or_else(|| {
            crate::Error::message("Daytona sandbox not initialized — call initialize() first")
        })
    }

    fn repo_cloned(&self) -> bool {
        self.repo_cloned.get().copied().unwrap_or(false)
    }

    /// Build `SandboxBaseParams` from config, generating a unique sandbox name.
    fn base_params(&self) -> daytona_sdk::SandboxBaseParams {
        let name = if let Some(ref id) = self.run_id {
            format!("fabro-{id}")
        } else {
            format!(
                "fabro-{}-{:04x}",
                chrono::Utc::now().format("%Y%m%d-%H%M%S"),
                rand::rng().random_range(0..0x10000u32),
            )
        };
        let (network_block_all, network_allow_list) = match &self.config.network {
            Some(DaytonaNetwork::Block) => (Some(true), None),
            Some(DaytonaNetwork::AllowAll) => (Some(false), None),
            Some(DaytonaNetwork::AllowList(cidrs)) => (None, Some(cidrs.clone())),
            None => (None, None),
        };
        daytona_sdk::SandboxBaseParams {
            name: Some(name),
            auto_stop_interval: self.config.auto_stop_interval,
            labels: self.config.labels.clone(),
            ephemeral: Some(true),
            network_block_all,
            network_allow_list,
            ..Default::default()
        }
    }

    /// Ensure the named snapshot exists and is active.
    ///
    /// If the snapshot doesn't exist and a dockerfile is provided, creates it
    /// and polls until it reaches `Active` state. Returns an error if the
    /// snapshot is in a terminal failure state.
    async fn ensure_snapshot(&self, snap_cfg: &DaytonaSnapshotConfig) -> crate::Result<()> {
        match self.client.snapshot.get(&snap_cfg.name).await {
            Ok(dto) => {
                use daytona_api_client::models::SnapshotState;
                match dto.state {
                    SnapshotState::Active => return Ok(()),
                    SnapshotState::Error | SnapshotState::BuildFailed => {
                        return Err(crate::Error::message(format!(
                            "Snapshot '{}' is in state '{}': {}",
                            snap_cfg.name,
                            dto.state,
                            dto.error_reason.unwrap_or_default()
                        )));
                    }
                    _ => {
                        // Building/Pending/Pulling — fall through to poll
                    }
                }
            }
            Err(daytona_sdk::DaytonaError::NotFound { .. }) => {
                let dockerfile = match &snap_cfg.dockerfile {
                    Some(DockerfileSource::Inline(s)) => s.as_str(),
                    Some(DockerfileSource::Path { .. }) => {
                        return Err(crate::Error::message(format!(
                            "Snapshot '{}': dockerfile path should have been resolved to inline content before sandbox creation",
                            snap_cfg.name
                        )));
                    }
                    None => {
                        return Err(crate::Error::message(format!(
                            "Snapshot '{}' does not exist and no dockerfile provided to create it",
                            snap_cfg.name
                        )));
                    }
                };

                let params = daytona_sdk::CreateSnapshotParams {
                    name:       snap_cfg.name.clone(),
                    image:      daytona_sdk::ImageSource::Custom(
                        daytona_sdk::DockerImage::from_dockerfile(dockerfile),
                    ),
                    resources:  Some(daytona_sdk::Resources {
                        cpu: snap_cfg.cpu,
                        memory: snap_cfg.memory,
                        disk: snap_cfg.disk,
                        ..Default::default()
                    }),
                    entrypoint: None,
                };
                self.client.snapshot.create(&params).await.map_err(|e| {
                    crate::Error::context(
                        format!("Failed to create snapshot '{}'", snap_cfg.name),
                        e,
                    )
                })?;
            }
            Err(e) => {
                return Err(crate::Error::context(
                    format!("Failed to get snapshot '{}'", snap_cfg.name),
                    e,
                ));
            }
        }

        // Poll until Active (or terminal failure).
        self.poll_snapshot_active(&snap_cfg.name).await
    }

    /// Poll a snapshot until it reaches `Active` state, with exponential
    /// back-off.
    async fn poll_snapshot_active(&self, name: &str) -> crate::Result<()> {
        use daytona_api_client::models::SnapshotState;
        let mut delay = std::time::Duration::from_secs(2);
        let max_delay = std::time::Duration::from_secs(30);
        let deadline = Instant::now() + std::time::Duration::from_mins(10);

        while Instant::now() < deadline {
            time::sleep(delay).await;
            let dto = self.client.snapshot.get(name).await.map_err(|e| {
                crate::Error::context(format!("Failed to poll snapshot '{name}'"), e)
            })?;

            match dto.state {
                SnapshotState::Active => return Ok(()),
                SnapshotState::Error | SnapshotState::BuildFailed => {
                    return Err(crate::Error::message(format!(
                        "Snapshot '{name}' failed ({}): {}",
                        dto.state,
                        dto.error_reason.unwrap_or_default()
                    )));
                }
                _ => {
                    delay = (delay * 2).min(max_delay);
                }
            }
        }

        Err(crate::Error::message(format!(
            "Timed out waiting for snapshot '{name}' to become active"
        )))
    }
}

/// Detect the git remote URL and current branch from a local repository.
///
/// Uses `git2` to discover the repo at `path`, reads the `origin` remote URL
/// and the HEAD branch name.
pub fn detect_repo_info(path: &Path) -> crate::Result<(String, Option<String>)> {
    let repo = git2::Repository::discover(path).map_err(|e| {
        crate::Error::context(
            format!("Failed to discover git repo at {}", path.display()),
            e,
        )
    })?;

    let url = repo
        .find_remote("origin")
        .map_err(|e| crate::Error::context("Failed to find 'origin' remote", e))?
        .url()
        .ok_or_else(|| crate::Error::message("origin remote URL is not valid UTF-8"))?
        .to_string();

    let branch = repo
        .head()
        .ok()
        .and_then(|head| head.shorthand().map(String::from));

    Ok((url, branch))
}

#[async_trait]
impl Sandbox for DaytonaSandbox {
    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> crate::Result<()> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(remote_path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| crate::Error::context("Failed to get fs service", e))?;

        let bytes = fs_svc
            .download_file(&resolved)
            .await
            .map_err(|e| crate::Error::context(format!("Failed to download file {resolved}"), e))?;

        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| crate::Error::context("Failed to create parent dirs", e))?;
        }
        fs::write(local_path, &bytes).await.map_err(|e| {
            crate::Error::context(format!("Failed to write {}", local_path.display()), e)
        })?;

        Ok(())
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> crate::Result<()> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(remote_path);

        // Ensure parent directory exists
        if let Some(parent) = Path::new(&resolved).parent() {
            let parent_str = parent.to_string_lossy();
            if parent_str != "/" {
                let fs_svc = sandbox
                    .fs()
                    .await
                    .map_err(|e| crate::Error::context("Failed to get fs service", e))?;
                let _ = fs_svc.create_folder(&parent_str, None).await;
            }
        }

        let bytes = fs::read(local_path).await.map_err(|e| {
            crate::Error::context(format!("Failed to read {}", local_path.display()), e)
        })?;

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| crate::Error::context("Failed to get fs service", e))?;

        fs_svc
            .upload_file_bytes(&resolved, &bytes)
            .await
            .map_err(|e| crate::Error::context(format!("Failed to upload file {resolved}"), e))?;

        Ok(())
    }

    async fn initialize(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::Initializing {
            provider: "daytona".into(),
        });
        let init_start = Instant::now();

        let params = if let Some(ref snap_cfg) = self.config.snapshot {
            self.emit(SandboxEvent::SnapshotEnsuring {
                name: snap_cfg.name.clone(),
            });
            let snap_start = Instant::now();
            if let Err(e) = self.ensure_snapshot(snap_cfg).await {
                self.emit(SandboxEvent::SnapshotFailed {
                    name:   snap_cfg.name.clone(),
                    error:  e.to_string(),
                    causes: e.causes(),
                });
                return Err(self.fail_init(init_start, e));
            }
            let snap_duration = u64::try_from(snap_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::SnapshotReady {
                name:        snap_cfg.name.clone(),
                duration_ms: snap_duration,
            });

            daytona_sdk::CreateParams::Snapshot(daytona_sdk::SnapshotParams {
                base:     self.base_params(),
                snapshot: snap_cfg.name.clone(),
            })
        } else {
            daytona_sdk::CreateParams::Snapshot(daytona_sdk::SnapshotParams {
                base:     self.base_params(),
                snapshot: DEFAULT_SNAPSHOT.to_string(),
            })
        };

        tracing::info!("Creating Daytona sandbox");
        let sandbox = self
            .client
            .create(params, daytona_sdk::CreateSandboxOptions::default())
            .await
            .map_err(|e| {
                self.fail_init(
                    init_start,
                    crate::Error::context("Failed to create Daytona sandbox", e),
                )
            })?;

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
                        provider = "daytona",
                        reason = reason.message(),
                        "Clone source missing for clone-based sandbox"
                    );
                }
                let fs_svc = sandbox
                    .fs()
                    .await
                    .map_err(|e| crate::Error::context("Failed to get Daytona fs service", e))?;
                fs_svc
                    .create_folder(WORKING_DIRECTORY, None)
                    .await
                    .map_err(|e| crate::Error::context("Failed to create working directory", e))?;
                let _ = self.repo_cloned.set(false);
            }
            CloneDecision::GitHub { origin_url, branch } => {
                self.emit(SandboxEvent::GitCloneStarted {
                    url:    origin_url.clone(),
                    branch: branch.clone(),
                });
                let clone_start = Instant::now();

                let (username, password) = match &self.github_app {
                    Some(creds) => {
                        let (owner, repo) = fabro_github::parse_github_owner_repo(&origin_url)
                            .map_err(|e| {
                                let err = crate::Error::message(format!(
                                    "Failed to parse GitHub URL for clone: {e}"
                                ));
                                self.emit(SandboxEvent::GitCloneFailed {
                                    url:    origin_url.clone(),
                                    error:  err.to_string(),
                                    causes: err.causes(),
                                });
                                err
                            })?;
                        fabro_github::resolve_clone_credentials(
                            &fabro_github::GitHubContext::new(
                                creds,
                                &fabro_github::github_api_base_url(),
                            ),
                            &owner,
                            &repo,
                        )
                        .await
                        .map_err(|e| {
                            let err = crate::Error::message(format!(
                                "Failed to get GitHub App credentials for clone: {e}"
                            ));
                            self.emit(SandboxEvent::GitCloneFailed {
                                url:    origin_url.clone(),
                                error:  err.to_string(),
                                causes: err.causes(),
                            });
                            self.fail_init(init_start, err)
                        })?
                    }
                    None => (None, None),
                };

                let git_svc = sandbox.git().await.map_err(|e| {
                    let err = crate::Error::context("Failed to get Daytona git service", e);
                    self.emit(SandboxEvent::GitCloneFailed {
                        url:    origin_url.clone(),
                        error:  err.to_string(),
                        causes: err.causes(),
                    });
                    self.fail_init(init_start, err)
                })?;

                let clone_token = password.clone();
                let clone_result = git_svc
                    .clone(
                        &origin_url,
                        WORKING_DIRECTORY,
                        daytona_sdk::GitCloneOptions {
                            branch,
                            username,
                            password,
                            ..Default::default()
                        },
                    )
                    .await;

                match clone_result {
                    Ok(()) => {
                        let clone_duration =
                            u64::try_from(clone_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                        self.emit(SandboxEvent::GitCloneCompleted {
                            url:         origin_url.clone(),
                            duration_ms: clone_duration,
                        });

                        let _ = self.repo_cloned.set(true);
                        let _ = self.origin_url.set(origin_url.clone());
                        if let Some(token) = clone_token {
                            let process_svc = sandbox.process().await.ok();
                            if let Some(ps) = process_svc {
                                match fabro_github::embed_token_in_url(&origin_url, &token) {
                                    Ok(auth_url) => {
                                        let cmd = format!(
                                            "git -c maintenance.auto=0 remote set-url origin {}",
                                            shell_quote(auth_url.as_raw_url().as_str()),
                                        );
                                        let opts = daytona_sdk::ExecuteCommandOptions {
                                            cwd: Some(WORKING_DIRECTORY.to_string()),
                                            ..Default::default()
                                        };
                                        let wrapped = wrap_bash_command(&cmd);
                                        match ps.execute_command(&wrapped, opts).await {
                                            Ok(r) if r.exit_code != 0 => {
                                                let stderr = r.result.replace(
                                                    &auth_url.raw_string(),
                                                    &auth_url.redacted_string(),
                                                );
                                                tracing::warn!(
                                                    exit_code = r.exit_code,
                                                    output = %stderr.trim(),
                                                    "Failed to set Daytona sandbox push credentials \
                                                     on origin — subsequent git push from this \
                                                     sandbox will fail"
                                                );
                                            }
                                            Ok(_) => {}
                                            Err(e) => {
                                                tracing::warn!(
                                                    error = %e,
                                                    "Daytona exec failed while setting push credentials \
                                                     on origin — subsequent git push from this \
                                                     sandbox will fail"
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            origin = %origin_url,
                                            error = %e,
                                            "Failed to build authenticated origin URL — \
                                             subsequent git push from this sandbox will fail"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) if self.github_app.is_none() => {
                        let err = crate::Error::context(
                            "Git clone failed. If this is a private repository, \
                             configure a GitHub App with `fabro install` and install it \
                             for your organization.",
                            e,
                        );
                        self.emit(SandboxEvent::GitCloneFailed {
                            url:    origin_url,
                            error:  err.to_string(),
                            causes: err.causes(),
                        });
                        return Err(self.fail_init(init_start, err));
                    }
                    Err(e) => {
                        let err =
                            crate::Error::context("Failed to clone repo into Daytona sandbox", e);
                        self.emit(SandboxEvent::GitCloneFailed {
                            url:    origin_url,
                            error:  err.to_string(),
                            causes: err.causes(),
                        });
                        return Err(self.fail_init(init_start, err));
                    }
                }
            }
        }

        let sandbox_name = sandbox.name.clone();
        let sandbox_cpu = sandbox.cpu;
        let sandbox_memory = sandbox.memory;
        self.sandbox
            .set(sandbox)
            .map_err(|_| crate::Error::message("Daytona sandbox already initialized"))?;
        tracing::info!("Daytona sandbox ready");

        let init_duration = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::Ready {
            provider:    "daytona".into(),
            duration_ms: init_duration,
            name:        Some(sandbox_name),
            cpu:         Some(sandbox_cpu),
            memory:      Some(sandbox_memory),
            url:         Some("https://app.daytona.io/dashboard/sandboxes".into()),
        });

        Ok(())
    }

    async fn cleanup(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::CleanupStarted {
            provider: "daytona".into(),
        });
        let start = Instant::now();
        if let Some(sandbox) = self.sandbox.get() {
            tracing::info!("Destroying Daytona sandbox");
            if let Err(e) = sandbox.delete().await {
                let err = crate::Error::context("Failed to delete Daytona sandbox", e);
                self.emit(SandboxEvent::CleanupFailed {
                    provider: "daytona".into(),
                    error:    err.to_string(),
                    causes:   err.causes(),
                });
                return Err(err);
            }
        }
        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::CleanupCompleted {
            provider: "daytona".into(),
            duration_ms,
        });
        Ok(())
    }

    fn working_directory(&self) -> &str {
        WORKING_DIRECTORY
    }

    fn platform(&self) -> &'static str {
        "linux"
    }

    fn os_version(&self) -> String {
        "Linux (Daytona)".to_string()
    }

    fn sandbox_info(&self) -> String {
        self.sandbox
            .get()
            .map(|s| s.name.clone())
            .unwrap_or_default()
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

    async fn git_push_ref(&self, refspec: &str) -> bool {
        if !self.repo_cloned() {
            return false;
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

    async fn ssh_access_command(&self) -> crate::Result<Option<String>> {
        self.create_ssh_access(Some(60.0)).await.map(Some)
    }

    fn origin_url(&self) -> Option<&str> {
        if !self.repo_cloned() {
            return None;
        }
        self.origin_url.get().map(String::as_str)
    }

    async fn get_preview_url(
        &self,
        port: u16,
    ) -> crate::Result<Option<(String, HashMap<String, String>)>> {
        let sandbox = self.sandbox()?;
        let preview = sandbox.get_preview_link(port).await.map_err(|e| {
            crate::Error::context(format!("Failed to get preview link for port {port}"), e)
        })?;
        let mut headers = HashMap::new();
        if !preview.token.is_empty() {
            headers.insert("x-daytona-preview-token".to_string(), preview.token);
        }
        headers.insert(
            "X-Daytona-Skip-Preview-Warning".to_string(),
            "true".to_string(),
        );
        Ok(Some((preview.url, headers)))
    }

    async fn refresh_push_credentials(&self) -> crate::Result<()> {
        if !self.repo_cloned() {
            return Ok(());
        }
        let Some(origin_url) = self.origin_url.get() else {
            return Ok(()); // no authenticated origin — nothing to refresh
        };
        let Some(creds) = &self.github_app else {
            return Ok(());
        };

        let auth_url = fabro_github::resolve_authenticated_url(
            &fabro_github::GitHubContext::new(creds, &fabro_github::github_api_base_url()),
            origin_url,
        )
        .await
        .map_err(|e| crate::Error::message(format!("Failed to refresh GitHub App token: {e}")))?;

        let cmd = format!(
            "git -c maintenance.auto=0 remote set-url origin {}",
            shell_quote(auth_url.as_raw_url().as_str()),
        );
        let result = self
            .exec_command(&cmd, 10_000, None, None, None)
            .await
            .map_err(|e| crate::Error::context("Failed to set refreshed push credentials", e))?;
        if result.exit_code != 0 {
            let stderr = result
                .stderr
                .replace(&auth_url.raw_string(), &auth_url.redacted_string());
            return Err(crate::Error::message(format!(
                "Failed to set refreshed push credentials (exit {}): {}",
                result.exit_code, stderr
            )));
        }

        Ok(())
    }

    async fn set_autostop_interval(&self, minutes: i32) -> crate::Result<()> {
        let sandbox_id = self.sandbox()?.id.clone();
        let mut sandbox =
            self.client.get(&sandbox_id).await.map_err(|e| {
                crate::Error::context("Failed to get sandbox for autostop update", e)
            })?;
        sandbox
            .set_autostop_interval(minutes)
            .await
            .map_err(|e| crate::Error::context("Failed to set autostop interval", e))
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> crate::Result<String> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| crate::Error::context("Failed to get fs service", e))?;

        let bytes = fs_svc
            .download_file(&resolved)
            .await
            .map_err(|e| crate::Error::context(format!("Failed to read file {resolved}"), e))?;

        let content = String::from_utf8(bytes)
            .map_err(|e| crate::Error::context("File is not valid UTF-8", e))?;

        Ok(format_lines_numbered(&content, offset, limit))
    }

    async fn write_file(&self, path: &str, content: &str) -> crate::Result<()> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        // Ensure parent directory exists
        if let Some(parent) = Path::new(&resolved).parent() {
            let parent_str = parent.to_string_lossy();
            if parent_str != "/" {
                let fs_svc = sandbox
                    .fs()
                    .await
                    .map_err(|e| crate::Error::context("Failed to get fs service", e))?;
                let _ = fs_svc.create_folder(&parent_str, None).await;
            }
        }

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| crate::Error::context("Failed to get fs service", e))?;

        fs_svc
            .upload_file_bytes(&resolved, content.as_bytes())
            .await
            .map_err(|e| crate::Error::context(format!("Failed to write file {resolved}"), e))?;

        Ok(())
    }

    async fn delete_file(&self, path: &str) -> crate::Result<()> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| crate::Error::context("Failed to get fs service", e))?;

        fs_svc
            .delete_file(&resolved, false)
            .await
            .map_err(|e| crate::Error::context(format!("Failed to delete file {resolved}"), e))?;

        Ok(())
    }

    async fn file_exists(&self, path: &str) -> crate::Result<bool> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| crate::Error::context("Failed to get fs service", e))?;

        match fs_svc.get_file_info(&resolved).await {
            Ok(_) => Ok(true),
            Err(daytona_sdk::DaytonaError::NotFound { .. }) => Ok(false),
            Err(e) => Err(crate::Error::context(
                format!("Failed to check file existence {resolved}"),
                e,
            )),
        }
    }

    async fn list_directory(
        &self,
        path: &str,
        _depth: Option<usize>,
    ) -> crate::Result<Vec<DirEntry>> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| crate::Error::context("Failed to get fs service", e))?;

        let files = fs_svc.list_files(&resolved).await.map_err(|e| {
            crate::Error::context(format!("Failed to list directory {resolved}"), e)
        })?;

        Ok(files
            .into_iter()
            .map(|f| DirEntry {
                name:   f.name,
                is_dir: f.is_dir,
                size:   if f.size > 0 {
                    u64::try_from(f.size).ok()
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
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> crate::Result<ExecResult> {
        tracing::info!(command, timeout_ms, "exec_command: entered");

        let sandbox = self.sandbox()?;
        let start = Instant::now();

        let cwd =
            working_dir.map_or_else(|| WORKING_DIRECTORY.to_string(), |d| self.resolve_path(d));

        let process_svc = sandbox
            .process()
            .await
            .map_err(|e| crate::Error::context("Failed to get process service", e))?;

        tracing::info!(
            elapsed_ms = elapsed_ms(&start),
            "exec_command: process service acquired, starting select"
        );

        let options = daytona_sdk::ExecuteCommandOptions {
            cwd:     Some(cwd),
            env:     env_vars.cloned(),
            timeout: Some(std::time::Duration::from_millis(timeout_ms)),
        };

        // The Daytona toolbox's /process/execute endpoint does not yet
        // process the `envs` field (not in its OpenAPI spec), so we also
        // prepend `export` statements as a fallback until server support
        // lands. The SDK sends `envs` too for forward compatibility.
        let command_with_env = if let Some(vars) = env_vars {
            if vars.is_empty() {
                command.to_string()
            } else {
                let exports: Vec<String> = vars
                    .iter()
                    .map(|(k, v)| format!("export {}={}", shell_quote(k), shell_quote(v)))
                    .collect();
                format!("{}\n{}", exports.join("\n"), command)
            }
        } else {
            command.to_string()
        };

        // Wrap with `bash -c` so pipes, env vars, and shell features work.
        // The Daytona API uses direct exec, not a shell.
        let wrapped = wrap_bash_command(&command_with_env);

        let timeout_duration = std::time::Duration::from_millis(timeout_ms + 2000); // 2s grace period
        let token = cancel_token.unwrap_or_default();
        let exec_future = process_svc.execute_command(&wrapped, options);

        let result = tokio::select! {
            res = exec_future => {
                tracing::info!(
                    elapsed_ms = elapsed_ms(&start),
                    ok = res.is_ok(),
                    "exec_command: HTTP response received"
                );
                res.map_err(|e| crate::Error::context("Failed to execute command", e))?
            }
            () = time::sleep(timeout_duration) => {
                tracing::info!(
                    elapsed_ms = elapsed_ms(&start),
                    timeout_ms,
                    "exec_command: client-side timeout fired"
                );
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command timed out locally".to_string(),
                    exit_code: -1,
                    timed_out: true,
                    duration_ms: elapsed_ms(&start),
                });
            }
            () = token.cancelled() => {
                tracing::info!(
                    elapsed_ms = elapsed_ms(&start),
                    "exec_command: cancelled via token"
                );
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command cancelled".to_string(),
                    exit_code: -1,
                    timed_out: true,
                    duration_ms: elapsed_ms(&start),
                });
            }
        };

        let duration_ms = elapsed_ms(&start);

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
    ) -> crate::Result<Vec<String>> {
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
                let _ = write!(cmd, " --glob {}", shell_quote(glob_filter));
            }
            if let Some(max) = options.max_results {
                let _ = write!(cmd, " --max-count {max}");
            }
            let _ = write!(
                cmd,
                " -- {} {}",
                shell_quote(pattern),
                shell_quote(&resolved)
            );
            cmd
        } else {
            let mut cmd = "grep -rn".to_string();
            if options.case_insensitive {
                cmd.push_str(" -i");
            }
            if let Some(ref glob_filter) = options.glob_filter {
                let _ = write!(cmd, " --include {}", shell_quote(glob_filter));
            }
            if let Some(max) = options.max_results {
                let _ = write!(cmd, " -m {max}");
            }
            let _ = write!(
                cmd,
                " -- {} {}",
                shell_quote(pattern),
                shell_quote(&resolved)
            );
            cmd
        };

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;

        if result.exit_code == 1 {
            // Both rg and grep exit 1 for no matches
            return Ok(Vec::new());
        }
        if result.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "grep failed (exit {}): {}",
                result.exit_code, result.stderr
            )));
        }

        Ok(result.stdout.lines().map(String::from).collect())
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> crate::Result<Vec<String>> {
        let base = path.map_or_else(|| WORKING_DIRECTORY.to_string(), |p| self.resolve_path(p));

        let cmd = format!(
            "find {} -name {} -type f | sort",
            shell_quote(&base),
            shell_quote(pattern),
        );

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;

        if result.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "glob failed (exit {}): {}",
                result.exit_code, result.stderr
            )));
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
    use base64::engine::general_purpose::STANDARD;
    let encoded = STANDARD.encode(command);
    format!("sh -c \"echo '{encoded}' | base64 -d | sh\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daytona_config_defaults() {
        let config = DaytonaConfig::default();
        assert!(config.snapshot.is_none());
        assert!(config.auto_stop_interval.is_none());
        assert!(config.labels.is_none());
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
    fn network_block_from_string() {
        let config: DaytonaConfig = toml::from_str(r#"network = "block""#).unwrap();
        assert_eq!(config.network, Some(DaytonaNetwork::Block));
    }

    #[test]
    fn network_allow_all_from_string() {
        let config: DaytonaConfig = toml::from_str(r#"network = "allow_all""#).unwrap();
        assert_eq!(config.network, Some(DaytonaNetwork::AllowAll));
    }

    #[test]
    fn network_allow_list_from_table() {
        let config: DaytonaConfig =
            toml::from_str(r#"network = { allow_list = ["10.0.0.0/8", "172.16.0.0/12"] }"#)
                .unwrap();
        assert_eq!(
            config.network,
            Some(DaytonaNetwork::AllowList(vec![
                "10.0.0.0/8".into(),
                "172.16.0.0/12".into(),
            ]))
        );
    }

    #[test]
    fn network_typo_string_error() {
        let err = toml::from_str::<DaytonaConfig>(r#"network = "blck""#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(r#"unknown network mode "blck""#),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn network_wrong_type_error() {
        let err = toml::from_str::<DaytonaConfig>("network = 42").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("expected") && msg.contains("allow_list"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn network_unknown_key_error() {
        let err = toml::from_str::<DaytonaConfig>(r#"network = { mode = "block" }"#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(r#"unknown key "mode""#),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn network_empty_table_error() {
        let err = toml::from_str::<DaytonaConfig>("network = {}").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("empty table"), "unexpected error: {msg}");
    }

    #[test]
    fn network_empty_allow_list_error() {
        let err = toml::from_str::<DaytonaConfig>("network = { allow_list = [] }").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("allow_list must not be empty"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn network_extra_key_error() {
        let err = toml::from_str::<DaytonaConfig>(
            r#"network = { allow_list = ["10.0.0.0/8"], extra = true }"#,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(r#"unexpected key "extra""#),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn detect_repo_info_returns_worktree_branch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        // Create an initial commit so HEAD exists
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit = repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        repo.remote("origin", "https://github.com/org/repo.git")
            .unwrap();

        // Create and check out a fabro/run/... branch (simulating worktree setup)
        let commit_obj = repo.find_commit(commit).unwrap();
        repo.branch("fabro/run/ABC", &commit_obj, false).unwrap();
        repo.set_head("refs/heads/fabro/run/ABC").unwrap();

        let (_, branch) = detect_repo_info(dir.path()).unwrap();
        // Documents the current behavior: detect_repo_info returns whatever HEAD points
        // to
        assert_eq!(branch, Some("fabro/run/ABC".into()));
    }
}
