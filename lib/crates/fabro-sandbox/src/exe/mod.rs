mod openssh_runner;

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use crate::shell_quote;
use crate::ssh_common;
use crate::{
    format_lines_numbered, DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent,
    SandboxEventCallback,
};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

pub use crate::ssh_common::{GitCloneParams, SshOutput, SshRunner};
pub use openssh_runner::OpensshRunner;

pub use fabro_config::sandbox::ExeSettings as ExeConfig;

const WORKING_DIRECTORY: &str = "/home/exedev";
const PROVIDER: &str = "exe";

/// No-op SSH runner used as a placeholder for the management plane
/// when reconnecting to an existing VM via `from_existing`.
struct NoopSshRunner;

#[async_trait]
impl SshRunner for NoopSshRunner {
    async fn run_command(&self, _command: &str) -> Result<SshOutput, String> {
        Err("NoopSshRunner: management plane not available on reconnected sandbox".to_string())
    }
    async fn run_command_with_timeout(
        &self,
        _command: &str,
        _timeout: std::time::Duration,
    ) -> Result<SshOutput, String> {
        Err("NoopSshRunner: management plane not available on reconnected sandbox".to_string())
    }
    async fn upload_file(&self, _path: &str, _content: &[u8]) -> Result<(), String> {
        Err("NoopSshRunner: management plane not available on reconnected sandbox".to_string())
    }
    async fn download_file(&self, _path: &str) -> Result<Vec<u8>, String> {
        Err("NoopSshRunner: management plane not available on reconnected sandbox".to_string())
    }
}

/// Factory function type for creating data-plane SSH runners.
type DataSshFactory = Box<
    dyn Fn(
            &str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Box<dyn SshRunner>, String>> + Send>,
        > + Send
        + Sync,
>;

/// Sandbox that runs all operations inside an exe.dev VM via SSH.
///
/// Uses two SSH connections:
/// - Management plane (`ssh exe.dev`) for VM lifecycle (create/destroy)
/// - Data plane (`ssh <vmname>.exe.xyz`) for command execution and file I/O
pub struct ExeSandbox {
    mgmt_ssh: Box<dyn SshRunner>,
    data_ssh: tokio::sync::OnceCell<Box<dyn SshRunner>>,
    vm_name: tokio::sync::OnceCell<String>,
    data_host: tokio::sync::OnceCell<String>,
    rg_available: tokio::sync::OnceCell<bool>,
    event_callback: Option<SandboxEventCallback>,
    /// Factory for creating data-plane SSH runners, used during initialize().
    /// In production, this connects to the VM host via OpensshRunner.
    /// In tests, this is replaced with a closure that returns a MockSshRunner.
    data_ssh_factory: DataSshFactory,
    config: ExeConfig,
    clone_params: Option<GitCloneParams>,
    run_id: Option<String>,
    origin_url: tokio::sync::OnceCell<String>,
    github_app: Option<fabro_github::GitHubAppCredentials>,
}

impl ExeSandbox {
    /// Creates a new `ExeSandbox` with a management-plane SSH runner.
    pub fn new(
        mgmt_ssh: Box<dyn SshRunner>,
        config: ExeConfig,
        clone_params: Option<GitCloneParams>,
        run_id: Option<String>,
        github_app: Option<fabro_github::GitHubAppCredentials>,
    ) -> Self {
        Self {
            mgmt_ssh,
            data_ssh: tokio::sync::OnceCell::new(),
            vm_name: tokio::sync::OnceCell::new(),
            data_host: tokio::sync::OnceCell::new(),
            rg_available: tokio::sync::OnceCell::const_new(),
            event_callback: None,
            data_ssh_factory: Box::new(|host: &str| {
                let host = host.to_string();
                Box::pin(async move {
                    OpensshRunner::connect(&host)
                        .await
                        .map(|r| Box::new(r) as Box<dyn SshRunner>)
                })
            }),
            config,
            clone_params,
            run_id,
            origin_url: tokio::sync::OnceCell::new(),
            github_app,
        }
    }

    /// Create an `ExeSandbox` from a pre-connected data-plane SSH runner.
    /// Used for reconnection (e.g. `fabro cp`) when the VM already exists.
    pub fn from_existing(data_ssh: Box<dyn SshRunner>) -> Self {
        let data_cell = tokio::sync::OnceCell::new();
        let _ = data_cell.set(data_ssh);
        Self {
            mgmt_ssh: Box::new(NoopSshRunner),
            data_ssh: data_cell,
            vm_name: tokio::sync::OnceCell::new(),
            data_host: tokio::sync::OnceCell::new(),
            rg_available: tokio::sync::OnceCell::const_new(),
            event_callback: None,
            data_ssh_factory: Box::new(|_: &str| {
                Box::pin(async {
                    Err("from_existing sandbox cannot create new SSH connections".to_string())
                })
            }),
            config: ExeConfig::default(),
            clone_params: None,
            run_id: None,
            origin_url: tokio::sync::OnceCell::new(),
            github_app: None,
        }
    }

    /// The display URL of the cloned origin remote, if a clone was performed.
    pub fn origin_url(&self) -> Option<&str> {
        self.origin_url.get().map(String::as_str)
    }

    /// The VM name, available after initialization.
    pub fn vm_name(&self) -> Option<&str> {
        self.vm_name.get().map(String::as_str)
    }

    /// The data-plane SSH host, available after initialization.
    pub fn data_host(&self) -> Option<&str> {
        self.data_host.get().map(String::as_str)
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

    /// Get the data-plane SSH runner, returning an error if not yet initialized.
    fn data_ssh(&self) -> Result<&dyn SshRunner, String> {
        self.data_ssh
            .get()
            .map(|b| b.as_ref())
            .ok_or_else(|| "Exe sandbox not initialized — call initialize() first".to_string())
    }

    /// Return the SSH command to connect to this VM's data host.
    pub fn ssh_command(&self) -> Result<String, String> {
        let host = self.data_host.get().ok_or("Exe sandbox not initialized")?;
        Ok(format!("ssh {host}"))
    }

    /// Clone a git repo into the sandbox working directory.
    async fn clone_repo(&self, params: &GitCloneParams) -> Result<(), String> {
        let ssh = self.data_ssh()?;
        ssh_common::clone_repo(
            ssh,
            WORKING_DIRECTORY,
            params,
            self.github_app.as_ref(),
            &self.origin_url,
            &|event| self.emit(event),
        )
        .await
    }

    fn resolve_path(&self, path: &str) -> String {
        crate::sandbox::resolve_path(path, WORKING_DIRECTORY)
    }
}

#[async_trait]
impl Sandbox for ExeSandbox {
    async fn initialize(&self) -> Result<(), String> {
        self.emit(SandboxEvent::Initializing {
            provider: PROVIDER.into(),
        });
        let init_start = Instant::now();

        // Create a new VM via the management plane
        let mut cmd = "new --json".to_string();
        if let Some(ref image) = self.config.image {
            cmd.push_str(&format!(" --image {}", shell_quote(image)));
        }
        let output = self.mgmt_ssh.run_command(&cmd).await.map_err(|e| {
            let err = format!("Failed to create exe.dev VM: {e}");
            let duration_ms = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::InitializeFailed {
                provider: PROVIDER.into(),
                error: err.clone(),
                duration_ms,
            });
            err
        })?;

        if output.exit_code != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let err = format!(
                "exe.dev VM creation failed (exit {}): {stderr}",
                output.exit_code
            );
            let duration_ms = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::InitializeFailed {
                provider: PROVIDER.into(),
                error: err.clone(),
                duration_ms,
            });
            return Err(err);
        }

        // Parse JSON response to get VM name and host
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
            let err = format!("Failed to parse exe.dev response: {e}");
            let duration_ms = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::InitializeFailed {
                provider: PROVIDER.into(),
                error: err.clone(),
                duration_ms,
            });
            err
        })?;

        let vm_name = json["vm_name"]
            .as_str()
            .ok_or_else(|| "Missing 'vm_name' in exe.dev response".to_string())?
            .to_string();
        let data_host = json["ssh_dest"]
            .as_str()
            .ok_or_else(|| "Missing 'ssh_dest' in exe.dev response".to_string())?
            .to_string();

        self.vm_name
            .set(vm_name.clone())
            .map_err(|_| "Exe sandbox already initialized".to_string())?;
        self.data_host
            .set(data_host.clone())
            .map_err(|_| "Exe sandbox already initialized".to_string())?;

        // Create data-plane SSH connection
        let runner = (self.data_ssh_factory)(&data_host).await.map_err(|e| {
            let err = format!("Failed to connect to exe.dev VM {data_host}: {e}");
            let duration_ms = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::InitializeFailed {
                provider: PROVIDER.into(),
                error: err.clone(),
                duration_ms,
            });
            err
        })?;
        self.data_ssh
            .set(runner)
            .map_err(|_| "Exe sandbox data SSH already set".to_string())?;

        // Clone git repo if clone params were provided
        if let Some(ref params) = self.clone_params {
            self.clone_repo(params).await?;
        }

        let init_duration = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::Ready {
            provider: PROVIDER.into(),
            duration_ms: init_duration,
            name: Some(vm_name),
            cpu: None,
            memory: None,
            url: None,
        });

        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        self.emit(SandboxEvent::CleanupStarted {
            provider: PROVIDER.into(),
        });
        let start = Instant::now();

        if let Some(vm_name) = self.vm_name.get() {
            let cmd = format!("rm {}", shell_quote(vm_name));
            if let Err(e) = self.mgmt_ssh.run_command(&cmd).await {
                let err = format!("Failed to destroy exe.dev VM: {e}");
                self.emit(SandboxEvent::CleanupFailed {
                    provider: PROVIDER.into(),
                    error: err.clone(),
                });
                return Err(err);
            }
        }

        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::CleanupCompleted {
            provider: PROVIDER.into(),
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
        let ssh = self.data_ssh()?;
        let start = Instant::now();

        // Build inner script as plain text, then base64-wrap for safe transport
        let mut script = String::new();

        if let Some(vars) = env_vars {
            for (key, value) in vars {
                script.push_str(&format!(
                    "export {}={}\n",
                    shell_quote(key),
                    shell_quote(value)
                ));
            }
        }

        let dir = match working_dir {
            Some(dir) => self.resolve_path(dir),
            None => WORKING_DIRECTORY.to_string(),
        };
        script.push_str(&format!("cd {} && {command}", shell_quote(&dir)));

        let full_cmd = ssh_common::wrap_bash_command(&script);

        let timeout = std::time::Duration::from_millis(timeout_ms);
        let token = cancel_token.unwrap_or_default();
        let output = tokio::select! {
            res = ssh.run_command_with_timeout(&full_cmd, timeout) => res,
            () = token.cancelled() => {
                let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "Command cancelled".to_string(),
                    exit_code: -1,
                    timed_out: true,
                    duration_ms,
                });
            }
        };

        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        match output {
            Ok(out) => Ok(ExecResult {
                stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                exit_code: out.exit_code,
                timed_out: false,
                duration_ms,
            }),
            Err(e) if e.contains("timed out") => Ok(ExecResult {
                stdout: String::new(),
                stderr: "Command timed out".to_string(),
                exit_code: -1,
                timed_out: true,
                duration_ms,
            }),
            Err(e) => Err(e),
        }
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve_path(path);

        let output = ssh
            .run_command(&format!("cat {}", shell_quote(&resolved)))
            .await?;

        if output.exit_code != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Failed to read {resolved}: {stderr}"));
        }

        let content = String::from_utf8(output.stdout)
            .map_err(|e| format!("File is not valid UTF-8: {e}"))?;

        Ok(format_lines_numbered(&content, offset, limit))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve_path(path);

        // Ensure parent directory exists
        if let Some(parent) = Path::new(&resolved).parent() {
            let parent_str = parent.to_string_lossy();
            if parent_str != "/" {
                ssh.run_command(&format!("mkdir -p {}", shell_quote(&parent_str)))
                    .await?;
            }
        }

        ssh.upload_file(&resolved, content.as_bytes()).await
    }

    async fn delete_file(&self, path: &str) -> Result<(), String> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve_path(path);

        let output = ssh
            .run_command(&format!("rm -f {}", shell_quote(&resolved)))
            .await?;

        if output.exit_code != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Failed to delete {resolved}: {stderr}"));
        }
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve_path(path);

        let output = ssh
            .run_command(&format!("test -e {}", shell_quote(&resolved)))
            .await?;

        Ok(output.exit_code == 0)
    }

    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        let resolved = self.resolve_path(path);
        let max_depth = depth.unwrap_or(1);

        let cmd = format!(
            "find {} -mindepth 1 -maxdepth {} -printf '%y\\t%s\\t%P\\n'",
            shell_quote(&resolved),
            max_depth,
        );

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;

        if result.exit_code != 0 {
            return Err(format!(
                "Failed to list directory {resolved}: {}",
                result.stderr
            ));
        }

        let mut entries: Vec<DirEntry> = result
            .stdout
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

    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<(), String> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve_path(remote_path);

        let bytes = ssh.download_file(&resolved).await?;

        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent dirs: {e}"))?;
        }
        tokio::fs::write(local_path, &bytes)
            .await
            .map_err(|e| format!("Failed to write {}: {e}", local_path.display()))?;

        Ok(())
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<(), String> {
        let ssh = self.data_ssh()?;
        let resolved = self.resolve_path(remote_path);

        let bytes = tokio::fs::read(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {e}", local_path.display()))?;

        ssh.upload_file(&resolved, &bytes)
            .await
            .map_err(|e| format!("Failed to upload file {resolved}: {e}"))?;

        Ok(())
    }

    fn working_directory(&self) -> &str {
        WORKING_DIRECTORY
    }

    fn platform(&self) -> &str {
        "linux"
    }

    fn os_version(&self) -> String {
        "Linux (exe.dev)".to_string()
    }

    fn sandbox_info(&self) -> String {
        match (self.vm_name.get(), &self.run_id) {
            (Some(name), Some(id)) => format!("{name} (run {id})"),
            (Some(name), None) => name.clone(),
            _ => String::new(),
        }
    }

    async fn refresh_push_credentials(&self) -> Result<(), String> {
        let origin_url = match self.origin_url() {
            Some(url) => url,
            None => return Ok(()),
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
            shell_quote(&auth_url)
        );
        self.exec_command(&cmd, 10_000, None, None, None)
            .await
            .map_err(|e| format!("Failed to set refreshed push credentials: {e}"))?;

        Ok(())
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
        self.ssh_command().map(Some)
    }

    fn origin_url(&self) -> Option<&str> {
        self.origin_url.get().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::sync::{Arc, Mutex};

    /// A recorded command sent to the mock SSH runner.
    #[derive(Debug, Clone)]
    struct RecordedCommand {
        command: String,
    }

    /// A queued response for MockSshRunner.
    struct MockResponse {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_code: i32,
    }

    /// Mock upload record.
    #[derive(Debug, Clone)]
    struct RecordedUpload {
        path: String,
        content: Vec<u8>,
    }

    /// Mock download response.
    struct MockDownload {
        content: Vec<u8>,
    }

    /// Mock SSH runner for unit tests.
    struct MockSshRunner {
        commands: Arc<Mutex<Vec<RecordedCommand>>>,
        responses: Arc<Mutex<Vec<MockResponse>>>,
        uploads: Arc<Mutex<Vec<RecordedUpload>>>,
        downloads: Arc<Mutex<Vec<MockDownload>>>,
    }

    impl MockSshRunner {
        fn new() -> Self {
            Self {
                commands: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(Vec::new())),
                uploads: Arc::new(Mutex::new(Vec::new())),
                downloads: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn queue_response(&self, stdout: &str, stderr: &str, exit_code: i32) {
            self.responses.lock().unwrap().push(MockResponse {
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
                exit_code,
            });
        }

        fn queue_response_bytes(&self, stdout: Vec<u8>, stderr: &str, exit_code: i32) {
            self.responses.lock().unwrap().push(MockResponse {
                stdout,
                stderr: stderr.as_bytes().to_vec(),
                exit_code,
            });
        }

        fn queue_download(&self, content: Vec<u8>) {
            self.downloads
                .lock()
                .unwrap()
                .push(MockDownload { content });
        }

        fn pop_response(&self) -> MockResponse {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                MockResponse {
                    stdout: Vec::new(),
                    stderr: b"no mock response queued".to_vec(),
                    exit_code: 1,
                }
            } else {
                responses.remove(0)
            }
        }
    }

    #[async_trait]
    impl SshRunner for MockSshRunner {
        async fn run_command(&self, command: &str) -> Result<SshOutput, String> {
            self.commands.lock().unwrap().push(RecordedCommand {
                command: command.to_string(),
            });
            let resp = self.pop_response();
            Ok(SshOutput {
                stdout: resp.stdout,
                stderr: resp.stderr,
                exit_code: resp.exit_code,
            })
        }

        async fn run_command_with_timeout(
            &self,
            command: &str,
            _timeout: std::time::Duration,
        ) -> Result<SshOutput, String> {
            self.commands.lock().unwrap().push(RecordedCommand {
                command: command.to_string(),
            });
            let resp = self.pop_response();
            if resp.exit_code == -99 {
                return Err("Command timed out".to_string());
            }
            Ok(SshOutput {
                stdout: resp.stdout,
                stderr: resp.stderr,
                exit_code: resp.exit_code,
            })
        }

        async fn upload_file(&self, path: &str, content: &[u8]) -> Result<(), String> {
            self.uploads.lock().unwrap().push(RecordedUpload {
                path: path.to_string(),
                content: content.to_vec(),
            });
            Ok(())
        }

        async fn download_file(&self, _path: &str) -> Result<Vec<u8>, String> {
            let mut downloads = self.downloads.lock().unwrap();
            if downloads.is_empty() {
                Err("no mock download queued".to_string())
            } else {
                Ok(downloads.remove(0).content)
            }
        }
    }

    /// Extract and decode the inner command from a base64-wrapped SSH command.
    /// The format is: echo '<base64>' | base64 -d | sh
    fn decode_bash_payload(wrapped: &str) -> String {
        let start = wrapped.find("echo '").expect("missing echo prefix") + 6;
        let end = wrapped[start..].find('\'').expect("missing closing quote") + start;
        let encoded = &wrapped[start..end];
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("invalid base64");
        String::from_utf8(bytes).expect("invalid utf8")
    }

    /// Helper: create an ExeSandbox with mock data SSH already initialized (skipping lifecycle).
    fn sandbox_with_mock_data(data_ssh: impl SshRunner + 'static) -> ExeSandbox {
        let mgmt = MockSshRunner::new();
        let sandbox = ExeSandbox::new(Box::new(mgmt), ExeConfig::default(), None, None, None);
        let _ = sandbox.vm_name.set("test-vm".to_string());
        let _ = sandbox.data_host.set("test-vm.exe.xyz".to_string());
        let _ = sandbox.data_ssh.set(Box::new(data_ssh));
        sandbox
    }

    // ---- Step 1: Metadata accessors ----

    #[test]
    fn working_directory_returns_home_user() {
        let sandbox = sandbox_with_mock_data(MockSshRunner::new());
        assert_eq!(sandbox.working_directory(), "/home/exedev");
    }

    #[test]
    fn platform_returns_linux() {
        let sandbox = sandbox_with_mock_data(MockSshRunner::new());
        assert_eq!(sandbox.platform(), "linux");
    }

    #[test]
    fn sandbox_info_returns_vm_name() {
        let sandbox = sandbox_with_mock_data(MockSshRunner::new());
        assert_eq!(sandbox.sandbox_info(), "test-vm");
    }

    #[test]
    fn os_version_returns_linux_exe() {
        let sandbox = sandbox_with_mock_data(MockSshRunner::new());
        assert_eq!(sandbox.os_version(), "Linux (exe.dev)");
    }

    // ---- ssh_command ----

    #[test]
    fn ssh_command_returns_host_after_init() {
        let sandbox = sandbox_with_mock_data(MockSshRunner::new());
        assert_eq!(sandbox.ssh_command().unwrap(), "ssh test-vm.exe.xyz");
    }

    #[test]
    fn ssh_command_errors_before_init() {
        let mgmt = MockSshRunner::new();
        let sandbox = ExeSandbox::new(Box::new(mgmt), ExeConfig::default(), None, None, None);
        assert!(sandbox.ssh_command().is_err());
    }

    // ---- Step 2: exec_command ----

    #[tokio::test]
    async fn exec_command_runs_via_ssh() {
        let data = MockSshRunner::new();
        data.queue_response("hello world\n", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        let result = sandbox
            .exec_command("echo hello world", 5000, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.stdout.trim(), "hello world");
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn exec_command_with_working_dir() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        sandbox
            .exec_command("ls", 5000, Some("/tmp/work"), None, None)
            .await
            .unwrap();

        let recorded = commands.lock().unwrap();
        let inner = decode_bash_payload(&recorded[0].command);
        assert!(
            inner.contains("cd /tmp/work"),
            "expected cd to working dir, got: {inner}",
        );
    }

    #[tokio::test]
    async fn exec_command_with_env_vars() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());

        sandbox
            .exec_command("echo $FOO", 5000, None, Some(&env), None)
            .await
            .unwrap();

        let recorded = commands.lock().unwrap();
        let inner = decode_bash_payload(&recorded[0].command);
        assert!(
            inner.contains("export FOO=bar"),
            "expected env var export, got: {inner}",
        );
    }

    #[tokio::test]
    async fn exec_command_timeout() {
        let data = MockSshRunner::new();
        // Use exit code -99 as the sentinel for timeout in our mock
        data.queue_response_bytes(Vec::new(), "", -99);
        let sandbox = sandbox_with_mock_data(data);

        let result = sandbox
            .exec_command("sleep 999", 100, None, None, None)
            .await
            .unwrap();

        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
    }

    /// SSH runner that never completes — blocks forever on a Notify.
    struct HangingSshRunner;

    #[async_trait]
    impl SshRunner for HangingSshRunner {
        async fn run_command(&self, _command: &str) -> Result<SshOutput, String> {
            std::future::pending().await
        }

        async fn run_command_with_timeout(
            &self,
            _command: &str,
            _timeout: std::time::Duration,
        ) -> Result<SshOutput, String> {
            std::future::pending().await
        }

        async fn upload_file(&self, _path: &str, _content: &[u8]) -> Result<(), String> {
            Ok(())
        }

        async fn download_file(&self, _path: &str) -> Result<Vec<u8>, String> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn exec_command_cancelled() {
        let sandbox = sandbox_with_mock_data(HangingSshRunner);

        let token = CancellationToken::new();
        let token_clone = token.clone();

        // Cancel immediately so the select! picks it up
        token_clone.cancel();

        let result = sandbox
            .exec_command("sleep 999", 60_000, None, None, Some(token))
            .await
            .unwrap();

        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
        assert_eq!(result.stderr, "Command cancelled");
        assert!(result.stdout.is_empty());
    }

    // ---- Step 3: read_file ----

    #[tokio::test]
    async fn read_file_returns_numbered_lines() {
        let data = MockSshRunner::new();
        data.queue_response("line one\nline two\nline three\n", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        let content = sandbox.read_file("test.txt", None, None).await.unwrap();
        assert!(content.contains("1 | line one"));
        assert!(content.contains("2 | line two"));
        assert!(content.contains("3 | line three"));
    }

    #[tokio::test]
    async fn read_file_with_offset_and_limit() {
        let data = MockSshRunner::new();
        data.queue_response("a\nb\nc\nd\ne\n", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        let content = sandbox
            .read_file("test.txt", Some(1), Some(2))
            .await
            .unwrap();
        assert!(content.contains("2 | b"));
        assert!(content.contains("3 | c"));
        assert!(!content.contains("1 | a"));
        assert!(!content.contains("4 | d"));
    }

    #[tokio::test]
    async fn read_file_absolute_path() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        data.queue_response("content\n", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        sandbox.read_file("/etc/hosts", None, None).await.unwrap();

        let recorded = commands.lock().unwrap();
        assert!(
            recorded[0].command.contains("/etc/hosts"),
            "expected absolute path, got: {}",
            recorded[0].command,
        );
        assert!(
            !recorded[0].command.contains("/home/user"),
            "should not prepend working dir for absolute path",
        );
    }

    // ---- Step 4: write_file ----

    #[tokio::test]
    async fn write_file_uploads_content() {
        let data = MockSshRunner::new();
        let uploads = data.uploads.clone();
        // Response for mkdir -p
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        sandbox
            .write_file("src/main.rs", "fn main() {}")
            .await
            .unwrap();

        let recorded = uploads.lock().unwrap();
        assert_eq!(recorded[0].path, "/home/exedev/src/main.rs");
        assert_eq!(recorded[0].content, b"fn main() {}");
    }

    #[tokio::test]
    async fn write_file_creates_parent_dirs() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        // Response for mkdir -p
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        sandbox
            .write_file("deep/nested/file.txt", "content")
            .await
            .unwrap();

        let recorded = commands.lock().unwrap();
        assert!(
            recorded[0].command.contains("mkdir -p"),
            "expected mkdir -p, got: {}",
            recorded[0].command,
        );
        assert!(
            recorded[0].command.contains("/home/exedev/deep/nested"),
            "expected parent path, got: {}",
            recorded[0].command,
        );
    }

    // ---- Step 5: delete_file + file_exists ----

    #[tokio::test]
    async fn delete_file_runs_rm() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        sandbox.delete_file("old.txt").await.unwrap();

        let recorded = commands.lock().unwrap();
        assert!(
            recorded[0].command.contains("rm -f"),
            "expected rm -f, got: {}",
            recorded[0].command,
        );
    }

    #[tokio::test]
    async fn file_exists_true() {
        let data = MockSshRunner::new();
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        assert!(sandbox.file_exists("exists.txt").await.unwrap());
    }

    #[tokio::test]
    async fn file_exists_false() {
        let data = MockSshRunner::new();
        data.queue_response("", "", 1);
        let sandbox = sandbox_with_mock_data(data);

        assert!(!sandbox.file_exists("missing.txt").await.unwrap());
    }

    // ---- Step 6: list_directory ----

    #[tokio::test]
    async fn list_directory_parses_find_output() {
        let data = MockSshRunner::new();
        // find output for list_directory (run via exec_command, so two responses:
        // first for the rg_available check if it fires... but exec_command calls
        // run_command_with_timeout directly, which will get the next response)
        data.queue_response(
            "f\t1024\tfile.txt\nd\t4096\tsrc\nf\t512\tREADME.md\n",
            "",
            0,
        );
        let sandbox = sandbox_with_mock_data(data);

        let entries = sandbox.list_directory(".", None).await.unwrap();
        assert_eq!(entries.len(), 3);
        // Sorted alphabetically
        assert_eq!(entries[0].name, "README.md");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, Some(512));
        assert_eq!(entries[1].name, "file.txt");
        assert_eq!(entries[2].name, "src");
        assert!(entries[2].is_dir);
        assert!(entries[2].size.is_none());
    }

    // ---- Step 7: grep ----

    #[tokio::test]
    async fn grep_returns_matches() {
        let data = MockSshRunner::new();
        // First call: rg --version check (cached)
        data.queue_response("ripgrep 14.0.0", "", 0);
        // Second call: the actual grep
        data.queue_response(
            "src/main.rs:1:fn main() {}\nsrc/lib.rs:5:fn helper() {}\n",
            "",
            0,
        );
        let sandbox = sandbox_with_mock_data(data);

        let results = sandbox
            .grep("fn ", ".", &GrepOptions::default())
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].contains("main.rs"));
    }

    #[tokio::test]
    async fn grep_no_matches_returns_empty() {
        let data = MockSshRunner::new();
        // rg --version
        data.queue_response("ripgrep 14.0.0", "", 0);
        // grep with no matches (exit code 1)
        data.queue_response("", "", 1);
        let sandbox = sandbox_with_mock_data(data);

        let results = sandbox
            .grep("nonexistent", ".", &GrepOptions::default())
            .await
            .unwrap();

        assert!(results.is_empty());
    }

    // ---- Step 8: glob ----

    #[tokio::test]
    async fn glob_finds_files() {
        let data = MockSshRunner::new();
        data.queue_response("/home/user/src/main.rs\n/home/user/src/lib.rs\n", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        let results = sandbox.glob("*.rs", Some("src")).await.unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].contains("main.rs"));
    }

    // ---- Step 9: download_file_to_local ----

    #[tokio::test]
    async fn download_file_to_local_writes_bytes() {
        let data = MockSshRunner::new();
        data.queue_download(b"binary content".to_vec());
        let sandbox = sandbox_with_mock_data(data);

        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("downloaded.bin");
        sandbox
            .download_file_to_local("artifact.bin", &local)
            .await
            .unwrap();

        let bytes = tokio::fs::read(&local).await.unwrap();
        assert_eq!(bytes, b"binary content");
    }

    // ---- Step 10: initialize + cleanup (VM lifecycle) ----

    #[tokio::test]
    async fn initialize_creates_vm() {
        let mgmt = MockSshRunner::new();
        mgmt.queue_response(
            r#"{"vm_name": "my-vm", "ssh_dest": "my-vm.exe.xyz"}"#,
            "",
            0,
        );

        let data_for_init = MockSshRunner::new();

        let mut sandbox = ExeSandbox::new(Box::new(mgmt), ExeConfig::default(), None, None, None);
        // Override factory to return our mock data SSH
        let data_box: Arc<Mutex<Option<Box<dyn SshRunner>>>> =
            Arc::new(Mutex::new(Some(Box::new(data_for_init))));
        sandbox.data_ssh_factory = Box::new(move |_host: &str| {
            let data_box = Arc::clone(&data_box);
            Box::pin(async move {
                data_box
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| "mock data SSH already taken".to_string())
            })
        });

        sandbox.initialize().await.unwrap();

        assert_eq!(sandbox.sandbox_info(), "my-vm");
    }

    #[tokio::test]
    async fn initialize_emits_events() {
        let mgmt = MockSshRunner::new();
        mgmt.queue_response(
            r#"{"vm_name": "ev-vm", "ssh_dest": "ev-vm.exe.xyz"}"#,
            "",
            0,
        );

        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let events_cb = Arc::clone(&events);

        let mut sandbox = ExeSandbox::new(Box::new(mgmt), ExeConfig::default(), None, None, None);
        sandbox.set_event_callback(Arc::new(move |event| {
            events_cb.lock().unwrap().push(format!("{event:?}"));
        }));

        let data_for_init = MockSshRunner::new();
        let data_box: Arc<Mutex<Option<Box<dyn SshRunner>>>> =
            Arc::new(Mutex::new(Some(Box::new(data_for_init))));
        sandbox.data_ssh_factory = Box::new(move |_host: &str| {
            let data_box = Arc::clone(&data_box);
            Box::pin(async move {
                data_box
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| "mock data SSH already taken".to_string())
            })
        });

        sandbox.initialize().await.unwrap();

        let captured = events.lock().unwrap();
        assert!(
            captured.iter().any(|e| e.contains("Initializing")),
            "expected Initializing event, got: {captured:?}"
        );
        assert!(
            captured.iter().any(|e| e.contains("Ready")),
            "expected Ready event, got: {captured:?}"
        );
    }

    #[tokio::test]
    async fn cleanup_destroys_vm() {
        let mgmt = MockSshRunner::new();
        let mgmt_commands = mgmt.commands.clone();
        // Response for `rm <vm_name>`
        mgmt.queue_response("", "", 0);

        let sandbox = ExeSandbox::new(Box::new(mgmt), ExeConfig::default(), None, None, None);
        let _ = sandbox.vm_name.set("doomed-vm".to_string());

        sandbox.cleanup().await.unwrap();

        let recorded = mgmt_commands.lock().unwrap();
        assert_eq!(recorded[0].command, "rm doomed-vm");
    }

    #[tokio::test]
    async fn cleanup_before_initialize_is_noop() {
        let mgmt = MockSshRunner::new();
        let sandbox = ExeSandbox::new(Box::new(mgmt), ExeConfig::default(), None, None, None);
        // Should not error — no VM to destroy
        sandbox.cleanup().await.unwrap();
    }

    // ---- clone_repo ----

    #[tokio::test]
    async fn initialize_with_clone_params_clones_repo() {
        let mgmt = MockSshRunner::new();
        mgmt.queue_response(
            r#"{"vm_name": "clone-vm", "ssh_dest": "clone-vm.exe.xyz"}"#,
            "",
            0,
        );

        let data = MockSshRunner::new();
        let data_commands = data.commands.clone();
        // Response for git clone
        data.queue_response("", "", 0);

        let data_box: Arc<Mutex<Option<Box<dyn SshRunner>>>> =
            Arc::new(Mutex::new(Some(Box::new(data))));

        let clone_params = GitCloneParams {
            url: "https://github.com/org/repo.git".to_string(),
            branch: Some("main".to_string()),
        };
        let mut sandbox = ExeSandbox::new(
            Box::new(mgmt),
            ExeConfig::default(),
            Some(clone_params),
            None,
            None,
        );
        sandbox.data_ssh_factory = Box::new(move |_host: &str| {
            let data_box = Arc::clone(&data_box);
            Box::pin(async move {
                data_box
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| "mock data SSH already taken".to_string())
            })
        });

        sandbox.initialize().await.unwrap();

        let recorded = data_commands.lock().unwrap();
        let clone_inner = decode_bash_payload(&recorded[0].command);
        assert!(
            clone_inner.contains("git clone"),
            "expected git clone, got: {clone_inner}",
        );
        assert!(
            clone_inner.contains("--branch main"),
            "expected branch flag, got: {clone_inner}",
        );
        assert_eq!(
            sandbox.origin_url(),
            Some("https://github.com/org/repo.git"),
        );
    }

    #[tokio::test]
    async fn initialize_without_clone_params_skips_clone() {
        let mgmt = MockSshRunner::new();
        mgmt.queue_response(
            r#"{"vm_name": "no-clone-vm", "ssh_dest": "no-clone-vm.exe.xyz"}"#,
            "",
            0,
        );

        let data = MockSshRunner::new();
        let data_commands = data.commands.clone();

        let data_box: Arc<Mutex<Option<Box<dyn SshRunner>>>> =
            Arc::new(Mutex::new(Some(Box::new(data))));

        let mut sandbox = ExeSandbox::new(Box::new(mgmt), ExeConfig::default(), None, None, None);
        sandbox.data_ssh_factory = Box::new(move |_host: &str| {
            let data_box = Arc::clone(&data_box);
            Box::pin(async move {
                data_box
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| "mock data SSH already taken".to_string())
            })
        });

        sandbox.initialize().await.unwrap();

        let recorded = data_commands.lock().unwrap();
        assert!(
            recorded.is_empty(),
            "expected no data SSH commands without clone params, got: {recorded:?}",
        );
        assert!(sandbox.origin_url().is_none());
    }

    #[tokio::test]
    async fn initialize_clone_failure_emits_event_and_errors() {
        let mgmt = MockSshRunner::new();
        mgmt.queue_response(
            r#"{"vm_name": "fail-vm", "ssh_dest": "fail-vm.exe.xyz"}"#,
            "",
            0,
        );

        let data = MockSshRunner::new();
        // git clone fails
        data.queue_response("", "auth failed", 128);

        let data_box: Arc<Mutex<Option<Box<dyn SshRunner>>>> =
            Arc::new(Mutex::new(Some(Box::new(data))));

        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let events_cb = Arc::clone(&events);

        let clone_params = GitCloneParams {
            url: "https://github.com/org/repo.git".to_string(),
            branch: None,
        };
        let mut sandbox = ExeSandbox::new(
            Box::new(mgmt),
            ExeConfig::default(),
            Some(clone_params),
            None,
            None,
        );
        sandbox.set_event_callback(Arc::new(move |event| {
            events_cb.lock().unwrap().push(format!("{event:?}"));
        }));
        sandbox.data_ssh_factory = Box::new(move |_host: &str| {
            let data_box = Arc::clone(&data_box);
            Box::pin(async move {
                data_box
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| "mock data SSH already taken".to_string())
            })
        });

        let result = sandbox.initialize().await;
        assert!(result.is_err());

        let captured = events.lock().unwrap();
        assert!(
            captured.iter().any(|e| e.contains("GitCloneFailed")),
            "expected GitCloneFailed event, got: {captured:?}",
        );
    }

    // ---- shell_quote injection tests ----

    #[tokio::test]
    async fn clone_quotes_branch_with_shell_metacharacters() {
        let data = MockSshRunner::new();
        let data_commands = data.commands.clone();
        // Response for git clone
        data.queue_response("", "", 0);

        let clone_params = GitCloneParams {
            url: "https://github.com/org/repo.git".to_string(),
            branch: Some("feat;id".to_string()),
        };
        let sandbox = sandbox_with_mock_data(data);
        sandbox.clone_repo(&clone_params).await.unwrap();

        let recorded = data_commands.lock().unwrap();
        let clone_inner = decode_bash_payload(&recorded[0].command);
        assert!(
            clone_inner.contains("--branch 'feat;id'"),
            "expected quoted branch, got: {clone_inner}",
        );
    }

    #[tokio::test]
    async fn clone_quotes_url_with_spaces() {
        let data = MockSshRunner::new();
        let data_commands = data.commands.clone();
        data.queue_response("", "", 0);

        let clone_params = GitCloneParams {
            url: "https://example.com/has space/repo.git".to_string(),
            branch: None,
        };
        let sandbox = sandbox_with_mock_data(data);
        sandbox.clone_repo(&clone_params).await.unwrap();

        let recorded = data_commands.lock().unwrap();
        let clone_inner = decode_bash_payload(&recorded[0].command);
        assert!(
            clone_inner.contains("'https://example.com/has space/repo.git'"),
            "expected quoted URL, got: {clone_inner}",
        );
    }

    #[tokio::test]
    async fn initialize_quotes_image_in_mgmt_command() {
        let mgmt = MockSshRunner::new();
        let mgmt_commands = mgmt.commands.clone();
        mgmt.queue_response(
            r#"{"vm_name": "img-vm", "ssh_dest": "img-vm.exe.xyz"}"#,
            "",
            0,
        );

        let data = MockSshRunner::new();
        let data_box: Arc<Mutex<Option<Box<dyn SshRunner>>>> =
            Arc::new(Mutex::new(Some(Box::new(data))));

        let config = ExeConfig {
            image: Some("ubuntu;evil".to_string()),
        };
        let mut sandbox = ExeSandbox::new(Box::new(mgmt), config, None, None, None);
        sandbox.data_ssh_factory = Box::new(move |_host: &str| {
            let data_box = Arc::clone(&data_box);
            Box::pin(async move {
                data_box
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| "mock data SSH already taken".to_string())
            })
        });

        sandbox.initialize().await.unwrap();

        let recorded = mgmt_commands.lock().unwrap();
        assert!(
            recorded[0].command.contains("--image 'ubuntu;evil'"),
            "expected quoted image, got: {}",
            recorded[0].command,
        );
    }

    #[tokio::test]
    async fn exec_command_quotes_env_values_with_metacharacters() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        let mut env = HashMap::new();
        env.insert("KEY".to_string(), "val;rm -rf /".to_string());

        sandbox
            .exec_command("echo $KEY", 5000, None, Some(&env), None)
            .await
            .unwrap();

        let recorded = commands.lock().unwrap();
        let inner = decode_bash_payload(&recorded[0].command);
        assert!(
            inner.contains("export KEY='val;rm -rf /'"),
            "expected quoted env value, got: {inner}",
        );
    }

    #[tokio::test]
    async fn exec_command_quotes_working_dir_with_spaces() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock_data(data);

        sandbox
            .exec_command("ls", 5000, Some("/tmp/my dir"), None, None)
            .await
            .unwrap();

        let recorded = commands.lock().unwrap();
        let inner = decode_bash_payload(&recorded[0].command);
        assert!(
            inner.contains("cd '/tmp/my dir'"),
            "expected quoted working dir, got: {inner}",
        );
    }

    #[tokio::test]
    async fn grep_quotes_glob_filter() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        // rg --version
        data.queue_response("ripgrep 14.0.0", "", 0);
        // grep result
        data.queue_response("", "", 1);
        let sandbox = sandbox_with_mock_data(data);

        let options = GrepOptions {
            glob_filter: Some("*.rs'".to_string()),
            ..GrepOptions::default()
        };
        sandbox.grep("pattern", ".", &options).await.unwrap();

        let recorded = commands.lock().unwrap();
        // The grep command goes through exec_command, so decode second command
        let inner = decode_bash_payload(&recorded[1].command);
        assert!(
            !inner.contains("--glob '*.rs''"),
            "glob filter should be properly escaped, got: {inner}",
        );
    }
}
