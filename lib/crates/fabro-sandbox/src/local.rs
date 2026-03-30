use crate::{
    DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback,
    format_lines_numbered,
};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::time;
use tokio_util::sync::CancellationToken;

pub struct LocalSandbox {
    working_directory: PathBuf,
    event_callback: Option<SandboxEventCallback>,
    rg_available: std::sync::OnceLock<bool>,
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
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "LANG",
        "TERM",
        "TMPDIR",
        "GOPATH",
        "CARGO_HOME",
        "NVM_DIR",
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
}

#[async_trait]
impl Sandbox for LocalSandbox {
    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        let full_path = self.resolve_path(path);
        let content = fs::read_to_string(&full_path)
            .await
            .map_err(|e| format!("Failed to read {}: {e}", full_path.display()))?;

        Ok(format_lines_numbered(&content, offset, limit))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        let full_path = self.resolve_path(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent dirs: {e}"))?;
        }
        fs::write(&full_path, content)
            .await
            .map_err(|e| format!("Failed to write {}: {e}", full_path.display()))
    }

    async fn delete_file(&self, path: &str) -> Result<(), String> {
        let full_path = self.resolve_path(path);
        fs::remove_file(&full_path)
            .await
            .map_err(|e| format!("Failed to delete {}: {e}", full_path.display()))
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        let full_path = self.resolve_path(path);
        Ok(full_path.exists())
    }

    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        fn list_recursive(
            base: &std::path::Path,
            prefix: &str,
            current_depth: usize,
            max_depth: usize,
            entries: &mut Vec<DirEntry>,
        ) -> Result<(), String> {
            let mut dir_entries: Vec<std::fs::DirEntry> = std::fs::read_dir(base)
                .map_err(|e| format!("Failed to read directory {}: {e}", base.display()))?
                .filter_map(std::result::Result::ok)
                .collect();
            dir_entries.sort_by_key(std::fs::DirEntry::file_name);

            for entry in dir_entries {
                let metadata = entry
                    .metadata()
                    .map_err(|e| format!("Failed to read metadata: {e}"))?;
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
        let mut entries = Vec::new();
        list_recursive(&full_path, "", 0, max_depth, &mut entries)?;
        Ok(entries)
    }

    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&std::collections::HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        let start = Instant::now();

        let mut filtered_env: Vec<(String, String)> = std::env::vars()
            .filter(|(key, _)| !Self::should_filter_env_var(key))
            .collect();

        if let Some(extra) = env_vars {
            for (k, v) in extra {
                filtered_env.push((k.clone(), v.clone()));
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
        // SAFETY: setpgid(0, 0) is safe to call in a pre_exec hook — it places
        // the child into its own process group so we can signal the whole group.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn command: {e}"))?;

        let timeout_duration = std::time::Duration::from_millis(timeout_ms);
        let token = cancel_token.unwrap_or_default();

        // Take stdout/stderr handles before entering the select! so we can
        // drain them concurrently.  Without this, the child can deadlock: if
        // it writes more than the OS pipe buffer (~64 KB) the write() syscall
        // blocks until the parent drains the pipe, but the parent is blocked
        // on child.wait().
        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let stdout_task = tokio::spawn(async move {
            let mut buf = String::new();
            if let Some(ref mut r) = stdout_pipe {
                let _ = r.read_to_string(&mut buf).await;
            }
            buf
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            if let Some(ref mut r) = stderr_pipe {
                let _ = r.read_to_string(&mut buf).await;
            }
            buf
        });

        let (timed_out, exit_code) = tokio::select! {
            status_result = child.wait() => {
                let status = status_result.map_err(|e| format!("Failed to wait for process: {e}"))?;
                (false, status.code().unwrap_or(-1))
            }
            () = time::sleep(timeout_duration) => {
                sigterm_then_kill(&mut child).await;
                (true, -1)
            }
            () = token.cancelled() => {
                sigterm_then_kill(&mut child).await;
                (true, -1)
            }
        };

        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        let stdout_str = stdout_task.await.unwrap_or_default();
        let stderr_str = stderr_task.await.unwrap_or_default();

        Ok(ExecResult {
            stdout: stdout_str,
            stderr: stderr_str,
            exit_code,
            timed_out,
            duration_ms,
        })
    }

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> Result<Vec<String>, String> {
        let full_path = self.resolve_path(path);

        // Try rg (ripgrep) first, fall back to grep
        let use_rg = *self.rg_available.get_or_init(|| {
            std::process::Command::new("rg")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        });

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

            std::process::Command::new("rg")
                .args(&args)
                .output()
                .map_err(|e| format!("Failed to run rg: {e}"))?
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

            std::process::Command::new("grep")
                .args(&args)
                .output()
                .map_err(|e| format!("Failed to run grep: {e}"))?
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let results: Vec<String> = stdout
            .lines()
            .map(String::from)
            .filter(|l| !l.is_empty())
            .collect();
        Ok(results)
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String> {
        let base_dir =
            path.map_or_else(|| self.working_directory.clone(), std::path::PathBuf::from);

        let full_pattern = if Path::new(pattern).is_absolute() {
            pattern.to_string()
        } else {
            format!("{}/{pattern}", base_dir.display())
        };

        let mut results: Vec<String> = glob::glob(&full_pattern)
            .map_err(|e| format!("Invalid glob pattern: {e}"))?
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
    ) -> Result<(), String> {
        let full_path = self.resolve_path(remote_path);
        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent dirs: {e}"))?;
        }
        fs::copy(&full_path, local_path).await.map_err(|e| {
            format!(
                "Failed to copy {} to {}: {e}",
                full_path.display(),
                local_path.display()
            )
        })?;
        Ok(())
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<(), String> {
        let full_path = self.resolve_path(remote_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent dirs: {e}"))?;
        }
        fs::copy(local_path, &full_path).await.map_err(|e| {
            format!(
                "Failed to copy {} to {}: {e}",
                local_path.display(),
                full_path.display()
            )
        })?;
        Ok(())
    }

    async fn initialize(&self) -> Result<(), String> {
        self.emit(SandboxEvent::Initializing {
            provider: "local".into(),
        });
        let start = Instant::now();
        let result = fs::create_dir_all(&self.working_directory)
            .await
            .map_err(|e| format!("Failed to create working directory: {e}"));
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
                error: e.clone(),
                duration_ms,
            }),
        }
        result
    }

    async fn cleanup(&self) -> Result<(), String> {
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

/// Send SIGTERM to the process group, wait 2s for graceful shutdown, then SIGKILL.
#[allow(unsafe_code)]
async fn sigterm_then_kill(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: kill with a negative pid signals the entire process group.
        // The pid is valid because we just obtained it from child.id().
        unsafe {
            libc::kill(-i32::try_from(pid).unwrap(), libc::SIGTERM);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("local_env_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);
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

        assert_eq!(result.exit_code, 42);
        assert!(!result.timed_out);
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

        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
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

    #[test]
    fn env_var_filtering() {
        assert!(LocalSandbox::should_filter_env_var("OPENAI_API_KEY"));
        assert!(LocalSandbox::should_filter_env_var("ANTHROPIC_API_KEY"));
        assert!(LocalSandbox::should_filter_env_var("DB_PASSWORD"));
        assert!(LocalSandbox::should_filter_env_var("AWS_SECRET"));
        assert!(LocalSandbox::should_filter_env_var("AUTH_TOKEN"));
        assert!(LocalSandbox::should_filter_env_var("MY_CREDENTIAL"));
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
        use crate::SandboxEvent;
        use std::sync::{Arc, Mutex};

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
        use crate::SandboxEvent;
        use std::sync::{Arc, Mutex};

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
            .grep(
                "hello",
                "test.txt",
                &GrepOptions {
                    case_insensitive: true,
                    ..Default::default()
                },
            )
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
            .grep(
                "match",
                "test.txt",
                &GrepOptions {
                    max_results: Some(2),
                    ..Default::default()
                },
            )
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
