use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use async_trait::async_trait;
use daytona_api_client::apis::api_keys_api;
use daytona_api_client::apis::configuration::Configuration;
use daytona_api_client::models::api_key_list::Permissions;
use daytona_sdk::api_types::SignedPortPreviewUrl;
use daytona_sdk::toolbox_types::Command as SessionCommandResult;
use daytona_sdk::{DaytonaError, SessionCommandLogsResult};
use fabro_github::GitHubCredentials;
use fabro_static::EnvVars;
use fabro_types::{CommandOutputStream, CommandTermination, RunId};
use fabro_util::time::elapsed_ms;
use rand::Rng;
use tokio::runtime::Handle;
use tokio::sync::{Mutex, OnceCell};
use tokio::task::JoinHandle;
use tokio::{fs, time};
use tokio_util::sync::CancellationToken;

use crate::clone_source::{self, CloneDecision, EmptyWorkspaceReason};
use crate::redact::redact_auth_url;
use crate::sandbox::resolve_path;
use crate::{
    CommandOutputCallback, DirEntry, ExecResult, ExecStreamingResult, GrepOptions, Sandbox,
    SandboxEvent, SandboxEventCallback, format_lines_numbered, shell_quote,
};

const WORKING_DIRECTORY: &str = "/home/daytona/workspace";
const DEFAULT_SNAPSHOT: &str = "daytona-medium";
pub const DEFAULT_DAYTONA_API_URL: &str = "https://app.daytona.io/api";
const FABRO_SANDBOX_USER_AGENT: &str = concat!("fabro-sandbox/", env!("CARGO_PKG_VERSION"));
const DAYTONA_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Permissions a Daytona API key needs for Fabro's snapshot and sandbox flow.
pub const REQUIRED_DAYTONA_PERMISSIONS: &[Permissions] = &[
    Permissions::WriteColonSnapshots,
    Permissions::DeleteColonSnapshots,
    Permissions::WriteColonSandboxes,
    Permissions::DeleteColonSandboxes,
];

pub use crate::config::{
    DaytonaNetwork, DaytonaSettings as DaytonaConfig,
    DaytonaSnapshotSettings as DaytonaSnapshotConfig, DockerfileSource,
};

#[derive(Debug)]
pub struct DaytonaKeyCheck {
    pub key_name: String,
    pub missing:  Vec<Permissions>,
}

impl DaytonaKeyCheck {
    pub fn ok(&self) -> bool {
        self.missing.is_empty()
    }

    pub fn missing_display(&self) -> String {
        join_perms(&self.missing)
    }

    pub fn missing_message(&self) -> String {
        format!(
            "API key '{}' is missing required Daytona scopes: {}. \
             Regenerate the key with all snapshot and sandbox scopes.",
            self.key_name,
            self.missing_display()
        )
    }
}

pub fn required_perms_display() -> String {
    join_perms(REQUIRED_DAYTONA_PERMISSIONS)
}

fn join_perms(perms: &[Permissions]) -> String {
    perms
        .iter()
        .copied()
        .map(perm_wire_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn perm_wire_str(permission: Permissions) -> &'static str {
    match permission {
        Permissions::WriteColonSnapshots => "write:snapshots",
        Permissions::DeleteColonSnapshots => "delete:snapshots",
        Permissions::WriteColonSandboxes => "write:sandboxes",
        Permissions::DeleteColonSandboxes => "delete:sandboxes",
        _ => "unknown",
    }
}

/// Build a [`daytona_sdk::Client`], forwarding an optional API key from the
/// vault so the SDK doesn't have to rely on `DAYTONA_API_KEY` being in the
/// process environment.
async fn build_daytona_client(
    api_key: Option<String>,
) -> Result<daytona_sdk::Client, daytona_sdk::DaytonaError> {
    build_daytona_client_with(api_key, None, None).await
}

async fn build_daytona_client_with(
    api_key: Option<String>,
    api_url: Option<String>,
    organization_id: Option<String>,
) -> Result<daytona_sdk::Client, daytona_sdk::DaytonaError> {
    let sdk_config = daytona_sdk::DaytonaConfig {
        api_key,
        api_url,
        organization_id,
        ..Default::default()
    };
    daytona_sdk::Client::new_with_config(sdk_config).await
}

#[expect(
    clippy::disallowed_methods,
    reason = "This is the production env-resolving Daytona credential probe facade."
)]
pub async fn check_daytona_api_key(api_key: String) -> anyhow::Result<DaytonaKeyCheck> {
    let base_url = std::env::var(EnvVars::DAYTONA_API_URL)
        .or_else(|_| std::env::var(EnvVars::DAYTONA_SERVER_URL))
        .unwrap_or_else(|_| DEFAULT_DAYTONA_API_URL.to_string());
    let org_id = std::env::var(EnvVars::DAYTONA_ORGANIZATION_ID).ok();
    check_daytona_api_key_with(&base_url, org_id.as_deref(), api_key).await
}

pub async fn check_daytona_api_key_with(
    base_url: &str,
    org_id: Option<&str>,
    api_key: String,
) -> anyhow::Result<DaytonaKeyCheck> {
    let work = async {
        let client = build_daytona_client_with(
            Some(api_key.clone()),
            Some(base_url.to_string()),
            org_id.map(str::to_string),
        )
        .await
        .map_err(anyhow::Error::new)
        .context("failed to construct Daytona client")?;
        client
            .list(None, Some(1), Some(1))
            .await
            .map_err(anyhow::Error::new)
            .context("failed to authenticate with Daytona")?;

        let api_config = build_api_keys_configuration(base_url, &api_key);
        let info = api_keys_api::get_current_api_key(&api_config, org_id)
            .await
            .map_err(anyhow::Error::new)
            .context("failed to read current Daytona API key")?;
        let missing = REQUIRED_DAYTONA_PERMISSIONS
            .iter()
            .copied()
            .filter(|permission| !info.permissions.contains(permission))
            .collect();

        Ok::<_, anyhow::Error>(DaytonaKeyCheck {
            key_name: info.name,
            missing,
        })
    };

    match time::timeout(DAYTONA_PROBE_TIMEOUT, work).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "Daytona credential probe timed out after {}s",
            DAYTONA_PROBE_TIMEOUT.as_secs()
        )),
    }
}

fn build_api_keys_configuration(base_url: &str, api_key: &str) -> Configuration {
    let mut cfg = Configuration::new();
    cfg.base_path = base_url.to_string();
    cfg.bearer_access_token = Some(api_key.to_string());
    cfg.user_agent = Some(FABRO_SANDBOX_USER_AGENT.to_string());
    cfg
}

fn command_kind(command: &str) -> &'static str {
    match command.split_whitespace().next().unwrap_or_default() {
        "git" => "git",
        "sh" | "/bin/sh" => "sh",
        "bash" | "/bin/bash" => "bash",
        "rg" => "rg",
        "grep" => "grep",
        "find" => "find",
        "cat" => "cat",
        "ls" => "ls",
        "mkdir" => "mkdir",
        "rm" => "rm",
        "printf" => "printf",
        _ => "other",
    }
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
                        self.emit(SandboxEvent::SnapshotCreating {
                            name: snap_cfg.name.clone(),
                        });
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

                self.emit(SandboxEvent::SnapshotCreating {
                    name: snap_cfg.name.clone(),
                });

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
                                                let err = crate::Error::exec(
                                                    "git remote set-url origin (Daytona post-clone)",
                                                    ExecResult {
                                                        stdout:      String::new(),
                                                        stderr:      redact_auth_url(
                                                            &r.result,
                                                            Some(&auth_url),
                                                        ),
                                                        exit_code:   Some(r.exit_code),
                                                        termination: CommandTermination::Exited,
                                                        duration_ms: 0,
                                                    },
                                                );
                                                tracing::warn!(
                                                    error = %crate::display_for_log(&err),
                                                    "Failed to set Daytona sandbox push credentials \
                                                     on origin — subsequent git push from this \
                                                     sandbox will fail"
                                                );
                                            }
                                            Ok(_) => {}
                                            Err(_) => {
                                                tracing::warn!(
                                                    error_class = "daytona_set_url_exec_failed",
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
        .map_err(|_| {
            crate::Error::message("Failed to refresh push credentials: token_mint_failed")
        })?;

        let cmd = format!(
            "git -c maintenance.auto=0 remote set-url origin {}",
            shell_quote(auth_url.as_raw_url().as_str()),
        );
        let result = self
            .exec_command(&cmd, 10_000, None, None, None)
            .await
            .map_err(|_| {
                crate::Error::message("Failed to refresh push credentials: set_url_exec_failed")
            })?;
        if !result.is_success() {
            return Err(result.into_exec_error_with_redactor(
                "git remote set-url origin (refresh push credentials)",
                |s| redact_auth_url(s, Some(&auth_url)),
            ));
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
        tracing::info!(
            timeout_ms,
            command_kind = command_kind(command),
            command_len = command.len(),
            "exec_command: entered"
        );

        let sandbox = self.sandbox()?;
        let start = Instant::now();

        let cwd =
            working_dir.map_or_else(|| WORKING_DIRECTORY.to_string(), |d| self.resolve_path(d));

        let process_svc = sandbox
            .process()
            .await
            .map_err(|e| crate::Error::context("Failed to get process service", e))?;

        tracing::info!(
            elapsed_ms = elapsed_ms(start),
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
                    elapsed_ms = elapsed_ms(start),
                    ok = res.is_ok(),
                    "exec_command: HTTP response received"
                );
                res.map_err(|e| crate::Error::context("Failed to execute command", e))?
            }
            () = time::sleep(timeout_duration) => {
                tracing::info!(
                    elapsed_ms = elapsed_ms(start),
                    timeout_ms,
                    "exec_command: client-side timeout fired"
                );
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command timed out locally".to_string(),
                    exit_code: None,
                    termination: CommandTermination::TimedOut,
                    duration_ms: elapsed_ms(start),
                });
            }
            () = token.cancelled() => {
                tracing::info!(
                    elapsed_ms = elapsed_ms(start),
                    "exec_command: cancelled via token"
                );
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command cancelled".to_string(),
                    exit_code: None,
                    termination: CommandTermination::Cancelled,
                    duration_ms: elapsed_ms(start),
                });
            }
        };

        let duration_ms = elapsed_ms(start);

        // The Daytona SDK returns combined output in `result` field.
        // Separate stderr isn't available in the simple execute_command API.
        Ok(ExecResult {
            stdout: result.result.clone(),
            stderr: String::new(),
            exit_code: Some(result.exit_code),
            termination: CommandTermination::Exited,
            duration_ms,
        })
    }

    async fn exec_command_streaming(
        &self,
        command: &str,
        timeout_ms: Option<u64>,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
        output_callback: CommandOutputCallback,
    ) -> crate::Result<ExecStreamingResult> {
        let sandbox = self.sandbox()?;
        let start = Instant::now();
        let cwd =
            working_dir.map_or_else(|| WORKING_DIRECTORY.to_string(), |d| self.resolve_path(d));

        let mut session = DaytonaSession::create(sandbox).await?;

        let session_command = build_session_command(command, &cwd, env_vars);
        let session_exec = match session.execute(&session_command, true, true).await {
            Ok(result) => result,
            Err(err) => {
                session.close("command start failure").await;
                return Err(crate::Error::context(
                    "Failed to execute Daytona session command",
                    err,
                ));
            }
        };
        let command_id = session_exec.cmd_id;

        let stream_process_svc = match sandbox.process().await {
            Ok(process_svc) => process_svc,
            Err(err) => {
                session.close("stream setup failure").await;
                return Err(crate::Error::context("Failed to get process service", err));
            }
        };
        let stdout_seen = Arc::new(Mutex::new(Vec::new()));
        let stderr_seen = Arc::new(Mutex::new(Vec::new()));
        let saw_live_chunk = Arc::new(AtomicBool::new(false));

        let stream_session_id = session.id().to_string();
        let stream_command_id = command_id.clone();
        let stdout_callback = output_callback.clone();
        let stderr_callback = output_callback.clone();
        let stdout_seen_for_stream = Arc::clone(&stdout_seen);
        let stderr_seen_for_stream = Arc::clone(&stderr_seen);
        let stdout_live = Arc::clone(&saw_live_chunk);
        let stderr_live = Arc::clone(&saw_live_chunk);
        let mut stream_task = tokio::spawn(async move {
            stream_process_svc
                .get_session_command_logs_stream(
                    &stream_session_id,
                    &stream_command_id,
                    move |chunk| {
                        let callback = stdout_callback.clone();
                        let stdout_seen = Arc::clone(&stdout_seen_for_stream);
                        let saw_live_chunk = Arc::clone(&stdout_live);
                        async move {
                            let bytes = chunk.into_bytes();
                            if !bytes.is_empty() {
                                saw_live_chunk.store(true, Ordering::Relaxed);
                                stdout_seen.lock().await.extend_from_slice(&bytes);
                                callback(CommandOutputStream::Stdout, bytes)
                                    .await
                                    .map_err(|err| daytona_callback_error(&err))?;
                            }
                            Ok(())
                        }
                    },
                    move |chunk| {
                        let callback = stderr_callback.clone();
                        let stderr_seen = Arc::clone(&stderr_seen_for_stream);
                        let saw_live_chunk = Arc::clone(&stderr_live);
                        async move {
                            let bytes = chunk.into_bytes();
                            if !bytes.is_empty() {
                                saw_live_chunk.store(true, Ordering::Relaxed);
                                stderr_seen.lock().await.extend_from_slice(&bytes);
                                callback(CommandOutputStream::Stderr, bytes)
                                    .await
                                    .map_err(|err| daytona_callback_error(&err))?;
                            }
                            Ok(())
                        }
                    },
                )
                .await
        });

        let outcome = match wait_for_completion(
            &session,
            &command_id,
            session_exec.exit_code,
            timeout_ms.map(Duration::from_millis),
            cancel_token.unwrap_or_default(),
            &mut stream_task,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                session.close("status poll failure").await;
                return Err(err);
            }
        };
        let exit_code = outcome.exit_code;
        let termination = outcome.termination;
        let mut final_logs = outcome.final_logs;

        // On timeout/cancel, delete the session early so the streaming task can
        // terminate (Daytona closes the log stream when the session is deleted).
        // On natural exit we delete after the stream task drains.
        if termination != CommandTermination::Exited {
            session.close("terminal command state").await;
        }

        let stream_succeeded = match finish_daytona_log_stream(&mut stream_task).await {
            Ok(stream_succeeded) => stream_succeeded,
            Err(err) => {
                session.close("log stream failure").await;
                return Err(err);
            }
        };

        if final_logs.is_none() {
            final_logs = session.fetch_logs(&command_id).await;
        }

        session.close("command finished").await;

        let mut streams_separated = stream_succeeded;
        if let Some(logs) = final_logs.as_ref() {
            streams_separated |= logs.streams_separated;
            append_missing_log_suffix(
                CommandOutputStream::Stdout,
                logs.stdout.as_bytes(),
                &stdout_seen,
                &output_callback,
            )
            .await?;
            append_missing_log_suffix(
                CommandOutputStream::Stderr,
                logs.stderr.as_bytes(),
                &stderr_seen,
                &output_callback,
            )
            .await?;
        }

        let stdout = String::from_utf8_lossy(&stdout_seen.lock().await).into_owned();
        let stderr = String::from_utf8_lossy(&stderr_seen.lock().await).into_owned();

        Ok(ExecStreamingResult {
            result: ExecResult {
                stdout,
                stderr,
                exit_code: (termination == CommandTermination::Exited)
                    .then_some(exit_code)
                    .flatten(),
                termination,
                duration_ms: elapsed_ms(start),
            },
            streams_separated,
            live_streaming: saw_live_chunk.load(Ordering::Relaxed),
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
                matches!(result, Ok(r) if r.is_success())
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

        if result.exit_code == Some(1) {
            // Both rg and grep exit 1 for no matches
            return Ok(Vec::new());
        }
        if !result.is_success() {
            return Err(crate::Error::message(format!(
                "grep failed (exit {}): {}",
                result.display_exit_code(),
                result.stderr
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

        if !result.is_success() {
            return Err(crate::Error::message(format!(
                "glob failed (exit {}): {}",
                result.display_exit_code(),
                result.stderr
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

fn daytona_callback_error(err: &crate::Error) -> DaytonaError {
    DaytonaError::general(format!("output callback failed: {err}"))
}

async fn finish_daytona_log_stream(
    stream_task: &mut JoinHandle<Result<(), DaytonaError>>,
) -> crate::Result<bool> {
    match time::timeout(Duration::from_secs(2), &mut *stream_task).await {
        Ok(Ok(Ok(()))) => Ok(true),
        Ok(Ok(Err(err))) => {
            let message = err.to_string();
            if message.contains("output callback failed") {
                return Err(crate::Error::context(
                    "Daytona log stream callback failed",
                    err,
                ));
            }
            tracing::warn!(error = %message, "Daytona log stream ended with an error");
            Ok(false)
        }
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "Daytona log stream task failed");
            Ok(false)
        }
        Err(_) => {
            stream_task.abort();
            tracing::warn!("Daytona log stream did not close after command completion");
            Ok(false)
        }
    }
}

/// RAII wrapper around a Daytona toolbox session.
///
/// Holds the per-session [`ProcessService`] handle and the session id, and
/// guarantees the session is deleted exactly once. Callers should invoke
/// [`close`] on every path. If a `DaytonaSession` is dropped while still
/// active, [`Drop`] spawns a cleanup task on the current Tokio runtime as a
/// safety net (matching the [`DetachedRunBootstrapGuard`] pattern in
/// `fabro-workflow`).
struct DaytonaSession {
    process_svc: Option<daytona_sdk::ProcessService>,
    session_id:  String,
    active:      bool,
}

impl DaytonaSession {
    async fn create(sandbox: &daytona_sdk::Sandbox) -> crate::Result<Self> {
        let process_svc = sandbox
            .process()
            .await
            .map_err(|e| crate::Error::context("Failed to get process service", e))?;
        let session_id = format!("fabro-{:016x}", rand::rng().random::<u64>());
        process_svc
            .create_session(&session_id)
            .await
            .map_err(|e| crate::Error::context("Failed to create Daytona session", e))?;
        Ok(Self {
            process_svc: Some(process_svc),
            session_id,
            active: true,
        })
    }

    fn id(&self) -> &str {
        &self.session_id
    }

    fn process_svc(&self) -> &daytona_sdk::ProcessService {
        self.process_svc
            .as_ref()
            .expect("DaytonaSession used after close")
    }

    async fn execute(
        &self,
        command: &str,
        run_async: bool,
        suppress_input_echo: bool,
    ) -> Result<daytona_sdk::SessionExecuteResult, daytona_sdk::DaytonaError> {
        self.process_svc()
            .execute_session_command(&self.session_id, command, run_async, suppress_input_echo)
            .await
    }

    async fn get_command_status(
        &self,
        command_id: &str,
    ) -> Result<SessionCommandResult, DaytonaError> {
        self.process_svc()
            .get_session_command(&self.session_id, command_id)
            .await
    }

    async fn fetch_logs(&self, command_id: &str) -> Option<SessionCommandLogsResult> {
        let svc = self.process_svc.as_ref()?;
        fetch_daytona_session_logs(svc, &self.session_id, command_id).await
    }

    /// Idempotent: a second call after `active=false` is a no-op.
    async fn close(&mut self, reason: &'static str) {
        if !self.active {
            return;
        }
        self.active = false;
        if let Some(svc) = self.process_svc.take() {
            if let Err(err) = svc.delete_session(&self.session_id).await {
                tracing::warn!(
                    error = %err,
                    session_id = %self.session_id,
                    reason,
                    "failed to delete Daytona session"
                );
            }
        }
    }
}

impl Drop for DaytonaSession {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let svc = self.process_svc.take();
        let session_id = std::mem::take(&mut self.session_id);
        match (svc, Handle::try_current()) {
            (Some(svc), Ok(handle)) => {
                handle.spawn(async move {
                    if let Err(err) = svc.delete_session(&session_id).await {
                        tracing::warn!(
                            error = %err,
                            session_id,
                            "Daytona session leaked; deleted from Drop"
                        );
                    }
                });
            }
            _ => {
                tracing::error!(session_id, "Daytona session leaked; no runtime to clean up");
            }
        }
    }
}

struct WaitOutcome {
    exit_code:   Option<i32>,
    termination: CommandTermination,
    final_logs:  Option<SessionCommandLogsResult>,
}

/// Wait for the session command to terminate by polling status, the timeout
/// timer, and the cancel token. On status-poll failure, aborts `stream_task`
/// and returns an error; the caller is responsible for closing the session.
async fn wait_for_completion(
    session: &DaytonaSession,
    command_id: &str,
    initial_exit_code: Option<i32>,
    timeout: Option<Duration>,
    cancel_token: CancellationToken,
    stream_task: &mut JoinHandle<Result<(), DaytonaError>>,
) -> crate::Result<WaitOutcome> {
    if let Some(code) = initial_exit_code {
        return Ok(WaitOutcome {
            exit_code:   Some(code),
            termination: CommandTermination::Exited,
            final_logs:  None,
        });
    }

    let timeout_future = async {
        match timeout {
            Some(dur) => time::sleep(dur).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(timeout_future);
    loop {
        tokio::select! {
            () = time::sleep(Duration::from_millis(250)) => {
                let status = match session.get_command_status(command_id).await {
                    Ok(status) => status,
                    Err(err) => {
                        stream_task.abort();
                        return Err(crate::Error::context(
                            "Failed to get Daytona session command status",
                            err,
                        ));
                    }
                };
                if let Some(code) = status.exit_code {
                    return Ok(WaitOutcome {
                        exit_code:   Some(code),
                        termination: CommandTermination::Exited,
                        final_logs:  None,
                    });
                }
            }
            () = &mut timeout_future => {
                return Ok(WaitOutcome {
                    exit_code:   None,
                    termination: CommandTermination::TimedOut,
                    final_logs:  session.fetch_logs(command_id).await,
                });
            }
            () = cancel_token.cancelled() => {
                return Ok(WaitOutcome {
                    exit_code:   None,
                    termination: CommandTermination::Cancelled,
                    final_logs:  session.fetch_logs(command_id).await,
                });
            }
        }
    }
}

async fn fetch_daytona_session_logs(
    process_svc: &daytona_sdk::ProcessService,
    session_id: &str,
    command_id: &str,
) -> Option<SessionCommandLogsResult> {
    match process_svc
        .get_session_command_logs(session_id, command_id)
        .await
    {
        Ok(logs) => Some(logs),
        Err(err) => {
            tracing::warn!(
                error = %err,
                session_id,
                command_id,
                "failed to fetch Daytona session command logs"
            );
            None
        }
    }
}

async fn append_missing_log_suffix(
    stream: CommandOutputStream,
    final_bytes: &[u8],
    seen: &Arc<Mutex<Vec<u8>>>,
    output_callback: &CommandOutputCallback,
) -> crate::Result<()> {
    if final_bytes.is_empty() {
        return Ok(());
    }

    let mut seen = seen.lock().await;
    let offset = missing_log_suffix_offset(&seen, final_bytes);
    if offset >= final_bytes.len() {
        return Ok(());
    }

    let missing = final_bytes[offset..].to_vec();
    seen.extend_from_slice(&missing);
    drop(seen);
    output_callback(stream, missing).await
}

fn missing_log_suffix_offset(seen: &[u8], final_bytes: &[u8]) -> usize {
    if final_bytes.starts_with(seen) {
        return seen.len();
    }
    if seen.starts_with(final_bytes) {
        return final_bytes.len();
    }

    let max_overlap = seen.len().min(final_bytes.len());
    for overlap in (1..=max_overlap).rev() {
        if seen[seen.len() - overlap..] == final_bytes[..overlap] {
            return overlap;
        }
    }
    0
}

fn build_session_command(
    command: &str,
    cwd: &str,
    env_vars: Option<&HashMap<String, String>>,
) -> String {
    let mut lines = vec![format!("cd {} || exit $?", shell_quote(cwd))];

    if let Some(vars) = env_vars {
        let mut entries: Vec<_> = vars.iter().collect();
        entries.sort_by_key(|(key, _)| *key);
        for (key, value) in entries {
            lines.push(format!(
                "export {}={}",
                shell_quote(key),
                shell_quote(value)
            ));
        }
    }

    lines.push("(".to_string());
    lines.push(command.to_string());
    lines.push(")".to_string());
    lines.join("\n")
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
    use daytona_api_client::models::api_key_list::Permissions;
    use httpmock::Method::GET;
    use httpmock::MockServer;

    use super::*;

    fn api_key_body(permissions: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "name": "delete-only",
            "value": "dtn_****",
            "createdAt": "2026-05-01T00:00:00Z",
            "permissions": permissions,
            "lastUsedAt": null,
            "expiresAt": null,
            "userId": "user_123"
        })
    }

    async fn mock_auth_probe(server: &MockServer, status: usize) -> httpmock::Mock<'_> {
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path("/sandbox/paginated")
                    .query_param("page", "1")
                    .query_param("limit", "1");
                then.status(status)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({
                        "items": [],
                        "total": 0,
                        "page": 1,
                        "totalPages": 0
                    }));
            })
            .await
    }

    async fn mock_current_key<'a>(
        server: &'a MockServer,
        permissions: Vec<&'static str>,
    ) -> httpmock::Mock<'a> {
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path("/api-keys/current")
                    .header("authorization", "Bearer dtn_test");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(api_key_body(&permissions));
            })
            .await
    }

    #[test]
    fn daytona_config_defaults() {
        let config = DaytonaConfig::default();
        assert!(config.snapshot.is_none());
        assert!(config.auto_stop_interval.is_none());
        assert!(config.labels.is_none());
    }

    #[test]
    fn command_kind_classifies_known_prefixes() {
        assert_eq!(command_kind(" git status"), "git");
        assert_eq!(command_kind("bash -lc 'echo ok'"), "bash");
        assert_eq!(command_kind("/bin/sh -c 'echo ok'"), "sh");
        assert_eq!(command_kind("rg --version"), "rg");
        assert_eq!(command_kind("find . -maxdepth 1"), "find");
        assert_eq!(command_kind(""), "other");
    }

    #[test]
    fn command_kind_does_not_echo_auth_url_commands() {
        assert_eq!(
            command_kind("https://x-access-token:ghs_FAKE@github.com/owner/repo.git"),
            "other"
        );
    }

    #[test]
    fn missing_display_uses_daytona_wire_scope_names() {
        let check = DaytonaKeyCheck {
            key_name: "delete-only".to_string(),
            missing:  vec![
                Permissions::WriteColonSnapshots,
                Permissions::WriteColonSandboxes,
            ],
        };

        assert_eq!(check.missing_display(), "write:snapshots, write:sandboxes");
        assert_eq!(
            check.missing_message(),
            "API key 'delete-only' is missing required Daytona scopes: \
             write:snapshots, write:sandboxes. Regenerate the key with all \
             snapshot and sandbox scopes."
        );
        assert_eq!(
            required_perms_display(),
            "write:snapshots, delete:snapshots, write:sandboxes, delete:sandboxes"
        );
    }

    #[tokio::test]
    async fn check_daytona_api_key_with_reports_missing_scopes() {
        let server = MockServer::start_async().await;
        let auth = mock_auth_probe(&server, 200).await;
        let current_key = mock_current_key(&server, vec![
            "delete:snapshots",
            "delete:sandboxes",
            "delete:volumes",
        ])
        .await;

        let check = check_daytona_api_key_with(&server.base_url(), None, "dtn_test".to_string())
            .await
            .expect("probe should succeed");

        assert!(!check.ok());
        assert_eq!(check.key_name, "delete-only");
        assert_eq!(check.missing_display(), "write:snapshots, write:sandboxes");
        auth.assert_async().await;
        current_key.assert_async().await;
    }

    #[tokio::test]
    async fn check_daytona_api_key_with_accepts_full_scopes() {
        let server = MockServer::start_async().await;
        let auth = mock_auth_probe(&server, 200).await;
        let current_key = mock_current_key(&server, vec![
            "write:snapshots",
            "delete:snapshots",
            "write:sandboxes",
            "delete:sandboxes",
        ])
        .await;

        let check = check_daytona_api_key_with(&server.base_url(), None, "dtn_test".to_string())
            .await
            .expect("probe should succeed");

        assert!(check.ok());
        assert!(check.missing.is_empty());
        auth.assert_async().await;
        current_key.assert_async().await;
    }

    #[tokio::test]
    async fn check_daytona_api_key_with_preserves_auth_failure_context() {
        let server = MockServer::start_async().await;
        let auth = mock_auth_probe(&server, 401).await;

        let err = check_daytona_api_key_with(&server.base_url(), None, "dtn_test".to_string())
            .await
            .expect_err("auth probe should fail");
        let chain = err.chain().map(ToString::to_string).collect::<Vec<_>>();

        assert!(
            chain
                .iter()
                .any(|cause| cause == "failed to authenticate with Daytona"),
            "expected auth context in chain, got {chain:#?}"
        );
        auth.assert_async().await;
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
    fn build_session_command_adds_cwd_and_sorted_exports() {
        let env = HashMap::from([
            ("BETA".to_string(), "two words".to_string()),
            ("ALPHA".to_string(), "one".to_string()),
        ]);

        let command = build_session_command("echo $ALPHA $BETA", "/tmp/with space", Some(&env));

        assert_eq!(
            command,
            "cd '/tmp/with space' || exit $?\n\
             export ALPHA=one\n\
             export BETA='two words'\n\
             (\n\
             echo $ALPHA $BETA\n\
             )"
        );
    }

    #[test]
    fn missing_log_suffix_offset_handles_prefix_overlap() {
        assert_eq!(missing_log_suffix_offset(b"hello", b"hello world"), 5);
        assert_eq!(missing_log_suffix_offset(b"hello wor", b"hello world"), 9);
        assert_eq!(missing_log_suffix_offset(b"abcxyz", b"xyz123"), 3);
        assert_eq!(missing_log_suffix_offset(b"hello world", b"hello"), 5);
        assert_eq!(missing_log_suffix_offset(b"abc", b"def"), 0);
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
