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
use daytona_api_client::models::SandboxState;
use daytona_api_client::models::api_key_list::Permissions;
use daytona_sdk::api_types::SignedPortPreviewUrl;
use daytona_sdk::toolbox_types::Command as SessionCommandResult;
use daytona_sdk::{DaytonaError, SessionCommandLogsResult};
use fabro_github::GitHubCredentials;
use fabro_static::EnvVars;
use fabro_types::{CommandOutputStream, CommandTermination, RunId, SandboxProviderKind};
use fabro_util::time::elapsed_ms;
use rand::Rng;
use tokio::runtime::Handle;
use tokio::sync::{Mutex, OnceCell};
use tokio::task::JoinHandle;
use tokio::{fs, time};
use tokio_util::sync::CancellationToken;

use crate::clone_retry::{self, CloneRetryReason};
use crate::clone_source::{self, CloneDecision, EmptyWorkspaceReason};
use crate::redact::redact_auth_url;
use crate::sandbox::{
    self, BASH_ENV_VAR, BASH_PROBE_MARKER, BASH_PROBE_SCRIPT, BASH_PROBE_TIMEOUT_MS, REMOTE_BASH,
    REMOTE_WALK_TIMEOUT_MS, RefreshOutcome, optional_timeout, resolve_path, validate_bash_probe,
};
use crate::{
    CommandOutputCallback, DirEntry, ExecResult, ExecStreamingRequest, ExecStreamingResult,
    GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback, SandboxFile, StdioProcess,
    WalkOptions, managed_labels, shell_quote,
};

/// Remediation shown when a Daytona sandbox has no usable Bash.
const DAYTONA_BASH_REMEDIATION: &str = "Daytona sandboxes require /bin/bash for every command, with no `sh` fallback. Use the \
     built-in Daytona snapshot, or a custom snapshot whose Dockerfile installs bash.";

/// Remediation shown when the session transport reaches Bash but never
/// completes.
///
/// The direct transport is probed first, so a sandbox that reaches this failure
/// already has a usable Bash. What is left is the session contract: Daytona
/// sources the submitted command inside a wrapper that resumes afterward to
/// drain the log labelers and persist the exit code, so the command has to
/// return control to it.
const DAYTONA_BASH_SESSION_REMEDIATION: &str = "Daytona ran the direct command transport but not its streaming session transport. \
     A session command must leave Daytona's wrapper shell in place so the provider can \
     record the exit code; replacing that shell reports no completion and stalls every \
     streaming command until its timeout.";

pub(crate) const WORKING_DIRECTORY: &str = "/home/daytona/workspace";
pub(crate) const REPOS_ROOT: &str = "/home/daytona/repos";
const DEFAULT_SNAPSHOT: &str = "daytona-medium";
pub const DEFAULT_DAYTONA_API_URL: &str = "https://app.daytona.io/api";
pub(crate) const DAYTONA_DASHBOARD_SANDBOXES_URL: &str =
    "https://app.daytona.io/dashboard/sandboxes";
const FABRO_SANDBOX_USER_AGENT: &str = concat!("fabro-sandbox/", env!("CARGO_PKG_VERSION"));
const DAYTONA_PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const DAYTONA_START_TIMEOUT: Duration = Duration::from_mins(1);
/// Upper bound on explicit and Drop-triggered Daytona cleanup calls (session
/// deletion, temporary stdin files) so a stalled REST call cannot block
/// cancellation/timeout paths indefinitely.
const DAYTONA_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
/// Auto-stop applied when `lifecycle.auto_stop` is unset. Omitting the field
/// would inherit Daytona's server-side default of 15 idle minutes, which is
/// shorter than a single long inference call and stops the sandbox mid-run;
/// 120 minutes clears any realistic call while still reclaiming sandboxes
/// leaked by a dead worker. An explicit `0` disables auto-stop entirely.
const DEFAULT_AUTO_STOP_INTERVAL_MINUTES: i32 = 120;

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

pub mod snapshot_identity {
    use hmac::{Hmac, Mac};
    use serde::Serialize;
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::{DaytonaSnapshotConfig, DockerfileSource};

    const IDENTITY_VERSION: u8 = 1;
    const PROVIDER: &str = "daytona";
    const TENANT: &str = "single-tenant";

    type HmacSha256 = Hmac<Sha256>;

    #[derive(Serialize)]
    struct SnapshotManifest<'a> {
        identity_version:  u8,
        provider:          &'static str,
        tenant:            &'static str,
        dockerfile_sha256: &'a str,
        cpu:               Option<i32>,
        memory_gb:         Option<i32>,
        disk_gb:           Option<i32>,
        entrypoint:        Option<&'static str>,
    }

    pub fn snapshot_name(api_key: &str, config: &DaytonaSnapshotConfig) -> crate::Result<String> {
        let manifest = canonical_manifest(config)?;
        let mut mac = HmacSha256::new_from_slice(api_key.as_bytes())
            .expect("HMAC-SHA256 accepts keys of any length");
        mac.update(&manifest);
        let digest = mac.finalize().into_bytes();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        Ok(format!("fabro-{}", Uuid::new_v8(bytes)))
    }

    fn canonical_manifest(config: &DaytonaSnapshotConfig) -> crate::Result<Vec<u8>> {
        let dockerfile = match &config.dockerfile {
            Some(DockerfileSource::Inline(text)) => text.as_str(),
            Some(DockerfileSource::Path { .. }) => {
                return Err(crate::Error::message(
                    "Daytona snapshot dockerfile path should have been resolved to inline content before sandbox creation",
                ));
            }
            None => {
                return Err(crate::Error::message(
                    "Daytona custom snapshots require image.dockerfile",
                ));
            }
        };
        let dockerfile_sha256 = hex::encode(Sha256::digest(dockerfile.as_bytes()));
        let manifest = SnapshotManifest {
            identity_version:  IDENTITY_VERSION,
            provider:          PROVIDER,
            tenant:            TENANT,
            dockerfile_sha256: &dockerfile_sha256,
            cpu:               config.cpu,
            memory_gb:         config.memory,
            disk_gb:           config.disk,
            entrypoint:        None,
        };
        serde_json::to_vec(&manifest).map_err(|err| {
            crate::Error::context("Failed to serialize Daytona snapshot identity", err)
        })
    }
}

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
    build_daytona_client_with(api_key, None, None, None).await
}

#[expect(
    clippy::disallowed_methods,
    reason = "Standalone Daytona sandbox construction falls back to the documented process env var."
)]
fn resolve_daytona_api_key(api_key: Option<String>) -> Option<String> {
    api_key.filter(|key| !key.is_empty()).or_else(|| {
        std::env::var(EnvVars::DAYTONA_API_KEY)
            .ok()
            .filter(|key| !key.is_empty())
    })
}

pub(crate) async fn build_daytona_client_with(
    api_key: Option<String>,
    api_url: Option<String>,
    organization_id: Option<String>,
    http_client: Option<fabro_http::HttpClient>,
) -> Result<daytona_sdk::Client, daytona_sdk::DaytonaError> {
    let sdk_config = daytona_sdk::DaytonaConfig {
        api_key,
        api_url,
        organization_id,
        http_client,
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
    let http_client = fabro_http::http_client().context("failed to build HTTP client")?;
    check_daytona_api_key_with(&base_url, org_id.as_deref(), api_key, http_client).await
}

pub async fn check_daytona_api_key_with(
    base_url: &str,
    org_id: Option<&str>,
    api_key: String,
    http_client: fabro_http::HttpClient,
) -> anyhow::Result<DaytonaKeyCheck> {
    let work = async {
        let client = build_daytona_client_with(
            Some(api_key.clone()),
            Some(base_url.to_string()),
            org_id.map(str::to_string),
            Some(http_client.clone()),
        )
        .await
        .map_err(anyhow::Error::new)
        .context("failed to construct Daytona client")?;
        client
            .list(None, Some(1), Some(1))
            .await
            .map_err(anyhow::Error::new)
            .context("failed to authenticate with Daytona")?;

        let api_config = build_api_keys_configuration(base_url, &api_key, http_client);
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

fn build_api_keys_configuration(
    base_url: &str,
    api_key: &str,
    http_client: fabro_http::HttpClient,
) -> Configuration {
    Configuration {
        base_path:           base_url.to_string(),
        user_agent:          Some(FABRO_SANDBOX_USER_AGENT.to_string()),
        client:              reqwest_middleware::ClientBuilder::new(http_client).build(),
        basic_auth:          None,
        oauth_access_token:  None,
        bearer_access_token: Some(api_key.to_string()),
        api_key:             None,
    }
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
    config:            DaytonaConfig,
    client:            daytona_sdk::Client,
    api_key:           Option<String>,
    github_app:        Option<GitHubCredentials>,
    sandbox:           OnceCell<daytona_sdk::Sandbox>,
    snapshot_name:     OnceCell<String>,
    rg_available:      OnceCell<bool>,
    event_callback:    Option<SandboxEventCallback>,
    /// HTTPS origin URL stored after clone so we can refresh push credentials
    /// later.
    origin_url:        OnceCell<String>,
    repo_cloned:       OnceCell<bool>,
    working_directory: OnceCell<String>,
    run_id:            Option<RunId>,
    clone_origin_url:  Option<String>,
    /// Explicit branch to clone. When set, overrides the branch detected by
    /// the submitted run spec.
    clone_branch:      Option<String>,
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
        let api_key = resolve_daytona_api_key(api_key);
        let client = build_daytona_client(api_key.clone())
            .await
            .map_err(|e| crate::Error::context("Failed to create Daytona client", e))?;
        Ok(Self {
            config,
            client,
            api_key,
            github_app,
            sandbox: OnceCell::new(),
            snapshot_name: OnceCell::new(),
            rg_available: OnceCell::const_new(),
            event_callback: None,
            origin_url: OnceCell::new(),
            repo_cloned: OnceCell::new(),
            working_directory: OnceCell::new(),
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
        working_directory: String,
        clone_origin_url: Option<String>,
        clone_branch: Option<String>,
    ) -> crate::Result<Self> {
        let api_key = resolve_daytona_api_key(api_key);
        let client = build_daytona_client(api_key.clone())
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
        let working_directory_cell = OnceCell::new();
        let _ = working_directory_cell.set(working_directory);
        Ok(Self {
            config: DaytonaConfig::default(),
            client,
            api_key,
            github_app: None,
            sandbox: sandbox_cell,
            snapshot_name: OnceCell::new(),
            rg_available: OnceCell::const_new(),
            event_callback: None,
            origin_url,
            repo_cloned: repo_cloned_cell,
            working_directory: working_directory_cell,
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

    fn resolve_path(&self, path: &str) -> String {
        resolve_path(path, self.working_directory())
    }

    async fn upload_file_content(&self, resolved_path: &str, content: &str) -> crate::Result<()> {
        let sandbox = self.sandbox()?;
        let fs_svc = sandbox
            .fs()
            .await
            .map_err(|e| crate::Error::context("Failed to get fs service", e))?;

        fs_svc
            .upload_file_bytes(resolved_path, content.as_bytes())
            .await
            .map_err(|e| crate::Error::context(format!("Failed to write file {resolved_path}"), e))
    }

    /// Verify a Daytona sandbox evaluates commands as non-login Bash.
    ///
    /// Runs on a freshly created sandbox before any Fabro-owned setup, and
    /// again after a reconnected sandbox starts, so a snapshot without Bash
    /// fails at the lifecycle boundary rather than on some later command.
    ///
    /// Both transports are probed. They build different requests — a direct
    /// process exec and a toolbox session — so neither is evidence for the
    /// other, and a session transport that never yields an exit code otherwise
    /// surfaces as every streaming command timing out rather than as an init
    /// failure. The direct probe runs first because it isolates "no usable
    /// Bash" from "Bash runs but the session contract is broken".
    async fn probe_bash(sandbox: &daytona_sdk::Sandbox) -> crate::Result<()> {
        Self::probe_bash_exec(sandbox).await?;
        Self::probe_bash_session(sandbox).await
    }

    /// Probe Bash over the direct process-exec transport.
    async fn probe_bash_exec(sandbox: &daytona_sdk::Sandbox) -> crate::Result<()> {
        let start = Instant::now();
        let execution = time::timeout(Duration::from_millis(BASH_PROBE_TIMEOUT_MS), async {
            let process_svc = sandbox
                .process()
                .await
                .map_err(|e| crate::Error::context("Failed to get Daytona process service", e))?;
            let result = process_svc
                .execute_command(
                    &wrap_bash_command(BASH_PROBE_SCRIPT),
                    daytona_sdk::ExecuteCommandOptions {
                        cwd: Some("/".to_string()),
                        timeout: Some(Duration::from_millis(BASH_PROBE_TIMEOUT_MS)),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| crate::Error::context("Failed to run Daytona Bash check", e))?;
            Ok(ExecResult {
                stdout:      result.result,
                stderr:      String::new(),
                exit_code:   Some(result.exit_code),
                termination: CommandTermination::Exited,
                duration_ms: elapsed_ms(start),
            })
        })
        .await;
        let execution = match execution {
            Ok(result) => result,
            Err(_) => Err(crate::Error::message(format!(
                "Daytona Bash check timed out after {BASH_PROBE_TIMEOUT_MS}ms"
            ))),
        };

        daytona_bash_probe_outcome(execution)
    }

    /// Probe Bash over the streaming toolbox-session transport.
    ///
    /// Builds, submits, and awaits the command exactly the way
    /// [`Sandbox::exec_command_streaming`] does, so the provider's exit-code
    /// bookkeeping is part of what passes or fails here. The probe script's
    /// `BASH_ENV` assertion is vacuous on this path — the generated session
    /// script blanks `BASH_ENV` itself — but the interpreter, non-login,
    /// non-POSIX, and completion assertions all hold.
    ///
    /// Costs one session round trip plus a single status poll per sandbox
    /// lifecycle transition. `DAYTONA_PROBE_TIMEOUT` is the outer backstop for
    /// a stalled REST call; the inner [`BASH_PROBE_TIMEOUT_MS`] is the deadline
    /// for the command itself. Session cleanup runs outside that deadline under
    /// its own bounded timeout.
    async fn probe_bash_session(sandbox: &daytona_sdk::Sandbox) -> crate::Result<()> {
        let deadline = time::Instant::now() + DAYTONA_PROBE_TIMEOUT;
        let timeout_error = || {
            crate::Error::message(format!(
                "Daytona Bash session check timed out after {}s",
                DAYTONA_PROBE_TIMEOUT.as_secs()
            ))
        };
        let mut session = match time::timeout_at(deadline, DaytonaSession::create(sandbox)).await {
            Ok(Ok(session)) => session,
            Ok(Err(err)) => return daytona_bash_session_probe_outcome(Err(err)),
            Err(_) => return daytona_bash_session_probe_outcome(Err(timeout_error())),
        };

        let execution =
            match time::timeout_at(deadline, Self::run_bash_session_probe(&session)).await {
                Ok(result) => result,
                Err(_) => Err(timeout_error()),
            };
        let close_reason = if execution.is_ok() {
            "bash session probe finished"
        } else {
            "bash session probe failure"
        };
        session.close(close_reason).await;
        daytona_bash_session_probe_outcome(execution)
    }

    /// Run one probe command through a Daytona session and collect its result.
    async fn run_bash_session_probe(session: &DaytonaSession) -> crate::Result<ExecResult> {
        let start = Instant::now();
        let session_exec = session
            .execute(
                &build_bash_session_command(BASH_PROBE_SCRIPT, "/", None),
                true,
                true,
            )
            .await
            .map_err(|err| {
                crate::Error::context("Failed to execute Daytona session command", err)
            })?;

        if let Some(exit_code) = session_exec.exit_code {
            return Ok(ExecResult {
                stdout:      session_exec
                    .stdout
                    .or(session_exec.output)
                    .unwrap_or_default(),
                stderr:      session_exec.stderr.unwrap_or_default(),
                exit_code:   Some(exit_code),
                termination: CommandTermination::Exited,
                duration_ms: elapsed_ms(start),
            });
        }
        let command_id = session_exec.cmd_id;

        let outcome = wait_for_completion(
            session,
            &command_id,
            None,
            Some(BASH_PROBE_TIMEOUT_MS),
            CancellationToken::new(),
        )
        .await?;

        let logs = match outcome.final_logs {
            Some(logs) => Some(logs),
            None => session.fetch_logs(&command_id).await,
        };
        let (stdout, stderr) = logs
            .map(|logs| (logs.stdout, logs.stderr))
            .unwrap_or_default();

        Ok(ExecResult {
            stdout,
            stderr,
            exit_code: outcome.exit_code,
            termination: outcome.termination,
            duration_ms: elapsed_ms(start),
        })
    }

    /// Discard a sandbox that failed its Bash probe.
    ///
    /// A failed cleanup is logged rather than returned: the Bash failure is
    /// what the operator needs to act on.
    async fn delete_unusable_sandbox(
        sandbox: &daytona_sdk::Sandbox,
        bash_error: crate::Error,
    ) -> crate::Error {
        if let Err(cleanup_error) = sandbox.delete().await {
            tracing::warn!(
                error = %cleanup_error,
                sandbox = %sandbox.name,
                "Failed to delete Daytona sandbox after its Bash check failed"
            );
        }
        bash_error
    }

    /// Get the sandbox, returning an error if not yet initialized.
    fn sandbox(&self) -> crate::Result<&daytona_sdk::Sandbox> {
        self.sandbox.get().ok_or_else(|| {
            crate::Error::message("Daytona sandbox not initialized — call initialize() first")
        })
    }

    /// Read-only access to the SDK sandbox once initialized. Returns `None`
    /// before `initialize()` or `reconnect()` has populated the cell.
    pub fn sandbox_handle(&self) -> Option<&daytona_sdk::Sandbox> {
        self.sandbox.get()
    }

    pub(crate) fn daytona_id(&self) -> crate::Result<&str> {
        Ok(&self.sandbox()?.id)
    }

    fn repo_cloned(&self) -> bool {
        self.repo_cloned.get().copied().unwrap_or(false)
    }

    fn set_working_directory(&self, working_directory: impl Into<String>) -> crate::Result<()> {
        self.working_directory
            .set(working_directory.into())
            .map_err(|_| crate::Error::message("Daytona working directory already initialized"))
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
            env_vars: Some(clean_bash_env(None)),
            auto_stop_interval: self
                .config
                .auto_stop_interval
                .or(Some(DEFAULT_AUTO_STOP_INTERVAL_MINUTES)),
            labels: Some(managed_labels::merge_for_run(
                self.config.labels.as_ref(),
                self.run_id.as_ref(),
            )),
            auto_delete_interval: Some(-1),
            ephemeral: Some(false),
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
    async fn ensure_snapshot(
        &self,
        name: &str,
        snap_cfg: &DaytonaSnapshotConfig,
    ) -> crate::Result<()> {
        match self.client.snapshot.get(name).await {
            Ok(dto) => {
                use daytona_api_client::models::SnapshotState;
                match dto.state {
                    SnapshotState::Active => return Ok(()),
                    SnapshotState::Error | SnapshotState::BuildFailed => {
                        return Err(crate::Error::message(format!(
                            "Snapshot '{}' is in state '{}': {}",
                            name,
                            dto.state,
                            dto.error_reason.unwrap_or_default()
                        )));
                    }
                    _ => {
                        // Building/Pending/Pulling — fall through to poll
                        self.emit(SandboxEvent::SnapshotCreating {
                            name: name.to_string(),
                        });
                    }
                }
            }
            Err(daytona_sdk::DaytonaError::NotFound { .. }) => {
                let dockerfile = match &snap_cfg.dockerfile {
                    Some(DockerfileSource::Inline(s)) => s.as_str(),
                    Some(DockerfileSource::Path { .. }) => {
                        return Err(crate::Error::message(format!(
                            "Snapshot '{name}': dockerfile path should have been resolved to inline content before sandbox creation"
                        )));
                    }
                    None => {
                        return Err(crate::Error::message(format!(
                            "Snapshot '{name}' does not exist and no dockerfile provided to create it"
                        )));
                    }
                };

                self.emit(SandboxEvent::SnapshotCreating {
                    name: name.to_string(),
                });

                let params = daytona_sdk::CreateSnapshotParams {
                    name:       name.to_string(),
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
                    crate::Error::context(format!("Failed to create snapshot '{name}'"), e)
                })?;
            }
            Err(e) => {
                return Err(crate::Error::context(
                    format!("Failed to get snapshot '{name}'"),
                    e,
                ));
            }
        }

        // Poll until Active (or terminal failure).
        self.poll_snapshot_active(name).await
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

        let params = if let Some(snap_cfg) = self
            .config
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.dockerfile.is_some())
        {
            let api_key = self.api_key.as_deref().ok_or_else(|| {
                self.fail_init(
                    init_start,
                    crate::Error::message(format!(
                        "{} is required to compute Daytona snapshot identity",
                        EnvVars::DAYTONA_API_KEY
                    )),
                )
            })?;
            let snapshot_name = match snapshot_identity::snapshot_name(api_key, snap_cfg) {
                Ok(name) => name,
                Err(err) => return Err(self.fail_init(init_start, err)),
            };
            let snap_start = Instant::now();
            if let Err(e) = self.ensure_snapshot(&snapshot_name, snap_cfg).await {
                self.emit(SandboxEvent::SnapshotFailed {
                    name:   snapshot_name.clone(),
                    error:  e.to_string(),
                    causes: e.causes(),
                });
                return Err(self.fail_init(init_start, e));
            }
            let snap_duration = u64::try_from(snap_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::SnapshotReady {
                name:        snapshot_name.clone(),
                duration_ms: snap_duration,
            });
            let _ = self.snapshot_name.set(snapshot_name.clone());

            daytona_sdk::CreateParams::Snapshot(daytona_sdk::SnapshotParams {
                base:     self.base_params(),
                snapshot: snapshot_name,
            })
        } else {
            let _ = self.snapshot_name.set(DEFAULT_SNAPSHOT.to_string());
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

        if let Err(bash_error) = Self::probe_bash(&sandbox).await {
            let err = Self::delete_unusable_sandbox(&sandbox, bash_error).await;
            return Err(self.fail_init(init_start, err));
        }

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
                self.set_working_directory(WORKING_DIRECTORY)
                    .map_err(|err| self.fail_init(init_start, err))?;
            }
            CloneDecision::GitHub { origin_url, branch } => {
                let layout =
                    clone_source::github_repo_layout(&origin_url, WORKING_DIRECTORY, REPOS_ROOT)
                        .map_err(|err| self.fail_init(init_start, err))?;
                let token_was_freshly_minted = self
                    .github_app
                    .as_ref()
                    .is_some_and(GitHubCredentials::mints_installation_token);
                self.emit(SandboxEvent::GitCloneStarted {
                    url:    origin_url.clone(),
                    branch: branch.clone(),
                });
                let clone_start = Instant::now();

                let (username, password) = match &self.github_app {
                    Some(creds) => fabro_github::resolve_clone_credentials(
                        &fabro_github::GitHubContext::new(
                            creds,
                            &fabro_github::github_api_base_url(),
                        ),
                        &layout.owner,
                        &layout.repo,
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
                    })?,
                    None => (None, None),
                };

                let fs_svc = sandbox.fs().await.map_err(|e| {
                    let err = crate::Error::context("Failed to get Daytona fs service", e);
                    self.emit(SandboxEvent::GitCloneFailed {
                        url:    origin_url.clone(),
                        error:  err.to_string(),
                        causes: err.causes(),
                    });
                    self.fail_init(init_start, err)
                })?;
                fs_svc
                    .create_folder(WORKING_DIRECTORY, None)
                    .await
                    .map_err(|e| {
                        let err = wrap_fs_error(
                            "Failed to create Daytona workspace root",
                            WORKING_DIRECTORY,
                            e,
                        );
                        self.emit(SandboxEvent::GitCloneFailed {
                            url:    origin_url.clone(),
                            error:  err.to_string(),
                            causes: err.causes(),
                        });
                        self.fail_init(init_start, err)
                    })?;
                fs_svc.create_folder(REPOS_ROOT, None).await.map_err(|e| {
                    let err = wrap_fs_error("Failed to create Daytona repos root", REPOS_ROOT, e);
                    self.emit(SandboxEvent::GitCloneFailed {
                        url:    origin_url.clone(),
                        error:  err.to_string(),
                        causes: err.causes(),
                    });
                    self.fail_init(init_start, err)
                })?;
                fs_svc
                    .create_folder(&layout.repos_owner_path, None)
                    .await
                    .map_err(|e| {
                        let err = wrap_fs_error(
                            "Failed to create Daytona repos owner directory",
                            &layout.repos_owner_path,
                            e,
                        );
                        self.emit(SandboxEvent::GitCloneFailed {
                            url:    origin_url.clone(),
                            error:  err.to_string(),
                            causes: err.causes(),
                        });
                        self.fail_init(init_start, err)
                    })?;

                let git_svc = sandbox.git().await.map_err(|e| {
                    let err = crate::Error::context("Failed to get Daytona git service", e);
                    self.emit(SandboxEvent::GitCloneFailed {
                        url:    origin_url.clone(),
                        error:  err.to_string(),
                        causes: err.causes(),
                    });
                    self.fail_init(init_start, err)
                })?;

                let clone_result = clone_retry::retry_clone(
                    SandboxProviderKind::Daytona,
                    None,
                    |_attempt| {
                        let git_svc = &git_svc;
                        let origin = origin_url.as_str();
                        let target = layout.primary_repo_path.as_str();
                        let options = daytona_sdk::GitCloneOptions {
                            branch: branch.clone(),
                            username: username.clone(),
                            password: password.clone(),
                            ..Default::default()
                        };
                        async move { git_svc.clone(origin, target, options).await }
                    },
                    |err: &DaytonaError| classify_clone_failure(err, token_was_freshly_minted),
                )
                .await;

                match clone_result {
                    Ok(()) => {
                        let process_svc = sandbox.process().await.map_err(|e| {
                            let err =
                                crate::Error::context("Failed to get Daytona process service", e);
                            self.emit(SandboxEvent::GitCloneFailed {
                                url:    origin_url.clone(),
                                error:  err.to_string(),
                                causes: err.causes(),
                            });
                            self.fail_init(init_start, err)
                        })?;
                        let symlink_cmd = clone_source::repo_symlink_command(&layout);
                        let symlink_result = process_svc
                            .execute_command(
                                &wrap_bash_command(&symlink_cmd),
                                daytona_sdk::ExecuteCommandOptions {
                                    cwd: Some("/".to_string()),
                                    ..Default::default()
                                },
                            )
                            .await
                            .map_err(|e| {
                                let err = crate::Error::context(
                                    "Failed to create Daytona workspace repo symlink",
                                    e,
                                );
                                self.emit(SandboxEvent::GitCloneFailed {
                                    url:    origin_url.clone(),
                                    error:  err.to_string(),
                                    causes: err.causes(),
                                });
                                self.fail_init(init_start, err)
                            })?;
                        if symlink_result.exit_code != 0 {
                            let err = crate::Error::exec(
                                "create Daytona workspace repo symlink",
                                ExecResult {
                                    stdout:      symlink_result.result.clone(),
                                    stderr:      String::new(),
                                    exit_code:   Some(symlink_result.exit_code),
                                    termination: CommandTermination::Exited,
                                    duration_ms: 0,
                                },
                            );
                            self.emit(SandboxEvent::GitCloneFailed {
                                url:    origin_url.clone(),
                                error:  err.to_string(),
                                causes: err.causes(),
                            });
                            return Err(self.fail_init(init_start, err));
                        }

                        let clone_duration =
                            u64::try_from(clone_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                        self.emit(SandboxEvent::GitCloneCompleted {
                            url:         origin_url.clone(),
                            duration_ms: clone_duration,
                        });

                        let _ = self.repo_cloned.set(true);
                        let _ = self.origin_url.set(origin_url.clone());
                        self.set_working_directory(layout.execution_directory.clone())
                            .map_err(|err| self.fail_init(init_start, err))?;
                        if let Some(token) = password.as_deref() {
                            match fabro_github::embed_token_in_url(&origin_url, token) {
                                Ok(auth_url) => {
                                    let cmd = format!(
                                        "git -c maintenance.auto=0 remote set-url origin {}",
                                        shell_quote(auth_url.as_raw_url().as_str()),
                                    );
                                    let opts = daytona_sdk::ExecuteCommandOptions {
                                        cwd: Some(layout.execution_directory.clone()),
                                        ..Default::default()
                                    };
                                    let wrapped = wrap_bash_command(&cmd);
                                    match process_svc.execute_command(&wrapped, opts).await {
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
            url:         Some(DAYTONA_DASHBOARD_SANDBOXES_URL.into()),
        });

        Ok(())
    }

    async fn start(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::StartStarted {
            provider: "daytona".into(),
        });
        let start = Instant::now();
        let sandbox = self.sandbox()?;
        if let Err(e) = self.client.start(&sandbox.name).await {
            let err = crate::Error::context("Failed to start Daytona sandbox", e);
            self.emit(SandboxEvent::StartFailed {
                provider: "daytona".into(),
                error:    err.to_string(),
                causes:   err.causes(),
            });
            return Err(err);
        }
        if let Err(err) = Self::probe_bash(sandbox).await {
            self.emit(SandboxEvent::StartFailed {
                provider: "daytona".into(),
                error:    err.to_string(),
                causes:   err.causes(),
            });
            return Err(err);
        }
        let duration_ms = elapsed_ms(start);
        self.emit(SandboxEvent::StartCompleted {
            provider: "daytona".into(),
            duration_ms,
        });
        Ok(())
    }

    async fn activate(&self) -> crate::Result<()> {
        let sandbox = self.sandbox()?;
        let current = self.client.get(&sandbox.name).await.map_err(|e| {
            crate::Error::context("Failed to inspect Daytona sandbox before activation", e)
        })?;
        if current.state == Some(SandboxState::Started) {
            return Ok(());
        }
        if current.state == Some(SandboxState::Starting) {
            return current
                .wait_for_start(Some(DAYTONA_START_TIMEOUT))
                .await
                .map_err(|e| {
                    crate::Error::context("Failed to wait for Daytona sandbox activation", e)
                });
        }
        self.start().await
    }

    async fn stop(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::StopStarted {
            provider: "daytona".into(),
        });
        let start = Instant::now();
        let sandbox = self.sandbox()?;
        if let Err(e) = self.client.stop(&sandbox.name).await {
            let err = crate::Error::context("Failed to stop Daytona sandbox", e);
            self.emit(SandboxEvent::StopFailed {
                provider: "daytona".into(),
                error:    err.to_string(),
                causes:   err.causes(),
            });
            return Err(err);
        }
        let duration_ms = elapsed_ms(start);
        self.emit(SandboxEvent::StopCompleted {
            provider: "daytona".into(),
            duration_ms,
        });
        Ok(())
    }

    async fn delete(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::DeleteStarted {
            provider: "daytona".into(),
        });
        let start = Instant::now();
        if let Some(sandbox) = self.sandbox.get() {
            tracing::info!("Deleting Daytona sandbox");
            if let Err(e) = sandbox.delete().await {
                let err = crate::Error::context("Failed to delete Daytona sandbox", e);
                self.emit(SandboxEvent::DeleteFailed {
                    provider: "daytona".into(),
                    error:    err.to_string(),
                    causes:   err.causes(),
                });
                return Err(err);
            }
        }
        let duration_ms = elapsed_ms(start);
        self.emit(SandboxEvent::DeleteCompleted {
            provider: "daytona".into(),
            duration_ms,
        });
        Ok(())
    }

    async fn cleanup(&self) -> crate::Result<()> {
        self.delete().await
    }

    fn working_directory(&self) -> &str {
        self.working_directory
            .get()
            .map_or(WORKING_DIRECTORY, String::as_str)
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

    fn snapshot_info(&self) -> Option<String> {
        self.snapshot_name.get().cloned()
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

    #[tracing::instrument(name = "git_op", skip_all, fields(op = "refresh-credentials"))]
    async fn refresh_push_credentials(&self) -> crate::Result<RefreshOutcome> {
        if !self.repo_cloned() {
            return Ok(RefreshOutcome::Skipped);
        }
        let Some(origin_url) = self.origin_url.get() else {
            return Ok(RefreshOutcome::Skipped); // no authenticated origin — nothing to refresh
        };
        let Some(creds) = &self.github_app else {
            return Ok(RefreshOutcome::Skipped);
        };
        // Only a GitHub App installation token can be re-minted; a static PAT or
        // a pre-minted Installation token is fixed, so re-embedding it changes
        // nothing. Short-circuit to Skipped before the resolve + set-url exec.
        if !creds.mints_installation_token() {
            return Ok(RefreshOutcome::Skipped);
        }

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

        // Static creds were short-circuited to Skipped above; reaching here means
        // a GitHub App installation token was freshly minted.
        Ok(RefreshOutcome::Refreshed)
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

    async fn read_file_bytes(&self, path: &str) -> crate::Result<Vec<u8>> {
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

        Ok(bytes)
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

        self.upload_file_content(&resolved, content).await
    }

    async fn write_existing_file(&self, path: &str, content: &str) -> crate::Result<()> {
        let resolved = self.resolve_path(path);
        self.upload_file_content(&resolved, content).await
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

        let cwd = working_dir.map_or_else(
            || self.working_directory().to_string(),
            |d| self.resolve_path(d),
        );

        let process_svc = sandbox
            .process()
            .await
            .map_err(|e| crate::Error::context("Failed to get process service", e))?;

        tracing::info!(
            elapsed_ms = elapsed_ms(start),
            "exec_command: process service acquired, starting select"
        );

        let clean_env = clean_bash_env(env_vars);
        let options = daytona_sdk::ExecuteCommandOptions {
            cwd:     Some(cwd),
            env:     Some(clean_env.clone()),
            timeout: Some(std::time::Duration::from_millis(timeout_ms)),
        };

        // The Daytona toolbox's /process/execute endpoint does not yet
        // process the `envs` field (not in its OpenAPI spec), so we also
        // prepend `export` statements as a fallback until server support
        // lands. The SDK sends `envs` too for forward compatibility.
        let command_with_env = format!("{}\n{command}", bash_export_lines(&clean_env).join("\n"));

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
        request: ExecStreamingRequest<'_>,
    ) -> crate::Result<ExecStreamingResult> {
        let ExecStreamingRequest {
            command,
            timeout_ms,
            working_dir,
            env_vars,
            cancel_token,
            stdin,
            output_callback,
        } = request;
        let sandbox = self.sandbox()?;
        let start = Instant::now();
        let cwd = working_dir.map_or_else(
            || self.working_directory().to_string(),
            |d| self.resolve_path(d),
        );
        let stdin_upload = async {
            match stdin {
                Some(stdin) => DaytonaStdinFile::create(sandbox, &stdin).await.map(Some),
                None => Ok(None),
            }
        };
        let (mut stdin_file, mut session) =
            tokio::try_join!(stdin_upload, DaytonaSession::create(sandbox))?;
        let command_with_stdin = stdin_file
            .as_ref()
            .map(|stdin_file| stdin_file.redirect(command));
        let command = command_with_stdin.as_deref().unwrap_or(command);

        let session_command = build_bash_session_command(command, &cwd, env_vars);
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
                                if let Some(callback) = callback {
                                    callback(CommandOutputStream::Stdout, bytes)
                                        .await
                                        .map_err(|err| daytona_callback_error(&err))?;
                                }
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
                                if let Some(callback) = callback {
                                    callback(CommandOutputStream::Stderr, bytes)
                                        .await
                                        .map_err(|err| daytona_callback_error(&err))?;
                                }
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
            timeout_ms,
            cancel_token.unwrap_or_default(),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                stream_task.abort();
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
                output_callback.as_ref(),
            )
            .await?;
            append_missing_log_suffix(
                CommandOutputStream::Stderr,
                logs.stderr.as_bytes(),
                &stderr_seen,
                output_callback.as_ref(),
            )
            .await?;
        }

        let stdout = String::from_utf8_lossy(&stdout_seen.lock().await).into_owned();
        let stderr = String::from_utf8_lossy(&stderr_seen.lock().await).into_owned();

        let result = ExecStreamingResult {
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
        };
        if let Some(stdin_file) = stdin_file.as_mut() {
            stdin_file.close().await;
        }
        Ok(result)
    }

    async fn spawn_stdio_process(
        &self,
        _command: &str,
        _working_dir: Option<&str>,
        _env_vars: Option<&HashMap<String, String>>,
        _cancel_token: Option<CancellationToken>,
    ) -> crate::Result<StdioProcess> {
        Err(crate::Error::message(
            "ACP backend requires bidirectional stdio; the Daytona sandbox provider does not support it yet",
        ))
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

    async fn walk_files(
        &self,
        base: &str,
        relative_start: &str,
        options: &WalkOptions,
    ) -> crate::Result<Vec<SandboxFile>> {
        if options.excludes_relative_path(relative_start) {
            return Ok(Vec::new());
        }

        let base = self.resolve_path(base);
        let command = sandbox::build_remote_walk_command(&base, relative_start, options);
        let result = self
            .exec_command(&command, REMOTE_WALK_TIMEOUT_MS, None, None, None)
            .await?;
        if !result.is_success() {
            return Err(crate::Error::exec("recursive file traversal", result));
        }

        sandbox::parse_remote_walk_output(&base, relative_start, &result.stdout)
    }
}

fn daytona_callback_error(err: &crate::Error) -> DaytonaError {
    DaytonaError::general(format!("output callback failed: {err}"))
}

/// Wrap a Daytona filesystem error with a richer message that includes the
/// attempted path and a hint when the status code suggests a configuration
/// issue. Preserves the underlying `DaytonaError` in the source chain so
/// callers can still inspect status/headers via downcasting.
fn wrap_fs_error(operation: &str, path: &str, error: DaytonaError) -> crate::Error {
    let message = match error.status_code() {
        Some(400) => format!(
            "{operation} '{path}' failed (HTTP 400). This usually means the sandbox user \
             lacks write permission on the parent directory. If you're using a custom \
             Daytona snapshot, ensure the sandbox user can write to '{path}', or use a \
             path under the user's home directory (e.g. /home/daytona/...)."
        ),
        Some(status @ (401 | 403)) => format!(
            "{operation} '{path}' rejected by Daytona (HTTP {status}) — check that your \
             DAYTONA_API_KEY has the required permissions."
        ),
        _ => format!("{operation} '{path}' failed"),
    };
    crate::Error::context(message, error)
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

/// A temporary Daytona file used to provide exact stdin bytes and EOF.
///
/// Daytona sessions accept input strings but do not expose a reliable EOF
/// operation. A file redirection preserves arbitrary bytes and gives the
/// command EOF without embedding workflow data in shell source.
struct DaytonaStdinFile {
    fs:   Option<daytona_sdk::FileSystemService>,
    path: String,
}

impl DaytonaStdinFile {
    async fn create(sandbox: &daytona_sdk::Sandbox, stdin: &[u8]) -> crate::Result<Self> {
        let fs = sandbox
            .fs()
            .await
            .map_err(|err| crate::Error::context("Failed to get Daytona file service", err))?;
        let path = format!(
            "/tmp/fabro-command-stdin-{:016x}",
            rand::rng().random::<u64>()
        );
        fs.upload_file_bytes(&path, stdin)
            .await
            .map_err(|err| crate::Error::context("Failed to upload Daytona command stdin", err))?;
        Ok(Self { fs: Some(fs), path })
    }

    /// Wrap `command` so it reads this file as its standard input.
    fn redirect(&self, command: &str) -> String {
        redirect_command_stdin(command, &self.path)
    }

    /// Idempotent, best-effort deletion bounded by
    /// [`DAYTONA_CLEANUP_TIMEOUT`]. Failures are logged rather than surfaced
    /// so cleanup can never fail a command that already completed.
    async fn close(&mut self) {
        let Some(fs) = self.fs.as_ref() else {
            return;
        };
        let deletion =
            time::timeout(DAYTONA_CLEANUP_TIMEOUT, fs.delete_file(&self.path, false)).await;

        // Keep the service owned until the delete future completes. If this
        // method is cancelled at the await above, Drop still has everything it
        // needs to retry cleanup.
        self.fs.take();
        match deletion {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "Failed to delete Daytona command stdin");
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms =
                        u64::try_from(DAYTONA_CLEANUP_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
                    "Timed out deleting Daytona command stdin"
                );
            }
        }
    }
}

impl Drop for DaytonaStdinFile {
    fn drop(&mut self) {
        let Some(fs) = self.fs.take() else {
            return;
        };
        let path = std::mem::take(&mut self.path);
        match Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    match time::timeout(DAYTONA_CLEANUP_TIMEOUT, fs.delete_file(&path, false)).await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => {
                            tracing::warn!(
                                error = %err,
                                "Failed to delete Daytona command stdin from Drop"
                            );
                        }
                        Err(_) => {
                            tracing::warn!(
                                timeout_ms = u64::try_from(DAYTONA_CLEANUP_TIMEOUT.as_millis())
                                    .unwrap_or(u64::MAX),
                                "Timed out deleting Daytona command stdin from Drop"
                            );
                        }
                    }
                });
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "Could not schedule Daytona command stdin cleanup"
                );
            }
        }
    }
}

/// RAII wrapper around a Daytona toolbox session.
///
/// Holds the per-session [`ProcessService`] handle and the session id. Callers
/// should invoke [`close`] on every path. If that future is cancelled, or a
/// `DaytonaSession` is otherwise dropped while it still owns its process
/// service, [`Drop`] spawns a bounded cleanup task on the current Tokio runtime
/// as a safety net (matching the
/// [`DetachedRunBootstrapGuard`] pattern in `fabro-workflow`).
struct DaytonaSession {
    process_svc: Option<daytona_sdk::ProcessService>,
    session_id:  String,
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

    /// Idempotent: a second call after the process service is consumed is a
    /// no-op.
    ///
    /// `delete_session` is bounded by [`DAYTONA_CLEANUP_TIMEOUT`] so a
    /// stalled Daytona REST call cannot block cancellation paths indefinitely.
    async fn close(&mut self, reason: &'static str) {
        let Some(svc) = self.process_svc.as_ref() else {
            return;
        };
        let deletion = time::timeout(
            DAYTONA_CLEANUP_TIMEOUT,
            svc.delete_session(&self.session_id),
        )
        .await;

        // Keep the service owned until the delete future completes. If this
        // method is cancelled at the await above, Drop still has everything it
        // needs to retry cleanup.
        self.process_svc.take();
        match deletion {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(
                    error = %err,
                    session_id = %self.session_id,
                    reason,
                    "failed to delete Daytona session"
                );
            }
            Err(_) => {
                tracing::warn!(
                    session_id = %self.session_id,
                    reason,
                    timeout_ms = u64::try_from(DAYTONA_CLEANUP_TIMEOUT.as_millis())
                        .unwrap_or(u64::MAX),
                    "timed out deleting Daytona session"
                );
            }
        }
    }
}

impl Drop for DaytonaSession {
    fn drop(&mut self) {
        let Some(svc) = self.process_svc.take() else {
            return;
        };
        let session_id = std::mem::take(&mut self.session_id);
        match Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    match time::timeout(DAYTONA_CLEANUP_TIMEOUT, svc.delete_session(&session_id))
                        .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => {
                            tracing::warn!(
                                error = %err,
                                session_id,
                                "Daytona session leaked; failed to delete from Drop"
                            );
                        }
                        Err(_) => {
                            tracing::warn!(
                                session_id,
                                timeout_ms = u64::try_from(DAYTONA_CLEANUP_TIMEOUT.as_millis())
                                    .unwrap_or(u64::MAX),
                                "Daytona session leaked; timed out deleting from Drop"
                            );
                        }
                    }
                });
            }
            Err(_) => {
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
/// timer, and the cancel token. The caller owns any associated log-stream task
/// and is responsible for aborting it and closing the session on poll failure.
async fn wait_for_completion(
    session: &DaytonaSession,
    command_id: &str,
    initial_exit_code: Option<i32>,
    timeout_ms: Option<u64>,
    cancel_token: CancellationToken,
) -> crate::Result<WaitOutcome> {
    if let Some(code) = initial_exit_code {
        return Ok(WaitOutcome {
            exit_code:   Some(code),
            termination: CommandTermination::Exited,
            final_logs:  None,
        });
    }

    let timeout_future = optional_timeout(timeout_ms);
    tokio::pin!(timeout_future);
    loop {
        tokio::select! {
            () = time::sleep(Duration::from_millis(250)) => {
                let status = match session.get_command_status(command_id).await {
                    Ok(status) => status,
                    Err(err) => {
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
    output_callback: Option<&CommandOutputCallback>,
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
    match output_callback {
        Some(output_callback) => output_callback(stream, missing).await,
        None => Ok(()),
    }
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

/// Override caller or snapshot startup-file injection for a Bash process.
fn clean_bash_env(env_vars: Option<&HashMap<String, String>>) -> HashMap<String, String> {
    let mut clean = env_vars.cloned().unwrap_or_default();
    clean.insert(BASH_ENV_VAR.to_string(), String::new());
    clean
}

fn bash_export_lines(env_vars: &HashMap<String, String>) -> Vec<String> {
    let mut entries: Vec<_> = env_vars.iter().collect();
    entries.sort_by_key(|(key, _)| *key);
    entries
        .into_iter()
        .map(|(key, value)| format!("export {}={}", shell_quote(key), shell_quote(value)))
        .collect()
}

/// Build the inner Bash script a session command evaluates.
///
/// The result is Bash source, not something Daytona can exec directly — it is
/// passed through [`wrap_bash_session_script`] before reaching the toolbox.
fn build_bash_session_script(
    command: &str,
    cwd: &str,
    env_vars: Option<&HashMap<String, String>>,
) -> String {
    let mut lines = vec![format!("cd {} || exit $?", shell_quote(cwd))];
    lines.extend(bash_export_lines(&clean_bash_env(env_vars)));

    lines.push("(".to_string());
    lines.push(command.to_string());
    lines.push(")".to_string());
    lines.join("\n")
}

/// Interpret the outcome of the direct-exec Daytona Bash probe.
///
/// A failure to run the probe at all, a nonzero exit, and a zero exit without
/// the marker are all probe failures, and all carry the snapshot remediation.
fn daytona_bash_probe_outcome(execution: crate::Result<ExecResult>) -> crate::Result<()> {
    match execution {
        Err(err) => Err(crate::Error::context(DAYTONA_BASH_REMEDIATION, err)),
        Ok(result) => validate_bash_probe(result, DAYTONA_BASH_REMEDIATION),
    }
}

/// Interpret the outcome of the session Daytona Bash probe.
///
/// The marker has to be its own line rather than the whole of stdout: session
/// output arrives through Daytona's log labelers rather than as a single
/// captured buffer, and a probe that rejected any surrounding transport bytes
/// would fail every sandbox creation instead of the transport defect it exists
/// to catch. Substring matching would be too weak in the other direction —
/// [`BASH_PROBE_SCRIPT`] contains the marker literal, so a transport that
/// echoed the submitted script back would pass.
fn daytona_bash_session_probe_outcome(execution: crate::Result<ExecResult>) -> crate::Result<()> {
    let result = match execution {
        Ok(result) => result,
        Err(err) => {
            return Err(crate::Error::context(DAYTONA_BASH_SESSION_REMEDIATION, err));
        }
    };

    if result.is_success()
        && result
            .stdout
            .lines()
            .any(|line| line.trim() == BASH_PROBE_MARKER)
    {
        return Ok(());
    }

    Err(crate::Error::context(
        DAYTONA_BASH_SESSION_REMEDIATION,
        result.into_exec_error("Sandbox Bash session probe"),
    ))
}

/// Classify a failed Daytona clone for retry.
///
/// The GitHub 404 does not arrive as an HTTP 404 on the Daytona call. git runs
/// inside the sandbox, so its stderr comes back through the toolbox as the
/// error message — the credential race has to be matched on text. Daytona's own
/// transport failures are visible structurally.
fn classify_clone_failure(
    err: &DaytonaError,
    token_was_freshly_minted: bool,
) -> Option<CloneRetryReason> {
    // A Daytona request timeout does not prove that the remote clone stopped.
    // Retrying could overlap the still-running first request.
    if matches!(err, DaytonaError::Timeout { .. }) {
        return None;
    }

    match clone_retry::classify_message(err.message(), token_was_freshly_minted) {
        clone_retry::CloneMessageClass::Retry(reason) => Some(reason),
        clone_retry::CloneMessageClass::Permanent => None,
        clone_retry::CloneMessageClass::Unknown => match err {
            DaytonaError::RateLimit { .. } => Some(CloneRetryReason::TransientInfra),
            DaytonaError::Api { status_code, .. } if (500..600).contains(status_code) => {
                Some(CloneRetryReason::TransientInfra)
            }
            DaytonaError::Timeout { .. }
            | DaytonaError::Api { .. }
            | DaytonaError::NotFound { .. }
            | DaytonaError::General(_) => None,
        },
    }
}

/// Wrap Bash source in the canonical non-login Bash transport.
///
/// The Daytona API uses direct exec (not a shell), so pipes, env vars,
/// semicolons, etc. won't work without this wrapper. Every path that sends a
/// caller-supplied or Fabro-built command to Daytona goes through here, so
/// there is exactly one interpreter on both sides of the transport.
///
/// Uses base64 encoding (matching the TypeScript/Python/Ruby Daytona SDKs)
/// to avoid shell escaping issues with quotes and special characters. The
/// pipeline's exit status is the inner Bash's, so command exit codes survive
/// the transport.
fn wrap_bash_command(command: &str) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let encoded = STANDARD.encode(command);
    format!("{REMOTE_BASH} -c \"echo '{encoded}' | base64 -d | {REMOTE_BASH}\"")
}

/// Invoke the canonical Bash interpreter from a Daytona streaming session.
///
/// Session commands already pass through the provider's shell parser, so an
/// audited shell-quoted argument avoids the direct-exec path's base64 process
/// and second Bash while keeping caller source inert until `/bin/bash -c`
/// evaluates it. Bash must remain a child process: Daytona sources this command
/// inside a wrapper that resumes afterward to drain logs and persist the exit
/// code.
fn wrap_bash_session_script(script: &str) -> String {
    format!("{REMOTE_BASH} -c {}", shell_quote(script))
}

fn redirect_command_stdin(command: &str, stdin_path: &str) -> String {
    format!("(\n{command}\n) < {}", shell_quote(stdin_path))
}

/// Build a command for Daytona's streaming session transport.
///
/// Both [`Sandbox::exec_command_streaming`] and the lifecycle probe call this
/// helper, so they cannot drift onto different script construction or Bash
/// wrappers.
fn build_bash_session_command(
    command: &str,
    cwd: &str,
    env_vars: Option<&HashMap<String, String>>,
) -> String {
    wrap_bash_session_script(&build_bash_session_script(command, cwd, env_vars))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use daytona_api_client::models::api_key_list::Permissions;
    use fabro_util::error::collect_chain;
    use httpmock::Method::{DELETE, GET, POST};
    use httpmock::{HttpMockResponse, MockServer};

    use super::*;
    use crate::sandbox::BASH_PROBE_MARKER;

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

    async fn mock_daytona_sandbox(
        server: &MockServer,
        api_key: &str,
        config: DaytonaConfig,
    ) -> DaytonaSandbox {
        let client = build_daytona_client_with(
            Some(api_key.to_string()),
            Some(server.base_url()),
            None,
            Some(fabro_test::test_http_client()),
        )
        .await
        .expect("mock Daytona client should build");

        DaytonaSandbox {
            config,
            client,
            api_key: Some(api_key.to_string()),
            github_app: None,
            sandbox: OnceCell::new(),
            snapshot_name: OnceCell::new(),
            rg_available: OnceCell::const_new(),
            event_callback: None,
            origin_url: OnceCell::new(),
            repo_cloned: OnceCell::new(),
            working_directory: OnceCell::new(),
            run_id: None,
            clone_origin_url: None,
            clone_branch: None,
        }
    }

    fn snapshot_body(name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": name,
            "name": name,
            "state": "active",
            "general": false,
            "cpu": 2.0,
            "gpu": 0.0,
            "mem": 4.0,
            "disk": 20.0,
            "size": null,
            "entrypoint": null,
            "errorReason": null,
            "lastUsedAt": null,
            "createdAt": "2026-05-01T00:00:00Z",
            "updatedAt": "2026-05-01T00:00:00Z"
        })
    }

    fn sandbox_body(name: &str, state: SandboxState) -> serde_json::Value {
        serde_json::json!({
            "id": name,
            "organizationId": "org-1",
            "name": name,
            "user": "daytona",
            "env": {},
            "labels": {},
            "public": false,
            "networkBlockAll": false,
            "target": "us",
            "cpu": 2.0,
            "gpu": 0.0,
            "memory": 4.0,
            "disk": 20.0,
            "state": state.to_string()
        })
    }

    #[test]
    fn daytona_config_defaults() {
        let config = DaytonaConfig::default();
        assert!(config.snapshot.is_none());
        assert!(config.auto_stop_interval.is_none());
        assert!(config.labels.is_none());
    }

    #[test]
    fn computed_snapshot_identity_is_deterministic_and_keyed() {
        let config = DaytonaSnapshotConfig {
            cpu:        Some(2),
            memory:     Some(4),
            disk:       Some(10),
            dockerfile: Some(DockerfileSource::Inline(
                "FROM ubuntu:24.04\nRUN apt-get update".to_string(),
            )),
        };

        let first = snapshot_identity::snapshot_name("dtn_secret", &config).unwrap();
        let second = snapshot_identity::snapshot_name("dtn_secret", &config).unwrap();
        let rotated_key = snapshot_identity::snapshot_name("dtn_rotated", &config).unwrap();

        assert_eq!(first, second);
        assert_ne!(first, rotated_key);
        let uuid = first
            .strip_prefix("fabro-")
            .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
            .expect("snapshot name should be fabro-<uuid>");
        assert_eq!(uuid.get_version_num(), 8);
        assert_eq!(uuid.get_variant(), uuid::Variant::RFC4122);
    }

    #[test]
    fn computed_snapshot_identity_changes_for_generation_inputs() {
        let base = DaytonaSnapshotConfig {
            cpu:        Some(2),
            memory:     Some(4),
            disk:       Some(10),
            dockerfile: Some(DockerfileSource::Inline("FROM ubuntu:24.04".to_string())),
        };
        let base_name = snapshot_identity::snapshot_name("dtn_secret", &base).unwrap();

        let cases = [
            DaytonaSnapshotConfig {
                dockerfile: Some(DockerfileSource::Inline(
                    "FROM ubuntu:24.04\n# roll cache".to_string(),
                )),
                ..base.clone()
            },
            DaytonaSnapshotConfig {
                cpu: Some(4),
                ..base.clone()
            },
            DaytonaSnapshotConfig {
                memory: Some(8),
                ..base.clone()
            },
            DaytonaSnapshotConfig {
                disk: Some(20),
                ..base.clone()
            },
        ];

        for changed in cases {
            let changed_name = snapshot_identity::snapshot_name("dtn_secret", &changed).unwrap();
            assert_ne!(base_name, changed_name);
        }
    }

    #[test]
    fn computed_snapshot_identity_excludes_raw_dockerfile_and_key_material() {
        let config = DaytonaSnapshotConfig {
            cpu:        None,
            memory:     None,
            disk:       None,
            dockerfile: Some(DockerfileSource::Inline(
                "FROM private.example.com/secret-image\nRUN echo raw-secret".to_string(),
            )),
        };

        let name = snapshot_identity::snapshot_name("dtn_super_secret_key", &config).unwrap();

        assert!(name.starts_with("fabro-"));
        assert!(!name.contains("private.example.com"));
        assert!(!name.contains("raw-secret"));
        assert!(!name.contains("dtn_super_secret_key"));
    }

    #[tokio::test]
    async fn ensure_snapshot_uses_computed_snapshot_name_for_daytona_api_calls() {
        let api_key = "dtn_secret";
        let snapshot = DaytonaSnapshotConfig {
            cpu:        Some(2),
            memory:     Some(4),
            disk:       Some(10),
            dockerfile: Some(DockerfileSource::Inline("FROM ubuntu:24.04".to_string())),
        };
        let computed_name = snapshot_identity::snapshot_name(api_key, &snapshot).unwrap();
        let server = MockServer::start_async().await;
        let path = format!("/snapshots/{computed_name}");
        let get_snapshot = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path(path.as_str())
                    .header("authorization", "Bearer dtn_secret");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(snapshot_body(&computed_name));
            })
            .await;
        let config = DaytonaConfig {
            snapshot: Some(snapshot.clone()),
            ..DaytonaConfig::default()
        };
        let sandbox = mock_daytona_sandbox(&server, api_key, config).await;

        sandbox
            .ensure_snapshot(&computed_name, &snapshot)
            .await
            .expect("existing computed snapshot should be accepted");

        get_snapshot.assert_async().await;
    }

    #[tokio::test]
    async fn base_params_create_run_owned_non_ephemeral_sandbox() {
        let sandbox = DaytonaSandbox::new(
            DaytonaConfig::default(),
            None,
            None,
            None,
            None,
            Some("dtn_test".to_string()),
        )
        .await
        .expect("sandbox config should be valid");

        let params = sandbox.base_params();

        assert_eq!(params.ephemeral, Some(false));
        assert_eq!(params.auto_delete_interval, Some(-1));
        assert_eq!(
            params.auto_stop_interval,
            Some(DEFAULT_AUTO_STOP_INTERVAL_MINUTES)
        );
        assert_eq!(
            params.env_vars,
            Some(HashMap::from([(BASH_ENV_VAR.to_string(), String::new())]))
        );
        assert_eq!(
            params.labels,
            Some(HashMap::from([(
                managed_labels::MANAGED_LABEL.to_string(),
                "true".to_string(),
            )]))
        );
    }

    #[tokio::test]
    async fn base_params_passes_explicit_auto_stop_through() {
        for interval in [0, 45] {
            let sandbox = DaytonaSandbox::new(
                DaytonaConfig {
                    auto_stop_interval: Some(interval),
                    ..DaytonaConfig::default()
                },
                None,
                None,
                None,
                None,
                Some("dtn_test".to_string()),
            )
            .await
            .expect("sandbox config should be valid");

            assert_eq!(sandbox.base_params().auto_stop_interval, Some(interval));
        }
    }

    #[tokio::test]
    async fn activate_skips_start_when_daytona_reports_started() {
        let server = MockServer::start_async().await;
        let get_sandbox = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/sandbox/test-sandbox")
                    .header("authorization", "Bearer dtn_test");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(sandbox_body("test-sandbox", SandboxState::Started));
            })
            .await;
        let start_sandbox = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/sandbox/test-sandbox/start")
                    .header("authorization", "Bearer dtn_test");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(sandbox_body("test-sandbox", SandboxState::Started));
            })
            .await;
        let sandbox = mock_daytona_sandbox(&server, "dtn_test", DaytonaConfig::default()).await;
        let sdk_sandbox = sandbox
            .client
            .get("test-sandbox")
            .await
            .expect("test sandbox should load");
        sandbox
            .sandbox
            .set(sdk_sandbox)
            .expect("test sandbox should initialize once");

        let get_calls_before = get_sandbox.calls_async().await;
        sandbox
            .activate()
            .await
            .expect("an active sandbox should require no restart");

        assert_eq!(get_sandbox.calls_async().await, get_calls_before + 1);
        start_sandbox.assert_calls_async(0).await;
    }

    #[tokio::test]
    async fn activate_waits_for_a_daytona_start_already_in_progress() {
        let server = MockServer::start_async().await;
        let response_count = Arc::new(AtomicU32::new(0));
        let get_sandbox = server
            .mock_async({
                let response_count = Arc::clone(&response_count);
                move |when, then| {
                    when.method(GET)
                        .path("/sandbox/test-sandbox")
                        .header("authorization", "Bearer dtn_test");
                    then.respond_with(move |_| {
                        let state = if response_count.fetch_add(1, Ordering::Relaxed) == 1 {
                            SandboxState::Starting
                        } else {
                            SandboxState::Started
                        };
                        HttpMockResponse::builder()
                            .status(200)
                            .header("content-type", "application/json")
                            .body(sandbox_body("test-sandbox", state).to_string())
                            .build()
                    });
                }
            })
            .await;
        let start_sandbox = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/sandbox/test-sandbox/start")
                    .header("authorization", "Bearer dtn_test");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(sandbox_body("test-sandbox", SandboxState::Started));
            })
            .await;
        let sandbox = mock_daytona_sandbox(&server, "dtn_test", DaytonaConfig::default()).await;
        let sdk_sandbox = sandbox
            .client
            .get("test-sandbox")
            .await
            .expect("test sandbox should load");
        sandbox
            .sandbox
            .set(sdk_sandbox)
            .expect("test sandbox should initialize once");

        let get_calls_before = get_sandbox.calls_async().await;
        sandbox
            .activate()
            .await
            .expect("an in-progress start should be awaited");

        assert_eq!(get_sandbox.calls_async().await, get_calls_before + 2);
        start_sandbox.assert_calls_async(0).await;
    }

    #[tokio::test]
    async fn base_params_merges_managed_daytona_labels() {
        let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
        let sandbox = DaytonaSandbox::new(
            DaytonaConfig {
                labels: Some(HashMap::from([
                    ("team".to_string(), "platform".to_string()),
                    (
                        managed_labels::MANAGED_LABEL.to_string(),
                        "false".to_string(),
                    ),
                    (
                        managed_labels::RUN_ID_LABEL.to_string(),
                        "wrong".to_string(),
                    ),
                ])),
                ..Default::default()
            },
            None,
            Some(run_id),
            None,
            None,
            Some("dtn_test".to_string()),
        )
        .await
        .expect("sandbox config should be valid");

        assert_eq!(
            sandbox.base_params().labels,
            Some(HashMap::from([
                ("team".to_string(), "platform".to_string()),
                (
                    managed_labels::MANAGED_LABEL.to_string(),
                    "true".to_string()
                ),
                (
                    managed_labels::RUN_ID_LABEL.to_string(),
                    "01HY0000000000000000000000".to_string(),
                ),
            ]))
        );
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
    fn clone_not_found_after_a_successful_mint_is_retried() {
        // The exact error from run 01KYM99DF27JRRW4XSYZBP27K7: git's stderr,
        // relayed through the toolbox, five seconds after a token was minted.
        let err = DaytonaError::general("repository not found: Repository not found.");

        assert_eq!(
            classify_clone_failure(&err, true),
            Some(CloneRetryReason::TokenReplication)
        );
        assert_eq!(
            classify_clone_failure(&err, false),
            None,
            "without credentials there is no token to replicate"
        );
    }

    #[test]
    fn clone_transient_transport_failures_are_retried() {
        for err in [
            DaytonaError::rate_limit("too many requests"),
            DaytonaError::api(503, ""),
        ] {
            assert_eq!(
                classify_clone_failure(&err, false),
                Some(CloneRetryReason::TransientInfra),
                "expected {err:?} to be transient"
            );
        }
    }

    #[test]
    fn clone_timeout_is_not_retried_without_remote_termination() {
        let err = DaytonaError::timeout("request timed out");

        assert_eq!(classify_clone_failure(&err, true), None);
    }

    #[test]
    fn clone_api_failure_message_takes_precedence_over_status() {
        let not_found = DaytonaError::api(500, "repository not found: Repository not found.");
        assert_eq!(
            classify_clone_failure(&not_found, true),
            Some(CloneRetryReason::TokenReplication)
        );
        assert_eq!(classify_clone_failure(&not_found, false), None);

        for message in [
            "fatal: destination path 'fabro' already exists",
            "remote: Permission to fabro-sh/fabro.git denied",
        ] {
            assert_eq!(
                classify_clone_failure(&DaytonaError::api(500, message), true),
                None,
                "expected {message:?} to take precedence over HTTP 500"
            );
        }
    }

    #[test]
    fn clone_client_errors_are_not_retried() {
        for err in [
            DaytonaError::api(400, "bad request"),
            DaytonaError::api(403, "forbidden"),
            DaytonaError::not_found("Sandbox not found"),
            DaytonaError::general("fatal: could not read Username for 'https://github.com'"),
        ] {
            assert_eq!(
                classify_clone_failure(&err, true),
                None,
                "expected {err:?} to fail fast"
            );
        }
    }

    #[test]
    fn wrap_fs_error_classifies_http_400_and_403() {
        let err_400 = wrap_fs_error(
            "Failed to create Daytona repos root",
            "/home/daytona/repos",
            DaytonaError::api(400, ""),
        );
        let top_400 = err_400.to_string();
        assert!(
            top_400.contains("/home/daytona/repos"),
            "400 top-level message should include the attempted path, got: {top_400}"
        );
        assert!(
            top_400.contains("HTTP 400") && top_400.contains("write permission"),
            "400 top-level message should classify as a permission issue, got: {top_400}"
        );

        let chain_400 = collect_chain(&err_400);
        assert!(
            chain_400
                .iter()
                .skip(1)
                .any(|cause| cause.contains("HTTP 400") || cause.is_empty()),
            "400 source chain should preserve the underlying DaytonaError, got: {chain_400:?}"
        );
        let source_400 = std::error::Error::source(&err_400)
            .and_then(|s| s.downcast_ref::<DaytonaError>())
            .expect("source should be a DaytonaError");
        assert_eq!(
            source_400.status_code(),
            Some(400),
            "downcast source should preserve the original status code"
        );

        let err_403 = wrap_fs_error(
            "Failed to create Daytona repos root",
            "/home/daytona/repos",
            DaytonaError::api(403, ""),
        );
        let top_403 = err_403.to_string();
        assert!(
            top_403.contains("/home/daytona/repos") && top_403.contains("HTTP 403"),
            "403 top-level message should include the path and status, got: {top_403}"
        );
        assert!(
            top_403.contains("DAYTONA_API_KEY"),
            "403 top-level message should hint at API key permissions, got: {top_403}"
        );
        let source_403 = std::error::Error::source(&err_403)
            .and_then(|s| s.downcast_ref::<DaytonaError>())
            .expect("source should be a DaytonaError");
        assert_eq!(source_403.status_code(), Some(403));
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

        let check = check_daytona_api_key_with(
            &server.base_url(),
            None,
            "dtn_test".to_string(),
            fabro_test::test_http_client(),
        )
        .await
        .expect("probe should succeed");

        assert!(!check.ok());
        assert_eq!(check.key_name, "delete-only");
        assert_eq!(check.missing_display(), "write:snapshots, write:sandboxes");
        auth.assert_async().await;
        current_key.assert_async().await;
    }

    #[tokio::test]
    async fn check_daytona_api_key_with_accepts_full_scopes_and_new_scopes() {
        let server = MockServer::start_async().await;
        let auth = mock_auth_probe(&server, 200).await;
        let current_key = mock_current_key(&server, vec![
            "write:snapshots",
            "delete:snapshots",
            "write:sandboxes",
            "delete:sandboxes",
            "manage:secrets",
            "read:limits",
            "manage:sso",
        ])
        .await;

        let check = check_daytona_api_key_with(
            &server.base_url(),
            None,
            "dtn_test".to_string(),
            fabro_test::test_http_client(),
        )
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

        let err = check_daytona_api_key_with(
            &server.base_url(),
            None,
            "dtn_test".to_string(),
            fabro_test::test_http_client(),
        )
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

    #[tokio::test]
    async fn daytona_stdin_file_uploads_exact_bytes_and_is_deleted() {
        let server = MockServer::start_async().await;
        let server_url = server.base_url();
        let sandbox_response = server
            .mock_async(|when, then| {
                when.method(GET).path("/sandbox/sandbox-stdin");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({
                        "id": "sandbox-stdin",
                        "organizationId": "org-1",
                        "name": "stdin-test",
                        "user": "daytona",
                        "env": {},
                        "labels": {},
                        "public": false,
                        "networkBlockAll": false,
                        "target": "us",
                        "cpu": 2.0,
                        "gpu": 0.0,
                        "memory": 4.0,
                        "disk": 20.0,
                        "state": "started"
                    }));
            })
            .await;
        let toolbox_response = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/sandbox/sandbox-stdin/toolbox-proxy-url");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({"url": server_url}));
            })
            .await;
        let upload = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/sandbox-stdin/files/upload")
                    .body_includes("opaque\n$(not shell)\nlast");
                then.status(200);
            })
            .await;
        let delete = server
            .mock_async(|when, then| {
                when.method(DELETE)
                    .path("/sandbox-stdin/files")
                    .query_param("recursive", "false");
                then.status(200);
            })
            .await;
        let client = build_daytona_client_with(
            Some("dtn_test".to_string()),
            Some(server.base_url()),
            None,
            Some(fabro_test::test_http_client()),
        )
        .await
        .expect("create Daytona client");
        let sandbox = client.get("sandbox-stdin").await.expect("get mock sandbox");

        let mut file = DaytonaStdinFile::create(&sandbox, b"opaque\n$(not shell)\nlast")
            .await
            .expect("upload stdin file");
        assert!(file.path.starts_with("/tmp/fabro-command-stdin-"));
        file.close().await;

        sandbox_response.assert_async().await;
        toolbox_response.assert_async().await;
        upload.assert_async().await;
        delete.assert_async().await;
    }

    #[tokio::test]
    async fn write_existing_file_skips_parent_directory_creation() {
        let server = MockServer::start_async().await;
        let server_url = server.base_url();
        let sandbox_response = server
            .mock_async(|when, then| {
                when.method(GET).path("/sandbox/sandbox-edit");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(sandbox_body("sandbox-edit", SandboxState::Started));
            })
            .await;
        let toolbox_response = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/sandbox/sandbox-edit/toolbox-proxy-url");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({"url": server_url}));
            })
            .await;
        let folder = server
            .mock_async(|when, then| {
                when.method(POST).path("/sandbox-edit/files/folder");
                then.status(200);
            })
            .await;
        let upload = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/sandbox-edit/files/upload")
                    .query_param("path", "/home/daytona/workspace/src/lib.rs")
                    .body_includes("updated contents");
                then.status(200);
            })
            .await;

        let sandbox = mock_daytona_sandbox(&server, "dtn_test", DaytonaConfig::default()).await;
        let sdk_sandbox = sandbox
            .client
            .get("sandbox-edit")
            .await
            .expect("get mock sandbox");
        assert!(sandbox.sandbox.set(sdk_sandbox).is_ok());

        sandbox
            .write_existing_file("src/lib.rs", "updated contents")
            .await
            .expect("write existing file");

        sandbox_response.assert_async().await;
        toolbox_response.assert_async().await;
        upload.assert_async().await;
        folder.assert_calls_async(0).await;
    }

    /// Recover the inner command a wrapper carries, proving it survives the
    /// base64 transport byte-for-byte.
    fn decode_wrapped_command(wrapped: &str) -> String {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;

        let prefix = "/bin/bash -c \"echo '";
        let suffix = "' | base64 -d | /bin/bash\"";
        let encoded = wrapped
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .unwrap_or_else(|| panic!("wrapper should have the canonical bash shape: {wrapped}"));
        String::from_utf8(
            STANDARD
                .decode(encoded)
                .expect("wrapper payload should be base64"),
        )
        .expect("wrapper payload should be UTF-8")
    }

    #[test]
    fn wrap_bash_uses_bash_on_both_sides_of_the_transport() {
        let wrapped = wrap_bash_command("echo hello");

        assert!(
            wrapped.starts_with("/bin/bash -c \"echo '"),
            "should start with a non-login /bin/bash wrapper: {wrapped}"
        );
        assert!(
            wrapped.ends_with("' | base64 -d | /bin/bash\""),
            "should pipe the decoded payload into /bin/bash: {wrapped}"
        );
        // The base64 of "echo hello" is "ZWNobyBoZWxsbw=="
        assert!(
            wrapped.contains("ZWNobyBoZWxsbw=="),
            "should contain base64 of 'echo hello'"
        );
    }

    #[test]
    fn wrap_bash_never_names_sh_as_an_interpreter() {
        for command in ["echo hello", "ls | grep foo", "sh -c 'echo explicit'"] {
            let wrapped = wrap_bash_command(command);
            let transport = wrapped
                .strip_suffix('"')
                .and_then(|rest| rest.split_once("| base64 -d | "))
                .map(|(_, interpreter)| interpreter)
                .expect("wrapper should end with its inner interpreter");

            assert_eq!(transport, "/bin/bash", "inner interpreter for {command:?}");
            assert!(
                !wrapped.starts_with("sh ") && !wrapped.starts_with("/bin/sh"),
                "outer interpreter for {command:?} should not be sh: {wrapped}"
            );
        }
    }

    #[test]
    fn wrap_bash_passes_arbitrary_text_through_unchanged() {
        for command in [
            "echo 'hello world'",
            "printf '%s\\n' \"quoted\"",
            "ls | grep foo",
            "echo line1\necho line2",
            "echo \"it's mixed 'quotes'\"",
        ] {
            assert_eq!(
                decode_wrapped_command(&wrap_bash_command(command)),
                command,
                "inner command should reach bash unchanged"
            );
        }
    }

    #[test]
    fn wrap_bash_handles_single_quotes_safely() {
        // Single quotes in the original command are safely encoded in base64
        let wrapped = wrap_bash_command("echo 'hello world'");
        assert!(
            wrapped.starts_with("/bin/bash -c \"echo '"),
            "should use the /bin/bash wrapper"
        );
        // No raw single quotes from the original command should appear in the base64
        assert!(
            !wrapped.contains("hello world"),
            "original command should be base64 encoded, not literal"
        );
    }

    #[test]
    fn build_bash_session_script_adds_cwd_and_sorted_exports() {
        let env = HashMap::from([
            ("BETA".to_string(), "two words".to_string()),
            ("ALPHA".to_string(), "one".to_string()),
            (
                BASH_ENV_VAR.to_string(),
                "/tmp/untrusted-startup".to_string(),
            ),
        ]);

        let command = build_bash_session_script("echo $ALPHA $BETA", "/tmp/with space", Some(&env));

        assert_eq!(
            command,
            "cd '/tmp/with space' || exit $?\n\
             export ALPHA=one\n\
             export BASH_ENV=''\n\
             export BETA='two words'\n\
             (\n\
             echo $ALPHA $BETA\n\
             )"
        );
    }

    #[test]
    fn streaming_session_script_reaches_bash_through_the_canonical_wrapper() {
        let script = build_bash_session_script("[[ -d / ]] && echo ok", "/tmp", None);
        let wrapped = wrap_bash_session_script(&script);

        assert_eq!(
            wrapped,
            format!("/bin/bash -c {}", shell_quote(&script)),
            "the streaming path must enter the canonical Bash exactly once"
        );
        assert!(!wrapped.contains("base64"));
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test executes the generated stdin redirection to verify exact bytes and EOF"
    )]
    fn stdin_file_redirection_applies_to_the_whole_command() {
        let dir = tempfile::tempdir().expect("create stdin transport temp dir");
        let stdin_path = dir.path().join("stdin data");
        let injection_path = dir.path().join("must-not-run");
        let stdin = format!(
            "first line\n$(touch {})\nlast line",
            injection_path.display()
        );
        std::fs::write(&stdin_path, &stdin).expect("write stdin fixture");
        let command = redirect_command_stdin(
            "IFS= read -r first\nprintf '%s\\n' \"$first\"\ncat",
            stdin_path.to_str().expect("temp path should be UTF-8"),
        );

        let output = std::process::Command::new(REMOTE_BASH)
            .args(["-c", &command])
            .env_remove(BASH_ENV_VAR)
            .output()
            .expect("execute redirected command");

        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), stdin);
        assert!(!injection_path.exists());
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test executes the generated shell transport to verify provider bookkeeping resumes"
    )]
    fn streaming_session_script_returns_to_provider_bookkeeping() {
        let dir = tempfile::tempdir().expect("create session transport temp dir");
        let command_file = dir.path().join("cmd.sh");
        let exit_code_file = dir.path().join("exit_code");
        let script = build_bash_session_script("printf 'command-finished\\n'; exit 7", "/", None);
        std::fs::write(&command_file, wrap_bash_session_script(&script))
            .expect("write generated session command");

        // Daytona sources the command file, then records the exit code. The
        // generated command must return control so that bookkeeping can run.
        let provider_wrapper = format!(
            "{{ . {}; }}\n\
             command_exit_code=$?\n\
             printf '%s\\n' \"$command_exit_code\" > {}",
            shell_quote(&command_file.to_string_lossy()),
            shell_quote(&exit_code_file.to_string_lossy()),
        );
        let output = std::process::Command::new(REMOTE_BASH)
            .args(["-c", &provider_wrapper])
            .env_remove(BASH_ENV_VAR)
            .output()
            .expect("execute generated session command");

        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "command-finished\n"
        );
        assert_eq!(
            std::fs::read_to_string(exit_code_file)
                .expect("provider bookkeeping should record the exit code"),
            "7\n"
        );
        assert!(output.status.success(), "{output:?}");
    }

    fn bash_probe_result(exit_code: i32, stdout: impl Into<String>) -> ExecResult {
        ExecResult {
            stdout:      stdout.into(),
            stderr:      String::new(),
            exit_code:   Some(exit_code),
            termination: CommandTermination::Exited,
            duration_ms: 1,
        }
    }

    #[test]
    fn bash_probe_outcome_accepts_a_marked_zero_exit() {
        assert!(
            daytona_bash_probe_outcome(Ok(bash_probe_result(0, format!("{BASH_PROBE_MARKER}\n"))))
                .is_ok()
        );
    }

    #[test]
    fn bash_probe_outcome_rejects_failures_with_snapshot_remediation() {
        let failures = [
            Err(crate::Error::message("connection reset")),
            Ok(bash_probe_result(1, "bash: not found")),
            Ok(bash_probe_result(0, "ready")),
        ];

        for failure in failures {
            let err = daytona_bash_probe_outcome(failure)
                .expect_err("probe should fail")
                .display_with_causes();

            assert!(
                err.contains("/bin/bash") && err.contains("snapshot"),
                "probe failure should carry the Daytona snapshot remediation: {err}"
            );
            assert!(
                !err.contains("bash: not found"),
                "raw process output must not enter lifecycle errors: {err}"
            );
        }
    }

    /// The session probe exists to catch a transport that runs the command but
    /// never returns control to Daytona's bookkeeping. That failure arrives as
    /// a timeout with no exit code — with the marker already on stdout — not as
    /// a nonzero exit.
    #[test]
    fn bash_session_probe_outcome_rejects_a_command_that_never_reports_completion() {
        let never_completed = ExecResult {
            stdout:      format!("{BASH_PROBE_MARKER}\n"),
            stderr:      String::new(),
            exit_code:   None,
            termination: CommandTermination::TimedOut,
            duration_ms: BASH_PROBE_TIMEOUT_MS,
        };

        let err = daytona_bash_session_probe_outcome(Ok(never_completed))
            .expect_err("a command with no recorded exit code must fail the probe")
            .display_with_causes();

        assert!(
            err.contains("wrapper shell") && err.contains("exit code"),
            "session probe failure should explain the completion contract: {err}"
        );
    }

    #[test]
    fn bash_session_probe_outcome_accepts_a_marker_line_amid_transport_output() {
        assert!(
            daytona_bash_session_probe_outcome(Ok(bash_probe_result(
                0,
                format!("\n{BASH_PROBE_MARKER}\r\n"),
            )))
            .is_ok()
        );
    }

    /// The probe script contains the marker literal, so a session transport
    /// that echoed the submitted script instead of running it must not pass.
    #[test]
    fn bash_session_probe_outcome_rejects_echoed_script_source() {
        let echoed = daytona_bash_session_probe_outcome(Ok(bash_probe_result(
            0,
            build_bash_session_command(BASH_PROBE_SCRIPT, "/", None),
        )));

        assert!(echoed.is_err());
    }

    #[test]
    fn bash_session_command_enters_bash_once_without_exec() {
        let command = build_bash_session_command(BASH_PROBE_SCRIPT, "/", None);

        assert!(
            command.starts_with(&format!("{REMOTE_BASH} -c ")),
            "{command}"
        );
        assert!(
            !command.contains("exec "),
            "the session probe must leave Daytona's wrapper shell in place: {command}"
        );
        assert_eq!(command.matches(REMOTE_BASH).count(), 1, "{command}");
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
