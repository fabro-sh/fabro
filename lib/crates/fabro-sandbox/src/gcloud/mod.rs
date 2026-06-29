//! Gcloud (GCE-per-run) sandbox provider.
//!
//! One Fabro server fans out to ephemeral GCE VMs: each run gets a freshly
//! inserted VM reached over a host-key-pinned SSH session keyed by a per-run
//! ephemeral ed25519 key. The VM carries **no attached service account**;
//! egress is constrained by VPC firewall tag + host iptables. On run end the
//! VM is deleted.
//!
//! Structure mirrors the Daytona provider: this module owns the [`Sandbox`]
//! implementation ([`GcloudSandbox`]) and the lifecycle, while
//! [`crate::provider::gcloud`] owns the [`crate::SandboxProvider`]
//! (list/get/create/delete) surface. Transport, REST, auth, egress and key
//! generation live in dedicated submodules.
//!
//! # Operator setup (control-plane service account)
//!
//! The control plane needs a least-privilege custom role bound (with an
//! instance name-prefix IAM condition `resource.name.startsWith("fabro-run-")`)
//! to cap blast radius. The minimum permission set is:
//!
//! * `compute.instances.insert`
//! * `compute.instances.delete`
//! * `compute.instances.get`
//! * `compute.instances.getGuestAttributes` — **required for host-key pinning**;
//!   without it [`compute::ComputeClient::host_key_from_guest_attributes`]
//!   returns 403 and `initialize()` fails fast with an actionable error rather
//!   than hanging until the poll timeout.
//! * `compute.disks.create`
//! * `compute.subnetworks.use`
//! * `compute.zoneOperations.get`
//!
//! Created VMs carry **no attached service account**, so do **not** grant the
//! role `roles/compute.instanceAdmin.v1`. See `OPERATOR.md` in this directory
//! for the full out-of-band setup (firewall tag, VM image, env vars).

pub mod auth;
pub mod compute;
pub mod config;
pub mod egress;
pub mod keypair;
pub mod ssh;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fabro_github::GitHubCredentials;
use fabro_types::{CommandOutputStream, CommandTermination, RunId};
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, OnceCell};
use tokio_util::sync::CancellationToken;

pub use self::config::{GcloudConfig, GcloudSettings};
pub use self::egress::EgressPolicy;

use self::auth::GcpAuth;
use self::compute::ComputeClient;
use self::keypair::EphemeralKeypair;
use self::ssh::{OpensshRunner, SshConnectParams, SshOutput, SshRunner};
use crate::sandbox::shell_quote;
use crate::{
    CommandOutputCallback, DirEntry, ExecResult, ExecStreamingResult, GrepOptions, Sandbox,
    SandboxEvent, SandboxEventCallback, StderrCollector, StdioProcess, StdioProcessHandle,
    StdioProcessTermination,
};

const PROVIDER: &str = "gcloud";

/// A single-run sandbox backed by an ephemeral GCE VM.
pub struct GcloudSandbox {
    config: GcloudConfig,
    compute: Arc<ComputeClient>,
    keypair: EphemeralKeypair,
    run_id: Option<RunId>,
    clone_url: Option<String>,
    clone_branch: Option<String>,
    github_app: Option<GitHubCredentials>,
    instance_name: OnceCell<String>,
    data_ssh: OnceCell<OpensshRunner>,
    origin_url: OnceCell<String>,
    event_callback: Option<SandboxEventCallback>,
}

impl GcloudSandbox {
    /// Construct a sandbox for a single run. The ephemeral keypair is
    /// generated here and lives only in memory.
    ///
    /// `auth` is shared (an [`Arc`]) so the token cache lives with the provider
    /// and the access token is reused across operations rather than re-minted
    /// per sandbox.
    pub fn new(
        config: GcloudConfig,
        http: reqwest::Client,
        auth: Arc<GcpAuth>,
        run_id: Option<RunId>,
        clone_url: Option<String>,
        clone_branch: Option<String>,
        github_app: Option<GitHubCredentials>,
    ) -> crate::Result<Self> {
        let compute = Arc::new(ComputeClient::new(http, auth));
        let keypair = EphemeralKeypair::generate()?;
        Ok(Self {
            config,
            compute,
            keypair,
            run_id,
            clone_url,
            clone_branch,
            github_app,
            instance_name: OnceCell::new(),
            data_ssh: OnceCell::new(),
            origin_url: OnceCell::new(),
            event_callback: None,
        })
    }

    pub fn set_event_callback(&mut self, callback: SandboxEventCallback) {
        self.event_callback = Some(callback);
    }

    /// The provisioned instance name, available after `initialize`.
    #[must_use]
    pub fn instance_name(&self) -> Option<&str> {
        self.instance_name.get().map(String::as_str)
    }

    fn emit(&self, event: SandboxEvent) {
        event.trace();
        if let Some(callback) = &self.event_callback {
            callback(event);
        }
    }

    fn data_ssh(&self) -> crate::Result<&OpensshRunner> {
        self.data_ssh.get().ok_or_else(|| {
            crate::Error::message("gcloud sandbox not initialized — call initialize() first")
        })
    }

    fn resolve(&self, path: &str) -> String {
        if Path::new(path).is_absolute() {
            path.to_string()
        } else {
            format!("{}/{path}", self.config.working_dir)
        }
    }

    fn new_instance_name(&self) -> String {
        let suffix: String = uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(12)
            .collect();
        format!("{}{suffix}", self.config.name_prefix)
    }

    /// Render the VM startup script: (unless the image owns egress) apply the
    /// egress iptables policy.
    ///
    /// Host keys are NOT published here. The GCE guest agent already publishes
    /// each VM's SSH host keys to the `hostkeys/<algo>` guest attributes (bare
    /// base64 blob, the canonical format `gcloud compute ssh` consumes) when
    /// `enable-guest-attributes=TRUE`. Having the provider ALSO PUT to the same
    /// `hostkeys/<algo>` keys made two publishers race last-writer-wins with
    /// different value formats (full `.pub` line vs bare blob), so the control
    /// plane sometimes pinned a value that didn't match the served key →
    /// `StrictHostKeyChecking` failed. `poll_host_key`/`parse_host_key` read the
    /// guest agent's bare-blob entries directly.
    ///
    /// When `skip_egress_guard` is set the provider renders no iptables: the VM
    /// image is the authoritative fail-closed egress guard (e.g. the narayan
    /// golden image's nftables + dnsmasq + squid). Rendering the provider's
    /// blanket `169.254.169.254` DROP here would sever that image's dnsmasq DNS
    /// floor (`169.254.169.254:53`), breaking host resolution for the squid
    /// allowlist and killing all run egress.
    fn startup_script(&self) -> String {
        let mut script = "#!/usr/bin/env bash\n\
             set -euo pipefail\n"
            .to_string();

        if self.config.skip_egress_guard {
            script.push_str(
                "# Egress is image-managed (FABRO_GCLOUD_SKIP_EGRESS_GUARD); provider renders no iptables.\n",
            );
        } else {
            let egress_b64 = STANDARD.encode(self.config.egress.iptables_script().as_bytes());
            let _ = write!(
                script,
                "# Apply egress policy (defence in depth alongside the VPC firewall tag).\n\
                 echo {egress_b64} | base64 -d | bash || true\n"
            );
        }

        script
    }

    async fn poll_host_key(&self, name: &str) -> crate::Result<String> {
        let deadline = Instant::now() + self.config.host_key_poll_timeout;
        loop {
            if let Some(line) = self
                .compute
                .host_key_from_guest_attributes(&self.config, name)
                .await?
            {
                return Ok(line);
            }
            if Instant::now() >= deadline {
                return Err(crate::Error::message(
                    "gcloud sandbox: VM host key did not appear in guest attributes within timeout",
                ));
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    /// Connect over SSH, retrying until the VM accepts the per-run key or the
    /// deadline elapses. The host key is published to guest attributes (which
    /// `poll_host_key` waits for) BEFORE the guest agent writes the injected
    /// public key to `~/.ssh/authorized_keys`, so the first connect after the
    /// host key appears often hits `Permission denied (publickey)`. Retrying
    /// absorbs that window instead of failing the whole run on a transient
    /// not-yet-provisioned auth state. Each attempt keeps the per-attempt
    /// `ssh_connect_timeout` for the TCP connect itself.
    async fn connect_with_retry(
        &self,
        ip: &str,
        host_key_line: String,
    ) -> crate::Result<OpensshRunner> {
        let deadline = Instant::now() + self.config.host_key_poll_timeout;
        loop {
            let attempt = OpensshRunner::connect(&SshConnectParams {
                host: ip.to_string(),
                user: self.config.ssh_user.clone(),
                private_key: self.keypair.private_openssh().to_string(),
                host_key_line: host_key_line.clone(),
                connect_timeout: self.config.ssh_connect_timeout,
            })
            .await;
            match attempt {
                Ok(runner) => return Ok(runner),
                Err(err) if Instant::now() < deadline => {
                    tracing::debug!(%ip, error = %err, "SSH connect not ready yet; retrying");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    async fn clone_repo(&self, ssh: &OpensshRunner) -> crate::Result<()> {
        let Some(url) = self.clone_url.clone() else {
            return Ok(());
        };
        self.emit(SandboxEvent::GitCloneStarted {
            url: url.clone(),
            branch: self.clone_branch.clone(),
        });
        let started = Instant::now();

        // Resolve an authenticated HTTPS clone URL from the GitHub App (same path
        // the docker provider uses). The in-VM clone must go over HTTPS: the
        // sandbox egress guard permits 443 via squid but blocks raw SSH (github
        // port 22), and the golden image ships no git credential (the build-time
        // token is scrubbed). Falls back to the bare origin only when no App is
        // configured (public repos / non-github remotes).
        let auth_url = match &self.github_app {
            Some(creds) => {
                // `resolve_authenticated_url` needs an `https://github.com/...`
                // URL; the origin is the SSH form (`git@github.com:...`), so
                // normalize it first (same helper the layout code uses).
                let https_url = fabro_github::normalize_repo_origin_url(&url);
                Some(
                    fabro_github::resolve_authenticated_url(
                        &fabro_github::GitHubContext::new(
                            creds,
                            &fabro_github::github_api_base_url(),
                        ),
                        &https_url,
                    )
                    .await
                    .map_err(|e| {
                        crate::Error::message(format!(
                            "Failed to get GitHub App credentials for clone: {e}"
                        ))
                    })?,
                )
            }
            None => None,
        };
        let clone_target = auth_url
            .as_ref()
            .map_or(url.as_str(), |u| u.as_raw_url().as_str());

        let branch_flag = self
            .clone_branch
            .as_deref()
            .map(|b| format!(" --branch {}", shell_quote(b)))
            .unwrap_or_default();
        let script = format!(
            "git clone{branch_flag} {} {}",
            shell_quote(clone_target),
            shell_quote(&self.config.working_dir)
        );
        let output = ssh
            .run_command_with_timeout(&wrap_bash(&script), Duration::from_secs(300))
            .await?;
        if output.exit_code != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Strip the embedded token from any surfaced error.
            let stderr = match &auth_url {
                Some(u) => stderr.replace(u.as_raw_url().as_str(), &u.redacted_string()),
                None => stderr.into_owned(),
            };
            let err = format!("git clone failed (exit {}): {}", output.exit_code, stderr);
            self.emit(SandboxEvent::GitCloneFailed {
                url: url.clone(),
                error: err.clone(),
                causes: Vec::new(),
            });
            return Err(crate::Error::message(err));
        }

        let _ = self.origin_url.set(url.clone());
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::GitCloneCompleted { url, duration_ms });
        Ok(())
    }

    /// Best-effort VM teardown shared by `cleanup` and the
    /// initialize-failure compensation path.
    async fn delete_vm(&self, name: &str) -> crate::Result<()> {
        let operation = self.compute.delete_instance(&self.config, name).await?;
        self.compute
            .await_zonal_operation(&self.config, &operation)
            .await
    }

    fn build_exec_script(
        &self,
        command: &str,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
    ) -> String {
        let mut script = String::new();
        if let Some(vars) = env_vars {
            for (key, value) in vars {
                let _ = writeln!(script, "export {}={}", shell_quote(key), shell_quote(value));
            }
        }
        let dir = working_dir.map_or_else(|| self.config.working_dir.clone(), |d| self.resolve(d));
        let _ = write!(script, "cd {} && {command}", shell_quote(&dir));
        wrap_bash(&script)
    }
}

fn wrap_bash(command: &str) -> String {
    let encoded = STANDARD.encode(command.as_bytes());
    format!("echo {} | base64 -d | bash", shell_quote(&encoded))
}

fn ssh_to_exec(output: SshOutput, duration_ms: u64) -> ExecResult {
    ExecResult {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: Some(output.exit_code),
        termination: CommandTermination::Exited,
        duration_ms,
    }
}

#[async_trait]
impl Sandbox for GcloudSandbox {
    async fn initialize(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::Initializing {
            provider: PROVIDER.into(),
        });
        let started = Instant::now();

        let name = self.new_instance_name();
        let ssh_keys = self.keypair.ssh_keys_metadata(&self.config.ssh_user);
        let startup = self.startup_script();
        let run_id = self.run_id.as_ref().map(ToString::to_string);

        let result = async {
            let operation = self
                .compute
                .insert_instance(&self.config, &name, &ssh_keys, &startup, run_id.as_deref())
                .await?;
            self.compute
                .await_zonal_operation(&self.config, &operation)
                .await?;

            let instance = self.compute.get_instance(&self.config, &name).await?;
            let ip = instance
                .internal_ip()
                .ok_or_else(|| crate::Error::message("gcloud VM has no internal IP"))?
                .to_string();

            let host_key_line = self.poll_host_key(&name).await?;

            let runner = self.connect_with_retry(&ip, host_key_line).await?;

            self.clone_repo(&runner).await?;
            Ok::<OpensshRunner, crate::Error>(runner)
        }
        .await;

        match result {
            Ok(runner) => {
                let _ = self.instance_name.set(name.clone());
                let _ = self.data_ssh.set(runner);
                let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.emit(SandboxEvent::Ready {
                    provider: PROVIDER.into(),
                    duration_ms,
                    name: Some(name),
                    cpu: None,
                    memory: None,
                    url: None,
                });
                Ok(())
            }
            Err(err) => {
                // Compensate: tear down the half-provisioned VM. Best-effort —
                // a failed delete is logged via the orphan path, not fatal.
                if let Err(cleanup_err) = self.delete_vm(&name).await {
                    tracing::error!(
                        instance = %name,
                        zone = %self.config.zone,
                        error = %cleanup_err,
                        "gcloud sandbox left an orphaned GCE instance after failed init; reconcile manually"
                    );
                }
                let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.emit(SandboxEvent::InitializeFailed {
                    provider: PROVIDER.into(),
                    error: err.to_string(),
                    causes: Vec::new(),
                    duration_ms,
                });
                Err(err)
            }
        }
    }

    async fn cleanup(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::CleanupStarted {
            provider: PROVIDER.into(),
        });
        let started = Instant::now();
        if let Some(name) = self.instance_name.get() {
            if let Err(err) = self.delete_vm(name).await {
                self.emit(SandboxEvent::CleanupFailed {
                    provider: PROVIDER.into(),
                    error: err.to_string(),
                    causes: Vec::new(),
                });
                return Err(err);
            }
        }
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::CleanupCompleted {
            provider: PROVIDER.into(),
            duration_ms,
        });
        Ok(())
    }

    async fn read_file_bytes(&self, path: &str) -> crate::Result<Vec<u8>> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve(path);
        let output = ssh
            .run_command(&format!("cat {}", shell_quote(&resolved)))
            .await?;
        if output.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "Failed to read {resolved}: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(output.stdout)
    }

    async fn write_file(&self, path: &str, content: &str) -> crate::Result<()> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve(path);
        if let Some(parent) = Path::new(&resolved).parent() {
            let parent = parent.to_string_lossy();
            if parent != "/" && !parent.is_empty() {
                ssh.run_command(&format!("mkdir -p {}", shell_quote(&parent)))
                    .await?;
            }
        }
        ssh.upload_file(&resolved, content.as_bytes()).await
    }

    async fn delete_file(&self, path: &str) -> crate::Result<()> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve(path);
        let output = ssh
            .run_command(&format!("rm -f {}", shell_quote(&resolved)))
            .await?;
        if output.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "Failed to delete {resolved}: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> crate::Result<bool> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve(path);
        let output = ssh
            .run_command(&format!("test -e {}", shell_quote(&resolved)))
            .await?;
        Ok(output.exit_code == 0)
    }

    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> crate::Result<Vec<DirEntry>> {
        let resolved = self.resolve(path);
        let max_depth = depth.unwrap_or(1);
        let cmd = format!(
            "find {} -mindepth 1 -maxdepth {} -printf '%y\\t%s\\t%P\\n'",
            shell_quote(&resolved),
            max_depth
        );
        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;
        if result.exit_code != Some(0) {
            return Err(crate::Error::message(format!(
                "Failed to list directory {resolved}: {}",
                result.stderr
            )));
        }
        let mut entries: Vec<DirEntry> = result
            .stdout
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let mut parts = line.splitn(3, '\t');
                let file_type = parts.next()?;
                let size = parts.next()?;
                let name = parts.next()?.to_string();
                let is_dir = file_type == "d";
                Some(DirEntry {
                    name,
                    is_dir,
                    size: if is_dir { None } else { size.parse().ok() },
                })
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> crate::Result<ExecResult> {
        let ssh = self.data_ssh()?;
        let full = self.build_exec_script(command, working_dir, env_vars);
        let started = Instant::now();
        let token = cancel_token.unwrap_or_default();
        let timeout = Duration::from_millis(timeout_ms);
        let outcome = tokio::select! {
            res = ssh.run_command_with_timeout(&full, timeout) => res,
            () = token.cancelled() => {
                let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command cancelled".to_string(),
                    exit_code: None,
                    termination: CommandTermination::Cancelled,
                    duration_ms,
                });
            }
        };
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match outcome {
            Ok(output) => Ok(ssh_to_exec(output, duration_ms)),
            Err(err) if err.to_string().contains("timed out") => Ok(ExecResult {
                stdout: String::new(),
                stderr: "Command timed out".to_string(),
                exit_code: None,
                termination: CommandTermination::TimedOut,
                duration_ms,
            }),
            Err(err) => Err(err),
        }
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
        // A genuine streaming impl: spawn the command with piped stdout and
        // forward chunks live, collecting them for the final ExecResult.
        // `spawn_stdio_process` applies the working-dir + env wrapping itself, so
        // pass the raw command through rather than wrapping it twice.
        let mut process = self
            .spawn_stdio_process(command, working_dir, env_vars, cancel_token)
            .await?;
        let started = Instant::now();

        let mut collected = Vec::new();
        let read_loop = async {
            let mut buf = [0u8; 8 * 1024];
            loop {
                let n = process.stdout.read(&mut buf).await.map_err(|err| {
                    crate::Error::context("gcloud streaming stdout read failed", err)
                })?;
                if n == 0 {
                    break;
                }
                collected.extend_from_slice(&buf[..n]);
                output_callback(CommandOutputStream::Stdout, buf[..n].to_vec()).await?;
            }
            Ok::<(), crate::Error>(())
        };

        let timed_out = match timeout_ms {
            Some(ms) => match tokio::time::timeout(Duration::from_millis(ms), read_loop).await {
                Ok(result) => {
                    result?;
                    false
                }
                Err(_) => true,
            },
            None => {
                read_loop.await?;
                false
            }
        };

        if timed_out {
            process.handle.terminate().await?;
        }
        let termination = process.handle.wait().await?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let stderr = process.stderr.tail_string().await;
        if !stderr.is_empty() {
            output_callback(CommandOutputStream::Stderr, stderr.clone().into_bytes()).await?;
        }

        Ok(ExecStreamingResult {
            result: ExecResult {
                stdout: String::from_utf8_lossy(&collected).into_owned(),
                stderr,
                exit_code: termination.exit_code,
                termination: if timed_out {
                    CommandTermination::TimedOut
                } else {
                    termination.termination
                },
                duration_ms,
            },
            streams_separated: true,
            live_streaming: true,
        })
    }

    async fn spawn_stdio_process(
        &self,
        command: &str,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> crate::Result<StdioProcess> {
        let ssh = self.data_ssh()?;
        let session = ssh.session();
        let script = self.build_exec_script(command, working_dir, env_vars);

        let mut child = session
            .arc_command("/usr/bin/env")
            .arg("bash")
            .arg("-c")
            .arg(script)
            .stdin(openssh::Stdio::piped())
            .stdout(openssh::Stdio::piped())
            .stderr(openssh::Stdio::piped())
            .spawn()
            .await
            .map_err(|err| crate::Error::context("Failed to spawn gcloud stdio process", err))?;

        let stdin = child
            .stdin()
            .take()
            .ok_or_else(|| crate::Error::message("gcloud stdio process had no stdin"))?;
        let stdout = child
            .stdout()
            .take()
            .ok_or_else(|| crate::Error::message("gcloud stdio process had no stdout"))?;
        let child_stderr = child
            .stderr()
            .take()
            .ok_or_else(|| crate::Error::message("gcloud stdio process had no stderr"))?;

        let stderr_collector = StderrCollector::new(crate::DEFAULT_EXEC_OUTPUT_TAIL_BYTES);
        stderr_collector.spawn_reader(child_stderr);

        let control = Arc::new(GcloudStdioControl {
            child: Mutex::new(Some(child)),
        });

        if let Some(token) = cancel_token {
            let control_for_cancel = Arc::clone(&control);
            tokio::spawn(async move {
                token.cancelled().await;
                let _ = control_for_cancel.terminate_inner().await;
            });
        }

        Ok(StdioProcess {
            stdin: Box::pin(stdin),
            stdout: Box::pin(stdout),
            stderr: stderr_collector,
            handle: StdioProcessHandle::new(GcloudStdioHandle { control }),
        })
    }

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> crate::Result<Vec<String>> {
        let resolved = self.resolve(path);
        let mut cmd = "grep -rn".to_string();
        if options.case_insensitive {
            cmd.push_str(" -i");
        }
        if let Some(glob) = &options.glob_filter {
            let _ = write!(cmd, " --include {}", shell_quote(glob));
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
        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;
        if result.exit_code == Some(1) {
            return Ok(Vec::new());
        }
        if result.exit_code != Some(0) {
            return Err(crate::Error::message(format!(
                "grep failed (exit {}): {}",
                result.display_exit_code(),
                result.stderr
            )));
        }
        Ok(result.stdout.lines().map(String::from).collect())
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> crate::Result<Vec<String>> {
        let base = path.map_or_else(|| self.config.working_dir.clone(), |p| self.resolve(p));
        let cmd = format!(
            "find {} -name {} -type f | sort",
            shell_quote(&base),
            shell_quote(pattern)
        );
        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;
        if result.exit_code != Some(0) {
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

    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> crate::Result<()> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve(remote_path);
        let bytes = ssh.download_file(&resolved).await?;
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| crate::Error::context("Failed to create parent dirs", err))?;
        }
        tokio::fs::write(local_path, &bytes)
            .await
            .map_err(|err| crate::Error::context("Failed to write downloaded file", err))
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> crate::Result<()> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve(remote_path);
        let bytes = tokio::fs::read(local_path)
            .await
            .map_err(|err| crate::Error::context("Failed to read local file", err))?;
        ssh.upload_file(&resolved, &bytes).await
    }

    fn working_directory(&self) -> &str {
        &self.config.working_dir
    }

    fn platform(&self) -> &str {
        "linux"
    }

    fn os_version(&self) -> String {
        "Linux (GCE)".to_string()
    }

    fn sandbox_info(&self) -> String {
        match (self.instance_name.get(), &self.run_id) {
            (Some(name), Some(id)) => format!("{name} (run {id})"),
            (Some(name), None) => name.clone(),
            _ => String::new(),
        }
    }

    fn origin_url(&self) -> Option<&str> {
        self.origin_url.get().map(String::as_str)
    }

    async fn setup_git(
        &self,
        intent: &crate::GitSetupIntent,
    ) -> crate::Result<Option<crate::GitRunInfo>> {
        crate::setup_git_via_exec(self, intent).await.map(Some)
    }

    async fn git_push_ref(&self, refspec: &str) -> crate::Result<()> {
        // Reuse the shared push ritual (credential refresh → push → log) rather
        // than hand-rolling `git push origin`, so the gcloud path can't silently
        // skip `refresh_push_credentials` the way docker/daytona don't.
        crate::git_push_via_exec(self, refspec).await
    }

    fn resume_setup_commands(&self, run_branch: &str) -> Vec<String> {
        vec![format!(
            "git fetch origin {run_branch} && git checkout {run_branch}"
        )]
    }
}

/// `StdioProcessControl` over an owned openssh child.
struct GcloudStdioControl {
    child: Mutex<Option<openssh::Child<Arc<openssh::Session>>>>,
}

impl GcloudStdioControl {
    async fn terminate_inner(&self) -> crate::Result<()> {
        if let Some(child) = self.child.lock().await.take() {
            child.disconnect().await.map_err(|err| {
                crate::Error::context("Failed to terminate gcloud stdio process", err)
            })?;
        }
        Ok(())
    }
}

struct GcloudStdioHandle {
    control: Arc<GcloudStdioControl>,
}

#[async_trait]
impl crate::sandbox::StdioProcessControl for GcloudStdioHandle {
    async fn terminate(&self) -> crate::Result<()> {
        self.control.terminate_inner().await
    }

    async fn wait(&self) -> crate::Result<StdioProcessTermination> {
        let child = self.control.child.lock().await.take();
        match child {
            Some(child) => {
                let status = child.wait().await.map_err(|err| {
                    crate::Error::context("gcloud stdio process wait failed", err)
                })?;
                Ok(StdioProcessTermination::exited(status.code()))
            }
            None => Ok(StdioProcessTermination::cancelled()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcloud::config::GcloudSettings;

    fn sandbox() -> GcloudSandbox {
        sandbox_with_skip_egress(false)
    }

    fn sandbox_with_skip_egress(skip: bool) -> GcloudSandbox {
        let settings = GcloudSettings {
            project: Some("proj".to_string()),
            zone: Some("us-central1-a".to_string()),
            subnetwork: Some("default".to_string()),
            vm_image: Some("img".to_string()),
            machine_type: Some("e2-standard-4".to_string()),
            skip_egress_guard: skip.then(|| "true".to_string()),
            ..Default::default()
        };
        let config = GcloudConfig::resolve(&settings, EgressPolicy::Block).unwrap();
        let auth = Arc::new(GcpAuth::new(reqwest::Client::new(), None));
        GcloudSandbox::new(config, reqwest::Client::new(), auth, None, None, None, None).unwrap()
    }

    #[test]
    fn instance_names_carry_prefix_and_are_unique() {
        let sb = sandbox();
        let a = sb.new_instance_name();
        let b = sb.new_instance_name();
        assert!(a.starts_with("fabro-run-"));
        assert_ne!(a, b);
    }

    #[test]
    fn startup_script_renders_egress_and_does_not_publish_host_keys() {
        let sb = sandbox();
        let script = sb.startup_script();
        // Host keys are published by the GCE guest agent, NOT here — a second
        // publisher raced the agent and broke strict host-key pinning.
        assert!(!script.contains("guest-attributes/hostkeys/"));
        // Default path renders the provider's egress fragment.
        assert!(script.contains("base64 -d | bash"));
        assert!(script.contains("Apply egress policy"));
    }

    #[test]
    fn startup_script_skips_egress_when_image_managed() {
        let sb = sandbox_with_skip_egress(true);
        let script = sb.startup_script();
        // No host-key publish (the guest agent owns it).
        assert!(!script.contains("guest-attributes/hostkeys/"));
        // And the provider renders NO iptables fragment: no metadata-IP DROP that
        // would sever the image's dnsmasq DNS floor.
        assert!(!script.contains("169.254.169.254 -j DROP"));
        assert!(!script.contains("Apply egress policy"));
        assert!(script.contains("image-managed"));
    }

    #[test]
    fn exec_script_sets_workdir_and_env() {
        let sb = sandbox();
        let env = HashMap::from([("FOO".to_string(), "bar".to_string())]);
        let script = sb.build_exec_script("echo hi", None, Some(&env));
        // wrap_bash → `echo <quoted-b64> | base64 -d | bash`; strip the shell
        // quoting before decoding.
        let token = script.split_whitespace().nth(1).unwrap();
        let token = token.trim_matches('\'');
        let decoded = STANDARD.decode(token).unwrap();
        let decoded = String::from_utf8(decoded).unwrap();
        assert!(decoded.contains("export FOO="));
        assert!(decoded.contains("bar"));
        assert!(decoded.contains("/home/fabro/workspace"));
        assert!(decoded.contains("echo hi"));
    }

    #[tokio::test]
    async fn operations_error_before_initialize() {
        let sb = sandbox();
        assert!(sb.read_file_bytes("x").await.is_err());
        assert!(sb.sandbox_info().is_empty());
    }
}
