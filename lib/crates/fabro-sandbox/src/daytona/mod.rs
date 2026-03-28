use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use crate::sandbox::resolve_path;
use crate::shell_quote;
use crate::{
    DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback,
    format_lines_numbered,
};
use async_trait::async_trait;
use daytona_sdk::api_types::SignedPortPreviewUrl;
use fabro_github::GitHubAppCredentials;
use rand::Rng;
use tokio::fs;
use tokio::sync::OnceCell;
use tokio::time;
use tokio_util::sync::CancellationToken;

const WORKING_DIRECTORY: &str = "/home/daytona/workspace";
const DEFAULT_SNAPSHOT: &str = "daytona-medium";

pub use fabro_config::sandbox::{
    DaytonaNetwork, DaytonaSettings as DaytonaConfig,
    DaytonaSnapshotSettings as DaytonaSnapshotConfig, DockerfileSource,
};

/// Sandbox that runs all operations inside a Daytona cloud sandbox.
pub struct DaytonaSandbox {
    config: DaytonaConfig,
    client: daytona_sdk::Client,
    github_app: Option<GitHubAppCredentials>,
    sandbox: OnceCell<daytona_sdk::Sandbox>,
    rg_available: OnceCell<bool>,
    event_callback: Option<SandboxEventCallback>,
    /// HTTPS origin URL stored after clone so we can refresh push credentials later.
    origin_url: OnceCell<String>,
    run_id: Option<String>,
    /// Explicit branch to clone. When set, overrides the branch detected by
    /// `detect_repo_info` — avoids cloning a local-only worktree branch
    /// (e.g. `fabro/run/...`) that was never pushed to origin.
    clone_branch: Option<String>,
}

impl DaytonaSandbox {
    /// Create a new `DaytonaSandbox`, creating the Daytona client internally.
    pub async fn new(
        config: DaytonaConfig,
        github_app: Option<GitHubAppCredentials>,
        run_id: Option<String>,
        clone_branch: Option<String>,
    ) -> Result<Self, String> {
        let client = daytona_sdk::Client::new()
            .await
            .map_err(|e| format!("Failed to create Daytona client: {e}"))?;
        Ok(Self {
            config,
            client,
            github_app,
            sandbox: OnceCell::new(),
            rg_available: OnceCell::const_new(),
            event_callback: None,
            origin_url: OnceCell::new(),
            run_id,
            clone_branch,
        })
    }

    /// Reconnect to an existing Daytona sandbox by name.
    ///
    /// Creates the client internally and fetches the sandbox, replacing the old
    /// `from_existing()` + manual client/get boilerplate at call sites.
    pub async fn reconnect(sandbox_name: &str) -> Result<Self, String> {
        let client = daytona_sdk::Client::new()
            .await
            .map_err(|e| format!("Failed to create Daytona client: {e}"))?;
        let sdk_sandbox = client
            .get(sandbox_name)
            .await
            .map_err(|e| format!("Failed to reconnect to Daytona sandbox '{sandbox_name}': {e}"))?;
        let sandbox_cell = OnceCell::new();
        let _ = sandbox_cell.set(sdk_sandbox);
        Ok(Self {
            config: DaytonaConfig::default(),
            client,
            github_app: None,
            sandbox: sandbox_cell,
            rg_available: OnceCell::const_new(),
            event_callback: None,
            origin_url: OnceCell::new(),
            run_id: None,
            clone_branch: None,
        })
    }

    pub fn set_event_callback(&mut self, cb: SandboxEventCallback) {
        self.event_callback = Some(cb);
    }

    /// Get the `ComputerUseService` for this sandbox.
    ///
    /// Requires the sandbox to be initialized first.
    pub async fn computer_use(&self) -> Result<daytona_sdk::ComputerUseService, String> {
        let sandbox = self.sandbox()?;
        sandbox
            .computer_use()
            .await
            .map_err(|e| format!("Failed to get computer use service: {e}"))
    }

    /// Create SSH access and return the connection command string.
    pub async fn create_ssh_access(&self, ttl_minutes: Option<f64>) -> Result<String, String> {
        let sandbox = self.sandbox()?;
        let dto = sandbox
            .create_ssh_access(ttl_minutes)
            .await
            .map_err(|e| format!("Failed to create SSH access: {e}"))?;
        Ok(dto.ssh_command)
    }

    /// Get a preview link (URL + token) for a port on this sandbox.
    pub async fn get_preview_link(&self, port: u16) -> Result<daytona_sdk::PreviewLink, String> {
        let sandbox = self.sandbox()?;
        sandbox
            .get_preview_link(port)
            .await
            .map_err(|e| format!("Failed to get preview link for port {port}: {e}"))
    }

    /// Get a signed preview URL for a port on this sandbox.
    pub async fn get_signed_preview_url(
        &self,
        port: u16,
        expires_in_seconds: Option<i32>,
    ) -> Result<SignedPortPreviewUrl, String> {
        let sandbox = self.sandbox()?;
        sandbox
            .get_signed_preview_url(port as i32, expires_in_seconds)
            .await
            .map_err(|e| format!("Failed to get signed preview URL for port {port}: {e}"))
    }

    fn emit(&self, event: SandboxEvent) {
        event.trace();
        if let Some(ref cb) = self.event_callback {
            cb(event);
        }
    }

    fn resolve_path(&self, path: &str) -> String {
        resolve_path(path, WORKING_DIRECTORY)
    }

    /// Get the sandbox, returning an error if not yet initialized.
    fn sandbox(&self) -> Result<&daytona_sdk::Sandbox, String> {
        self.sandbox
            .get()
            .ok_or_else(|| "Daytona sandbox not initialized — call initialize() first".to_string())
    }

    /// Build `SandboxBaseParams` from config, generating a unique sandbox name.
    fn base_params(&self) -> daytona_sdk::SandboxBaseParams {
        let name = if let Some(ref id) = self.run_id {
            format!("fabro-{id}")
        } else {
            format!(
                "fabro-{}-{:04x}",
                chrono::Utc::now().format("%Y%m%d-%H%M%S"),
                rand::thread_rng().gen_range(0..0x10000u32),
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
                let dockerfile = match &snap_cfg.dockerfile {
                    Some(DockerfileSource::Inline(s)) => s.as_str(),
                    Some(DockerfileSource::Path { .. }) => {
                        return Err(format!(
                            "Snapshot '{}': dockerfile path should have been resolved to inline content before sandbox creation",
                            snap_cfg.name
                        ));
                    }
                    None => {
                        return Err(format!(
                            "Snapshot '{}' does not exist and no dockerfile provided to create it",
                            snap_cfg.name
                        ));
                    }
                };

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
            time::sleep(delay).await;
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

use fabro_github::ssh_url_to_https;

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

#[async_trait]
impl Sandbox for DaytonaSandbox {
    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<(), String> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(remote_path);

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| format!("Failed to get fs service: {e}"))?;

        let bytes = fs_svc
            .download_file(&resolved)
            .await
            .map_err(|e| format!("Failed to download file {resolved}: {e}"))?;

        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent dirs: {e}"))?;
        }
        fs::write(local_path, &bytes)
            .await
            .map_err(|e| format!("Failed to write {}: {e}", local_path.display()))?;

        Ok(())
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<(), String> {
        let sandbox = self.sandbox()?;
        let resolved = self.resolve_path(remote_path);

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

        let bytes = fs::read(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {e}", local_path.display()))?;

        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| format!("Failed to get fs service: {e}"))?;

        fs_svc
            .upload_file_bytes(&resolved, &bytes)
            .await
            .map_err(|e| format!("Failed to upload file {resolved}: {e}"))?;

        Ok(())
    }

    async fn initialize(&self) -> Result<(), String> {
        self.emit(SandboxEvent::Initializing {
            provider: "daytona".into(),
        });
        let init_start = Instant::now();

        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        let params = if let Some(ref snap_cfg) = self.config.snapshot {
            self.emit(SandboxEvent::SnapshotEnsuring {
                name: snap_cfg.name.clone(),
            });
            let snap_start = Instant::now();
            if let Err(e) = self.ensure_snapshot(snap_cfg).await {
                self.emit(SandboxEvent::SnapshotFailed {
                    name: snap_cfg.name.clone(),
                    error: e.clone(),
                });
                let duration_ms =
                    u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.emit(SandboxEvent::InitializeFailed {
                    provider: "daytona".into(),
                    error: e.clone(),
                    duration_ms,
                });
                return Err(e);
            }
            let snap_duration = u64::try_from(snap_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::SnapshotReady {
                name: snap_cfg.name.clone(),
                duration_ms: snap_duration,
            });

            daytona_sdk::CreateParams::Snapshot(daytona_sdk::SnapshotParams {
                base: self.base_params(),
                snapshot: snap_cfg.name.clone(),
            })
        } else {
            daytona_sdk::CreateParams::Snapshot(daytona_sdk::SnapshotParams {
                base: self.base_params(),
                snapshot: DEFAULT_SNAPSHOT.to_string(),
            })
        };

        tracing::info!("Creating Daytona sandbox");
        let sandbox = self
            .client
            .create(params, daytona_sdk::CreateSandboxOptions::default())
            .await
            .map_err(|e| {
                let err = format!("Failed to create Daytona sandbox: {e}");
                let duration_ms =
                    u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.emit(SandboxEvent::InitializeFailed {
                    provider: "daytona".into(),
                    error: err.clone(),
                    duration_ms,
                });
                err
            })?;

        // Clone the repo into the sandbox
        if self.config.skip_clone {
            // Create working directory without cloning
            let fs_svc = sandbox
                .fs()
                .await
                .map_err(|e| format!("Failed to get Daytona fs service: {e}"))?;
            fs_svc
                .create_folder(WORKING_DIRECTORY, None)
                .await
                .map_err(|e| format!("Failed to create working directory: {e}"))?;
        } else {
            match detect_repo_info(&cwd) {
                Ok((detected_url, detected_branch)) => {
                    // Use explicit clone_branch if provided (avoids cloning a local-only
                    // worktree branch like fabro/run/... that hasn't been pushed).
                    let branch = self.clone_branch.clone().or(detected_branch);
                    // Daytona clones over HTTPS with token auth, so rewrite SSH URLs.
                    let url = ssh_url_to_https(&detected_url);
                    self.emit(SandboxEvent::GitCloneStarted {
                        url: url.clone(),
                        branch: branch.clone(),
                    });
                    let clone_start = Instant::now();

                    // Resolve clone credentials via GitHub App or fall back to no auth
                    let (username, password) = match &self.github_app {
                        Some(creds) => {
                            let (owner, repo) = fabro_github::parse_github_owner_repo(&url)
                                .map_err(|e| {
                                    let err = format!("Failed to parse GitHub URL for clone: {e}");
                                    self.emit(SandboxEvent::GitCloneFailed {
                                        url: url.clone(),
                                        error: err.clone(),
                                    });
                                    err
                                })?;
                            fabro_github::resolve_clone_credentials(creds, &owner, &repo)
                                .await
                                .map_err(|e| {
                                    let err = format!(
                                        "Failed to get GitHub App credentials for clone: {e}"
                                    );
                                    self.emit(SandboxEvent::GitCloneFailed {
                                        url: url.clone(),
                                        error: err.clone(),
                                    });
                                    let duration_ms =
                                        u64::try_from(init_start.elapsed().as_millis())
                                            .unwrap_or(u64::MAX);
                                    self.emit(SandboxEvent::InitializeFailed {
                                        provider: "daytona".into(),
                                        error: err.clone(),
                                        duration_ms,
                                    });
                                    err
                                })?
                        }
                        None => (None, None),
                    };

                    let git_svc = sandbox
                        .git()
                        .await
                        .map_err(|e| format!("Failed to get Daytona git service: {e}"));
                    let git_svc = match git_svc {
                        Ok(g) => g,
                        Err(e) => {
                            self.emit(SandboxEvent::GitCloneFailed {
                                url: url.clone(),
                                error: e.clone(),
                            });
                            let duration_ms =
                                u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                            self.emit(SandboxEvent::InitializeFailed {
                                provider: "daytona".into(),
                                error: e.clone(),
                                duration_ms,
                            });
                            return Err(e);
                        }
                    };

                    let clone_token = password.clone();
                    let clone_result = git_svc
                        .clone(
                            &url,
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
                            let clone_duration = u64::try_from(clone_start.elapsed().as_millis())
                                .unwrap_or(u64::MAX);
                            self.emit(SandboxEvent::GitCloneCompleted {
                                url: url.clone(),
                                duration_ms: clone_duration,
                            });

                            // Store origin URL and set push credentials for later pushes
                            if let Some(token) = clone_token {
                                let _ = self.origin_url.set(url);
                                let process_svc = sandbox.process().await.ok();
                                if let Some(ps) = process_svc {
                                    let origin = self.origin_url.get().expect("just set");
                                    let auth_url = origin.replacen(
                                        "https://",
                                        &format!("https://x-access-token:{token}@"),
                                        1,
                                    );
                                    let cmd = format!(
                                        "git -c maintenance.auto=0 remote set-url origin {}",
                                        shell_quote(&auth_url),
                                    );
                                    let opts = daytona_sdk::ExecuteCommandOptions {
                                        cwd: Some(WORKING_DIRECTORY.to_string()),
                                        ..Default::default()
                                    };
                                    let wrapped = wrap_bash_command(&cmd);
                                    if let Ok(r) = ps.execute_command(&wrapped, opts).await {
                                        if r.exit_code != 0 {
                                            tracing::warn!(
                                                exit_code = r.exit_code,
                                                "Failed to set push credentials on origin"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) if self.github_app.is_none() => {
                            let err = format!(
                                "Git clone failed: {e}. If this is a private repository, \
                             configure a GitHub App with `fabro install` and install it \
                             for your organization."
                            );
                            self.emit(SandboxEvent::GitCloneFailed {
                                url,
                                error: err.clone(),
                            });
                            let duration_ms =
                                u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                            self.emit(SandboxEvent::InitializeFailed {
                                provider: "daytona".into(),
                                error: err.clone(),
                                duration_ms,
                            });
                            return Err(err);
                        }
                        Err(e) => {
                            let err = format!("Failed to clone repo into Daytona sandbox: {e}");
                            self.emit(SandboxEvent::GitCloneFailed {
                                url,
                                error: err.clone(),
                            });
                            let duration_ms =
                                u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                            self.emit(SandboxEvent::InitializeFailed {
                                provider: "daytona".into(),
                                error: err.clone(),
                                duration_ms,
                            });
                            return Err(err);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Could not detect git repo for Daytona clone");
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
        }

        let sandbox_name = sandbox.name.clone();
        let sandbox_cpu = sandbox.cpu;
        let sandbox_memory = sandbox.memory;
        self.sandbox
            .set(sandbox)
            .map_err(|_| "Daytona sandbox already initialized".to_string())?;
        tracing::info!("Daytona sandbox ready");

        let init_duration = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::Ready {
            provider: "daytona".into(),
            duration_ms: init_duration,
            name: Some(sandbox_name),
            cpu: Some(sandbox_cpu),
            memory: Some(sandbox_memory),
            url: Some("https://app.daytona.io/dashboard/sandboxes".into()),
        });

        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        self.emit(SandboxEvent::CleanupStarted {
            provider: "daytona".into(),
        });
        let start = Instant::now();
        if let Some(sandbox) = self.sandbox.get() {
            tracing::info!("Destroying Daytona sandbox");
            if let Err(e) = sandbox.delete().await {
                let err = format!("Failed to delete Daytona sandbox: {e}");
                self.emit(SandboxEvent::CleanupFailed {
                    provider: "daytona".into(),
                    error: err.clone(),
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

    fn platform(&self) -> &str {
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

    async fn setup_git_for_run(&self, run_id: &str) -> Result<Option<crate::GitRunInfo>, String> {
        crate::setup_git_via_exec(self, run_id).await.map(Some)
    }

    fn resume_setup_commands(&self, run_branch: &str) -> Vec<String> {
        vec![format!(
            "git fetch origin {run_branch} && git checkout {run_branch}"
        )]
    }

    async fn git_push_branch(&self, branch: &str) -> bool {
        crate::git_push_via_exec(self, branch).await
    }

    fn parallel_worktree_path(
        &self,
        _run_dir: &std::path::Path,
        run_id: &str,
        node_id: &str,
        key: &str,
    ) -> String {
        format!(
            "{}/.fabro/runs/{}/parallel/{}/{}",
            self.working_directory(),
            run_id,
            node_id,
            key
        )
    }

    async fn ssh_access_command(&self) -> Result<Option<String>, String> {
        self.create_ssh_access(Some(60.0)).await.map(Some)
    }

    fn origin_url(&self) -> Option<&str> {
        self.origin_url.get().map(String::as_str)
    }

    async fn get_preview_url(
        &self,
        port: u16,
    ) -> Result<Option<(String, HashMap<String, String>)>, String> {
        let sandbox = self.sandbox()?;
        let preview = sandbox
            .get_preview_link(port)
            .await
            .map_err(|e| format!("Failed to get preview link for port {port}: {e}"))?;
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

    async fn refresh_push_credentials(&self) -> Result<(), String> {
        let origin_url = match self.origin_url.get() {
            Some(url) => url,
            None => return Ok(()), // no authenticated origin — nothing to refresh
        };
        let creds = match &self.github_app {
            Some(c) => c,
            None => return Ok(()),
        };

        let auth_url = fabro_github::resolve_authenticated_url(creds, origin_url)
            .await
            .map_err(|e| format!("Failed to refresh GitHub App token: {e}"))?;

        let cmd = format!(
            "git -c maintenance.auto=0 remote set-url origin {}",
            shell_quote(&auth_url),
        );
        self.exec_command(&cmd, 10_000, None, None, None)
            .await
            .map_err(|e| format!("Failed to set refreshed push credentials: {e}"))?;

        Ok(())
    }

    async fn set_autostop_interval(&self, minutes: i32) -> Result<(), String> {
        let sandbox_id = self.sandbox()?.id.clone();
        let mut sandbox = self
            .client
            .get(&sandbox_id)
            .await
            .map_err(|e| format!("Failed to get sandbox for autostop update: {e}"))?;
        sandbox
            .set_autostop_interval(minutes)
            .await
            .map_err(|e| format!("Failed to set autostop interval: {e}"))
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
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        tracing::info!(command, timeout_ms, "exec_command: entered");

        let sandbox = self.sandbox()?;
        let start = Instant::now();

        let cwd = working_dir
            .map(|d| self.resolve_path(d))
            .unwrap_or_else(|| WORKING_DIRECTORY.to_string());

        let process_svc = sandbox
            .process()
            .await
            .map_err(|e| format!("Failed to get process service: {e}"))?;

        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            "exec_command: process service acquired, starting select"
        );

        let options = daytona_sdk::ExecuteCommandOptions {
            cwd: Some(cwd),
            env: env_vars.cloned(),
            timeout: Some(std::time::Duration::from_millis(timeout_ms)),
        };

        // The Daytona toolbox's /process/execute endpoint does not yet
        // process the `envs` field (not in its OpenAPI spec), so we also
        // prepend `export` statements as a fallback until server support
        // lands. The SDK sends `envs` too for forward compatibility.
        let command_with_env = if let Some(vars) = env_vars {
            if !vars.is_empty() {
                let exports: Vec<String> = vars
                    .iter()
                    .map(|(k, v)| format!("export {}={}", shell_quote(k), shell_quote(v)))
                    .collect();
                format!("{}\n{}", exports.join("\n"), command)
            } else {
                command.to_string()
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
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    ok = res.is_ok(),
                    "exec_command: HTTP response received"
                );
                res.map_err(|e| format!("Failed to execute command: {e}"))?
            }
            () = time::sleep(timeout_duration) => {
                tracing::info!(
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    timeout_ms,
                    "exec_command: client-side timeout fired"
                );
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command timed out locally".to_string(),
                    exit_code: -1,
                    timed_out: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
            () = token.cancelled() => {
                tracing::info!(
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    "exec_command: cancelled via token"
                );
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command cancelled".to_string(),
                    exit_code: -1,
                    timed_out: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
        };

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
                cmd.push_str(&format!(" --glob {}", shell_quote(glob_filter)));
            }
            if let Some(max) = options.max_results {
                cmd.push_str(&format!(" --max-count {max}"));
            }
            cmd.push_str(&format!(
                " -- {} {}",
                shell_quote(pattern),
                shell_quote(&resolved)
            ));
            cmd
        } else {
            let mut cmd = "grep -rn".to_string();
            if options.case_insensitive {
                cmd.push_str(" -i");
            }
            if let Some(ref glob_filter) = options.glob_filter {
                cmd.push_str(&format!(" --include {}", shell_quote(glob_filter)));
            }
            if let Some(max) = options.max_results {
                cmd.push_str(&format!(" -m {max}"));
            }
            cmd.push_str(&format!(
                " -- {} {}",
                shell_quote(pattern),
                shell_quote(&resolved)
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
            "find {} -name {} -type f | sort",
            shell_quote(&base),
            shell_quote(pattern),
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
        // Documents the current behavior: detect_repo_info returns whatever HEAD points to
        assert_eq!(branch, Some("fabro/run/ABC".into()));
    }
}
