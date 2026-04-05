use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use assert_cmd::Command;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};

/// Walk up from `start` to find the repo-level `test/` fixtures directory.
pub fn find_test_fixtures_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let candidate = dir.join("test");
        if candidate.is_dir() {
            return candidate.canonicalize().ok();
        }
        dir = dir.parent()?;
    }
}

/// Static filters applied to every snapshot.
static INSTA_FILTERS: &[(&str, &str)] = &[
    (r"fabro \d+\.\d+\.\d+", "fabro [VERSION]"),
    (r"\([0-9a-f]{7} \d{4}-\d{2}-\d{2}\)", "([BUILD])"),
    (r"\b[0-9A-HJKMNP-TV-Z]{26}\b", "[ULID]"),
    (r"in \d+(\.\d+)?(ms|s)", "in [TIME]"),
    (
        r"\[STORAGE_DIR\]/runs/\d{8}-dry-run-\[ULID\]",
        "[DRY_RUN_DIR]",
    ),
    (r"\[STORAGE_DIR\]/runs/\d{8}-\[ULID\]", "[RUN_DIR]"),
    (
        r"Duration:\s+\d+\s+(seconds?|minutes?|hours?)",
        "Duration:  [DURATION]",
    ),
    (r"Base: [^\n]+ \([0-9a-f]{7,40}\)", "Base: [BASE]"),
    (r"\\([\w\d])", "/$1"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TestMode {
    #[default]
    Twin,
    Live,
    Strict,
}

impl TestMode {
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var("FABRO_TEST_MODE").as_deref() {
            Ok("live") => Self::Live,
            Ok("strict") => Self::Strict,
            _ => match std::env::var("NEXTEST_PROFILE").as_deref() {
                Ok("e2e") => Self::Strict,
                _ => Self::Twin,
            },
        }
    }

    #[must_use]
    pub fn is_twin(self) -> bool {
        matches!(self, Self::Twin)
    }

    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(self, Self::Live | Self::Strict)
    }
}

/// Read an env var required by an E2E test, with mode-aware skip/strict behavior.
#[must_use]
#[allow(clippy::print_stderr)]
pub fn require_env(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name) {
        Some(value)
    } else {
        assert!(
            TestMode::from_env() != TestMode::Strict,
            "{name} not set (FABRO_TEST_MODE=strict)"
        );
        eprintln!("skipping: {name} not set");
        None
    }
}

/// A test context for running fabro CLI commands.
///
/// Each context gets isolated home/temp directories. The storage directory is
/// shared per nextest run when `NEXTEST_RUN_ID` is present, otherwise shared
/// per test process.
pub struct TestContext {
    pub temp_dir: PathBuf,
    pub home_dir: PathBuf,
    pub storage_dir: PathBuf,
    test_case_id: String,
    test_run_id: String,
    session_root: PathBuf,
    fabro_bin: PathBuf,
    filters: Vec<(String, String)>,
    managed_storage_dirs: Vec<PathBuf>,
    _context_root: tempfile::TempDir,
}

#[derive(Debug, Clone)]
struct SessionPaths {
    root: PathBuf,
    storage_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
enum SessionMode {
    Nextest,
    Process,
}

#[derive(Debug, Serialize)]
struct ClientMarker {
    pid: u32,
    touched_at_ms: u128,
}

static SESSION_REFS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

fn session_refs() -> &'static Mutex<HashMap<PathBuf, usize>> {
    SESSION_REFS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn test_case_id() -> String {
    let ulid = std::process::Command::new("uuidgen")
        .arg("-r")
        .output()
        .ok()
        .and_then(|output| output.status.success().then_some(output.stdout))
        .and_then(|stdout| String::from_utf8(stdout).ok())
        .map(|value| value.trim().replace('-', ""))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            format!("{nanos:032x}")
        });
    ulid
}

fn current_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_millis()
}

fn current_pid() -> u32 {
    std::process::id()
}

fn session_paths() -> (SessionMode, String, SessionPaths) {
    let base_dir = short_session_base_dir();
    if let Ok(run_id) = std::env::var("NEXTEST_RUN_ID") {
        if !run_id.trim().is_empty() {
            let short_id = shorten_session_id(&run_id);
            let root = base_dir.join(format!("n-{short_id}"));
            return (
                SessionMode::Nextest,
                run_id,
                SessionPaths {
                    storage_dir: root.join("storage"),
                    root,
                },
            );
        }
    }

    let process_id = format!("process-{}", current_pid());
    let root = base_dir.join(format!("p-{}", current_pid()));
    (
        SessionMode::Process,
        process_id,
        SessionPaths {
            storage_dir: root.join("storage"),
            root,
        },
    )
}

fn short_session_base_dir() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/tmp/fx")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("fabro-test")
    }
}

fn shorten_session_id(id: &str) -> String {
    let trimmed = id.trim();
    let shortened: String = trimmed
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    if shortened.is_empty() {
        "session".to_string()
    } else {
        shortened
    }
}

fn session_lock_path(root: &Path) -> PathBuf {
    root.join("session.lock")
}

fn session_clients_dir(root: &Path) -> PathBuf {
    root.join("clients")
}

fn session_marker_path(root: &Path, pid: u32) -> PathBuf {
    session_clients_dir(root).join(pid.to_string())
}

fn ensure_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|err| panic!("failed to create {}: {err}", parent.display()));
    }
}

fn with_session_lock<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    std::fs::create_dir_all(root)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", root.display()));
    let lock_path = session_lock_path(root);
    ensure_parent_dir(&lock_path);
    let lock_file = File::create(&lock_path)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", lock_path.display()));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !fabro_proc::try_flock_exclusive(&lock_file)
        .unwrap_or_else(|err| panic!("failed to lock {}: {err}", lock_path.display()))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for session lock {}",
            lock_path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = f();
    fabro_proc::flock_unlock(&lock_file)
        .unwrap_or_else(|err| panic!("failed to unlock {}: {err}", lock_path.display()));
    result
}

fn live_marker_count(root: &Path) -> usize {
    let clients_dir = session_clients_dir(root);
    let Ok(entries) = std::fs::read_dir(&clients_dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .parse::<u32>()
                .ok()
                .map(|pid| (pid, entry.path()))
        })
        .filter(|(pid, path)| {
            if fabro_proc::process_alive(*pid) {
                true
            } else {
                let _ = std::fs::remove_file(path);
                false
            }
        })
        .count()
}

fn write_marker(root: &Path) {
    let marker = ClientMarker {
        pid: current_pid(),
        touched_at_ms: current_timestamp_ms(),
    };
    let marker_path = session_marker_path(root, marker.pid);
    ensure_parent_dir(&marker_path);
    let contents =
        serde_json::to_vec(&marker).expect("client marker should serialize to JSON bytes");
    std::fs::write(&marker_path, contents)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", marker_path.display()));
}

fn stop_session_server(storage_dir: &Path) {
    let record_path = storage_dir.join("server.json");
    let Ok(content) = std::fs::read_to_string(&record_path) else {
        return;
    };
    let Ok(record) = serde_json::from_str::<serde_json::Value>(&content) else {
        return;
    };
    let Some(pid) = record["pid"].as_u64().map(|p| p as u32) else {
        return;
    };

    fabro_proc::sigterm(pid);

    let poll = std::time::Duration::from_millis(50);
    let timeout = std::time::Duration::from_secs(3);
    let mut elapsed = std::time::Duration::ZERO;
    while elapsed < timeout && fabro_proc::process_alive(pid) {
        std::thread::sleep(poll);
        elapsed += poll;
    }
    if fabro_proc::process_alive(pid) {
        fabro_proc::sigkill(pid);
    }

    let _ = std::fs::remove_file(&record_path);
}

fn cleanup_session_root(root: &Path, storage_dir: &Path) {
    with_session_lock(root, || {
        let marker_path = session_marker_path(root, current_pid());
        let _ = std::fs::remove_file(&marker_path);
        let live_count = live_marker_count(root);
        if live_count == 0 {
            stop_session_server(storage_dir);
            let _ = std::fs::remove_dir_all(root);
        }
    });
}

fn reap_stale_session_roots(mode: SessionMode) {
    let base_dir = short_session_base_dir();
    let Ok(entries) = std::fs::read_dir(&base_dir) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let root = entry.path();
        if !root.is_dir() {
            continue;
        }
        let file_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let expected_prefix = match mode {
            SessionMode::Nextest => "n-",
            SessionMode::Process => "p-",
        };
        if !file_name.starts_with(expected_prefix) {
            continue;
        }
        with_session_lock(&root, || {
            if live_marker_count(&root) == 0 {
                let storage_dir = root.join("storage");
                stop_session_server(&storage_dir);
                let _ = std::fs::remove_dir_all(&root);
            }
        });
    }
}

impl TestContext {
    /// Create a new isolated test context.
    ///
    /// `fabro_bin` should be the path to the compiled `fabro` binary,
    /// typically obtained via `env!("CARGO_BIN_EXE_fabro")`.
    pub fn new(fabro_bin: PathBuf) -> Self {
        let test_name: String = std::thread::current()
            .name()
            .unwrap_or("unknown")
            .rsplit("::")
            .next()
            .unwrap_or("unknown")
            .to_string();
        // Truncate to keep total temp path under Unix socket limit (104 bytes).
        // Budget: TMPDIR (~49) + prefix + suffix (~6) + /home/fabro-data/fabro.sock (27) < 104
        let label = &test_name[..test_name.len().min(16)];
        let context_root = tempfile::Builder::new()
            .prefix(&format!(".ft-{label}-"))
            .tempdir()
            .expect("failed to create temp dir");
        let root_path = context_root.path().to_path_buf();
        let (_, test_run_id, session_paths) = session_paths();
        reap_stale_session_roots(SessionMode::Nextest);
        reap_stale_session_roots(SessionMode::Process);
        with_session_lock(&session_paths.root, || {
            std::fs::create_dir_all(session_clients_dir(&session_paths.root)).unwrap_or_else(
                |err| {
                    panic!(
                        "failed to create {}: {err}",
                        session_clients_dir(&session_paths.root).display()
                    )
                },
            );
            std::fs::create_dir_all(&session_paths.storage_dir).unwrap_or_else(|err| {
                panic!(
                    "failed to create {}: {err}",
                    session_paths.storage_dir.display()
                )
            });
            write_marker(&session_paths.root);
        });

        let temp_dir = root_path.join("temp");
        let home_dir = root_path.join("home");
        let storage_dir = session_paths.storage_dir.clone();
        let test_case_id = test_case_id();

        std::fs::create_dir_all(&temp_dir).expect("failed to create temp_dir");
        std::fs::create_dir_all(&home_dir).expect("failed to create home_dir");

        let filters = vec![
            (
                regex::escape(&format!("/private{}", temp_dir.to_str().unwrap())),
                "[TEMP_DIR]".to_string(),
            ),
            (
                regex::escape(temp_dir.to_str().unwrap()),
                "[TEMP_DIR]".to_string(),
            ),
            (
                regex::escape(&format!("/private{}", home_dir.to_str().unwrap())),
                "[HOME_DIR]".to_string(),
            ),
            (
                regex::escape(home_dir.to_str().unwrap()),
                "[HOME_DIR]".to_string(),
            ),
            (
                regex::escape(&format!("/private{}", storage_dir.to_str().unwrap())),
                "[STORAGE_DIR]".to_string(),
            ),
            (
                regex::escape(storage_dir.to_str().unwrap()),
                "[STORAGE_DIR]".to_string(),
            ),
            (regex::escape(&test_case_id), "[TEST_CASE]".to_string()),
            (regex::escape(&test_run_id), "[TEST_RUN]".to_string()),
        ];

        {
            let mut refs = session_refs().lock().expect("session refs lock poisoned");
            *refs.entry(session_paths.root.clone()).or_default() += 1;
        }

        Self {
            temp_dir,
            home_dir,
            storage_dir,
            test_case_id,
            test_run_id,
            session_root: session_paths.root,
            fabro_bin,
            filters,
            managed_storage_dirs: Vec::new(),
            _context_root: context_root,
        }
    }

    /// Register a custom filter (regex pattern → replacement).
    pub fn add_filter(&mut self, pattern: &str, replacement: &str) {
        self.filters
            .push((regex::escape(pattern), replacement.to_string()));
    }

    /// Returns the combined static + context-specific filters.
    pub fn filters(&self) -> Vec<(String, String)> {
        let mut filters = self.filters.clone();
        filters.extend(
            INSTA_FILTERS
                .iter()
                .map(|(pat, rep)| ((*pat).to_string(), (*rep).to_string())),
        );
        filters
    }

    pub fn test_run_id(&self) -> &str {
        &self.test_run_id
    }

    pub fn test_case_id(&self) -> &str {
        &self.test_case_id
    }

    pub fn test_run_label(&self) -> String {
        format!("fabro_test_run={}", self.test_run_id)
    }

    pub fn test_case_label(&self) -> String {
        format!("fabro_test_case={}", self.test_case_id)
    }

    fn append_test_labels(&self, cmd: &mut Command) {
        cmd.arg("--label");
        cmd.arg(self.test_run_label());
        cmd.arg("--label");
        cmd.arg(self.test_case_label());
    }

    /// Build a base `Command` with all isolation env vars set.
    ///
    /// The working directory defaults to `self.temp_dir` (a non-git temp
    /// directory) so tests never accidentally interact with the real repo.
    /// Tests that need a specific working directory can override this with
    /// a subsequent `.current_dir(path)` call.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.fabro_bin);
        cmd.current_dir(&self.temp_dir);
        cmd.env("NO_COLOR", "1");
        cmd.env("HOME", &self.home_dir);
        cmd.env("FABRO_NO_UPGRADE_CHECK", "true");
        cmd.env("FABRO_STORAGE_DIR", &self.storage_dir);
        cmd.env("FABRO_SERVER_MAX_CONCURRENT_RUNS", "64");
        cmd
    }

    /// Build a `validate` subcommand.
    pub fn validate(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("validate");
        cmd
    }

    /// Build a `run` subcommand.
    pub fn run_cmd(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("run");
        self.append_test_labels(&mut cmd);
        cmd
    }

    /// Build a `create` subcommand with per-test labels attached.
    pub fn create_cmd(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("create");
        self.append_test_labels(&mut cmd);
        cmd
    }

    /// Build a `ps` subcommand.
    pub fn ps(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("ps");
        cmd
    }

    /// Build a `model` subcommand.
    pub fn model(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("model");
        cmd
    }

    /// Build a `secret` subcommand.
    pub fn secret(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("secret");
        cmd
    }

    /// Build a `doctor` subcommand.
    pub fn doctor(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("doctor");
        cmd
    }

    /// Build a `exec` subcommand.
    pub fn exec_cmd(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("exec");
        cmd
    }

    /// Build a `llm` subcommand.
    pub fn llm(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("llm");
        cmd
    }

    /// Build a `settings` subcommand.
    pub fn settings(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("settings");
        cmd
    }

    /// Build a `sandbox` subcommand.
    pub fn sandbox(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("sandbox");
        cmd
    }

    /// Build a `sandbox cp` subcommand.
    pub fn cp(&self) -> Command {
        let mut cmd = self.sandbox();
        cmd.arg("cp");
        cmd
    }

    /// Build a `sandbox ssh` subcommand.
    pub fn ssh(&self) -> Command {
        let mut cmd = self.sandbox();
        cmd.arg("ssh");
        cmd
    }

    /// Build a `sandbox preview` subcommand.
    pub fn preview(&self) -> Command {
        let mut cmd = self.sandbox();
        cmd.arg("preview");
        cmd
    }

    /// Build an `init` subcommand.
    pub fn init_cmd(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("init");
        cmd
    }

    /// Build an `install` subcommand.
    pub fn install(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("install");
        cmd
    }

    /// Build a `pr` subcommand.
    pub fn pr(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("pr");
        cmd
    }

    /// Build a `repo` subcommand.
    pub fn repo(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("repo");
        cmd
    }

    /// Build a `system` subcommand.
    pub fn system(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("system");
        cmd
    }

    /// Write a file under `temp_dir`, creating parent directories as needed.
    ///
    /// `path` is relative to `temp_dir`.
    pub fn write_temp(
        &self,
        path: impl AsRef<std::path::Path>,
        content: impl AsRef<[u8]>,
    ) -> &Self {
        let full = self.temp_dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::write(&full, content).expect("failed to write file");
        self
    }

    /// Initialize a git repository in `temp_dir`.
    pub fn git_init(&self) -> &Self {
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&self.temp_dir)
            .output()
            .expect("git init should succeed");
        self
    }

    /// Write a file under `home_dir`, creating parent directories as needed.
    ///
    /// `path` is relative to `home_dir`.
    pub fn write_home(
        &self,
        path: impl AsRef<std::path::Path>,
        content: impl AsRef<[u8]>,
    ) -> &Self {
        let full = self.home_dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::write(&full, content).expect("failed to write file");
        self
    }

    /// Register an additional storage directory that this test may cause to
    /// auto-start a daemon for, so Drop can stop it.
    pub fn manage_storage_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        let path = path.as_ref().to_path_buf();
        if path != self.storage_dir && !self.managed_storage_dirs.contains(&path) {
            self.managed_storage_dirs.push(path);
        }
        self
    }

    /// Find a run directory whose name ends with `run_id_suffix`.
    pub fn find_run_dir(&self, run_id_suffix: &str) -> PathBuf {
        let runs_dir = self.storage_dir.join("runs");
        std::fs::read_dir(&runs_dir)
            .expect("runs directory should exist")
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with(run_id_suffix))
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected run directory for {run_id_suffix} under {}",
                    runs_dir.display()
                )
            })
    }

    /// Return the only run directory currently present under storage.
    pub fn single_run_dir(&self) -> PathBuf {
        let runs_dir = self.storage_dir.join("runs");
        let entries: Vec<_> = std::fs::read_dir(&runs_dir)
            .expect("runs directory should exist")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .filter(|path| {
                let Ok(contents) = std::fs::read_to_string(path.join("run.json")) else {
                    return false;
                };
                serde_json::from_str::<Value>(&contents)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("labels")
                            .and_then(Value::as_object)
                            .and_then(|labels| labels.get("fabro_test_case"))
                            .and_then(Value::as_str)
                            .map(|value| value == self.test_case_id())
                    })
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one run directory for fabro_test_case={} under {}",
            self.test_case_id(),
            runs_dir.display()
        );
        entries.into_iter().next().unwrap()
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        for storage_dir in &self.managed_storage_dirs {
            stop_session_server(storage_dir);
        }

        let is_last_ref = {
            let mut refs = session_refs().lock().expect("session refs lock poisoned");
            let Some(count) = refs.get_mut(&self.session_root) else {
                return;
            };
            *count -= 1;
            if *count == 0 {
                refs.remove(&self.session_root);
                true
            } else {
                false
            }
        };

        if !is_last_ref {
            return;
        }

        cleanup_session_root(&self.session_root, &self.storage_dir);
    }
}

/// Execute a command and format the output for snapshot testing.
///
/// Returns the formatted string and the raw `Output`.
/// Prints unfiltered output to stderr for debugging failed tests.
pub fn run_and_format(cmd: &mut Command, filters: &[(String, String)]) -> (String, Output) {
    let output = cmd.output().expect("failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Print unfiltered output for debugging
    #[allow(clippy::print_stderr)]
    {
        eprint!("{stdout}");
        eprint!("{stderr}");
    }

    let filtered_stdout = apply_filters(&stdout, filters);
    let filtered_stderr = apply_filters(&stderr, filters);

    let formatted = format!(
        "success: {success}\nexit_code: {code}\n----- stdout -----\n{stdout}----- stderr -----\n{stderr}",
        success = output.status.success(),
        code = output.status.code().unwrap_or(-1),
        stdout = filtered_stdout,
        stderr = filtered_stderr,
    );

    (formatted, output)
}

/// Apply regex-based filters to a snapshot string.
pub fn apply_filters(snapshot: &str, filters: &[(String, String)]) -> String {
    let mut result = snapshot.to_string();
    for (pattern, replacement) in filters {
        if let Ok(re) = Regex::new(pattern) {
            result = re.replace_all(&result, replacement.as_str()).to_string();
        }
    }
    result
}

/// Create a `TestContext` using the `fabro` binary built by cargo.
///
/// Automatically registers a `[FIXTURES]` snapshot filter for the `test/`
/// directory at the repository root (found by walking up from `CARGO_MANIFEST_DIR`).
#[macro_export]
macro_rules! test_context {
    () => {{
        let mut ctx =
            $crate::TestContext::new(std::path::PathBuf::from(env!("CARGO_BIN_EXE_fabro")));
        if let Some(fixtures_dir) =
            $crate::find_test_fixtures_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        {
            ctx.add_filter(fixtures_dir.to_str().unwrap(), "[FIXTURES]");
        }
        ctx
    }};
}

/// Snapshot test macro that runs a command and compares output using insta.
///
/// Usage:
/// ```ignore
/// fabro_snapshot!(context.filters(), context.validate().arg("--help"), @"...");
/// ```
#[macro_export]
macro_rules! fabro_snapshot {
    ($spawnable:expr, @$snapshot:literal) => {{
        let filters: Vec<(String, String)> = $crate::TestContext::default_filters();
        let mut cmd = $spawnable;
        let (snapshot, _output) = $crate::run_and_format(&mut cmd, &filters);
        insta::assert_snapshot!(snapshot, @$snapshot);
    }};
    ($filters:expr, $spawnable:expr, @$snapshot:literal) => {{
        let filters: Vec<(String, String)> = $filters;
        let mut cmd = $spawnable;
        let (snapshot, _output) = $crate::run_and_format(&mut cmd, &filters);
        insta::assert_snapshot!(snapshot, @$snapshot);
    }};
}

impl TestContext {
    /// Returns just the static default filters (no context-specific paths).
    pub fn default_filters() -> Vec<(String, String)> {
        INSTA_FILTERS
            .iter()
            .map(|(pat, rep)| ((*pat).to_string(), (*rep).to_string()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Twin server infrastructure
// ---------------------------------------------------------------------------

use tokio::net::TcpListener as TokioTcpListener;
use tokio::sync::OnceCell;
use tokio::time;
pub use twin_github::AppState as GitHubAppState;
pub use twin_github::state::AppOptions as GitHubAppOptions;
use twin_openai::config::Config as TwinConfig;

/// A shared twin-openai server instance.
pub struct TwinOpenAi {
    /// Base URL including `/v1`, e.g. `http://127.0.0.1:PORT/v1`.
    pub base_url: String,
}

pub struct TwinGitHub {
    pub base_url: String,
    server: twin_github::TestServer,
}

pub fn test_http_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

impl TwinGitHub {
    pub async fn start(state: twin_github::AppState) -> Self {
        let server = twin_github::TestServer::start(state).await;
        let base_url = server.url().to_string();
        Self { base_url, server }
    }

    pub async fn shutdown(self) {
        self.server.shutdown().await;
    }
}

impl TwinOpenAi {
    pub fn configure_command(&self, cmd: &mut Command, namespace: &str) {
        cmd.env("OPENAI_BASE_URL", &self.base_url);
        cmd.env("OPENAI_API_KEY", namespace);
    }

    #[must_use]
    pub fn admin_url(&self) -> String {
        self.base_url.trim_end_matches("/v1").to_string()
    }

    pub async fn reset_namespace(&self, namespace: &str) {
        let response = test_http_client()
            .post(format!("{}/__admin/reset", self.admin_url()))
            .bearer_auth(namespace)
            .send()
            .await
            .expect("reset twin-openai namespace");
        assert!(
            response.status().is_success(),
            "reset twin-openai namespace failed: {}",
            response.status()
        );
    }
}

#[derive(Debug, Default, Clone)]
pub struct TwinScenarios {
    namespace: String,
    scenarios: Vec<TwinScenario>,
}

impl TwinScenarios {
    #[must_use]
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            scenarios: Vec::new(),
        }
    }

    #[must_use]
    pub fn scenario(mut self, scenario: TwinScenario) -> Self {
        self.scenarios.push(scenario);
        self
    }

    pub async fn load(self, twin: &TwinOpenAi) {
        twin.reset_namespace(&self.namespace).await;

        let response = test_http_client()
            .post(format!("{}/__admin/scenarios", twin.admin_url()))
            .bearer_auth(&self.namespace)
            .json(&json!({
                "scenarios": self.scenarios.into_iter().map(TwinScenario::into_json).collect::<Vec<_>>(),
            }))
            .send()
            .await
            .expect("load twin-openai scenarios");
        assert!(
            response.status().is_success(),
            "load twin-openai scenarios failed: {}",
            response.status()
        );
    }
}

#[derive(Debug, Clone)]
pub struct TwinScenario {
    matcher: Map<String, Value>,
    script: Value,
}

impl TwinScenario {
    #[must_use]
    pub fn responses(model: impl Into<String>) -> Self {
        Self {
            matcher: Map::from_iter([
                (
                    "endpoint".to_string(),
                    Value::String("responses".to_string()),
                ),
                ("model".to_string(), Value::String(model.into())),
            ]),
            script: json!({ "kind": "success" }),
        }
    }

    #[must_use]
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.assert_script_kind("success", "text");
        self.script["response_text"] = Value::String(text.into());
        self
    }

    #[must_use]
    pub fn tool_call(self, tool_call: TwinToolCall) -> Self {
        self.tool_calls(vec![tool_call])
    }

    #[must_use]
    pub fn tool_calls(mut self, tool_calls: Vec<TwinToolCall>) -> Self {
        self.assert_script_kind("success", "tool_calls");
        self.script["tool_calls"] = Value::Array(
            tool_calls
                .into_iter()
                .map(TwinToolCall::into_json)
                .collect::<Vec<_>>(),
        );
        self
    }

    #[must_use]
    pub fn error(mut self, status: u16, message: impl Into<String>) -> Self {
        self.script = json!({
            "kind": "error",
            "status": status,
            "message": message.into(),
            "error_type": "invalid_request_error",
            "code": "twin_error",
        });
        self
    }

    #[must_use]
    pub fn retry_after(mut self, retry_after: impl Into<String>) -> Self {
        self.assert_script_kind("error", "retry_after");
        self.script["retry_after"] = Value::String(retry_after.into());
        self
    }

    #[must_use]
    pub fn stream(mut self, stream: bool) -> Self {
        self.matcher
            .insert("stream".to_string(), Value::Bool(stream));
        self
    }

    #[must_use]
    pub fn input_contains(mut self, needle: impl Into<String>) -> Self {
        self.matcher
            .insert("input_contains".to_string(), Value::String(needle.into()));
        self
    }

    #[must_use]
    pub fn metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        let metadata = self
            .matcher
            .entry("metadata".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        metadata
            .as_object_mut()
            .expect("metadata should be an object")
            .insert(key.into(), value);
        self
    }

    fn into_json(self) -> Value {
        json!({
            "matcher": self.matcher,
            "script": self.script,
        })
    }

    fn assert_script_kind(&self, expected: &str, method: &str) {
        let actual = self.script["kind"]
            .as_str()
            .expect("twin scenario script must have a kind");
        assert_eq!(
            actual, expected,
            "TwinScenario::{method} requires a {expected} script, got {actual}"
        );
    }
}

#[derive(Debug, Clone)]
pub struct TwinToolCall {
    name: String,
    arguments: Value,
}

impl TwinToolCall {
    #[must_use]
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }

    #[must_use]
    pub fn write_file(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(
            "write_file",
            json!({ "file_path": path.into(), "content": content.into() }),
        )
    }

    #[must_use]
    pub fn read_file(path: impl Into<String>) -> Self {
        Self::new("read_file", json!({ "file_path": path.into() }))
    }

    #[must_use]
    pub fn shell(command: impl Into<String>) -> Self {
        Self::new("shell", json!({ "command": command.into() }))
    }

    #[must_use]
    pub fn shell_with_timeout(command: impl Into<String>, timeout_ms: u64) -> Self {
        Self::new(
            "shell",
            json!({ "command": command.into(), "timeout_ms": timeout_ms }),
        )
    }

    #[must_use]
    pub fn grep_pattern(pattern: impl Into<String>, path: impl Into<String>) -> Self {
        Self::new(
            "grep",
            json!({ "pattern": pattern.into(), "path": path.into() }),
        )
    }

    #[must_use]
    pub fn glob_pattern(pattern: impl Into<String>, path: impl Into<String>) -> Self {
        Self::new(
            "glob",
            json!({ "pattern": pattern.into(), "path": path.into() }),
        )
    }

    #[must_use]
    pub fn apply_patch(patch: impl Into<String>) -> Self {
        Self::new("apply_patch", json!({ "patch": patch.into() }))
    }

    fn into_json(self) -> Value {
        json!({
            "name": self.name,
            "arguments": self.arguments,
        })
    }
}

static TWIN_OPENAI: OnceCell<TwinOpenAi> = OnceCell::const_new();

/// Returns a shared twin-openai server, starting it on first call.
#[allow(clippy::missing_panics_doc)]
pub async fn twin_openai() -> &'static TwinOpenAi {
    TWIN_OPENAI
        .get_or_init(|| async {
            let listener = TokioTcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind twin-openai");
            let addr = listener.local_addr().expect("local addr");
            let base_url = format!("http://127.0.0.1:{}/v1", addr.port());

            let config = TwinConfig {
                bind_addr: addr,
                require_auth: true,
                enable_admin: true,
            };
            let app = twin_openai::build_app_with_config(config);

            tokio::spawn(async move {
                axum::serve(listener, app).await.expect("twin-openai serve");
            });

            // Wait for server readiness
            let client = test_http_client();
            let healthz_url = format!("http://127.0.0.1:{}/healthz", addr.port());
            for _ in 0..50 {
                if let Ok(resp) = client.get(&healthz_url).send().await {
                    if resp.status().is_success() {
                        return TwinOpenAi { base_url };
                    }
                }
                time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("twin-openai failed to become ready");
        })
        .await
}

/// Returns `(base_url, api_key)` for the current test.
///
/// In twin mode: starts/reuses the twin server, generates a unique API key
/// from `module_path!()` and `line!()` to ensure per-test isolation.
/// In live mode: reads from environment.
#[macro_export]
macro_rules! e2e_openai {
    () => {{
        let mode = $crate::TestMode::from_env();
        if mode.is_twin() {
            let twin = $crate::twin_openai().await;
            let api_key = format!("{}::{}", module_path!(), line!());
            (twin.base_url.clone(), api_key)
        } else {
            let base_url = std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let api_key = std::env::var("OPENAI_API_KEY")
                .expect("OPENAI_API_KEY must be set in live/strict mode");
            (base_url, api_key)
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn twin_admin_url_removes_v1_suffix() {
        let twin = TwinOpenAi {
            base_url: "http://127.0.0.1:3000/v1".to_string(),
        };
        assert_eq!(twin.admin_url(), "http://127.0.0.1:3000");
    }

    #[test]
    fn twin_configure_command_sets_openai_env() {
        let twin = TwinOpenAi {
            base_url: "http://127.0.0.1:3000/v1".to_string(),
        };
        let mut cmd = Command::new("env");
        twin.configure_command(&mut cmd, "test-namespace");

        let envs = cmd.get_envs().collect::<Vec<_>>();
        assert!(envs.iter().any(|(key, value)| {
            *key == std::ffi::OsStr::new("OPENAI_BASE_URL")
                && *value == Some(std::ffi::OsStr::new("http://127.0.0.1:3000/v1"))
        }),);
        assert!(envs.iter().any(|(key, value)| {
            *key == std::ffi::OsStr::new("OPENAI_API_KEY")
                && *value == Some(std::ffi::OsStr::new("test-namespace"))
        }),);
    }

    #[test]
    fn twin_scenario_builder_matches_admin_contract() {
        let scenario = TwinScenario::responses("gpt-5.4-mini")
            .stream(false)
            .input_contains("Return JSON")
            .tool_call(TwinToolCall::write_file("hello.txt", "Hello"))
            .text(r#"{"greeting":"hello"}"#)
            .into_json();

        assert_eq!(scenario["matcher"]["endpoint"], "responses");
        assert_eq!(scenario["matcher"]["model"], "gpt-5.4-mini");
        assert_eq!(scenario["matcher"]["stream"], false);
        assert_eq!(scenario["matcher"]["input_contains"], "Return JSON");
        assert_eq!(scenario["script"]["kind"], "success");
        assert_eq!(
            scenario["script"]["response_text"],
            r#"{"greeting":"hello"}"#
        );
        assert_eq!(scenario["script"]["tool_calls"][0]["name"], "write_file");
        assert_eq!(
            scenario["script"]["tool_calls"][0]["arguments"]["file_path"],
            "hello.txt"
        );
    }

    #[test]
    #[should_panic(expected = "TwinScenario::retry_after requires a error script")]
    fn twin_scenario_rejects_retry_after_on_success() {
        let _ = TwinScenario::responses("gpt-5.4-mini").retry_after("30");
    }

    #[test]
    fn session_paths_share_nextest_storage_dir() {
        let _lock = env_lock().lock().expect("env lock poisoned");
        let _guard = EnvGuard::set("NEXTEST_RUN_ID", Some("nextest-run-123"));
        let (_, run_id, paths) = session_paths();
        assert_eq!(run_id, "nextest-run-123");
        assert!(paths.root.ends_with(Path::new("fx").join("n-nextestrun12")));
        assert_eq!(paths.storage_dir, paths.root.join("storage"));
    }

    #[test]
    fn session_paths_fall_back_to_process_storage_dir() {
        let _lock = env_lock().lock().expect("env lock poisoned");
        let _guard = EnvGuard::set("NEXTEST_RUN_ID", None);
        let (_, run_id, paths) = session_paths();
        assert_eq!(run_id, format!("process-{}", current_pid()));
        assert!(
            paths
                .root
                .ends_with(Path::new("fx").join(format!("p-{}", current_pid())))
        );
        assert_eq!(paths.storage_dir, paths.root.join("storage"));
    }

    #[test]
    fn run_and_create_commands_include_test_labels() {
        let _lock = env_lock().lock().expect("env lock poisoned");
        let _guard = EnvGuard::set("NEXTEST_RUN_ID", Some("run-cmd-labels"));
        let context = TestContext::new(PathBuf::from("/tmp/fabro"));

        let run_args = context
            .run_cmd()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(run_args[0], "run");
        assert!(run_args.contains(&"--label".to_string()));
        assert!(run_args.contains(&context.test_run_label()));
        assert!(run_args.contains(&context.test_case_label()));

        let create_args = context
            .create_cmd()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(create_args[0], "create");
        assert!(create_args.contains(&context.test_run_label()));
        assert!(create_args.contains(&context.test_case_label()));
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
