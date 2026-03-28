mod openssh_runner;

use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::time::Instant;

use crate::sandbox::resolve_path;
use crate::shell_quote;
use crate::ssh_common;
use crate::{
    DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback,
    format_lines_numbered,
};
use async_trait::async_trait;
use tokio::fs;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

pub use crate::ssh_common::{GitCloneParams, SshOutput, SshRunner};
pub use openssh_runner::OpensshRunner;

pub use fabro_config::sandbox::SshSettings as SshConfig;

const PROVIDER: &str = "ssh";

/// Sandbox that runs all operations on a user-provided SSH host.
///
/// Unlike ExeSandbox, there is no VM lifecycle management -- the host
/// must already be running and accessible via SSH.
pub struct SshSandbox {
    ssh: OnceCell<Box<dyn SshRunner>>,
    config: SshConfig,
    clone_params: Option<GitCloneParams>,
    run_id: Option<String>,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    rg_available: OnceCell<bool>,
    event_callback: Option<SandboxEventCallback>,
    origin_url: OnceCell<String>,
}

impl SshSandbox {
    /// Creates a new `SshSandbox` targeting the given SSH host.
    pub fn new(
        config: SshConfig,
        clone_params: Option<GitCloneParams>,
        run_id: Option<String>,
        github_app: Option<fabro_github::GitHubAppCredentials>,
    ) -> Self {
        Self {
            ssh: OnceCell::new(),
            config,
            clone_params,
            run_id,
            github_app,
            rg_available: OnceCell::const_new(),
            event_callback: None,
            origin_url: OnceCell::new(),
        }
    }

    /// Create an `SshSandbox` from a pre-connected SSH runner.
    /// Used for reconnection (e.g. `fabro cp`) when the host is already known.
    pub fn from_existing(ssh: Box<dyn SshRunner>, config: SshConfig) -> Self {
        let ssh_cell = OnceCell::new();
        let _ = ssh_cell.set(ssh);
        Self {
            ssh: ssh_cell,
            config,
            clone_params: None,
            run_id: None,
            github_app: None,
            rg_available: OnceCell::const_new(),
            event_callback: None,
            origin_url: OnceCell::new(),
        }
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

    /// Get the SSH runner, returning an error if not yet initialized.
    fn ssh(&self) -> Result<&dyn SshRunner, String> {
        self.ssh
            .get()
            .map(std::convert::AsRef::as_ref)
            .ok_or_else(|| "SSH sandbox not initialized -- call initialize() first".to_string())
    }

    /// Return the SSH command to connect to this host.
    pub fn ssh_command(&self) -> String {
        format!("ssh {}", self.config.destination)
    }

    /// Clone a git repo into the sandbox working directory.
    async fn clone_repo(&self, params: &GitCloneParams) -> Result<(), String> {
        let ssh = self.ssh()?;
        ssh_common::clone_repo(
            ssh,
            &self.config.working_directory,
            params,
            self.github_app.as_ref(),
            &self.origin_url,
            &|event| self.emit(event),
        )
        .await
    }

    fn resolve_path(&self, path: &str) -> String {
        resolve_path(path, &self.config.working_directory)
    }
}

#[async_trait]
impl Sandbox for SshSandbox {
    async fn initialize(&self) -> Result<(), String> {
        self.emit(SandboxEvent::Initializing {
            provider: PROVIDER.into(),
        });
        let init_start = Instant::now();

        // Connect SSH
        let runner =
            OpensshRunner::connect(&self.config.destination, self.config.config_file.as_deref())
                .await
                .map_err(|e| {
                    let err = format!("Failed to connect to {}: {e}", self.config.destination);
                    let duration_ms =
                        u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                    self.emit(SandboxEvent::InitializeFailed {
                        provider: PROVIDER.into(),
                        error: err.clone(),
                        duration_ms,
                    });
                    err
                })?;

        self.ssh
            .set(Box::new(runner))
            .map_err(|_| "SSH sandbox already initialized".to_string())?;

        // Create working directory
        let mkdir_cmd = format!("mkdir -p {}", shell_quote(&self.config.working_directory));
        let ssh = self.ssh()?;
        let output = ssh.run_command(&mkdir_cmd).await.map_err(|e| {
            let err = format!("Failed to create working directory: {e}");
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
            let err = format!("mkdir -p failed (exit {}): {stderr}", output.exit_code);
            let duration_ms = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::InitializeFailed {
                provider: PROVIDER.into(),
                error: err.clone(),
                duration_ms,
            });
            return Err(err);
        }

        // Clone git repo if clone params were provided
        if let Some(ref params) = self.clone_params {
            self.clone_repo(params).await?;
        }

        let init_duration = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::Ready {
            provider: PROVIDER.into(),
            duration_ms: init_duration,
            name: None,
            cpu: None,
            memory: None,
            url: None,
        });

        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        // No-op: we leave the workspace on the remote host
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
        let ssh = self.ssh()?;
        let start = Instant::now();

        // Build inner script as plain text, then base64-wrap for safe transport
        let mut script = String::new();

        if let Some(vars) = env_vars {
            for (key, value) in vars {
                let _ = writeln!(script, "export {}={}", shell_quote(key), shell_quote(value));
            }
        }

        let dir = match working_dir {
            Some(dir) => self.resolve_path(dir),
            None => self.config.working_directory.clone(),
        };
        let _ = write!(script, "cd {} && {command}", shell_quote(&dir));

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
        let ssh = self.ssh()?;
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
        let ssh = self.ssh()?;
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
        let ssh = self.ssh()?;
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
        let ssh = self.ssh()?;
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
        let base = path.map_or_else(
            || self.config.working_directory.clone(),
            |p| self.resolve_path(p),
        );

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
        let ssh = self.ssh()?;
        let resolved = self.resolve_path(remote_path);

        let bytes = ssh.download_file(&resolved).await?;

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
        let ssh = self.ssh()?;
        let resolved = self.resolve_path(remote_path);

        let bytes = fs::read(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {e}", local_path.display()))?;

        ssh.upload_file(&resolved, &bytes)
            .await
            .map_err(|e| format!("Failed to upload file {resolved}: {e}"))?;

        Ok(())
    }

    fn working_directory(&self) -> &str {
        &self.config.working_directory
    }

    fn platform(&self) -> &'static str {
        "linux"
    }

    fn os_version(&self) -> String {
        format!("Linux (ssh:{})", self.config.destination)
    }

    fn sandbox_info(&self) -> String {
        match &self.run_id {
            Some(id) => format!("{} (run {id})", self.config.destination),
            None => self.config.destination.clone(),
        }
    }

    async fn refresh_push_credentials(&self) -> Result<(), String> {
        let Some(origin_url) = self.origin_url() else {
            return Ok(());
        };
        let Some(creds) = &self.github_app else {
            return Ok(());
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
        Ok(Some(self.ssh_command()))
    }

    fn origin_url(&self) -> Option<&str> {
        self.origin_url.get().map(String::as_str)
    }

    async fn get_preview_url(
        &self,
        port: u16,
    ) -> Result<Option<(String, HashMap<String, String>)>, String> {
        Ok(self
            .config
            .preview_url_base
            .as_ref()
            .map(|base| (format!("{base}:{port}"), HashMap::new())))
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

    fn test_config() -> SshConfig {
        SshConfig {
            destination: "user@testhost".to_string(),
            working_directory: "/home/user/workspace".to_string(),
            config_file: None,
            preview_url_base: None,
        }
    }

    /// Helper: create an SshSandbox with mock SSH already initialized (skipping connect).
    fn sandbox_with_mock(ssh: impl SshRunner + 'static) -> SshSandbox {
        SshSandbox::from_existing(Box::new(ssh), test_config())
    }

    // ---- Metadata accessors ----

    #[tokio::test]
    async fn get_preview_url_returns_none_when_not_configured() {
        let sandbox = sandbox_with_mock(MockSshRunner::new());
        assert_eq!(sandbox.get_preview_url(3000).await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_preview_url_returns_url_when_configured() {
        let mut config = test_config();
        config.preview_url_base = Some("http://beast".to_string());
        let sandbox = SshSandbox::from_existing(Box::new(MockSshRunner::new()), config);
        let (url, headers) = sandbox.get_preview_url(3000).await.unwrap().unwrap();
        assert_eq!(url, "http://beast:3000");
        assert!(headers.is_empty());
    }

    #[test]
    fn working_directory_returns_configured_path() {
        let sandbox = sandbox_with_mock(MockSshRunner::new());
        assert_eq!(sandbox.working_directory(), "/home/user/workspace");
    }

    #[test]
    fn platform_returns_linux() {
        let sandbox = sandbox_with_mock(MockSshRunner::new());
        assert_eq!(sandbox.platform(), "linux");
    }

    #[test]
    fn sandbox_info_returns_destination() {
        let sandbox = sandbox_with_mock(MockSshRunner::new());
        assert_eq!(sandbox.sandbox_info(), "user@testhost");
    }

    #[test]
    fn os_version_returns_ssh_info() {
        let sandbox = sandbox_with_mock(MockSshRunner::new());
        assert_eq!(sandbox.os_version(), "Linux (ssh:user@testhost)");
    }

    #[test]
    fn ssh_command_returns_destination() {
        let sandbox = sandbox_with_mock(MockSshRunner::new());
        assert_eq!(sandbox.ssh_command(), "ssh user@testhost");
    }

    // ---- cleanup ----

    #[tokio::test]
    async fn cleanup_is_noop() {
        let sandbox = sandbox_with_mock(MockSshRunner::new());
        sandbox.cleanup().await.unwrap();
    }

    // ---- exec_command ----

    #[tokio::test]
    async fn exec_command_runs_via_ssh() {
        let data = MockSshRunner::new();
        data.queue_response("hello world\n", "", 0);
        let sandbox = sandbox_with_mock(data);

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
        let sandbox = sandbox_with_mock(data);

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
        let sandbox = sandbox_with_mock(data);

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
        data.queue_response_bytes(Vec::new(), "", -99);
        let sandbox = sandbox_with_mock(data);

        let result = sandbox
            .exec_command("sleep 999", 100, None, None, None)
            .await
            .unwrap();

        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
    }

    /// SSH runner that never completes -- blocks forever.
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
        let sandbox = sandbox_with_mock(HangingSshRunner);

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

    // ---- read_file ----

    #[tokio::test]
    async fn read_file_returns_numbered_lines() {
        let data = MockSshRunner::new();
        data.queue_response("line one\nline two\nline three\n", "", 0);
        let sandbox = sandbox_with_mock(data);

        let content = sandbox.read_file("test.txt", None, None).await.unwrap();
        assert!(content.contains("1 | line one"));
        assert!(content.contains("2 | line two"));
        assert!(content.contains("3 | line three"));
    }

    #[tokio::test]
    async fn read_file_with_offset_and_limit() {
        let data = MockSshRunner::new();
        data.queue_response("a\nb\nc\nd\ne\n", "", 0);
        let sandbox = sandbox_with_mock(data);

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
        let sandbox = sandbox_with_mock(data);

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

    // ---- write_file ----

    #[tokio::test]
    async fn write_file_uploads_content() {
        let data = MockSshRunner::new();
        let uploads = data.uploads.clone();
        // Response for mkdir -p
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock(data);

        sandbox
            .write_file("src/main.rs", "fn main() {}")
            .await
            .unwrap();

        let recorded = uploads.lock().unwrap();
        assert_eq!(recorded[0].path, "/home/user/workspace/src/main.rs");
        assert_eq!(recorded[0].content, b"fn main() {}");
    }

    #[tokio::test]
    async fn write_file_creates_parent_dirs() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        // Response for mkdir -p
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock(data);

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
            recorded[0]
                .command
                .contains("/home/user/workspace/deep/nested"),
            "expected parent path, got: {}",
            recorded[0].command,
        );
    }

    // ---- delete_file + file_exists ----

    #[tokio::test]
    async fn delete_file_runs_rm() {
        let data = MockSshRunner::new();
        let commands = data.commands.clone();
        data.queue_response("", "", 0);
        let sandbox = sandbox_with_mock(data);

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
        let sandbox = sandbox_with_mock(data);

        assert!(sandbox.file_exists("exists.txt").await.unwrap());
    }

    #[tokio::test]
    async fn file_exists_false() {
        let data = MockSshRunner::new();
        data.queue_response("", "", 1);
        let sandbox = sandbox_with_mock(data);

        assert!(!sandbox.file_exists("missing.txt").await.unwrap());
    }

    // ---- list_directory ----

    #[tokio::test]
    async fn list_directory_parses_find_output() {
        let data = MockSshRunner::new();
        data.queue_response(
            "f\t1024\tfile.txt\nd\t4096\tsrc\nf\t512\tREADME.md\n",
            "",
            0,
        );
        let sandbox = sandbox_with_mock(data);

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

    // ---- grep ----

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
        let sandbox = sandbox_with_mock(data);

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
        let sandbox = sandbox_with_mock(data);

        let results = sandbox
            .grep("nonexistent", ".", &GrepOptions::default())
            .await
            .unwrap();

        assert!(results.is_empty());
    }

    // ---- glob ----

    #[tokio::test]
    async fn glob_finds_files() {
        let data = MockSshRunner::new();
        data.queue_response(
            "/home/user/workspace/src/main.rs\n/home/user/workspace/src/lib.rs\n",
            "",
            0,
        );
        let sandbox = sandbox_with_mock(data);

        let results = sandbox.glob("*.rs", Some("src")).await.unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].contains("main.rs"));
    }

    // ---- download_file_to_local ----

    #[tokio::test]
    async fn download_file_to_local_writes_bytes() {
        let data = MockSshRunner::new();
        data.queue_download(b"binary content".to_vec());
        let sandbox = sandbox_with_mock(data);

        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("downloaded.bin");
        sandbox
            .download_file_to_local("artifact.bin", &local)
            .await
            .unwrap();

        let bytes = tokio::fs::read(&local).await.unwrap();
        assert_eq!(bytes, b"binary content");
    }

    // ---- from_existing ----

    #[tokio::test]
    async fn from_existing_reconnects() {
        let data = MockSshRunner::new();
        data.queue_response("hello\n", "", 0);

        let config = test_config();
        let sandbox = SshSandbox::from_existing(Box::new(data), config);

        // Should be able to use immediately (no initialize needed)
        let result = sandbox
            .exec_command("echo hello", 5000, None, None, None)
            .await
            .unwrap();
        assert_eq!(result.stdout.trim(), "hello");
    }

    // ---- path resolution ----

    #[test]
    fn resolve_path_relative() {
        let sandbox = sandbox_with_mock(MockSshRunner::new());
        assert_eq!(
            sandbox.resolve_path("src/main.rs"),
            "/home/user/workspace/src/main.rs"
        );
    }

    #[test]
    fn resolve_path_absolute() {
        let sandbox = sandbox_with_mock(MockSshRunner::new());
        assert_eq!(sandbox.resolve_path("/tmp/file.txt"), "/tmp/file.txt");
    }
}
