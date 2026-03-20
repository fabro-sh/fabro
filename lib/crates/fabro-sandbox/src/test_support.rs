use crate::*;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

// --- MockSandbox ---

pub struct MockSandbox {
    pub files: HashMap<String, String>,
    pub exec_result: ExecResult,
    pub grep_results: Vec<String>,
    pub glob_results: Vec<String>,
    pub working_dir: &'static str,
    pub platform_str: &'static str,
    pub os_version_str: String,
    /// When true, `read_file` applies offset/limit by splitting on lines.
    pub apply_read_offset_limit: bool,
    /// Captures (path, content) pairs from `write_file` calls.
    pub written_files: Mutex<Vec<(String, String)>>,
    /// Captures the `timeout_ms` argument from `exec_command` calls.
    pub captured_timeout: Mutex<Option<u64>>,
    /// Captures the `command` argument from `exec_command` calls (last only).
    pub captured_command: Mutex<Option<String>>,
    /// Captures all `command` arguments from `exec_command` calls in order.
    pub captured_commands: Mutex<Vec<String>>,
    /// Captures all `working_dir` arguments from `exec_command` calls in order.
    pub captured_working_dirs: Mutex<Vec<Option<String>>>,
    /// Captures the `env_vars` argument from `exec_command` calls.
    pub captured_env_vars: Mutex<Option<HashMap<String, String>>>,
    pub event_callback: Option<SandboxEventCallback>,
}

impl MockSandbox {
    pub fn linux() -> Self {
        Self {
            working_dir: "/home/test",
            platform_str: "linux",
            os_version_str: "Linux 6.1.0".into(),
            ..Default::default()
        }
    }
}

impl MockSandbox {
    fn emit(&self, event: SandboxEvent) {
        event.trace();
        if let Some(ref cb) = self.event_callback {
            cb(event);
        }
    }
}

impl Default for MockSandbox {
    fn default() -> Self {
        Self {
            files: HashMap::new(),
            exec_result: ExecResult {
                stdout: "mock output".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 10,
            },
            grep_results: vec![],
            glob_results: vec![],
            working_dir: "/work",
            platform_str: "darwin",
            os_version_str: "Darwin 24.0.0".into(),
            apply_read_offset_limit: false,
            written_files: Mutex::new(Vec::new()),
            captured_timeout: Mutex::new(None),
            captured_command: Mutex::new(None),
            captured_commands: Mutex::new(Vec::new()),
            captured_working_dirs: Mutex::new(Vec::new()),
            captured_env_vars: Mutex::new(None),
            event_callback: None,
        }
    }
}

#[async_trait]
impl Sandbox for MockSandbox {
    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        let content = self
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("File not found: {path}"))?;

        if self.apply_read_offset_limit {
            let lines: Vec<&str> = content.lines().collect();
            let start = offset.unwrap_or(1).saturating_sub(1);
            let count = limit.unwrap_or(2000);
            let selected: Vec<&str> = lines.into_iter().skip(start).take(count).collect();
            Ok(selected.join("\n"))
        } else {
            Ok(content)
        }
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        self.written_files
            .lock()
            .expect("written_files lock poisoned")
            .push((path.to_string(), content.to_string()));
        Ok(())
    }

    async fn delete_file(&self, _path: &str) -> Result<(), String> {
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        Ok(self.files.contains_key(path))
    }

    async fn list_directory(
        &self,
        _path: &str,
        _depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        Ok(vec![])
    }

    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&std::collections::HashMap<String, String>>,
        _cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        *self
            .captured_timeout
            .lock()
            .expect("captured_timeout lock poisoned") = Some(timeout_ms);
        *self
            .captured_command
            .lock()
            .expect("captured_command lock poisoned") = Some(command.to_string());
        self.captured_commands
            .lock()
            .expect("captured_commands lock poisoned")
            .push(command.to_string());
        self.captured_working_dirs
            .lock()
            .expect("captured_working_dirs lock poisoned")
            .push(working_dir.map(String::from));
        *self
            .captured_env_vars
            .lock()
            .expect("captured_env_vars lock poisoned") = env_vars.cloned();
        Ok(self.exec_result.clone())
    }

    async fn grep(
        &self,
        _pattern: &str,
        _path: &str,
        _options: &GrepOptions,
    ) -> Result<Vec<String>, String> {
        Ok(self.grep_results.clone())
    }

    async fn glob(&self, _pattern: &str, _path: Option<&str>) -> Result<Vec<String>, String> {
        Ok(self.glob_results.clone())
    }

    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &std::path::Path,
    ) -> Result<(), String> {
        let content = self
            .files
            .get(remote_path)
            .ok_or_else(|| format!("File not found: {remote_path}"))?;
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent dirs: {e}"))?;
        }
        tokio::fs::write(local_path, content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write {}: {e}", local_path.display()))?;
        Ok(())
    }

    async fn upload_file_from_local(
        &self,
        local_path: &std::path::Path,
        _remote_path: &str,
    ) -> Result<(), String> {
        if !local_path.exists() {
            return Err(format!("File not found: {}", local_path.display()));
        }
        Ok(())
    }

    async fn initialize(&self) -> Result<(), String> {
        self.emit(SandboxEvent::Initializing {
            provider: "mock".into(),
        });
        self.emit(SandboxEvent::Ready {
            provider: "mock".into(),
            duration_ms: 0,
            name: None,
            cpu: None,
            memory: None,
            url: None,
        });
        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        self.emit(SandboxEvent::CleanupStarted {
            provider: "mock".into(),
        });
        self.emit(SandboxEvent::CleanupCompleted {
            provider: "mock".into(),
            duration_ms: 0,
        });
        Ok(())
    }

    fn working_directory(&self) -> &str {
        self.working_dir
    }

    fn platform(&self) -> &str {
        self.platform_str
    }

    fn os_version(&self) -> String {
        self.os_version_str.clone()
    }
}

// --- MutableMockSandbox ---

/// A mock sandbox with Mutex-protected files for tests that need
/// write operations to be visible to subsequent reads (e.g., `apply_patch` tests).
pub struct MutableMockSandbox {
    pub files: Mutex<HashMap<String, String>>,
}

impl MutableMockSandbox {
    pub fn new(files: HashMap<String, String>) -> Self {
        Self {
            files: Mutex::new(files),
        }
    }
}

#[async_trait]
impl Sandbox for MutableMockSandbox {
    async fn read_file(
        &self,
        path: &str,
        _offset: Option<usize>,
        _limit: Option<usize>,
    ) -> Result<String, String> {
        self.files
            .lock()
            .expect("files lock poisoned")
            .get(path)
            .cloned()
            .ok_or_else(|| format!("File not found: {path}"))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        self.files
            .lock()
            .expect("files lock poisoned")
            .insert(path.to_string(), content.to_string());
        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<(), String> {
        self.files.lock().expect("files lock poisoned").remove(path);
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        Ok(self
            .files
            .lock()
            .expect("files lock poisoned")
            .contains_key(path))
    }

    async fn list_directory(
        &self,
        _path: &str,
        _depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        Ok(vec![])
    }

    async fn exec_command(
        &self,
        _command: &str,
        _timeout_ms: u64,
        _working_dir: Option<&str>,
        _env_vars: Option<&std::collections::HashMap<String, String>>,
        _cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        Ok(ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
            duration_ms: 0,
        })
    }

    async fn grep(
        &self,
        pattern: &str,
        _path: &str,
        _options: &GrepOptions,
    ) -> Result<Vec<String>, String> {
        let files = self.files.lock().expect("files lock poisoned");
        let mut results = Vec::new();
        for (path, content) in files.iter() {
            for (i, line) in content.lines().enumerate() {
                if line.contains(pattern) {
                    results.push(format!("{}:{}:{}", path, i + 1, line));
                }
            }
        }
        Ok(results)
    }

    async fn glob(&self, _pattern: &str, _path: Option<&str>) -> Result<Vec<String>, String> {
        Ok(vec![])
    }

    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &std::path::Path,
    ) -> Result<(), String> {
        let content = self
            .files
            .lock()
            .expect("files lock poisoned")
            .get(remote_path)
            .cloned()
            .ok_or_else(|| format!("File not found: {remote_path}"))?;
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent dirs: {e}"))?;
        }
        tokio::fs::write(local_path, content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write {}: {e}", local_path.display()))?;
        Ok(())
    }

    async fn upload_file_from_local(
        &self,
        local_path: &std::path::Path,
        remote_path: &str,
    ) -> Result<(), String> {
        let content = tokio::fs::read_to_string(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {e}", local_path.display()))?;
        self.files
            .lock()
            .expect("files lock poisoned")
            .insert(remote_path.to_string(), content);
        Ok(())
    }

    async fn initialize(&self) -> Result<(), String> {
        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        Ok(())
    }

    fn working_directory(&self) -> &'static str {
        "/work"
    }

    fn platform(&self) -> &'static str {
        "linux"
    }

    fn os_version(&self) -> String {
        "Linux 6.1.0".into()
    }
}
