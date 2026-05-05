use std::path::{Path, PathBuf};
use std::time::Instant;

use async_trait::async_trait;
use fabro_static::EnvVars;
use fabro_types::{CommandOutputStream, CommandTermination};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::task::spawn_blocking;
use tokio::{fs, time};
use tokio_util::sync::CancellationToken;

use crate::{
    CommandOutputCallback, DirEntry, ExecResult, ExecStreamingResult, GrepOptions, Sandbox,
    SandboxEvent, SandboxEventCallback, format_lines_numbered,
};

pub struct LocalSandbox {
    working_directory: PathBuf,
    event_callback:    Option<SandboxEventCallback>,
    rg_available:      std::sync::OnceLock<bool>,
}

impl LocalSandbox {
    #[must_use]
    pub fn new(working_directory: PathBuf) -> Self {
        Self {
            working_directory,
            event_callback: None,
            rg_available: std::sync::OnceLock::new(),
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

    const ENV_SAFELIST: &'static [&'static str] = &[
        EnvVars::PATH,
        EnvVars::HOME,
        EnvVars::USER,
        EnvVars::SHELL,
        EnvVars::LANG,
        EnvVars::TERM,
        EnvVars::TMPDIR,
        EnvVars::GOPATH,
        EnvVars::CARGO_HOME,
        EnvVars::NVM_DIR,
    ];

    fn should_filter_env_var(key: &str) -> bool {
        if Self::ENV_SAFELIST.contains(&key) {
            return false;
        }
        let lower = key.to_lowercase();
        lower.ends_with("_api_key")
            || lower.ends_with("_secret")
            || lower.ends_with("_token")
            || lower.ends_with("_password")
            || lower.ends_with("_credential")
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.working_directory.join(p)
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "Local sandbox command execution checks PATH/PATHEXT to select optional helpers."
    )]
    fn binary_on_path(binary: &str) -> bool {
        let Some(paths) = std::env::var_os(EnvVars::PATH) else {
            return false;
        };

        #[cfg(windows)]
        let extensions: Vec<String> = std::env::var_os(EnvVars::PATHEXT)
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .map(|ext| ext.to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_else(|| vec![".exe".to_string(), ".cmd".to_string(), ".bat".to_string()]);

        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(binary);
            if candidate.is_file() {
                return true;
            }

            #[cfg(windows)]
            {
                if candidate.extension().is_none() {
                    for ext in &extensions {
                        if dir.join(format!("{binary}{ext}")).is_file() {
                            return true;
                        }
                    }
                }
            }
        }

        false
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Local sandbox must snapshot the ambient process env before applying its fail-closed filter."
)]
fn process_env_vars() -> Vec<(String, String)> {
    std::env::vars().collect()
}

async fn drain_pipe<R>(mut pipe: Option<R>, stream: CommandOutputStream) -> String
where
    R: AsyncRead + Unpin,
{
    let mut buf = String::new();
    if let Some(ref mut reader) = pipe {
        if let Err(err) = reader.read_to_string(&mut buf).await {
            tracing::warn!(error = %err, ?stream, "Failed to drain child output");
        }
    }
    buf
}

#[async_trait]
impl Sandbox for LocalSandbox {
    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> crate::Result<String> {
        let full_path = self.resolve_path(path);
        let content = fs::read_to_string(&full_path).await.map_err(|e| {
            crate::Error::context(format!("Failed to read {}", full_path.display()), e)
        })?;

        Ok(format_lines_numbered(&content, offset, limit))
    }

    async fn write_file(&self, path: &str, content: &str) -> crate::Result<()> {
        let full_path = self.resolve_path(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| crate::Error::context("Failed to create parent dirs", e))?;
        }
        fs::write(&full_path, content).await.map_err(|e| {
            crate::Error::context(format!("Failed to write {}", full_path.display()), e)
        })
    }

    async fn delete_file(&self, path: &str) -> crate::Result<()> {
        let full_path = self.resolve_path(path);
        fs::remove_file(&full_path).await.map_err(|e| {
            crate::Error::context(format!("Failed to delete {}", full_path.display()), e)
        })
    }

    async fn file_exists(&self, path: &str) -> crate::Result<bool> {
        let full_path = self.resolve_path(path);
        Ok(full_path.exists())
    }

    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> crate::Result<Vec<DirEntry>> {
        #[expect(
            clippy::disallowed_methods,
            reason = "sync recursive read_dir; caller wraps invocation in tokio::task::spawn_blocking"
        )]
        fn list_recursive(
            base: &std::path::Path,
            prefix: &str,
            current_depth: usize,
            max_depth: usize,
            entries: &mut Vec<DirEntry>,
        ) -> crate::Result<()> {
            let mut dir_entries: Vec<std::fs::DirEntry> = std::fs::read_dir(base)
                .map_err(|e| {
                    crate::Error::context(format!("Failed to read directory {}", base.display()), e)
                })?
                .filter_map(std::result::Result::ok)
                .collect();
            dir_entries.sort_by_key(std::fs::DirEntry::file_name);

            for entry in dir_entries {
                let metadata = entry
                    .metadata()
                    .map_err(|e| crate::Error::context("Failed to read metadata", e))?;
                let name = if prefix.is_empty() {
                    entry.file_name().to_string_lossy().into_owned()
                } else {
                    format!("{prefix}/{}", entry.file_name().to_string_lossy())
                };
                let is_dir = metadata.is_dir();
                entries.push(DirEntry {
                    name: name.clone(),
                    is_dir,
                    size: if metadata.is_file() {
                        Some(metadata.len())
                    } else {
                        None
                    },
                });
                if is_dir && current_depth + 1 < max_depth {
                    list_recursive(&entry.path(), &name, current_depth + 1, max_depth, entries)?;
                }
            }
            Ok(())
        }

        let full_path = self.resolve_path(path);
        let max_depth = depth.unwrap_or(1);
        spawn_blocking(move || {
            let mut entries = Vec::new();
            list_recursive(&full_path, "", 0, max_depth, &mut entries)?;
            Ok(entries)
        })
        .await
        .map_err(|e| crate::Error::context("list_directory task failed", e))?
    }

    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&std::collections::HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> crate::Result<ExecResult> {
        let start = Instant::now();

        let mut filtered_env: Vec<(String, String)> = process_env_vars()
            .into_iter()
            .filter(|(key, _)| !Self::should_filter_env_var(key))
            .collect();

        if let Some(extra) = env_vars {
            for (k, v) in extra {
                if !Self::should_filter_env_var(k) {
                    filtered_env.push((k.clone(), v.clone()));
                }
            }
        }

        let effective_dir =
            working_dir.map_or_else(|| self.working_directory.clone(), std::path::PathBuf::from);

        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&effective_dir)
            .env_clear()
            .envs(filtered_env)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        #[cfg(unix)]
        fabro_proc::pre_exec_setpgid(cmd.as_std_mut());

        let mut child = cmd
            .spawn()
            .map_err(|e| crate::Error::context("Failed to spawn command", e))?;

        let timeout_duration = std::time::Duration::from_millis(timeout_ms);
        let token = cancel_token.unwrap_or_default();

        // Take stdout/stderr handles before entering the select! so we can
        // drain them concurrently.  Without this, the child can deadlock: if
        // it writes more than the OS pipe buffer (~64 KB) the write() syscall
        // blocks until the parent drains the pipe, but the parent is blocked
        // on child.wait().
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_task =
            tokio::spawn(async move { drain_pipe(stdout_pipe, CommandOutputStream::Stdout).await });
        let stderr_task =
            tokio::spawn(async move { drain_pipe(stderr_pipe, CommandOutputStream::Stderr).await });

        let (termination, exit_code) = tokio::select! {
            status_result = child.wait() => {
                let status = status_result
                    .map_err(|e| crate::Error::context("Failed to wait for process", e))?;
                (CommandTermination::Exited, status.code())
            }
            () = time::sleep(timeout_duration) => {
                sigterm_then_kill(&mut child).await;
                (CommandTermination::TimedOut, None)
            }
            () = token.cancelled() => {
                sigterm_then_kill(&mut child).await;
                (CommandTermination::Cancelled, None)
            }
        };

        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        let stdout_str = stdout_task.await.unwrap_or_default();
        let stderr_str = stderr_task.await.unwrap_or_default();

        Ok(ExecResult {
            stdout: stdout_str,
            stderr: stderr_str,
            exit_code,
            termination,
            duration_ms,
        })
    }

    async fn exec_command_streaming(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&std::collections::HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
        output_callback: CommandOutputCallback,
    ) -> crate::Result<ExecStreamingResult> {
        let start = Instant::now();

        let mut filtered_env: Vec<(String, String)> = process_env_vars()
            .into_iter()
            .filter(|(key, _)| !Self::should_filter_env_var(key))
            .collect();

        if let Some(extra) = env_vars {
            for (k, v) in extra {
                if !Self::should_filter_env_var(k) {
                    filtered_env.push((k.clone(), v.clone()));
                }
            }
        }

        let effective_dir =
            working_dir.map_or_else(|| self.working_directory.clone(), std::path::PathBuf::from);

        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&effective_dir)
            .env_clear()
            .envs(filtered_env)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        #[cfg(unix)]
        fabro_proc::pre_exec_setpgid(cmd.as_std_mut());

        let mut child = cmd
            .spawn()
            .map_err(|e| crate::Error::context("Failed to spawn command", e))?;

        let timeout_duration = std::time::Duration::from_millis(timeout_ms);
        let token = cancel_token.unwrap_or_default();

        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_callback = output_callback.clone();
        let stderr_callback = output_callback;
        let stdout_task = tokio::spawn(async move {
            drain_command_pipe(stdout_pipe, CommandOutputStream::Stdout, stdout_callback).await
        });
        let stderr_task = tokio::spawn(async move {
            drain_command_pipe(stderr_pipe, CommandOutputStream::Stderr, stderr_callback).await
        });

        let (termination, exit_code) = tokio::select! {
            status_result = child.wait() => {
                let status = status_result
                    .map_err(|e| crate::Error::context("Failed to wait for process", e))?;
                (CommandTermination::Exited, status.code())
            }
            () = time::sleep(timeout_duration) => {
                sigterm_then_kill(&mut child).await;
                (CommandTermination::TimedOut, None)
            }
            () = token.cancelled() => {
                sigterm_then_kill(&mut child).await;
                (CommandTermination::Cancelled, None)
            }
        };

        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let stdout_bytes = stdout_task
            .await
            .map_err(|e| crate::Error::context("stdout stream task failed", e))??;
        let stderr_bytes = stderr_task
            .await
            .map_err(|e| crate::Error::context("stderr stream task failed", e))??;

        Ok(ExecStreamingResult {
            result:            ExecResult {
                stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
                stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
                exit_code,
                termination,
                duration_ms,
            },
            streams_separated: true,
            live_streaming:    true,
        })
    }

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> crate::Result<Vec<String>> {
        let full_path = self.resolve_path(path);

        // Try rg (ripgrep) first, fall back to grep
        let use_rg = *self.rg_available.get_or_init(|| Self::binary_on_path("rg"));

        let output = if use_rg {
            let mut args = vec!["-n".to_string()];
            if options.case_insensitive {
                args.push("-i".into());
            }
            if let Some(ref glob_filter) = options.glob_filter {
                args.push("--glob".into());
                args.push(glob_filter.clone());
            }
            if let Some(max) = options.max_results {
                args.push("-m".into());
                args.push(max.to_string());
            }
            args.push(pattern.into());
            args.push(full_path.to_string_lossy().into_owned());

            Command::new("rg")
                .args(&args)
                .output()
                .await
                .map_err(|e| crate::Error::context("Failed to run rg", e))?
        } else {
            let mut args = vec!["-rn".to_string()];
            if options.case_insensitive {
                args.push("-i".into());
            }
            if let Some(ref glob_filter) = options.glob_filter {
                args.push("--include".into());
                args.push(glob_filter.clone());
            }
            if let Some(max) = options.max_results {
                args.push("-m".into());
                args.push(max.to_string());
            }
            args.push(pattern.into());
            args.push(full_path.to_string_lossy().into_owned());

            Command::new("grep")
                .args(&args)
                .output()
                .await
                .map_err(|e| crate::Error::context("Failed to run grep", e))?
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let results: Vec<String> = stdout
            .lines()
            .map(String::from)
            .filter(|l| !l.is_empty())
            .collect();
        Ok(results)
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> crate::Result<Vec<String>> {
        let base_dir =
            path.map_or_else(|| self.working_directory.clone(), std::path::PathBuf::from);

        let full_pattern = if Path::new(pattern).is_absolute() {
            pattern.to_string()
        } else {
            format!("{}/{pattern}", base_dir.display())
        };

        let mut results: Vec<String> = glob::glob(&full_pattern)
            .map_err(|e| crate::Error::context("Invalid glob pattern", e))?
            .filter_map(Result::ok)
            .map(|p| p.to_string_lossy().into_owned())
            .collect();

        // Sort by mtime (newest first), caching metadata to avoid O(n log n) syscalls
        results.sort_by_cached_key(|path| {
            std::cmp::Reverse(
                std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            )
        });

        Ok(results)
    }

    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> crate::Result<()> {
        let full_path = self.resolve_path(remote_path);
        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| crate::Error::context("Failed to create parent dirs", e))?;
        }
        fs::copy(&full_path, local_path).await.map_err(|e| {
            crate::Error::context(
                format!(
                    "Failed to copy {} to {}",
                    full_path.display(),
                    local_path.display()
                ),
                e,
            )
        })?;
        Ok(())
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> crate::Result<()> {
        let full_path = self.resolve_path(remote_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| crate::Error::context("Failed to create parent dirs", e))?;
        }
        fs::copy(local_path, &full_path).await.map_err(|e| {
            crate::Error::context(
                format!(
                    "Failed to copy {} to {}",
                    local_path.display(),
                    full_path.display()
                ),
                e,
            )
        })?;
        Ok(())
    }

    async fn initialize(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::Initializing {
            provider: "local".into(),
        });
        let start = Instant::now();
        let result = fs::create_dir_all(&self.working_directory)
            .await
            .map_err(|e| crate::Error::context("Failed to create working directory", e));
        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        match &result {
            Ok(()) => self.emit(SandboxEvent::Ready {
                provider: "local".into(),
                duration_ms,
                name: None,
                cpu: None,
                memory: None,
                url: None,
            }),
            Err(e) => self.emit(SandboxEvent::InitializeFailed {
                provider: "local".into(),
                error: e.to_string(),
                causes: e.causes(),
                duration_ms,
            }),
        }
        result
    }

    async fn git_push_ref(&self, refspec: &str) -> crate::Result<()> {
        let has_origin = match self
            .exec_command("git remote get-url origin", 10_000, None, None, None)
            .await
        {
            Ok(result) if result.is_success() => true,
            Ok(_) => false,
            Err(err) => return Err(crate::Error::context("git remote get-url origin", err)),
        };
        if !has_origin {
            return Ok(());
        }

        crate::git_push_via_exec(self, refspec).await
    }

    async fn cleanup(&self) -> crate::Result<()> {
        self.emit(SandboxEvent::CleanupStarted {
            provider: "local".into(),
        });
        let start = Instant::now();
        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(SandboxEvent::CleanupCompleted {
            provider: "local".into(),
            duration_ms,
        });
        Ok(())
    }

    fn working_directory(&self) -> &str {
        self.working_directory.to_str().unwrap_or(".")
    }

    fn platform(&self) -> &str {
        if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "unknown"
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "This synchronous host metadata probe only runs uname once while building the sandbox platform string."
    )]
    fn os_version(&self) -> String {
        #[cfg(unix)]
        {
            let output = std::process::Command::new("uname").arg("-r").output();
            match output {
                Ok(out) => {
                    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    format!("{} {version}", self.platform())
                }
                Err(_) => self.platform().to_string(),
            }
        }
        #[cfg(not(unix))]
        {
            self.platform().to_string()
        }
    }
}

/// Send SIGTERM to the process group, wait 2s for graceful shutdown, then
/// SIGKILL.
async fn sigterm_then_kill(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        fabro_proc::sigterm_process_group(pid);
        if time::timeout(std::time::Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    } else {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

async fn drain_command_pipe<R>(
    mut reader: Option<R>,
    stream: CommandOutputStream,
    output_callback: CommandOutputCallback,
) -> crate::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let Some(reader) = reader.as_mut() else {
        return Ok(output);
    };

    let mut buf = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buf)
            .await
            .map_err(|e| crate::Error::context("Failed to read command output", e))?;
        if read == 0 {
            return Ok(output);
        }
        output.extend_from_slice(&buf[..read]);
        output_callback(stream, buf[..read].to_vec()).await?;
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "sandbox tests stage fixtures with sync std::fs writes/reads"
)]
mod tests {
    use std::collections::HashMap;
    use std::io;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::task::{Context as TaskContext, Poll};

    use tokio::io::ReadBuf;

    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("local_env_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn drain_pipe_returns_empty_buffer_after_read_failure() {
        struct FailingReader;

        impl AsyncRead for FailingReader {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut TaskContext<'_>,
                _buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Ready(Err(io::Error::other("simulated read failure")))
            }
        }

        let output = drain_pipe(Some(FailingReader), CommandOutputStream::Stdout).await;

        assert!(output.is_empty());
    }

    #[tokio::test]
    async fn read_file_with_line_numbers() {
        let dir = temp_dir();
        std::fs::write(dir.join("test.txt"), "hello\nworld\nfoo").unwrap();

        let env = LocalSandbox::new(dir.clone());
        let result = env.read_file("test.txt", None, None).await.unwrap();

        assert_eq!(result, "1 | hello\n2 | world\n3 | foo\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn read_file_line_number_padding() {
        let dir = temp_dir();
        let content =
            (1..=12)
                .map(|i| format!("line {i}\n"))
                .fold(String::new(), |mut acc, line| {
                    acc.push_str(&line);
                    acc
                });
        std::fs::write(dir.join("padded.txt"), content.trim_end()).unwrap();

        let env = LocalSandbox::new(dir.clone());
        let result = env.read_file("padded.txt", None, None).await.unwrap();

        assert!(result.starts_with(" 1 | line 1\n"));
        assert!(result.contains("12 | line 12\n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn read_file_not_found() {
        let dir = temp_dir();
        let env = LocalSandbox::new(dir.clone());
        let result = env.read_file("nonexistent.txt", None, None).await;
        assert!(result.is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn write_file_creates_parent_dirs() {
        let dir = temp_dir();
        let env = LocalSandbox::new(dir.clone());
        env.write_file("sub/dir/test.txt", "content").await.unwrap();

        let written = std::fs::read_to_string(dir.join("sub/dir/test.txt")).unwrap();
        assert_eq!(written, "content");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn file_exists_true() {
        let dir = temp_dir();
        std::fs::write(dir.join("exists.txt"), "data").unwrap();

        let env = LocalSandbox::new(dir.clone());
        assert!(env.file_exists("exists.txt").await.unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn file_exists_false() {
        let dir = temp_dir();
        let env = LocalSandbox::new(dir.clone());
        assert!(!env.file_exists("nope.txt").await.unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn list_directory_sorted() {
        let dir = temp_dir();
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        std::fs::create_dir(dir.join("c_dir")).unwrap();

        let env = LocalSandbox::new(dir.clone());
        let entries = env.list_directory(".", None).await.unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "a.txt");
        assert!(!entries[0].is_dir);
        assert!(entries[0].size.is_some());
        assert_eq!(entries[1].name, "b.txt");
        assert_eq!(entries[2].name, "c_dir");
        assert!(entries[2].is_dir);
        assert!(entries[2].size.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn exec_command_echo() {
        let dir = temp_dir();
        let env = LocalSandbox::new(dir.clone());
        let result = env
            .exec_command("echo hello", 5000, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.termination, CommandTermination::Exited);
        assert!(result.duration_ms < 5000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn exec_command_exit_code() {
        let dir = temp_dir();
        let env = LocalSandbox::new(dir.clone());
        let result = env
            .exec_command("exit 42", 5000, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.exit_code, Some(42));
        assert_eq!(result.termination, CommandTermination::Exited);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn exec_command_timeout() {
        let dir = temp_dir();
        let env = LocalSandbox::new(dir.clone());
        let result = env
            .exec_command("sleep 10", 200, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.termination, CommandTermination::TimedOut);
        assert_eq!(result.exit_code, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn exec_command_cancelled() {
        let dir = temp_dir();
        let env = LocalSandbox::new(dir.clone());
        let token = CancellationToken::new();
        token.cancel();
        let result = env
            .exec_command("sleep 10", 5000, None, None, Some(token))
            .await
            .unwrap();

        assert_eq!(result.termination, CommandTermination::Cancelled);
        assert_eq!(result.exit_code, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn exec_command_stderr() {
        let dir = temp_dir();
        let env = LocalSandbox::new(dir.clone());
        let result = env
            .exec_command("echo err >&2", 5000, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.stderr.trim(), "err");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn exec_command_filters_sensitive_explicit_env_vars() {
        let dir = temp_dir();
        let env = LocalSandbox::new(dir.clone());
        let extra = HashMap::from([
            ("FABRO_WORKER_TOKEN".to_string(), "leaked".to_string()),
            ("MY_VAR".to_string(), "ok".to_string()),
        ]);
        let result = env
            .exec_command("env", 5000, None, Some(&extra), None)
            .await
            .unwrap();

        assert!(!result.stdout.contains("FABRO_WORKER_TOKEN=leaked"));
        assert!(result.stdout.contains("MY_VAR=ok"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_var_filtering() {
        assert!(LocalSandbox::should_filter_env_var("OPENAI_API_KEY"));
        assert!(LocalSandbox::should_filter_env_var("ANTHROPIC_API_KEY"));
        assert!(LocalSandbox::should_filter_env_var("DB_PASSWORD"));
        assert!(LocalSandbox::should_filter_env_var("AWS_SECRET"));
        assert!(LocalSandbox::should_filter_env_var("AUTH_TOKEN"));
        assert!(LocalSandbox::should_filter_env_var("MY_CREDENTIAL"));
        assert!(LocalSandbox::should_filter_env_var("FABRO_WORKER_TOKEN"));
        assert!(LocalSandbox::should_filter_env_var("SESSION_SECRET"));
        // Case insensitive
        assert!(LocalSandbox::should_filter_env_var("my_api_key"));
        assert!(LocalSandbox::should_filter_env_var("Some_Secret"));
        // Should not filter
        assert!(!LocalSandbox::should_filter_env_var("PATH"));
        assert!(!LocalSandbox::should_filter_env_var("HOME"));
        assert!(!LocalSandbox::should_filter_env_var("EDITOR"));
        assert!(!LocalSandbox::should_filter_env_var("SECRET_PATH"));
    }

    #[test]
    fn platform_is_known() {
        let env = LocalSandbox::new(PathBuf::from("/tmp"));
        let platform = env.platform();
        assert!(
            platform == "darwin" || platform == "linux" || platform == "windows",
            "Unknown platform: {platform}"
        );
    }

    #[test]
    fn os_version_contains_platform() {
        let env = LocalSandbox::new(PathBuf::from("/tmp"));
        let version = env.os_version();
        assert!(
            version.contains(env.platform()),
            "OS version should contain platform: {version}"
        );
    }

    #[test]
    fn working_directory_accessor() {
        let env = LocalSandbox::new(PathBuf::from("/tmp/test_dir"));
        assert_eq!(env.working_directory(), "/tmp/test_dir");
    }

    #[tokio::test]
    async fn initialize_creates_directory() {
        let dir = std::env::temp_dir().join(format!("init_test_{}", uuid::Uuid::new_v4()));
        let env = LocalSandbox::new(dir.clone());
        env.initialize().await.unwrap();
        assert!(dir.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn initialize_emits_events() {
        use std::sync::{Arc, Mutex};

        use crate::SandboxEvent;

        let dir = std::env::temp_dir().join(format!("init_event_test_{}", uuid::Uuid::new_v4()));
        let events: Arc<Mutex<Vec<SandboxEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        let mut env = LocalSandbox::new(dir.clone());
        env.set_event_callback(Arc::new(move |e| {
            events_clone.lock().unwrap().push(e);
        }));

        env.initialize().await.unwrap();

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(
            matches!(&captured[0], SandboxEvent::Initializing { provider } if provider == "local")
        );
        assert!(
            matches!(&captured[1], SandboxEvent::Ready { provider, .. } if provider == "local")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn cleanup_emits_events() {
        use std::sync::{Arc, Mutex};

        use crate::SandboxEvent;

        let dir = temp_dir();
        let events: Arc<Mutex<Vec<SandboxEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        let mut env = LocalSandbox::new(dir.clone());
        env.set_event_callback(Arc::new(move |e| {
            events_clone.lock().unwrap().push(e);
        }));

        env.cleanup().await.unwrap();

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(
            matches!(&captured[0], SandboxEvent::CleanupStarted { provider } if provider == "local")
        );
        assert!(
            matches!(&captured[1], SandboxEvent::CleanupCompleted { provider, .. } if provider == "local")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn grep_finds_matches() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("test.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let env = LocalSandbox::new(dir.clone());
        let results = env
            .grep("println", "test.rs", &GrepOptions::default())
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].contains("println"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn grep_case_insensitive() {
        let dir = temp_dir();
        std::fs::write(dir.join("test.txt"), "Hello\nhello\nHELLO\n").unwrap();

        let env = LocalSandbox::new(dir.clone());
        let results = env
            .grep("hello", "test.txt", &GrepOptions {
                case_insensitive: true,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn grep_max_results() {
        let dir = temp_dir();
        std::fs::write(dir.join("test.txt"), "match1\nmatch2\nmatch3\nmatch4\n").unwrap();

        let env = LocalSandbox::new(dir.clone());
        let results = env
            .grep("match", "test.txt", &GrepOptions {
                max_results: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn glob_finds_files() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.rs"), "").unwrap();
        std::fs::write(dir.join("b.rs"), "").unwrap();
        std::fs::write(dir.join("c.txt"), "").unwrap();

        let env = LocalSandbox::new(dir.clone());
        let results = env.glob("*.rs", None).await.unwrap();

        assert_eq!(results.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn local_sandbox_download_file_to_local() {
        let dir = temp_dir();
        std::fs::write(dir.join("source.txt"), "hello download").unwrap();

        let env = LocalSandbox::new(dir.clone());
        let dest = dir.join("output/downloaded.txt");
        env.download_file_to_local("source.txt", &dest)
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello download");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn local_sandbox_download_file_to_local_creates_parent_dirs() {
        let dir = temp_dir();
        std::fs::write(dir.join("data.bin"), "binary-ish").unwrap();

        let env = LocalSandbox::new(dir.clone());
        let dest = dir.join("deep/nested/dir/data.bin");
        env.download_file_to_local("data.bin", &dest).await.unwrap();

        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "binary-ish");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn local_sandbox_download_file_to_local_binary() {
        let dir = temp_dir();
        let binary_data: Vec<u8> = (0u8..=255).collect();
        std::fs::write(dir.join("binary.bin"), &binary_data).unwrap();

        let env = LocalSandbox::new(dir.clone());
        let dest = dir.join("out/binary.bin");
        env.download_file_to_local("binary.bin", &dest)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), binary_data);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
