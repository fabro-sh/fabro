mod cli_runner;

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use crate::{
    DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback,
    format_lines_numbered,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

pub use cli_runner::CliSpriteRunner;

use crate::shell_quote;

const WORKING_DIRECTORY: &str = "/home/sprite";
const PROVIDER: &str = "sprites";

/// Output from a sprite CLI command execution.
pub struct SpriteOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Trait abstracting sprite CLI operations for testability.
#[async_trait]
pub trait SpriteRunner: Send + Sync {
    async fn run(&self, args: &[&str]) -> Result<SpriteOutput, String>;

    async fn run_with_timeout(
        &self,
        args: &[&str],
        timeout: std::time::Duration,
    ) -> Result<SpriteOutput, String>;
}

/// Configuration for a Sprites sandbox (TOML target for `[sandbox.sprites]`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SpritesConfig {
    pub org: Option<String>,
    pub url_auth: Option<String>,
    pub sprite_name: Option<String>,
}

/// Sandbox that runs all operations inside a Sprites VM via the `sprite` CLI.
pub struct SpritesSandbox {
    config: SpritesConfig,
    runner: Box<dyn SpriteRunner>,
    sprite_name: tokio::sync::OnceCell<String>,
    preview_url: tokio::sync::OnceCell<String>,
    rg_available: tokio::sync::OnceCell<bool>,
    event_callback: Option<SandboxEventCallback>,
}

impl SpritesSandbox {
    pub fn new(runner: Box<dyn SpriteRunner>, config: SpritesConfig) -> Self {
        Self {
            config,
            runner,
            sprite_name: tokio::sync::OnceCell::new(),
            preview_url: tokio::sync::OnceCell::new(),
            rg_available: tokio::sync::OnceCell::const_new(),
            event_callback: None,
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

    fn resolve_path(&self, path: &str) -> String {
        crate::sandbox::resolve_path(path, WORKING_DIRECTORY)
    }

    fn sprite_name(&self) -> Result<&str, String> {
        self.sprite_name
            .get()
            .map(|s| s.as_str())
            .ok_or_else(|| "Sprites sandbox not initialized — call initialize() first".to_string())
    }

    /// Build args for `sprite exec -s <name> [-o org] -- bash -c <command>`.
    fn build_exec_args(&self, command: &str) -> Result<Vec<String>, String> {
        let name = self.sprite_name()?;
        let mut args = vec!["exec".to_string(), "-s".to_string(), name.to_string()];
        if let Some(ref org) = self.config.org {
            args.push("-o".to_string());
            args.push(org.clone());
        }
        args.push("--".to_string());
        args.push("bash".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());
        Ok(args)
    }

    /// Append `-o <org>` to base args if org is configured.
    fn build_sprite_args(&self, base_args: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = base_args.iter().map(|s| s.to_string()).collect();
        if let Some(ref org) = self.config.org {
            args.push("-o".to_string());
            args.push(org.clone());
        }
        args
    }
}

#[async_trait]
impl Sandbox for SpritesSandbox {
    async fn initialize(&self) -> Result<(), String> {
        self.emit(SandboxEvent::Initializing {
            provider: PROVIDER.into(),
        });
        let init_start = Instant::now();

        let name = if let Some(ref existing) = self.config.sprite_name {
            existing.clone()
        } else {
            let now = chrono::Utc::now();
            let suffix: u16 = rand::random::<u16>() % 10000;
            let name = format!("fabro-{}-{suffix:04}", now.format("%Y%m%d-%H%M%S"));

            let create_args = self.build_sprite_args(&["create", &name, "-skip-console"]);
            let create_refs: Vec<&str> = create_args.iter().map(|s| s.as_str()).collect();
            let output = self.runner.run(&create_refs).await.map_err(|e| {
                let err = format!("Failed to create sprite: {e}");
                let duration_ms =
                    u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.emit(SandboxEvent::InitializeFailed {
                    provider: PROVIDER.into(),
                    error: err.clone(),
                    duration_ms,
                });
                err
            })?;

            if output.exit_code != 0 {
                let err = format!(
                    "Sprite creation failed (exit {}): {}",
                    output.exit_code, output.stderr
                );
                let duration_ms =
                    u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.emit(SandboxEvent::InitializeFailed {
                    provider: PROVIDER.into(),
                    error: err.clone(),
                    duration_ms,
                });
                return Err(err);
            }

            name
        };

        self.sprite_name
            .set(name.clone())
            .map_err(|_| "Sprites sandbox already initialized".to_string())?;

        // Get preview URL
        let url_args = self.build_sprite_args(&["url", "-s", &name]);
        let url_refs: Vec<&str> = url_args.iter().map(|s| s.as_str()).collect();
        let url_output = self.runner.run(&url_refs).await.map_err(|e| {
            let err = format!("Failed to get sprite URL: {e}");
            let duration_ms = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.emit(SandboxEvent::InitializeFailed {
                provider: PROVIDER.into(),
                error: err.clone(),
                duration_ms,
            });
            err
        })?;

        let url = url_output
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("URL: "))
            .unwrap_or(url_output.stdout.trim())
            .trim()
            .to_string();
        let _ = self.preview_url.set(url.clone());

        // Set URL auth if configured
        if let Some(ref auth_mode) = self.config.url_auth {
            let auth_args =
                self.build_sprite_args(&["url", "update", "-s", &name, "--auth", auth_mode]);
            let auth_refs: Vec<&str> = auth_args.iter().map(|s| s.as_str()).collect();
            self.runner.run(&auth_refs).await.map_err(|e| {
                let err = format!("Failed to set URL auth: {e}");
                let duration_ms =
                    u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.emit(SandboxEvent::InitializeFailed {
                    provider: PROVIDER.into(),
                    error: err.clone(),
                    duration_ms,
                });
                err
            })?;
        }

        let init_duration = u64::try_from(init_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::Ready {
            provider: PROVIDER.into(),
            duration_ms: init_duration,
            name: Some(name),
            cpu: None,
            memory: None,
            url: Some(url),
        });

        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        self.emit(SandboxEvent::CleanupStarted {
            provider: PROVIDER.into(),
        });
        let start = Instant::now();

        if self.config.sprite_name.is_none() {
            if let Some(name) = self.sprite_name.get() {
                let args = self.build_sprite_args(&["destroy", "-s", name, "-force"]);
                let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                if let Err(e) = self.runner.run(&refs).await {
                    let err = format!("Failed to destroy sprite: {e}");
                    self.emit(SandboxEvent::CleanupFailed {
                        provider: PROVIDER.into(),
                        error: err.clone(),
                    });
                    return Err(err);
                }
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
        _cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        let start = Instant::now();

        let mut parts = Vec::new();

        if let Some(vars) = env_vars {
            for (key, value) in vars {
                parts.push(format!(
                    "export {}={};",
                    shell_quote(key),
                    shell_quote(value)
                ));
            }
        }

        if let Some(dir) = working_dir {
            let resolved = self.resolve_path(dir);
            parts.push(format!("cd {}", shell_quote(&resolved)));
            parts.push("&&".to_string());
        } else {
            parts.push(format!("cd {}", shell_quote(WORKING_DIRECTORY)));
            parts.push("&&".to_string());
        }

        parts.push(command.to_string());
        let full_cmd = parts.join(" ");

        let args = self.build_exec_args(&full_cmd)?;
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let output = self.runner.run_with_timeout(&refs, timeout).await;

        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        match output {
            Ok(out) => Ok(ExecResult {
                stdout: out.stdout,
                stderr: out.stderr,
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
        let resolved = self.resolve_path(path);
        let cmd = format!("cat {}", shell_quote(&resolved));

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;

        if result.exit_code != 0 {
            return Err(format!("Failed to read {resolved}: {}", result.stderr));
        }

        Ok(format_lines_numbered(&result.stdout, offset, limit))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        let resolved = self.resolve_path(path);

        // Ensure parent directory exists
        if let Some(parent) = Path::new(&resolved).parent() {
            let parent_str = parent.to_string_lossy();
            if parent_str != "/" {
                let mkdir_cmd = format!("mkdir -p {}", shell_quote(&parent_str));
                self.exec_command(&mkdir_cmd, 30_000, None, None, None)
                    .await?;
            }
        }

        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
        let cmd = format!(
            "echo '{}' | base64 -d > {}",
            encoded,
            shell_quote(&resolved),
        );

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;
        if result.exit_code != 0 {
            return Err(format!("Failed to write {resolved}: {}", result.stderr));
        }

        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<(), String> {
        let resolved = self.resolve_path(path);
        let cmd = format!("rm -f {}", shell_quote(&resolved));

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;
        if result.exit_code != 0 {
            return Err(format!("Failed to delete {resolved}: {}", result.stderr));
        }
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        let resolved = self.resolve_path(path);
        let cmd = format!("test -e {}", shell_quote(&resolved));

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;
        Ok(result.exit_code == 0)
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
        let resolved = self.resolve_path(remote_path);
        let cmd = format!("base64 {}", shell_quote(&resolved));

        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;
        if result.exit_code != 0 {
            return Err(format!("Failed to read {resolved}: {}", result.stderr));
        }

        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(result.stdout.trim())
            .map_err(|e| format!("Failed to decode base64: {e}"))?;

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
        let resolved = self.resolve_path(remote_path);

        let bytes = tokio::fs::read(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {e}", local_path.display()))?;

        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);

        // Ensure parent directory exists
        if let Some(parent) = Path::new(&resolved).parent() {
            let mkdir_cmd = format!("mkdir -p {}", shell_quote(&parent.to_string_lossy()));
            self.exec_command(&mkdir_cmd, 10_000, None, None, None)
                .await?;
        }

        let cmd = format!(
            "echo {} | base64 -d > {}",
            shell_quote(&encoded),
            shell_quote(&resolved)
        );
        let result = self.exec_command(&cmd, 30_000, None, None, None).await?;
        if result.exit_code != 0 {
            return Err(format!("Failed to write {resolved}: {}", result.stderr));
        }

        Ok(())
    }

    fn working_directory(&self) -> &str {
        WORKING_DIRECTORY
    }

    fn platform(&self) -> &str {
        "linux"
    }

    fn os_version(&self) -> String {
        "Linux (Sprites)".to_string()
    }

    fn sandbox_info(&self) -> String {
        self.sprite_name.get().cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    struct RecordedCommand {
        args: Vec<String>,
    }

    struct MockResponse {
        stdout: String,
        stderr: String,
        exit_code: i32,
    }

    struct MockSpriteRunner {
        commands: Arc<Mutex<Vec<RecordedCommand>>>,
        responses: Arc<Mutex<Vec<MockResponse>>>,
    }

    impl MockSpriteRunner {
        fn new() -> Self {
            Self {
                commands: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn queue_response(&self, stdout: &str, stderr: &str, exit_code: i32) {
            self.responses.lock().unwrap().push(MockResponse {
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
                exit_code,
            });
        }

        fn pop_response(&self) -> MockResponse {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                MockResponse {
                    stdout: String::new(),
                    stderr: "no mock response queued".to_string(),
                    exit_code: 1,
                }
            } else {
                responses.remove(0)
            }
        }
    }

    #[async_trait]
    impl SpriteRunner for MockSpriteRunner {
        async fn run(&self, args: &[&str]) -> Result<SpriteOutput, String> {
            self.commands.lock().unwrap().push(RecordedCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
            });
            let resp = self.pop_response();
            Ok(SpriteOutput {
                stdout: resp.stdout,
                stderr: resp.stderr,
                exit_code: resp.exit_code,
            })
        }

        async fn run_with_timeout(
            &self,
            args: &[&str],
            _timeout: std::time::Duration,
        ) -> Result<SpriteOutput, String> {
            self.commands.lock().unwrap().push(RecordedCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
            });
            let resp = self.pop_response();
            if resp.exit_code == -99 {
                return Err("Command timed out".to_string());
            }
            Ok(SpriteOutput {
                stdout: resp.stdout,
                stderr: resp.stderr,
                exit_code: resp.exit_code,
            })
        }
    }

    /// Helper: create a SpritesSandbox with sprite_name pre-set (skipping lifecycle).
    fn sandbox_with_mock(runner: MockSpriteRunner) -> SpritesSandbox {
        let config = SpritesConfig {
            sprite_name: Some("test-sprite".to_string()),
            ..Default::default()
        };
        let sandbox = SpritesSandbox::new(Box::new(runner), config);
        let _ = sandbox.sprite_name.set("test-sprite".to_string());
        sandbox
    }

    // ---- Metadata ----

    #[test]
    fn working_directory_returns_home_sprite() {
        let sandbox = sandbox_with_mock(MockSpriteRunner::new());
        assert_eq!(sandbox.working_directory(), "/home/sprite");
    }

    #[test]
    fn platform_returns_linux() {
        let sandbox = sandbox_with_mock(MockSpriteRunner::new());
        assert_eq!(sandbox.platform(), "linux");
    }

    #[test]
    fn os_version_returns_linux_sprites() {
        let sandbox = sandbox_with_mock(MockSpriteRunner::new());
        assert_eq!(sandbox.os_version(), "Linux (Sprites)");
    }

    #[test]
    fn sandbox_info_returns_sprite_name() {
        let sandbox = sandbox_with_mock(MockSpriteRunner::new());
        assert_eq!(sandbox.sandbox_info(), "test-sprite");
    }

    // ---- exec_command ----

    #[tokio::test]
    async fn exec_command_runs_via_sprite_exec() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        runner.queue_response("hello world\n", "", 0);
        let sandbox = sandbox_with_mock(runner);

        let result = sandbox
            .exec_command("echo hello world", 5000, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.stdout.trim(), "hello world");
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);

        let recorded = commands.lock().unwrap();
        assert_eq!(recorded[0].args[0], "exec");
        assert_eq!(recorded[0].args[1], "-s");
        assert_eq!(recorded[0].args[2], "test-sprite");
        assert_eq!(recorded[0].args[3], "--");
        assert_eq!(recorded[0].args[4], "bash");
        assert_eq!(recorded[0].args[5], "-c");
    }

    #[tokio::test]
    async fn exec_command_with_working_dir() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        runner.queue_response("", "", 0);
        let sandbox = sandbox_with_mock(runner);

        sandbox
            .exec_command("ls", 5000, Some("/tmp/work"), None, None)
            .await
            .unwrap();

        let recorded = commands.lock().unwrap();
        let cmd = recorded[0].args.last().unwrap();
        assert!(
            cmd.contains("cd /tmp/work"),
            "expected cd to working dir, got: {cmd}",
        );
    }

    #[tokio::test]
    async fn exec_command_with_env_vars() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        runner.queue_response("", "", 0);
        let sandbox = sandbox_with_mock(runner);

        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());

        sandbox
            .exec_command("echo $FOO", 5000, None, Some(&env), None)
            .await
            .unwrap();

        let recorded = commands.lock().unwrap();
        let cmd = recorded[0].args.last().unwrap();
        assert!(
            cmd.contains("export FOO=bar;"),
            "expected env var export, got: {cmd}",
        );
    }

    #[tokio::test]
    async fn exec_command_timeout() {
        let runner = MockSpriteRunner::new();
        runner.queue_response("", "", -99);
        let sandbox = sandbox_with_mock(runner);

        let result = sandbox
            .exec_command("sleep 999", 100, None, None, None)
            .await
            .unwrap();

        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
    }

    // ---- read_file ----

    #[tokio::test]
    async fn read_file_returns_numbered_lines() {
        let runner = MockSpriteRunner::new();
        runner.queue_response("line one\nline two\nline three\n", "", 0);
        let sandbox = sandbox_with_mock(runner);

        let content = sandbox.read_file("test.txt", None, None).await.unwrap();
        assert!(content.contains("1 | line one"));
        assert!(content.contains("2 | line two"));
        assert!(content.contains("3 | line three"));
    }

    #[tokio::test]
    async fn read_file_with_offset_and_limit() {
        let runner = MockSpriteRunner::new();
        runner.queue_response("a\nb\nc\nd\ne\n", "", 0);
        let sandbox = sandbox_with_mock(runner);

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
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        runner.queue_response("content\n", "", 0);
        let sandbox = sandbox_with_mock(runner);

        sandbox.read_file("/etc/hosts", None, None).await.unwrap();

        let recorded = commands.lock().unwrap();
        let cmd = recorded[0].args.last().unwrap();
        assert!(
            cmd.contains("cat /etc/hosts"),
            "expected absolute path in cat, got: {cmd}",
        );
        assert!(
            !cmd.contains("/home/sprite/etc"),
            "should not prepend working dir for absolute path, got: {cmd}",
        );
    }

    // ---- write_file ----

    #[tokio::test]
    async fn write_file_creates_parent_and_uploads() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        // Response for mkdir -p
        runner.queue_response("", "", 0);
        // Response for base64 write
        runner.queue_response("", "", 0);
        let sandbox = sandbox_with_mock(runner);

        sandbox
            .write_file("deep/nested/file.txt", "hello")
            .await
            .unwrap();

        let recorded = commands.lock().unwrap();
        let mkdir_cmd = recorded[0].args.last().unwrap();
        assert!(
            mkdir_cmd.contains("mkdir -p"),
            "expected mkdir -p, got: {mkdir_cmd}",
        );
        assert!(
            mkdir_cmd.contains("/home/sprite/deep/nested"),
            "expected parent path, got: {mkdir_cmd}",
        );

        let write_cmd = recorded[1].args.last().unwrap();
        assert!(
            write_cmd.contains("base64 -d"),
            "expected base64 decode, got: {write_cmd}",
        );
        assert!(
            write_cmd.contains("/home/sprite/deep/nested/file.txt"),
            "expected file path, got: {write_cmd}",
        );
    }

    // ---- delete_file ----

    #[tokio::test]
    async fn delete_file_runs_rm() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        runner.queue_response("", "", 0);
        let sandbox = sandbox_with_mock(runner);

        sandbox.delete_file("old.txt").await.unwrap();

        let recorded = commands.lock().unwrap();
        let cmd = recorded[0].args.last().unwrap();
        assert!(cmd.contains("rm -f"), "expected rm -f, got: {cmd}",);
    }

    // ---- file_exists ----

    #[tokio::test]
    async fn file_exists_true() {
        let runner = MockSpriteRunner::new();
        runner.queue_response("", "", 0);
        let sandbox = sandbox_with_mock(runner);

        assert!(sandbox.file_exists("exists.txt").await.unwrap());
    }

    #[tokio::test]
    async fn file_exists_false() {
        let runner = MockSpriteRunner::new();
        runner.queue_response("", "", 1);
        let sandbox = sandbox_with_mock(runner);

        assert!(!sandbox.file_exists("missing.txt").await.unwrap());
    }

    // ---- list_directory ----

    #[tokio::test]
    async fn list_directory_parses_find_output() {
        let runner = MockSpriteRunner::new();
        runner.queue_response(
            "f\t1024\tfile.txt\nd\t4096\tsrc\nf\t512\tREADME.md\n",
            "",
            0,
        );
        let sandbox = sandbox_with_mock(runner);

        let entries = sandbox.list_directory(".", None).await.unwrap();
        assert_eq!(entries.len(), 3);
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
        let runner = MockSpriteRunner::new();
        // rg --version check
        runner.queue_response("ripgrep 14.0.0", "", 0);
        // actual grep
        runner.queue_response(
            "src/main.rs:1:fn main() {}\nsrc/lib.rs:5:fn helper() {}\n",
            "",
            0,
        );
        let sandbox = sandbox_with_mock(runner);

        let results = sandbox
            .grep("fn ", ".", &GrepOptions::default())
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].contains("main.rs"));
    }

    #[tokio::test]
    async fn grep_no_matches_returns_empty() {
        let runner = MockSpriteRunner::new();
        // rg --version
        runner.queue_response("ripgrep 14.0.0", "", 0);
        // grep with no matches (exit code 1)
        runner.queue_response("", "", 1);
        let sandbox = sandbox_with_mock(runner);

        let results = sandbox
            .grep("nonexistent", ".", &GrepOptions::default())
            .await
            .unwrap();

        assert!(results.is_empty());
    }

    // ---- glob ----

    #[tokio::test]
    async fn glob_finds_files() {
        let runner = MockSpriteRunner::new();
        runner.queue_response("/home/sprite/src/main.rs\n/home/sprite/src/lib.rs\n", "", 0);
        let sandbox = sandbox_with_mock(runner);

        let results = sandbox.glob("*.rs", Some("src")).await.unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].contains("main.rs"));
    }

    // ---- download_file_to_local ----

    #[tokio::test]
    async fn download_file_to_local_writes_bytes() {
        let runner = MockSpriteRunner::new();
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"binary content");
        runner.queue_response(&encoded, "", 0);
        let sandbox = sandbox_with_mock(runner);

        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("downloaded.bin");
        sandbox
            .download_file_to_local("artifact.bin", &local)
            .await
            .unwrap();

        let bytes = tokio::fs::read(&local).await.unwrap();
        assert_eq!(bytes, b"binary content");
    }

    // ---- initialize ----

    #[tokio::test]
    async fn initialize_creates_sprite() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        // create response
        runner.queue_response("", "", 0);
        // url response
        runner.queue_response("https://arc-test.sprites.dev/", "", 0);

        let config = SpritesConfig::default();
        let sandbox = SpritesSandbox::new(Box::new(runner), config);

        sandbox.initialize().await.unwrap();

        let name = sandbox.sandbox_info();
        assert!(
            name.starts_with("fabro-"),
            "expected name to start with arc-, got: {name}",
        );

        let recorded = commands.lock().unwrap();
        assert!(
            recorded[0].args.contains(&"create".to_string()),
            "expected create command, got: {:?}",
            recorded[0].args,
        );
    }

    #[tokio::test]
    async fn initialize_emits_events() {
        let runner = MockSpriteRunner::new();
        runner.queue_response("", "", 0);
        runner.queue_response("https://arc-test.sprites.dev/", "", 0);

        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let events_cb = Arc::clone(&events);

        let config = SpritesConfig::default();
        let mut sandbox = SpritesSandbox::new(Box::new(runner), config);
        sandbox.set_event_callback(Arc::new(move |event| {
            events_cb.lock().unwrap().push(format!("{event:?}"));
        }));

        sandbox.initialize().await.unwrap();

        let captured = events.lock().unwrap();
        assert!(
            captured.iter().any(|e| e.contains("Initializing")),
            "expected Initializing event, got: {captured:?}",
        );
        assert!(
            captured.iter().any(|e| e.contains("Ready")),
            "expected Ready event, got: {captured:?}",
        );
    }

    #[tokio::test]
    async fn initialize_gets_preview_url() {
        let runner = MockSpriteRunner::new();
        runner.queue_response("", "", 0);
        runner.queue_response("https://my-sprite.sprites.dev/\n", "", 0);

        let config = SpritesConfig::default();
        let sandbox = SpritesSandbox::new(Box::new(runner), config);

        sandbox.initialize().await.unwrap();

        assert_eq!(
            sandbox.preview_url.get().unwrap(),
            "https://my-sprite.sprites.dev/",
        );
    }

    #[tokio::test]
    async fn initialize_with_org() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        runner.queue_response("", "", 0);
        runner.queue_response("https://arc-test-myorg.sprites.dev/", "", 0);

        let config = SpritesConfig {
            org: Some("myorg".to_string()),
            ..Default::default()
        };
        let sandbox = SpritesSandbox::new(Box::new(runner), config);

        sandbox.initialize().await.unwrap();

        let recorded = commands.lock().unwrap();
        assert!(
            recorded[0].args.contains(&"-o".to_string()),
            "expected -o in create args, got: {:?}",
            recorded[0].args,
        );
        assert!(
            recorded[0].args.contains(&"myorg".to_string()),
            "expected myorg in create args, got: {:?}",
            recorded[0].args,
        );
    }

    #[tokio::test]
    async fn initialize_sets_url_auth() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        runner.queue_response("", "", 0);
        runner.queue_response("https://arc-test.sprites.dev/", "", 0);
        runner.queue_response("", "", 0);

        let config = SpritesConfig {
            url_auth: Some("public".to_string()),
            ..Default::default()
        };
        let sandbox = SpritesSandbox::new(Box::new(runner), config);

        sandbox.initialize().await.unwrap();

        let recorded = commands.lock().unwrap();
        assert!(
            recorded[2].args.contains(&"update".to_string()),
            "expected url update command, got: {:?}",
            recorded[2].args,
        );
        assert!(
            recorded[2].args.contains(&"--auth".to_string()),
            "expected --auth flag, got: {:?}",
            recorded[2].args,
        );
        assert!(
            recorded[2].args.contains(&"public".to_string()),
            "expected public auth mode, got: {:?}",
            recorded[2].args,
        );
    }

    #[tokio::test]
    async fn initialize_reuses_existing_sprite() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        // Only url response needed (no create)
        runner.queue_response("https://test-sprite.sprites.dev/", "", 0);

        let config = SpritesConfig {
            sprite_name: Some("test-sprite".to_string()),
            ..Default::default()
        };
        let sandbox = SpritesSandbox::new(Box::new(runner), config);

        sandbox.initialize().await.unwrap();

        assert_eq!(sandbox.sandbox_info(), "test-sprite");

        let recorded = commands.lock().unwrap();
        assert!(
            !recorded
                .iter()
                .any(|c| c.args.contains(&"create".to_string())),
            "should not create when reusing existing sprite",
        );
    }

    // ---- cleanup ----

    #[tokio::test]
    async fn cleanup_destroys_sprite() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();
        runner.queue_response("", "", 0);

        let config = SpritesConfig::default();
        let sandbox = SpritesSandbox::new(Box::new(runner), config);
        let _ = sandbox.sprite_name.set("test-sprite".to_string());

        sandbox.cleanup().await.unwrap();

        let recorded = commands.lock().unwrap();
        assert!(
            recorded[0].args.contains(&"destroy".to_string()),
            "expected destroy command, got: {:?}",
            recorded[0].args,
        );
        assert!(
            recorded[0].args.contains(&"test-sprite".to_string()),
            "expected sprite name in destroy args, got: {:?}",
            recorded[0].args,
        );
    }

    #[tokio::test]
    async fn cleanup_before_initialize_is_noop() {
        let runner = MockSpriteRunner::new();
        let config = SpritesConfig::default();
        let sandbox = SpritesSandbox::new(Box::new(runner), config);
        // Should not error — no sprite to destroy
        sandbox.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn cleanup_preserves_existing_sprite() {
        let runner = MockSpriteRunner::new();
        let commands = runner.commands.clone();

        let config = SpritesConfig {
            sprite_name: Some("test-sprite".to_string()),
            ..Default::default()
        };
        let sandbox = SpritesSandbox::new(Box::new(runner), config);
        let _ = sandbox.sprite_name.set("test-sprite".to_string());

        sandbox.cleanup().await.unwrap();

        let recorded = commands.lock().unwrap();
        assert!(
            recorded.is_empty(),
            "should not destroy preserved sprite, got: {recorded:?}",
        );
    }
}
