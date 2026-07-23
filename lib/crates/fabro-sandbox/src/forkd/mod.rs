//! Forkd Firecracker microVM sandbox implementation.
//!
//! One long-lived sandbox per run:
//! - Created in `initialize()` via `POST /v1/sandboxes`.
//! - Destroyed in `cleanup()` via `DELETE /v1/sandboxes/{id}`.
//! - All file I/O goes through exec (base64 round-trips for binary safety).
//! - Controller URL, bearer token, and snapshot tag are never hardcoded — they
//!   come from `FORKD_URL` / `FORKD_TOKEN` / `FORKD_SNAPSHOT_TAG` environment
//!   variables resolved at provider construction time.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fabro_types::{CommandTermination, RunId};
use fabro_util::time::elapsed_ms;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::OnceCell;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::config::ForkdSettings;
use crate::sandbox::resolve_path;
use crate::{
    DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback, shell_quote,
};

/// VM guest working directory.
pub(crate) const WORKING_DIRECTORY: &str = "/home/fabro/workspace";
/// VM guest repos root.
pub(crate) const REPOS_ROOT: &str = "/home/fabro/repos";
/// Provider name string for event payloads.
const PROVIDER: &str = "forkd";

/// Default snapshot tag used when `FORKD_SNAPSHOT_TAG` is not set.
pub const DEFAULT_SNAPSHOT_TAG: &str = "zen-gate-base";

/// Maximum number of retry attempts for transient HTTP failures (5xx /
/// connect).
const HTTP_RETRY_LIMIT: u32 = 3;
/// Initial backoff before the first retry.
const HTTP_RETRY_INITIAL_BACKOFF: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Public config type (re-exported by lib.rs)
// ---------------------------------------------------------------------------

/// Server-level connectivity for a forkd controller, plus per-sandbox runtime
/// settings.  The URL and token are resolved from environment variables and
/// must never be hardcoded.
#[derive(Clone)]
pub struct ForkdConfig {
    /// Base URL of the forkd controller, e.g. `http://127.0.0.1:8889`.
    pub forkd_url:   String,
    /// Bearer token for the forkd controller API.
    /// NEVER include the real value in log output — use `[redacted]`.
    pub forkd_token: String,
    /// Per-sandbox runtime settings (snapshot_tag, skip_clone, etc.).
    pub settings:    ForkdSettings,
}

// Manual Debug implementation that redacts the bearer token so it never
// appears in tracing output, panic messages, or error chains.
impl std::fmt::Debug for ForkdConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForkdConfig")
            .field("forkd_url", &self.forkd_url)
            .field("forkd_token", &"[redacted]")
            .field("settings", &self.settings)
            .finish()
    }
}

impl Default for ForkdConfig {
    fn default() -> Self {
        Self {
            forkd_url:   "http://127.0.0.1:8889".to_string(),
            forkd_token: "forkd-local-token".to_string(),
            settings:    ForkdSettings::default(),
        }
    }
}

impl ForkdConfig {
    /// Build a `ForkdConfig` by reading `FORKD_URL`, `FORKD_TOKEN`, and
    /// `FORKD_SNAPSHOT_TAG` from the process environment.
    ///
    /// Use this wherever a full `RunEnvironmentSettings` is not available
    /// (provider construction, sandbox reconnect) instead of
    /// [`ForkdConfig::default`] — the default's URL/token are placeholders
    /// for local development only and must never be used to talk to a real
    /// forkd controller.
    #[expect(
        clippy::disallowed_methods,
        reason = "Forkd config resolves server-level credentials from the process environment."
    )]
    #[must_use]
    pub fn from_env() -> Self {
        let forkd_url =
            std::env::var("FORKD_URL").unwrap_or_else(|_| "http://127.0.0.1:8889".to_string());
        let forkd_token =
            std::env::var("FORKD_TOKEN").unwrap_or_else(|_| "forkd-local-token".to_string());
        let snapshot_tag = std::env::var("FORKD_SNAPSHOT_TAG")
            .unwrap_or_else(|_| DEFAULT_SNAPSHOT_TAG.to_string());
        Self {
            forkd_url,
            forkd_token,
            settings: ForkdSettings {
                snapshot_tag,
                ..ForkdSettings::default()
            },
        }
    }
}

// ---------------------------------------------------------------------------
// forkd REST API request/response shapes (forkd 0.5.2)
// ---------------------------------------------------------------------------

/// `POST /v1/sandboxes` request body.
#[derive(Debug, Serialize)]
struct CreateSandboxRequest {
    snapshot_tag: String,
}

/// A single sandbox entry returned inside the array from `POST /v1/sandboxes`.
#[derive(Debug, Deserialize)]
struct SandboxEntry {
    id:           String,
    /// The snapshot tag that was actually used by the server (may differ from
    /// the requested tag if the server resolved an alias).
    #[serde(default)]
    snapshot_tag: Option<String>,
}

/// Defensive response shape for `POST /v1/sandboxes`.
///
/// The forkd 0.5.2 spec returns a JSON array; the untagged enum lets us also
/// accept a bare object in case of a single-element regression in the server.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CreateSandboxResponse {
    Array(Vec<SandboxEntry>),
    Single(SandboxEntry),
}

impl CreateSandboxResponse {
    fn into_first(self) -> Option<SandboxEntry> {
        match self {
            Self::Array(mut v) => {
                if v.is_empty() {
                    None
                } else {
                    Some(v.remove(0))
                }
            }
            Self::Single(entry) => Some(entry),
        }
    }
}

/// `POST /v1/sandboxes/{id}/exec` request body.
///
/// forkd exec is ARGV-ONLY — there is no `working_dir`, `env`, or `command`
/// string field.  Callers that need cwd/env must fold them into the argv via
/// `["sh", "-lc", "cd <dir> && export K=V; <cmd>"]`.
#[derive(Debug, Serialize)]
struct ExecRequest {
    args:         Vec<String>,
    timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
struct ExecResponse {
    stdout:    Option<String>,
    stderr:    Option<String>,
    exit_code: Option<i32>,
}

// ---------------------------------------------------------------------------
// ForkdSandbox
// ---------------------------------------------------------------------------

/// A sandbox backed by a single Firecracker microVM managed via forkd.
pub struct ForkdSandbox {
    config:           ForkdConfig,
    #[expect(
        dead_code,
        reason = "kept for parity with the run correlation Docker/Daytona sandboxes store on self; forkd sandboxes are not yet labeled or looked up by run_id"
    )]
    run_id:           Option<RunId>,
    clone_origin_url: Option<String>,
    clone_branch:     Option<String>,
    /// The server-assigned sandbox id, populated after a successful
    /// `create_sandbox()`.
    sandbox_id:       OnceCell<String>,
    /// The snapshot tag reported by the server (may differ from the requested
    /// tag).
    active_snapshot:  OnceCell<String>,
    /// Populated after a successful `initialize()`.
    initialized:      OnceCell<bool>,
    origin_url:       OnceCell<String>,
    event_callback:   Option<SandboxEventCallback>,
}

impl ForkdSandbox {
    /// Create a new `ForkdSandbox`.  The VM is not created until
    /// [`initialize`](Self::initialize) is called.
    pub fn new(
        config: ForkdConfig,
        run_id: Option<RunId>,
        clone_origin_url: Option<String>,
        clone_branch: Option<String>,
    ) -> Self {
        Self {
            config,
            run_id,
            clone_origin_url,
            clone_branch,
            sandbox_id: OnceCell::new(),
            active_snapshot: OnceCell::new(),
            initialized: OnceCell::new(),
            origin_url: OnceCell::new(),
            event_callback: None,
        }
    }

    /// The server-assigned sandbox id (available after `initialize()`).
    pub fn sandbox_id(&self) -> Option<&str> {
        self.sandbox_id.get().map(String::as_str)
    }

    /// Attach a callback that receives [`SandboxEvent`]s.
    pub fn set_event_callback(&mut self, cb: SandboxEventCallback) {
        self.event_callback = Some(cb);
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn emit(&self, event: SandboxEvent) {
        event.trace();
        if let Some(ref cb) = self.event_callback {
            cb(event);
        }
    }

    fn http_client() -> crate::Result<reqwest::Client> {
        fabro_http::HttpClientBuilder::new()
            // Hard cap on individual HTTP requests to the forkd controller.
            // Exec calls use a per-request timeout via ExecRequest.timeout_secs;
            // this is a safety net for connect + response-header receipt.
            .timeout(std::time::Duration::from_mins(2))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| crate::Error::context("Failed to build HTTP client for forkd", e))
    }

    fn resolve_path(path: &str) -> String {
        resolve_path(path, WORKING_DIRECTORY)
    }

    /// Returns `true` if an HTTP status code is a transient server-side error
    /// that is safe to retry (5xx range).
    fn is_retryable_status(status: reqwest::StatusCode) -> bool {
        status.is_server_error()
    }

    /// Build the argv for a shell-wrapped command that folds cwd and env vars
    /// into a single `sh -lc` invocation.
    ///
    /// forkd exec is argv-only; working_dir and env must be inlined here.
    /// Env key names are validated: only `[A-Za-z_][A-Za-z0-9_]*` are accepted
    /// to prevent injection via malicious key names.
    fn build_exec_argv(
        command: &str,
        working_dir: &str,
        env_vars: Option<&HashMap<String, String>>,
    ) -> Vec<String> {
        let mut shell_body = format!("cd {} && ", shell_quote(working_dir));

        if let Some(vars) = env_vars {
            let mut sorted: Vec<(&String, &String)> = vars.iter().collect();
            sorted.sort_by_key(|(k, _)| k.as_str());
            for (key, value) in sorted {
                // Safety: skip env keys that are not valid identifier names to
                // prevent shell injection via a crafted key.
                if key.chars().enumerate().all(|(i, c)| {
                    if i == 0 {
                        c.is_ascii_alphabetic() || c == '_'
                    } else {
                        c.is_ascii_alphanumeric() || c == '_'
                    }
                }) {
                    let _ = write!(shell_body, "export {}={} && ", key, shell_quote(value));
                } else {
                    tracing::warn!(key, "forkd: skipping env var with non-identifier key");
                }
            }
        }

        shell_body.push_str(command);

        vec!["sh".to_string(), "-lc".to_string(), shell_body]
    }

    /// Execute an argv inside the sandbox.  Returns the raw `ExecResponse`
    /// so the caller decides how to interpret exit code / output.
    ///
    /// Transient HTTP errors (connect failures and 5xx responses) are retried
    /// up to `HTTP_RETRY_LIMIT` times with exponential back-off.
    async fn exec_in_sandbox(
        &self,
        args: Vec<String>,
        timeout_secs: u64,
    ) -> crate::Result<ExecResponse> {
        let id = self.sandbox_id.get().ok_or_else(|| {
            crate::Error::message("forkd sandbox not yet initialized (no sandbox id)")
        })?;

        let client = Self::http_client()?;
        let url = format!("{}/v1/sandboxes/{}/exec", self.config.forkd_url, id);
        let body = ExecRequest { args, timeout_secs };

        let mut backoff = HTTP_RETRY_INITIAL_BACKOFF;
        let mut attempt = 0u32;
        loop {
            let result = client
                .post(&url)
                .bearer_auth(&self.config.forkd_token)
                .json(&body)
                .send()
                .await;

            match result {
                Ok(resp) if resp.status().is_success() => {
                    return resp.json::<ExecResponse>().await.map_err(|e| {
                        crate::Error::context("Failed to parse forkd exec response", e)
                    });
                }
                Ok(resp)
                    if Self::is_retryable_status(resp.status()) && attempt < HTTP_RETRY_LIMIT =>
                {
                    let status = resp.status();
                    tracing::warn!(
                        attempt,
                        status = status.as_u16(),
                        "forkd exec transient error; retrying"
                    );
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    return Err(crate::Error::message(format!(
                        "forkd exec returned {status}: {text}"
                    )));
                }
                Err(e) if e.is_connect() && attempt < HTTP_RETRY_LIMIT => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "forkd exec connect error; retrying"
                    );
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Err(e) => {
                    return Err(crate::Error::context("forkd exec HTTP request failed", e));
                }
            }
        }
    }

    /// A convenience wrapper used internally for simple shell commands that do
    /// not need cwd/env folding (e.g. mkdir, clone, base64 reads/writes).
    ///
    /// Uses `sh -lc` with a default cwd of `/` so the caller can use absolute
    /// paths without worrying about the initial working directory.
    async fn exec_shell(&self, command: &str, timeout_secs: u64) -> crate::Result<ExecResponse> {
        let args = vec!["sh".to_string(), "-lc".to_string(), command.to_string()];
        self.exec_in_sandbox(args, timeout_secs).await
    }

    /// Create the sandbox via `POST /v1/sandboxes`.
    ///
    /// Transient HTTP errors are retried up to `HTTP_RETRY_LIMIT` times.
    /// On success, stores the server-assigned sandbox id in `self.sandbox_id`.
    async fn create_sandbox(&self) -> crate::Result<()> {
        let client = Self::http_client()?;
        let url = format!("{}/v1/sandboxes", self.config.forkd_url);
        let body = CreateSandboxRequest {
            snapshot_tag: self.config.settings.snapshot_tag.clone(),
        };

        let mut backoff = HTTP_RETRY_INITIAL_BACKOFF;
        let mut attempt = 0u32;
        loop {
            let result = client
                .post(&url)
                .bearer_auth(&self.config.forkd_token)
                .json(&body)
                .send()
                .await;

            match result {
                Ok(resp) if resp.status().is_success() => {
                    let parsed = resp.json::<CreateSandboxResponse>().await.map_err(|e| {
                        crate::Error::context("Failed to parse forkd create response", e)
                    })?;

                    let entry = parsed.into_first().ok_or_else(|| {
                        crate::Error::message("forkd create returned empty array")
                    })?;

                    self.sandbox_id.set(entry.id).map_err(|_| {
                        crate::Error::message("forkd sandbox_id already set (double-init?)")
                    })?;

                    if let Some(tag) = entry.snapshot_tag {
                        let _ = self.active_snapshot.set(tag);
                    }

                    return Ok(());
                }
                Ok(resp)
                    if Self::is_retryable_status(resp.status()) && attempt < HTTP_RETRY_LIMIT =>
                {
                    let status = resp.status();
                    tracing::warn!(
                        attempt,
                        status = status.as_u16(),
                        "forkd create_sandbox transient error; retrying"
                    );
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    return Err(crate::Error::message(format!(
                        "forkd create sandbox returned {status}: {text}"
                    )));
                }
                Err(e) if e.is_connect() && attempt < HTTP_RETRY_LIMIT => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "forkd create_sandbox connect error; retrying"
                    );
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Err(e) => {
                    return Err(crate::Error::context(
                        "forkd create sandbox HTTP request failed",
                        e,
                    ));
                }
            }
        }
    }

    /// Delete the sandbox via `DELETE /v1/sandboxes/{id}`.
    ///
    /// 404 is treated as "already gone" and returns `Ok(())`.
    /// Transient HTTP errors are retried up to `HTTP_RETRY_LIMIT` times.
    async fn delete_sandbox(&self) -> crate::Result<()> {
        // Never initialized — nothing to delete.
        let Some(id) = self.sandbox_id.get() else {
            return Ok(());
        };

        let client = Self::http_client()?;
        let url = format!("{}/v1/sandboxes/{}", self.config.forkd_url, id);

        let mut backoff = HTTP_RETRY_INITIAL_BACKOFF;
        let mut attempt = 0u32;
        loop {
            let result = client
                .delete(&url)
                .bearer_auth(&self.config.forkd_token)
                .send()
                .await;

            match result {
                // 404 means the sandbox is already gone — idempotent success.
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => return Ok(()),
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp)
                    if Self::is_retryable_status(resp.status()) && attempt < HTTP_RETRY_LIMIT =>
                {
                    let status = resp.status();
                    tracing::warn!(
                        attempt,
                        status = status.as_u16(),
                        "forkd delete_sandbox transient error; retrying"
                    );
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    return Err(crate::Error::message(format!(
                        "forkd delete sandbox returned {status}: {text}"
                    )));
                }
                Err(e) if e.is_connect() && attempt < HTTP_RETRY_LIMIT => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "forkd delete_sandbox connect error; retrying"
                    );
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Err(e) => {
                    return Err(crate::Error::context(
                        "forkd delete sandbox HTTP request failed",
                        e,
                    ));
                }
            }
        }
    }

    /// Clone a git repository into the VM working directory.
    async fn clone_repo(&self, origin_url: &str, branch: Option<&str>) -> crate::Result<()> {
        self.emit(SandboxEvent::GitCloneStarted {
            url:    origin_url.to_string(),
            branch: branch.map(str::to_string),
        });
        let start = Instant::now();

        // Ensure parent directory exists.
        let mkdir_cmd = format!("mkdir -p {}", shell_quote(WORKING_DIRECTORY));
        let mkdir_resp = self.exec_shell(&mkdir_cmd, 30).await?;
        if mkdir_resp.exit_code != Some(0) {
            return Err(crate::Error::message(format!(
                "forkd mkdir failed (exit {:?}): {}",
                mkdir_resp.exit_code,
                mkdir_resp.stderr.unwrap_or_default()
            )));
        }

        // Build clone command.
        let clone_cmd = if let Some(branch) = branch {
            format!(
                "git clone --branch {} {} {}",
                shell_quote(branch),
                shell_quote(origin_url),
                shell_quote(WORKING_DIRECTORY),
            )
        } else {
            format!(
                "git clone {} {}",
                shell_quote(origin_url),
                shell_quote(WORKING_DIRECTORY),
            )
        };

        let clone_resp = self.exec_shell(&clone_cmd, 300).await?;

        if clone_resp.exit_code != Some(0) {
            let err = crate::Error::message(format!(
                "git clone failed (exit {:?}): {}",
                clone_resp.exit_code,
                clone_resp.stderr.unwrap_or_default()
            ));
            self.emit(SandboxEvent::GitCloneFailed {
                url:    origin_url.to_string(),
                error:  err.to_string(),
                causes: Vec::new(),
            });
            return Err(err);
        }

        let duration_ms = elapsed_ms(start);
        self.emit(SandboxEvent::GitCloneCompleted {
            url: origin_url.to_string(),
            duration_ms,
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Sandbox trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Sandbox for ForkdSandbox {
    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    async fn initialize(&self) -> crate::Result<()> {
        if self.initialized.get().copied().unwrap_or(false) {
            return Ok(());
        }

        let start = Instant::now();
        self.emit(SandboxEvent::Initializing {
            provider: PROVIDER.into(),
        });

        // Create the sandbox (POST /v1/sandboxes).
        self.create_sandbox().await.inspect_err(|err| {
            let duration_ms = elapsed_ms(start);
            self.emit(SandboxEvent::InitializeFailed {
                provider: PROVIDER.into(),
                error: err.to_string(),
                causes: err.causes(),
                duration_ms,
            });
        })?;

        // Clone the repository if requested.
        if !self.config.settings.skip_clone {
            if let Some(ref origin_url) = self.clone_origin_url {
                self.clone_repo(origin_url, self.clone_branch.as_deref())
                    .await
                    .inspect_err(|err| {
                        let duration_ms = elapsed_ms(start);
                        self.emit(SandboxEvent::InitializeFailed {
                            provider: PROVIDER.into(),
                            error: err.to_string(),
                            causes: err.causes(),
                            duration_ms,
                        });
                    })?;
                let _ = self.origin_url.set(origin_url.clone());
            }
        }

        let _ = self.initialized.set(true);
        let duration_ms = elapsed_ms(start);
        self.emit(SandboxEvent::Ready {
            provider: PROVIDER.into(),
            duration_ms,
            name: self.sandbox_id.get().cloned(),
            cpu: None,
            memory: None,
            url: None,
        });

        Ok(())
    }

    async fn cleanup(&self) -> crate::Result<()> {
        let start = Instant::now();
        self.emit(SandboxEvent::CleanupStarted {
            provider: PROVIDER.into(),
        });

        match self.delete_sandbox().await {
            Ok(()) => {
                let duration_ms = elapsed_ms(start);
                self.emit(SandboxEvent::CleanupCompleted {
                    provider: PROVIDER.into(),
                    duration_ms,
                });
                Ok(())
            }
            Err(err) => {
                self.emit(SandboxEvent::CleanupFailed {
                    provider: PROVIDER.into(),
                    error:    err.to_string(),
                    causes:   err.causes(),
                });
                Err(err)
            }
        }
    }

    // ------------------------------------------------------------------
    // Command execution
    // ------------------------------------------------------------------

    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        _cancel_token: Option<CancellationToken>,
    ) -> crate::Result<ExecResult> {
        let effective_dir =
            working_dir.map_or_else(|| WORKING_DIRECTORY.to_string(), Self::resolve_path);

        // forkd exec is argv-only; fold cwd and env into sh -lc.
        let args = Self::build_exec_argv(command, &effective_dir, env_vars);

        // Convert ms to whole seconds (ceiling), minimum 1 second.
        let timeout_secs = timeout_ms.div_ceil(1000).max(1);

        let start = Instant::now();
        let resp = self.exec_in_sandbox(args, timeout_secs).await?;

        let duration_ms = elapsed_ms(start);
        let exit_code = resp.exit_code;
        // When forkd returns no exit code the process was killed, OOM-killed, or
        // timed out by the controller.  We approximate using TimedOut; a future
        // forkd API version may surface a richer termination reason.
        let termination = match exit_code {
            Some(_) => CommandTermination::Exited,
            None => CommandTermination::TimedOut,
        };

        Ok(ExecResult {
            stdout: resp.stdout.unwrap_or_default(),
            stderr: resp.stderr.unwrap_or_default(),
            exit_code,
            termination,
            duration_ms,
        })
    }

    // exec_command_streaming: use the trait default (non-live replay).
    // spawn_stdio_process: use the trait default (returns Err).

    // ------------------------------------------------------------------
    // File system
    // ------------------------------------------------------------------

    async fn read_file_bytes(&self, path: &str) -> crate::Result<Vec<u8>> {
        let abs_path = Self::resolve_path(path);
        // base64-encode the file so we can round-trip binary safely through the
        // exec response (which is a JSON string).
        let cmd = format!("base64 -w 0 {}", shell_quote(&abs_path));
        let resp = self.exec_shell(&cmd, 60).await?;

        if resp.exit_code != Some(0) {
            return Err(crate::Error::message(format!(
                "forkd read_file_bytes: base64 failed (exit {:?}): {}",
                resp.exit_code,
                resp.stderr.unwrap_or_default()
            )));
        }

        let encoded = resp.stdout.unwrap_or_default();
        BASE64
            .decode(encoded.trim())
            .map_err(|e| crate::Error::context("forkd read_file_bytes: base64 decode failed", e))
    }

    async fn write_file(&self, path: &str, content: &str) -> crate::Result<()> {
        let abs_path = Self::resolve_path(path);
        let encoded = BASE64.encode(content.as_bytes());
        // Ensure parent directory exists.
        let parent = std::path::Path::new(&abs_path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !parent.is_empty() {
            let mkdir_cmd = format!("mkdir -p {}", shell_quote(&parent));
            let mkdir_resp = self.exec_shell(&mkdir_cmd, 10).await?;
            if mkdir_resp.exit_code != Some(0) {
                return Err(crate::Error::message(format!(
                    "forkd write_file mkdir failed: {}",
                    mkdir_resp.stderr.unwrap_or_default()
                )));
            }
        }

        let cmd = format!(
            "echo {} | base64 -d > {}",
            shell_quote(&encoded),
            shell_quote(&abs_path)
        );
        let resp = self.exec_shell(&cmd, 30).await?;
        if resp.exit_code != Some(0) {
            return Err(crate::Error::message(format!(
                "forkd write_file failed (exit {:?}): {}",
                resp.exit_code,
                resp.stderr.unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn delete_file(&self, path: &str) -> crate::Result<()> {
        let abs_path = Self::resolve_path(path);
        let cmd = format!("rm -f {}", shell_quote(&abs_path));
        let resp = self.exec_shell(&cmd, 10).await?;
        if resp.exit_code != Some(0) {
            return Err(crate::Error::message(format!(
                "forkd delete_file failed (exit {:?}): {}",
                resp.exit_code,
                resp.stderr.unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> crate::Result<bool> {
        let abs_path = Self::resolve_path(path);
        let cmd = format!("test -e {}", shell_quote(&abs_path));
        let resp = self.exec_shell(&cmd, 10).await?;
        Ok(resp.exit_code == Some(0))
    }

    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> crate::Result<Vec<DirEntry>> {
        let abs_path = Self::resolve_path(path);
        let max_depth = depth.unwrap_or(1);
        // Output format: TYPE SIZE NAME per line (TYPE is 'f' or 'd')
        let cmd = format!(
            r"find {} -maxdepth {} -printf '%y %s %P\n' 2>/dev/null | tail -n +2",
            shell_quote(&abs_path),
            max_depth
        );
        let resp = self.exec_shell(&cmd, 30).await?;
        if resp.exit_code != Some(0) && resp.exit_code != Some(1) {
            return Err(crate::Error::message(format!(
                "forkd list_directory failed (exit {:?}): {}",
                resp.exit_code,
                resp.stderr.unwrap_or_default()
            )));
        }

        let stdout = resp.stdout.unwrap_or_default();
        let entries = stdout
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let mut parts = line.splitn(3, ' ');
                let kind = parts.next()?;
                let size_str = parts.next()?;
                let name = parts.next()?;
                if name.is_empty() {
                    return None;
                }
                let is_dir = kind == "d";
                let size = if is_dir {
                    None
                } else {
                    size_str.parse::<u64>().ok()
                };
                Some(DirEntry {
                    name: name.to_string(),
                    is_dir,
                    size,
                })
            })
            .collect();

        Ok(entries)
    }

    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> crate::Result<()> {
        let bytes = self.read_file_bytes(remote_path).await?;
        fs::write(local_path, &bytes)
            .await
            .map_err(|e| crate::Error::context("forkd download_file_to_local write failed", e))
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> crate::Result<()> {
        let bytes = fs::read(local_path)
            .await
            .map_err(|e| crate::Error::context("forkd upload_file_from_local read failed", e))?;
        // Binary-safe: base64-encode raw bytes and decode inside the VM, mirroring
        // how read_file_bytes works in reverse.  String::from_utf8_lossy is
        // deliberately avoided — it silently corrupts any non-UTF-8 byte sequence
        // (e.g. compiled binaries, images, zip archives).
        let abs_path = Self::resolve_path(remote_path);
        let encoded = BASE64.encode(&bytes);

        // Ensure parent directory exists.
        let parent = std::path::Path::new(&abs_path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !parent.is_empty() {
            let mkdir_cmd = format!("mkdir -p {}", shell_quote(&parent));
            let mkdir_resp = self.exec_shell(&mkdir_cmd, 10).await?;
            if mkdir_resp.exit_code != Some(0) {
                return Err(crate::Error::message(format!(
                    "forkd upload_file_from_local mkdir failed: {}",
                    mkdir_resp.stderr.unwrap_or_default()
                )));
            }
        }

        let cmd = format!(
            "echo {} | base64 -d > {}",
            shell_quote(&encoded),
            shell_quote(&abs_path)
        );
        let resp = self.exec_shell(&cmd, 60).await?;
        if resp.exit_code != Some(0) {
            return Err(crate::Error::message(format!(
                "forkd upload_file_from_local failed (exit {:?}): {}",
                resp.exit_code,
                resp.stderr.unwrap_or_default()
            )));
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Search
    // ------------------------------------------------------------------

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> crate::Result<Vec<String>> {
        let abs_path = Self::resolve_path(path);
        let mut args = vec!["rg".to_string(), "--line-number".to_string()];

        if options.case_insensitive {
            args.push("-i".to_string());
        }
        if let Some(max) = options.max_results {
            args.push("-m".to_string());
            args.push(max.to_string());
        }
        if let Some(ref glob) = options.glob_filter {
            args.push("-g".to_string());
            args.push(shell_quote(glob));
        }
        args.push(shell_quote(pattern));
        args.push(shell_quote(&abs_path));

        let cmd = args.join(" ");
        let resp = self.exec_shell(&cmd, 60).await?;

        // Exit code 1 means no matches (not an error).
        if resp.exit_code != Some(0) && resp.exit_code != Some(1) {
            // rg not available — fall back to grep.
            let mut grep_args = vec!["grep".to_string(), "-rn".to_string()];
            if options.case_insensitive {
                grep_args.push("-i".to_string());
            }
            if let Some(max) = options.max_results {
                grep_args.push("-m".to_string());
                grep_args.push(max.to_string());
            }
            grep_args.push(shell_quote(pattern));
            grep_args.push(shell_quote(&abs_path));

            let grep_cmd = grep_args.join(" ");
            let grep_resp = self.exec_shell(&grep_cmd, 60).await?;
            let stdout = grep_resp.stdout.unwrap_or_default();
            return Ok(stdout.lines().map(str::to_string).collect());
        }

        let stdout = resp.stdout.unwrap_or_default();
        Ok(stdout.lines().map(str::to_string).collect())
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> crate::Result<Vec<String>> {
        let base = path.map_or_else(|| WORKING_DIRECTORY.to_string(), Self::resolve_path);
        // Use `-name` for patterns without a directory separator (e.g. `*.rs`,
        // `Cargo.toml`) so they match at any depth without a spurious `*/` prefix
        // that would require at least one intermediate directory and would miss
        // files directly inside `base`.
        //
        // Use `-path` only when the caller supplies a path-qualified pattern
        // (e.g. `src/*.rs`), anchoring it to `<base>/` so the search is rooted.
        let find_filter = if pattern.contains('/') {
            format!("-path {}", shell_quote(&format!("{base}/{pattern}")))
        } else {
            format!("-name {}", shell_quote(pattern))
        };
        let cmd = format!(
            "find {} {} -print 2>/dev/null",
            shell_quote(&base),
            find_filter
        );
        let resp = self.exec_shell(&cmd, 30).await?;
        let stdout = resp.stdout.unwrap_or_default();
        Ok(stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    // ------------------------------------------------------------------
    // Metadata
    // ------------------------------------------------------------------

    fn working_directory(&self) -> &str {
        WORKING_DIRECTORY
    }

    fn platform(&self) -> &'static str {
        "linux"
    }

    fn os_version(&self) -> String {
        "linux (forkd microVM)".to_string()
    }

    fn sandbox_info(&self) -> String {
        match self.sandbox_id.get() {
            Some(id) => format!("forkd:{id}"),
            None => "forkd:(uninitialized)".to_string(),
        }
    }

    fn snapshot_info(&self) -> Option<String> {
        // Prefer the server-reported active snapshot; fall back to what was requested.
        self.active_snapshot
            .get()
            .cloned()
            .or_else(|| Some(self.config.settings.snapshot_tag.clone()))
    }

    fn origin_url(&self) -> Option<&str> {
        self.origin_url.get().map(String::as_str)
    }

    // ------------------------------------------------------------------
    // Git helpers (delegate to shared exec-based implementations)
    // ------------------------------------------------------------------

    async fn setup_git(
        &self,
        intent: &crate::GitSetupIntent,
    ) -> crate::Result<Option<crate::GitRunInfo>> {
        crate::setup_git_via_exec(self, intent).await.map(Some)
    }

    fn resume_setup_commands(&self, run_branch: &str) -> Vec<String> {
        vec![format!("git checkout {}", shell_quote(run_branch))]
    }

    async fn git_push_ref(&self, refspec: &str) -> crate::Result<()> {
        crate::git_push_via_exec(self, refspec).await
    }
}
