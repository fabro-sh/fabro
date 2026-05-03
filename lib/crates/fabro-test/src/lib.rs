#![expect(
    clippy::disallowed_methods,
    reason = "fabro-test: shared test infrastructure; sync std::fs throughout is intentional for \
              test fixtures, snapshots, scratch directories, and process-env harnessing. \
              Tokio-path code under test sits in other crates."
)]

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use assert_cmd::Command;
use fabro_config::daemon::ServerDaemon;
use fabro_config::{RuntimeDirectory, Storage, envfile};
pub use fabro_static::EnvVars;
use fabro_types::RunId;
use regex::Regex;
use serde_json::{Map, Value, json};
use toml::Value as TomlValue;
use toml::map::Map as TomlMap;

mod http_assert;

pub use http_assert::{
    assert_axum_status, assert_axum_status_in, assert_reqwest_status, assert_reqwest_status_in,
    expect_axum_bytes, expect_axum_json, expect_axum_status, expect_axum_status_in,
    expect_axum_text, expect_reqwest_bytes, expect_reqwest_json, expect_reqwest_status,
    expect_reqwest_status_in, expect_reqwest_text,
};

/// Re-export `LLVM_PROFILE_FILE` into a `Command` whose env was just cleared,
/// so subprocess coverage data lands in the profile path that
/// `cargo-llvm-cov` configured for the parent test process. Accepts both
/// `std::process::Command` and `assert_cmd::Command` (any type with an
/// `env(key, value)` method).
#[macro_export]
macro_rules! preserve_coverage_env {
    ($cmd:expr) => {{
        if let Some(val) = ::std::env::var_os($crate::EnvVars::LLVM_PROFILE_FILE) {
            $cmd.env($crate::EnvVars::LLVM_PROFILE_FILE, val);
        }
    }};
}

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
    (r"fabro \d+\.\d+\.\d+(?:-[\w.]+)?", "fabro [VERSION]"),
    (r"\([0-9a-f]{7} \d{4}-\d{2}-\d{2}(?: \w+)?\)", "([BUILD])"),
    (r"\b[0-9A-HJKMNP-TV-Z]{26}\b", "[ULID]"),
    (r"in \d+(\.\d+)?(ms|s)", "in [TIME]"),
    (
        r"\[STORAGE_DIR\]/scratch/\d{8}-dry-run-\[ULID\]",
        "[DRY_RUN_DIR]",
    ),
    (r"\[STORAGE_DIR\]/scratch/\d{8}-\[ULID\]", "[RUN_DIR]"),
    (
        r"Duration:\s+\d+\s+(seconds?|minutes?|hours?)",
        "Duration:  [DURATION]",
    ),
    (r"Base: [^\n]+ \([0-9a-f]{7,40}\)", "Base: [BASE]"),
    (r"\\([\w\d])", "/$1"),
];

const MANAGED_STORAGE_MARKER: &str = "# fabro-test managed storage_dir";
const SESSION_LOCK_TIMEOUT: Duration = Duration::from_secs(20);
const STALE_TMP_DAEMON_THRESHOLD: Duration = Duration::from_mins(30);
const TMP_DAEMON_REAPER_COOLDOWN: Duration = Duration::from_mins(5);
const TEST_SESSION_SECRET: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const TEST_DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";

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
        match std::env::var(EnvVars::FABRO_TEST_MODE).as_deref() {
            Ok("live") => Self::Live,
            Ok("strict") => Self::Strict,
            _ => match std::env::var(EnvVars::NEXTEST_PROFILE).as_deref() {
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

/// Read an env var required by an E2E test, with mode-aware skip/strict
/// behavior.
#[must_use]
#[allow(
    clippy::print_stderr,
    reason = "Missing test-env notices go to stderr so stdout stays assertable."
)]
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

/// Apply baseline environment isolation to a `Command` that spawns the
/// `fabro` binary (or a helper that will act like it).
///
/// Starts from a cleared environment and re-populates only the variables
/// the harness needs. Credentials (`GITHUB_TOKEN`, `GH_TOKEN`, provider API
/// keys, `SESSION_SECRET`, `GITHUB_APP_*`) and ambient `FABRO_*` overrides
/// from the developer shell or CI runner are dropped, so tests that assert
/// on "no credentials" error paths behave the same on a laptop with
/// `gh auth login` active and on a CI runner with a minted
/// `GITHUB_TOKEN`. Tests that deliberately need a credential set it with a
/// subsequent `.env(...)` call, which survives the clear.
pub fn apply_test_isolation(cmd: &mut std::process::Command, home_dir: &Path) {
    apply_test_isolation_with_lookup(cmd, home_dir, |name| std::env::var_os(name));
}

fn apply_test_isolation_with_lookup(
    cmd: &mut std::process::Command,
    home_dir: &Path,
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) {
    cmd.env_clear();
    if let Some(coverage) = lookup(EnvVars::LLVM_PROFILE_FILE) {
        cmd.env(EnvVars::LLVM_PROFILE_FILE, coverage);
    }
    if let Some(path) = lookup(EnvVars::PATH) {
        cmd.env(EnvVars::PATH, path);
    }
    cmd.env(EnvVars::NO_COLOR, "1");
    cmd.env(EnvVars::HOME, home_dir);
    cmd.env(EnvVars::FABRO_NO_UPGRADE_CHECK, "true")
        .env(EnvVars::FABRO_HTTP_PROXY_POLICY, "disabled")
        .env(EnvVars::FABRO_TELEMETRY, "off")
        .env(EnvVars::FABRO_SUPPRESS_OPEN_BROWSER, "1");
    cmd.env(EnvVars::FABRO_SERVER_MAX_CONCURRENT_RUNS, "64");
    cmd.env(EnvVars::FABRO_TEST_IN_MEMORY_STORE, "1");
}

/// Create a fresh tempdir containing an empty `storage/` subdirectory, for
/// isolating server lifecycle tests from the shared nextest session storage.
#[must_use]
pub fn isolated_storage_dir() -> tempfile::TempDir {
    let root = tempfile::tempdir_in("/tmp").expect("tempdir under /tmp");
    std::fs::create_dir_all(root.path().join("storage")).expect("create storage dir");
    root
}

/// Sleep tick for the test polling helpers below. These are synchronous
/// helpers called from blocking integration tests — there's no runtime to
/// hand off to, so `std::thread::sleep` is the right primitive.
#[expect(
    clippy::disallowed_methods,
    reason = "sync polling helper for blocking integration tests"
)]
fn poll_sleep() {
    std::thread::sleep(std::time::Duration::from_millis(50));
}

/// Poll up to 5s for a path to appear; panic on timeout.
pub fn wait_for_path(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        poll_sleep();
    }
    panic!("timed out waiting for {}", path.display());
}

/// Poll up to 5s for `needle` to appear in the contents of `path`; panic on
/// timeout.
pub fn wait_for_log_line(path: &Path, needle: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if std::fs::read_to_string(path)
            .ok()
            .is_some_and(|contents| contents.contains(needle))
        {
            return;
        }
        poll_sleep();
    }
    panic!("timed out waiting for {needle:?} in {}", path.display());
}

/// SIGTERM the pid, wait up to 5s for it to exit, then SIGKILL if still alive.
pub fn stop_pid(pid: u32) {
    fabro_proc::sigterm(pid);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !fabro_proc::process_running(pid) {
            return;
        }
        poll_sleep();
    }
    fabro_proc::sigkill(pid);
}

/// List any `server.*.log` files under `logs_dir`. Used by server-lifecycle
/// tests to assert that no server logs leak into the home logs directory.
#[must_use]
pub fn server_log_files(logs_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(logs_dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let has_log_ext = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("log"));
            let has_server_prefix = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("server."));
            has_log_ext && has_server_prefix
        })
        .collect()
}

/// A test context for running fabro CLI commands.
///
/// Each context gets isolated home/temp directories. The storage directory is
/// shared per nextest run when `NEXTEST_RUN_ID` is present, otherwise shared
/// per test process.
pub struct TestContext {
    pub temp_dir:         PathBuf,
    pub home_dir:         PathBuf,
    pub storage_dir:      PathBuf,
    test_case_id:         String,
    test_run_id:          String,
    session_root:         PathBuf,
    fabro_bin:            PathBuf,
    filters:              Vec<(String, String)>,
    active_socket_path:   PathBuf,
    isolated_server:      Option<ServerPaths>,
    managed_storage_dirs: Vec<PathBuf>,
    _context_root:        tempfile::TempDir,
}

#[derive(Debug, Clone)]
struct ServerPaths {
    root:        PathBuf,
    storage_dir: PathBuf,
    socket_path: PathBuf,
    config_path: PathBuf,
}

#[derive(Debug, Clone)]
struct SessionPaths {
    root:   PathBuf,
    server: ServerPaths,
}

#[derive(Debug, Clone, Copy)]
enum SessionMode {
    Nextest,
    Process,
}

static SESSION_REFS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

fn session_refs() -> &'static Mutex<HashMap<PathBuf, usize>> {
    SESSION_REFS.get_or_init(|| Mutex::new(HashMap::new()))
}

// Gate the stale-session reap so it fires at most once per process. The
// reap scans /tmp/fx/{n-*,p-*} to clean up after a prior nextest run
// that crashed; it's correctness-for-safety work that does not need to
// happen on every TestContext::new. One gate per SessionMode preserves
// the existing call structure without introducing cross-mode coupling.
static NEXTEST_REAPED: OnceLock<()> = OnceLock::new();
static PROCESS_REAPED: OnceLock<()> = OnceLock::new();
static TMP_DAEMONS_REAPED: OnceLock<()> = OnceLock::new();

// Advisory-lock-based peer-presence marker. Each test process opens
// `<session_root>/clients/<pid>` once, holds a shared (LOCK_SH) flock
// for the lifetime of any live TestContext in the process, and
// releases it explicitly in `cleanup_session_root` when the refcount
// drops to zero. Peers detect liveness by attempting LOCK_EX: if it
// succeeds, the owner is gone (normal exit, panic, SIGKILL, or zombie
// — the kernel releases flocks at process exit in every case), and
// the stale marker file is removed.
//
// The slot stores the marker path so subsequent rebounds in the same
// process (e.g., after a drop-to-zero followed by a new TestContext)
// can re-validate the invariant: a process only ever participates in
// one session root.
static MARKER_HANDLE: Mutex<Option<(PathBuf, File)>> = Mutex::new(None);

#[expect(
    clippy::disallowed_methods,
    reason = "This synchronous test-support helper uses uuidgen when available to create stable unique case IDs."
)]
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

fn current_pid() -> u32 {
    std::process::id()
}

fn session_paths() -> (SessionMode, String, SessionPaths) {
    let run_id = std::env::var(EnvVars::NEXTEST_RUN_ID).ok();
    session_paths_for_run_id(run_id.as_deref())
}

fn session_paths_for_run_id(run_id: Option<&str>) -> (SessionMode, String, SessionPaths) {
    let base_dir = short_session_base_dir();
    if let Some(run_id) = run_id {
        if !run_id.trim().is_empty() {
            let short_id = shorten_session_id(run_id);
            let root = base_dir.join(format!("n-{short_id}"));
            return (SessionMode::Nextest, run_id.to_string(), SessionPaths {
                server: ServerPaths {
                    root:        root.clone(),
                    storage_dir: root.join("storage"),
                    socket_path: root.join("fabro.sock"),
                    config_path: root.join("settings.toml"),
                },
                root,
            });
        }
    }

    let process_id = format!("process-{}", current_pid());
    let root = base_dir.join(format!("p-{}", current_pid()));
    (SessionMode::Process, process_id, SessionPaths {
        server: ServerPaths {
            root:        root.clone(),
            storage_dir: root.join("storage"),
            socket_path: root.join("fabro.sock"),
            config_path: root.join("settings.toml"),
        },
        root,
    })
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
    // Keep the lock outside the session root so cleanup can remove the
    // session directory without unlinking the lock file another process
    // is relying on for exclusion.
    let lock_dir = root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".locks");
    let lock_name = root
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("session"));
    lock_dir.join(Path::new(lock_name)).with_extension("lock")
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

#[expect(
    clippy::disallowed_methods,
    reason = "This sync test helper polls filesystem and flock state without requiring a Tokio runtime."
)]
fn with_session_lock<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    let lock_path = session_lock_path(root);
    // Retry create-dir + create-file as a unit: another process's
    // cleanup_session_root can remove_dir_all between the two calls.
    let deadline = std::time::Instant::now() + SESSION_LOCK_TIMEOUT;
    let lock_file = loop {
        std::fs::create_dir_all(root)
            .unwrap_or_else(|err| panic!("failed to create {}: {err}", root.display()));
        ensure_parent_dir(&lock_path);
        match File::create(&lock_path) {
            Ok(f) => break f,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("failed to create {}: {err}", lock_path.display()),
        }
    };
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

// Iterate `<root>/clients/` and for each marker file attempt a
// non-blocking exclusive flock. Success means the previous owner has
// released the lock (normal exit, panic, SIGKILL, or zombie — the
// kernel releases flocks at process exit regardless), and the stale
// marker file is removed. `EWOULDBLOCK` means the owner is still
// alive and holding LOCK_SH.
//
// Same-process subtlety: if our own PID's marker file is in the
// listing, we have LOCK_SH on it via `MARKER_HANDLE`. On Linux and
// macOS, flock locks are per-open-file-description, so a fresh
// `open()` here returns an FD that sees the shared lock and correctly
// reports EWOULDBLOCK when asked for LOCK_EX.
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
                .map(|_pid| entry.path())
        })
        .filter(|path| {
            // Open read-write so LOCK_EX has the access mode it expects
            // on the widest set of platforms. If the file is missing
            // between read_dir and open, treat it as already gone.
            let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
            else {
                return false;
            };
            // If the lock is acquired, the previous owner is gone; drop
            // the file handle (releasing our just-acquired lock) and
            // remove the marker. Anything else (`Ok(false)` meaning
            // still held, `Err(_)` for unexpected IO errors) is treated
            // conservatively as live.
            if matches!(fabro_proc::try_flock_exclusive(&file), Ok(true)) {
                drop(file);
                let _ = std::fs::remove_file(path);
                false
            } else {
                true
            }
        })
        .count()
}

// Open (or reopen, after a session-drop-to-zero) the per-process
// marker file and hold a shared advisory lock on it in
// `MARKER_HANDLE`. Called from inside the `with_session_lock` block
// of `TestContext::new`.
fn write_marker(root: &Path) {
    let marker_path = session_marker_path(root, current_pid());
    ensure_parent_dir(&marker_path);

    let mut slot = MARKER_HANDLE.lock().expect("MARKER_HANDLE lock poisoned");

    if let Some((existing_path, _)) = slot.as_ref() {
        debug_assert_eq!(
            existing_path, &marker_path,
            "marker handle path drifted — session root changed mid-process?"
        );
        if marker_path.exists() {
            return;
        }
        // Marker was removed (e.g., by a peer's reap) while we thought
        // we still owned it. Re-establish by replacing the handle.
        slot.take();
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&marker_path)
        .unwrap_or_else(|err| panic!("failed to open {}: {err}", marker_path.display()));
    let acquired = fabro_proc::try_flock_shared(&file)
        .unwrap_or_else(|err| panic!("failed to flock {}: {err}", marker_path.display()));
    assert!(
        acquired,
        "unexpected contention acquiring LOCK_SH on freshly created marker {}",
        marker_path.display()
    );
    *slot = Some((marker_path, file));
}

fn managed_storage_settings(storage_dir: &Path, rest: &str) -> String {
    format!(
        "{MANAGED_STORAGE_MARKER}\n_version = 1\n\n[server.storage]\nroot = \"{}\"\n\n{rest}",
        storage_dir.display()
    )
}

fn strip_managed_storage_settings(contents: &str) -> &str {
    if !contents.starts_with(MANAGED_STORAGE_MARKER) {
        return contents;
    }

    let after_marker = contents
        .strip_prefix(MANAGED_STORAGE_MARKER)
        .and_then(|rest| rest.strip_prefix('\n'))
        .unwrap_or("");
    after_marker
}

fn settings_storage_dir(settings_path: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(settings_path).ok()?;
    // Settings files that fabro-test injected with its managed marker are
    // not treated as user-explicit storage overrides — the override tracks
    // ONLY what the test itself asked for.
    if content.starts_with(MANAGED_STORAGE_MARKER) {
        return None;
    }
    let value = toml::from_str::<toml::Value>(&content).ok()?;
    value
        .get("server")
        .and_then(toml::Value::as_table)
        .and_then(|server| server.get("storage"))
        .and_then(toml::Value::as_table)
        .and_then(|storage| storage.get("root"))
        .and_then(toml::Value::as_str)
        .map(PathBuf::from)
}

fn home_settings_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".fabro/settings.toml")
}

fn write_settings_file(path: &Path, storage_dir: &Path, rest: &str) {
    ensure_parent_dir(path);
    std::fs::write(
        path,
        format!(
            "_version = 1\n\n[server.storage]\nroot = \"{}\"\n\n[server.auth]\nmethods = [\"dev-token\"]\n\n{rest}",
            storage_dir.display()
        ),
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", path.display()));
}

fn write_test_server_dev_token(storage_dir: &Path) {
    let runtime_directory = Storage::new(storage_dir).runtime_directory();
    let server_env_path = runtime_directory.env_path();
    envfile::merge_env_file(&server_env_path, [
        ("FABRO_DEV_TOKEN", TEST_DEV_TOKEN),
        ("SESSION_SECRET", TEST_SESSION_SECRET),
    ])
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", server_env_path.display()));
    let dev_token_path = runtime_directory.dev_token_path();
    ensure_parent_dir(&dev_token_path);
    std::fs::write(&dev_token_path, TEST_DEV_TOKEN)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", dev_token_path.display()));
}

fn parse_settings_table(contents: &str, source: &Path) -> TomlMap<String, TomlValue> {
    let stripped = strip_managed_storage_settings(contents);
    let value = toml::from_str::<TomlValue>(stripped)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", source.display()));
    let Some(table) = value.as_table() else {
        panic!("expected {} to contain a TOML table", source.display());
    };
    table.clone()
}

fn write_settings_table(path: &Path, table: &TomlMap<String, TomlValue>) {
    ensure_parent_dir(path);
    let mut contents = toml::to_string(table)
        .unwrap_or_else(|err| panic!("failed to serialize {}: {err}", path.display()));
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    std::fs::write(path, contents)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", path.display()));
}

fn server_target_from_table(table: &TomlMap<String, TomlValue>) -> Option<String> {
    table
        .get("cli")
        .and_then(TomlValue::as_table)
        .and_then(|cli| cli.get("target"))
        .and_then(TomlValue::as_table)
        .and_then(|target| target.get("path").or_else(|| target.get("url")))
        .and_then(TomlValue::as_str)
        .map(ToOwned::to_owned)
}

fn set_server_target(table: &mut TomlMap<String, TomlValue>, socket_path: &Path) {
    let cli_entry = table
        .entry("cli".to_string())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()));
    let Some(cli_table) = cli_entry.as_table_mut() else {
        panic!("expected [cli] to be a TOML table");
    };
    let target_entry = cli_table
        .entry("target".to_string())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()));
    let Some(target_table) = target_entry.as_table_mut() else {
        panic!("expected [cli.target] to be a TOML table");
    };
    target_table.insert("type".to_string(), TomlValue::String("unix".to_string()));
    target_table.insert(
        "path".to_string(),
        TomlValue::String(socket_path.display().to_string()),
    );
}

fn clear_server_target(table: &mut TomlMap<String, TomlValue>) {
    let Some(cli_entry) = table.get_mut("cli") else {
        return;
    };
    let Some(cli_table) = cli_entry.as_table_mut() else {
        return;
    };
    cli_table.remove("target");
    if cli_table.is_empty() {
        table.remove("cli");
    }
}

fn sync_home_settings(
    settings_path: &Path,
    storage_dir: &Path,
    socket_path: &Path,
    force_server_target: bool,
) {
    let (mut table, had_explicit_storage, had_explicit_target) =
        match std::fs::read_to_string(settings_path) {
            Ok(contents) => {
                let had_managed_storage = contents.starts_with(MANAGED_STORAGE_MARKER);
                let table = parse_settings_table(&contents, settings_path);
                let had_explicit_storage =
                    !had_managed_storage && has_explicit_storage_root(&table);
                let had_explicit_target = server_target_from_table(&table).is_some();
                (table, had_explicit_storage, had_explicit_target)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                (TomlMap::new(), false, false)
            }
            Err(err) => panic!("failed to read {}: {err}", settings_path.display()),
        };

    table
        .entry("_version".to_string())
        .or_insert(TomlValue::Integer(1));

    if !had_explicit_storage {
        set_server_storage_root(&mut table, storage_dir);
    }

    if force_server_target || (!had_explicit_storage && !had_explicit_target) {
        set_server_target(&mut table, socket_path);
    } else if had_explicit_storage && !had_explicit_target {
        clear_server_target(&mut table);
    }

    if !had_explicit_storage {
        let mut rest = table.clone();
        clear_server_storage(&mut rest);
        rest.remove("_version");
        let managed_target = !had_explicit_target && !force_server_target;
        if managed_target {
            clear_server_target(&mut rest);
        }
        let rest_toml = toml::to_string(&rest)
            .unwrap_or_else(|err| panic!("failed to serialize {}: {err}", settings_path.display()));
        let contents = if managed_target {
            format!(
                "{MANAGED_STORAGE_MARKER}\n_version = 1\n\n[server.storage]\nroot = \"{}\"\n\n[cli.target]\ntype = \"unix\"\npath = \"{}\"\n\n{rest_toml}",
                storage_dir.display(),
                socket_path.display()
            )
        } else {
            managed_storage_settings(storage_dir, &rest_toml)
        };
        ensure_parent_dir(settings_path);
        std::fs::write(settings_path, contents)
            .unwrap_or_else(|err| panic!("failed to write {}: {err}", settings_path.display()));
        return;
    }

    write_settings_table(settings_path, &table);
}

fn has_explicit_server_auth_methods(table: &TomlMap<String, TomlValue>) -> bool {
    table
        .get("server")
        .and_then(TomlValue::as_table)
        .and_then(|server| server.get("auth"))
        .and_then(TomlValue::as_table)
        .and_then(|auth| auth.get("methods"))
        .is_some()
}

fn set_server_auth_methods(table: &mut TomlMap<String, TomlValue>, methods: &[&str]) {
    let server_entry = table
        .entry("server".to_string())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()));
    let Some(server_table) = server_entry.as_table_mut() else {
        panic!("expected [server] to be a TOML table");
    };
    let auth_entry = server_table
        .entry("auth".to_string())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()));
    let Some(auth_table) = auth_entry.as_table_mut() else {
        panic!("expected [server.auth] to be a TOML table");
    };
    auth_table.insert(
        "methods".to_string(),
        TomlValue::Array(
            methods
                .iter()
                .map(|method| TomlValue::String((*method).to_string()))
                .collect(),
        ),
    );
}

fn ensure_home_server_auth_methods(
    settings_path: &Path,
    storage_dir: &Path,
    socket_path: &Path,
    force_server_target: bool,
) {
    let mut table = match std::fs::read_to_string(settings_path) {
        Ok(contents) => parse_settings_table(&contents, settings_path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => TomlMap::new(),
        Err(err) => panic!("failed to read {}: {err}", settings_path.display()),
    };

    if has_explicit_server_auth_methods(&table) {
        return;
    }

    table
        .entry("_version".to_string())
        .or_insert(TomlValue::Integer(1));
    set_server_auth_methods(&mut table, &["dev-token"]);
    write_settings_table(settings_path, &table);
    sync_home_settings(settings_path, storage_dir, socket_path, force_server_target);
}

fn has_explicit_storage_root(table: &TomlMap<String, TomlValue>) -> bool {
    table
        .get("server")
        .and_then(TomlValue::as_table)
        .and_then(|server| server.get("storage"))
        .and_then(TomlValue::as_table)
        .and_then(|storage| storage.get("root"))
        .is_some()
}

fn set_server_storage_root(table: &mut TomlMap<String, TomlValue>, storage_dir: &Path) {
    let server_entry = table
        .entry("server".to_string())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()));
    let Some(server_table) = server_entry.as_table_mut() else {
        panic!("expected [server] to be a TOML table");
    };
    let storage_entry = server_table
        .entry("storage".to_string())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()));
    let Some(storage_table) = storage_entry.as_table_mut() else {
        panic!("expected [server.storage] to be a TOML table");
    };
    storage_table.insert(
        "root".to_string(),
        TomlValue::String(storage_dir.display().to_string()),
    );
}

fn clear_server_storage(table: &mut TomlMap<String, TomlValue>) {
    let Some(server_entry) = table.get_mut("server") else {
        return;
    };
    let Some(server_table) = server_entry.as_table_mut() else {
        return;
    };
    server_table.remove("storage");
    if server_table.is_empty() {
        table.remove("server");
    }
}

fn server_runtime_directory(server: &ServerPaths) -> RuntimeDirectory {
    Storage::new(&server.storage_dir).runtime_directory()
}

fn server_running(server: &ServerPaths) -> bool {
    ServerDaemon::load_running(&server_runtime_directory(server))
        .ok()
        .flatten()
        .is_some()
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync test helper polls a child server process without requiring a Tokio runtime."
)]
fn wait_for_server_running(server: &ServerPaths) {
    let poll = std::time::Duration::from_millis(50);
    let timeout = std::time::Duration::from_secs(5);
    let mut elapsed = std::time::Duration::ZERO;
    while elapsed < timeout {
        if server_running(server) {
            return;
        }
        std::thread::sleep(poll);
        elapsed += poll;
    }
    panic!(
        "timed out waiting for test server record in {}",
        server.storage_dir.display()
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "This synchronous test-support helper launches the real fabro CLI server before reqwest clients connect to it."
)]
fn ensure_server_running(fabro_bin: &Path, server: &ServerPaths, config_path: &Path) {
    if server_running(server) {
        return;
    }

    ensure_parent_dir(&server.socket_path);
    ensure_parent_dir(config_path);
    std::fs::create_dir_all(&server.storage_dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", server.storage_dir.display()));
    write_test_server_dev_token(&server.storage_dir);
    ServerDaemon::remove(&server_runtime_directory(server));
    let _ = std::fs::remove_file(&server.socket_path);

    let mut bootstrap = std::process::Command::new(fabro_bin);
    apply_test_isolation(&mut bootstrap, &server.root);
    let output = bootstrap
        .env("SESSION_SECRET", TEST_SESSION_SECRET)
        .env("FABRO_HOME", &server.root)
        .args(["server", "start"])
        .arg("--storage-dir")
        .arg(&server.storage_dir)
        .arg("--bind")
        .arg(&server.socket_path)
        .arg("--config")
        .arg(config_path)
        .output()
        .unwrap_or_else(|err| panic!("failed to execute {}: {err}", fabro_bin.display()));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() || stderr.contains("Server already running"),
        "failed to start test server:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        stderr
    );

    wait_for_server_running(server);
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync test helper polls child shutdown during cleanup without requiring a Tokio runtime."
)]
fn stop_test_server(server: &ServerPaths) {
    let runtime_directory = server_runtime_directory(server);
    let Some(daemon) = ServerDaemon::read(&runtime_directory).ok().flatten() else {
        let _ = std::fs::remove_file(&server.socket_path);
        ServerDaemon::remove(&runtime_directory);
        return;
    };

    fabro_proc::sigterm(daemon.pid);

    let poll = std::time::Duration::from_millis(50);
    let timeout = test_server_stop_timeout();
    let mut elapsed = std::time::Duration::ZERO;
    while elapsed < timeout && fabro_proc::process_running(daemon.pid) {
        std::thread::sleep(poll);
        elapsed += poll;
    }
    if fabro_proc::process_running(daemon.pid) {
        fabro_proc::sigkill(daemon.pid);
    }

    let _ = std::fs::remove_file(&server.socket_path);
    ServerDaemon::remove(&runtime_directory);
}

fn test_server_stop_timeout() -> std::time::Duration {
    // Give the server a brief window to flush state, then escalate.
    // The server's own 5s worker-shutdown grace is unnecessary in tests
    // because no real work needs preserving — any lingering workers are
    // from already-completed runs racing to exit.
    std::time::Duration::from_millis(500)
}

fn shared_server_paths(root: &Path) -> ServerPaths {
    ServerPaths {
        root:        root.to_path_buf(),
        storage_dir: root.join("storage"),
        socket_path: root.join("fabro.sock"),
        config_path: root.join("settings.toml"),
    }
}

fn isolated_server_paths(
    root: &Path,
    test_case_id: &str,
    storage_dir: Option<PathBuf>,
) -> ServerPaths {
    let server_root = root.join("isolated").join(test_case_id);
    ServerPaths {
        root:        server_root.clone(),
        storage_dir: storage_dir.unwrap_or_else(|| server_root.join("storage")),
        socket_path: server_root.join("fabro.sock"),
        config_path: server_root.join("settings.toml"),
    }
}

fn reap_isolated_servers(root: &Path) {
    let isolated_root = root.join("isolated");
    let Ok(entries) = std::fs::read_dir(&isolated_root) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let server_root = entry.path();
        if !server_root.is_dir() {
            continue;
        }
        stop_test_server(&ServerPaths {
            root:        server_root.clone(),
            storage_dir: server_root.join("storage"),
            socket_path: server_root.join("fabro.sock"),
            config_path: server_root.join("settings.toml"),
        });
        let _ = std::fs::remove_dir_all(server_root);
    }
}

fn cleanup_session_root(root: &Path) {
    with_session_lock(root, || {
        // Release our own advisory lock first so `live_marker_count`
        // below can observe that no one is holding the marker file,
        // then remove the file. Order matters: if we unlinked before
        // dropping the handle, peers would still see our LOCK_SH on
        // the (now-unlinked but still open) inode and count us as
        // live, preventing the server teardown.
        {
            let mut slot = MARKER_HANDLE.lock().expect("MARKER_HANDLE lock poisoned");
            // Dropping the File closes the FD and releases LOCK_SH.
            slot.take();
        }
        let marker_path = session_marker_path(root, current_pid());
        let _ = std::fs::remove_file(&marker_path);
        let live_count = live_marker_count(root);
        if live_count == 0 {
            stop_test_server(&shared_server_paths(root));
            reap_isolated_servers(root);
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
                stop_test_server(&shared_server_paths(&root));
                reap_isolated_servers(&root);
                let _ = std::fs::remove_dir_all(&root);
            }
        });
    }
}

fn maybe_reap_stale_tmp_daemons() {
    let (lock_path, stamp_path) = tmp_daemon_reaper_paths();
    if stamp_is_recent(&stamp_path, TMP_DAEMON_REAPER_COOLDOWN) {
        return;
    }

    let Some(parent) = lock_path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    let Ok(lock_file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
    else {
        return;
    };
    let Ok(true) = fabro_proc::try_flock_exclusive(&lock_file) else {
        return;
    };

    if !stamp_is_recent(&stamp_path, TMP_DAEMON_REAPER_COOLDOWN) {
        reap_stale_tmp_daemons();
        let _ = File::create(&stamp_path);
    }
    let _ = fabro_proc::flock_unlock(&lock_file);
}

fn reap_stale_tmp_daemons() {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-ww", "-axo", "pid=,etime=,command="])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((pid, elapsed_secs, command)) = parse_tmp_daemon_ps_line(line) else {
            continue;
        };
        if Duration::from_secs(elapsed_secs) <= STALE_TMP_DAEMON_THRESHOLD {
            continue;
        }
        if tmp_daemon_socket_re().is_match(command) {
            fabro_proc::sigkill(pid);
        }
    }
}

fn tmp_daemon_reaper_paths() -> (PathBuf, PathBuf) {
    let base = short_session_base_dir();
    (
        base.join("tmp-daemon-reaper.lock"),
        base.join("tmp-daemon-reaper.stamp"),
    )
}

fn stamp_is_recent(path: &Path, cooldown: Duration) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified.elapsed().is_ok_and(|elapsed| elapsed < cooldown)
}

fn parse_tmp_daemon_ps_line(line: &str) -> Option<(u32, u64, &str)> {
    static PS_ROW_RE: OnceLock<Regex> = OnceLock::new();
    let captures = PS_ROW_RE
        .get_or_init(|| Regex::new(r"^\s*(\d+)\s+(\S+)\s+(.*)$").expect("static regex"))
        .captures(line)?;
    let pid = captures.get(1)?.as_str().parse::<u32>().ok()?;
    let elapsed_secs = parse_etime(captures.get(2)?.as_str())?;
    let command = captures.get(3)?.as_str();
    Some((pid, elapsed_secs, command))
}

fn tmp_daemon_socket_re() -> &'static Regex {
    static TMP_DAEMON_SOCKET_RE: OnceLock<Regex> = OnceLock::new();
    TMP_DAEMON_SOCKET_RE.get_or_init(|| {
        Regex::new(r"^fabro server unix:/tmp/\.tmp[^/]+/[^/]+\.sock\s*$").expect("static regex")
    })
}

fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, s),
    };
    let nums = rest
        .split(':')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let (hours, minutes, seconds) = match nums.as_slice() {
        [minutes, seconds] => (0, *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return None,
    };
    Some(days * 86_400 + hours * 3_600 + minutes * 60 + seconds)
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
        // Budget: TMPDIR (~49) + prefix + suffix (~6) + /home/fabro-data/fabro.sock
        // (27) < 104
        let label = &test_name[..test_name.len().min(16)];
        let context_root = tempfile::Builder::new()
            .prefix(&format!(".ft-{label}-"))
            .tempdir()
            .expect("failed to create temp dir");
        let root_path = context_root.path().to_path_buf();
        let (_, test_run_id, session_paths) = session_paths();
        NEXTEST_REAPED.get_or_init(|| reap_stale_session_roots(SessionMode::Nextest));
        PROCESS_REAPED.get_or_init(|| reap_stale_session_roots(SessionMode::Process));
        TMP_DAEMONS_REAPED.get_or_init(maybe_reap_stale_tmp_daemons);
        with_session_lock(&session_paths.root, || {
            std::fs::create_dir_all(session_clients_dir(&session_paths.root)).unwrap_or_else(
                |err| {
                    panic!(
                        "failed to create {}: {err}",
                        session_clients_dir(&session_paths.root).display()
                    )
                },
            );
            std::fs::create_dir_all(&session_paths.server.storage_dir).unwrap_or_else(|err| {
                panic!(
                    "failed to create {}: {err}",
                    session_paths.server.storage_dir.display()
                )
            });
            write_settings_file(
                &session_paths.server.config_path,
                &session_paths.server.storage_dir,
                "",
            );
            if fabro_bin.exists() {
                ensure_server_running(
                    &fabro_bin,
                    &session_paths.server,
                    &session_paths.server.config_path,
                );
            }
            write_marker(&session_paths.root);
        });

        let temp_dir = root_path.join("temp");
        let home_dir = root_path.join("home");
        let storage_dir = session_paths.server.storage_dir.clone();
        let test_case_id = test_case_id();

        std::fs::create_dir_all(&temp_dir).expect("failed to create temp_dir");
        std::fs::create_dir_all(&home_dir).expect("failed to create home_dir");
        sync_home_settings(
            &home_settings_path(&home_dir),
            &storage_dir,
            &session_paths.server.socket_path,
            false,
        );
        let temp_dir_str = temp_dir
            .to_str()
            .expect("temp_dir should be valid UTF-8 for snapshot filtering");
        let home_dir_str = home_dir
            .to_str()
            .expect("home_dir should be valid UTF-8 for snapshot filtering");
        let storage_dir_str = storage_dir
            .to_str()
            .expect("storage_dir should be valid UTF-8 for snapshot filtering");

        let filters = vec![
            (
                regex::escape(&format!("/private{temp_dir_str}")),
                "[TEMP_DIR]".to_string(),
            ),
            (regex::escape(temp_dir_str), "[TEMP_DIR]".to_string()),
            (
                regex::escape(&format!("/private{home_dir_str}")),
                "[HOME_DIR]".to_string(),
            ),
            (regex::escape(home_dir_str), "[HOME_DIR]".to_string()),
            (
                regex::escape(&format!("/private{storage_dir_str}")),
                "[STORAGE_DIR]".to_string(),
            ),
            (regex::escape(storage_dir_str), "[STORAGE_DIR]".to_string()),
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
            active_socket_path: session_paths.server.socket_path,
            isolated_server: None,
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
    #[expect(
        clippy::disallowed_methods,
        reason = "Tests spawn the real fabro CLI synchronously; assert_cmd wraps the std Command we build here."
    )]
    pub fn command(&self) -> Command {
        let mut inner = std::process::Command::new(&self.fabro_bin);
        apply_test_isolation(&mut inner, &self.home_dir);
        inner.current_dir(&self.temp_dir);
        Command::from_std(inner)
    }

    /// Build a `validate` subcommand.
    pub fn validate(&self) -> Command {
        self.ensure_home_server_auth_methods();
        let mut cmd = self.command();
        cmd.arg("validate");
        cmd
    }

    /// Build a `run` subcommand.
    pub fn run_cmd(&self) -> Command {
        self.ensure_home_server_auth_methods();
        let mut cmd = self.command();
        cmd.arg("run");
        self.append_test_labels(&mut cmd);
        cmd
    }

    /// Build a `create` subcommand with per-test labels attached.
    pub fn create_cmd(&self) -> Command {
        self.ensure_home_server_auth_methods();
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

    /// Copy a fixture file from the repo `test/` directory into `temp_dir`,
    /// isolating the test from the repo's `.fabro/project.toml`.
    ///
    /// Returns the path to the copied file inside `temp_dir`.
    pub fn install_fixture(&self, name: &str) -> PathBuf {
        let src =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../../test/{name}"));
        let src = src
            .canonicalize()
            .unwrap_or_else(|_| panic!("fixture {name} not found at {}", src.display()));
        let dest = self.temp_dir.join(name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::copy(&src, &dest)
            .unwrap_or_else(|_| panic!("failed to copy fixture {name} to {}", dest.display()));
        dest
    }

    /// Initialize a git repository in `temp_dir`.
    #[expect(
        clippy::disallowed_methods,
        reason = "This synchronous test-support helper initializes fixture repositories with the real git CLI."
    )]
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
        let path = path.as_ref();
        let full = self.home_dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        let content = content.as_ref();
        if path == std::path::Path::new(".fabro/settings.toml") {
            let contents =
                std::str::from_utf8(content).expect("settings.toml should be valid UTF-8");
            let table = parse_settings_table(contents, &full);
            write_settings_table(&full, &table);
            sync_home_settings(
                &full,
                &self.storage_dir,
                &self.active_socket_path,
                self.isolated_server.is_some(),
            );
        } else {
            std::fs::write(&full, content).expect("failed to write file");
        }
        self
    }

    pub fn set_http_target(&self, base_url: &str) -> &Self {
        self.write_home(
            ".fabro/settings.toml",
            format!("_version = 1\n\n[cli.target]\ntype = \"http\"\nurl = \"{base_url}/api/v1\"\n"),
        )
    }

    pub fn ensure_home_server_auth_methods(&self) -> &Self {
        let settings_path = home_settings_path(&self.home_dir);
        ensure_home_server_auth_methods(
            &settings_path,
            &self.storage_dir,
            &self.active_socket_path,
            self.isolated_server.is_some(),
        );
        self
    }

    pub fn server_target(&self) -> String {
        self.active_socket_path.display().to_string()
    }

    pub fn isolated_server(&mut self) -> &mut Self {
        if self.isolated_server.is_some() {
            return self;
        }

        let settings_path = home_settings_path(&self.home_dir);
        let storage_dir_override = settings_storage_dir(&settings_path);
        let server =
            isolated_server_paths(&self.session_root, &self.test_case_id, storage_dir_override);
        std::fs::create_dir_all(&server.root)
            .unwrap_or_else(|err| panic!("failed to create {}: {err}", server.root.display()));
        sync_home_settings(
            &settings_path,
            &server.storage_dir,
            &server.socket_path,
            true,
        );
        if fabro_bin_exists(&self.fabro_bin) {
            ensure_server_running(&self.fabro_bin, &server, &settings_path);
        }
        self.storage_dir.clone_from(&server.storage_dir);
        self.active_socket_path.clone_from(&server.socket_path);
        self.isolated_server = Some(server);
        self
    }

    /// Register an additional storage directory that this test may cause to
    /// auto-start a daemon for, so Drop can stop it.
    pub fn manage_storage_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        let path = path.as_ref().to_path_buf();
        if path != self.storage_dir && !self.managed_storage_dirs.contains(&path) {
            write_test_server_dev_token(&path);
            self.managed_storage_dirs.push(path);
        }
        self
    }

    /// Find a run directory whose name ends with `run_id_suffix`.
    pub fn find_run_dir(&self, run_id_suffix: &str) -> PathBuf {
        if let Ok(run_id) = run_id_suffix.parse::<RunId>() {
            let run_dir = Storage::new(&self.storage_dir)
                .run_scratch(&run_id)
                .root()
                .to_path_buf();
            if run_dir.is_dir() {
                return run_dir;
            }
        }

        let scratch_dir = self.storage_dir.join("scratch");
        std::fs::read_dir(&scratch_dir)
            .expect("scratch directory should exist")
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
                    scratch_dir.display()
                )
            })
    }

    /// Return the only run directory currently present under storage.
    pub fn single_run_dir(&self) -> PathBuf {
        let output = self
            .ps()
            .args(["-a", "--json", "--label", &self.test_case_label()])
            .output()
            .expect("ps should execute");
        assert!(
            output.status.success(),
            "ps should succeed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let runs: Vec<Value> =
            serde_json::from_slice(&output.stdout).expect("ps JSON should parse");
        let entries: Vec<_> = runs
            .into_iter()
            .filter_map(|run| {
                run.get("run_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .map(|run_id| self.find_run_dir(&run_id))
            .collect();
        let scratch_dir = self.storage_dir.join("scratch");
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one run directory for fabro_test_case={} under {}",
            self.test_case_id(),
            scratch_dir.display()
        );
        entries
            .into_iter()
            .next()
            .expect("exactly one run directory should exist after the length check")
    }
}

fn fabro_bin_exists(path: &Path) -> bool {
    path.exists()
}

impl Drop for TestContext {
    fn drop(&mut self) {
        for storage_dir in &self.managed_storage_dirs {
            stop_test_server(&ServerPaths {
                root:        storage_dir.clone(),
                storage_dir: storage_dir.clone(),
                socket_path: PathBuf::new(),
                config_path: PathBuf::new(),
            });
        }

        if let Some(server) = &self.isolated_server {
            stop_test_server(server);
            let _ = std::fs::remove_dir_all(&server.root);
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

        cleanup_session_root(&self.session_root);
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
    #[allow(
        clippy::print_stderr,
        reason = "Raw child output is mirrored to stderr for debugging failed tests."
    )]
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

#[doc(hidden)]
pub trait FabroSnapshotFilterSource {
    fn snapshot_filters(&self) -> Vec<(String, String)>;
}

impl FabroSnapshotFilterSource for TestContext {
    fn snapshot_filters(&self) -> Vec<(String, String)> {
        self.filters()
    }
}

impl FabroSnapshotFilterSource for Vec<(String, String)> {
    fn snapshot_filters(&self) -> Vec<(String, String)> {
        self.clone()
    }
}

impl FabroSnapshotFilterSource for [(String, String)] {
    fn snapshot_filters(&self) -> Vec<(String, String)> {
        self.to_vec()
    }
}

impl<T> FabroSnapshotFilterSource for &T
where
    T: FabroSnapshotFilterSource + ?Sized,
{
    fn snapshot_filters(&self) -> Vec<(String, String)> {
        (*self).snapshot_filters()
    }
}

#[doc(hidden)]
pub fn snapshot_filters_from<T>(source: &T) -> Vec<(String, String)>
where
    T: FabroSnapshotFilterSource + ?Sized,
{
    source.snapshot_filters()
}

/// Add JSON-specific normalizations to a snapshot filter set.
pub fn json_snapshot_filters(mut filters: Vec<(String, String)>) -> Vec<(String, String)> {
    filters.push((
        r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""id":\s*"[0-9a-f-]+""#.to_string(),
        r#""id": "[EVENT_ID]""#.to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": "[DURATION_MS]""#.to_string(),
    ));
    filters.push((
        r#""manifest_blob":\s*"[0-9a-f]{64}""#.to_string(),
        r#""manifest_blob": "[BLOB_ID]""#.to_string(),
    ));
    filters.push((
        r#""definition_blob":\s*"[0-9a-f]{64}""#.to_string(),
        r#""definition_blob": "[BLOB_ID]""#.to_string(),
    ));
    filters.push((
        r#""run_dir":\s*"\[STORAGE_DIR\]/scratch/\d{8}-\[ULID\]""#.to_string(),
        r#""run_dir": "[RUN_DIR]""#.to_string(),
    ));
    filters.push((
        regex::escape(env!("CARGO_PKG_VERSION")),
        "[VERSION]".to_string(),
    ));
    filters
}

/// Create a `TestContext` using the `fabro` binary built by cargo.
///
/// Automatically registers a `[FIXTURES]` snapshot filter for the `test/`
/// directory at the repository root (found by walking up from
/// `CARGO_MANIFEST_DIR`).
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

/// Snapshot a JSON-serializable value using insta with Fabro's default filters.
///
/// Usage:
/// ```ignore
/// fabro_json_snapshot!(context, value, @"...");
/// fabro_json_snapshot!(context.filters(), value, @"...");
/// fabro_json_snapshot!(value, @"...");
/// ```
#[macro_export]
macro_rules! fabro_json_snapshot {
    ($value:expr, @$snapshot:literal) => {{
        let filters = $crate::json_snapshot_filters($crate::TestContext::default_filters());
        let filters: Vec<(&str, &str)> = filters
            .iter()
            .map(|(pattern, replacement)| (pattern.as_str(), replacement.as_str()))
            .collect();
        let rendered = serde_json::to_string_pretty(&$value).unwrap();
        insta::with_settings!({ filters => filters }, {
            insta::assert_snapshot!(rendered, @$snapshot);
        });
    }};
    ($filter_source:expr, $value:expr, @$snapshot:literal) => {{
        let filters =
            $crate::json_snapshot_filters($crate::snapshot_filters_from(&$filter_source));
        let filters: Vec<(&str, &str)> = filters
            .iter()
            .map(|(pattern, replacement)| (pattern.as_str(), replacement.as_str()))
            .collect();
        let rendered = serde_json::to_string_pretty(&$value).unwrap();
        insta::with_settings!({ filters => filters }, {
            insta::assert_snapshot!(rendered, @$snapshot);
        });
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
    server:       twin_github::TestServer,
}

pub fn test_http_client() -> fabro_http::HttpClient {
    fabro_http::test_http_client().expect("test HTTP client should build")
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
        cmd.env(EnvVars::OPENAI_BASE_URL, &self.base_url);
        cmd.env(EnvVars::OPENAI_API_KEY, namespace);
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
        assert_reqwest_status(response, fabro_http::StatusCode::OK, "POST /__admin/reset").await;
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
        assert_reqwest_status(
            response,
            fabro_http::StatusCode::OK,
            "POST /__admin/scenarios",
        )
        .await;
    }
}

#[derive(Debug, Clone)]
pub struct TwinScenario {
    matcher: Map<String, Value>,
    script:  Value,
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
            script:  json!({ "kind": "success" }),
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
    name:      String,
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
#[allow(
    clippy::missing_panics_doc,
    reason = "Test bootstrap panics on startup failure by design."
)]
pub async fn twin_openai() -> &'static TwinOpenAi {
    TWIN_OPENAI
        .get_or_init(|| async {
            let listener = TokioTcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind twin-openai");
            let addr = listener.local_addr().expect("local addr");
            let base_url = format!("http://127.0.0.1:{}/v1", addr.port());

            let config = TwinConfig {
                bind_addr:    addr,
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
                let response =
                    time::timeout(Duration::from_millis(250), client.get(&healthz_url).send())
                        .await;
                if let Ok(Ok(resp)) = response {
                    let status = resp.status();
                    if status == fabro_http::StatusCode::OK {
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
            let base_url = std::env::var($crate::EnvVars::OPENAI_BASE_URL)
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let api_key = std::env::var($crate::EnvVars::OPENAI_API_KEY)
                .expect("OPENAI_API_KEY must be set in live/strict mode");
            (base_url, api_key)
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

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
            *key == std::ffi::OsStr::new(EnvVars::OPENAI_BASE_URL)
                && *value == Some(std::ffi::OsStr::new("http://127.0.0.1:3000/v1"))
        }),);
        assert!(envs.iter().any(|(key, value)| {
            *key == std::ffi::OsStr::new(EnvVars::OPENAI_API_KEY)
                && *value == Some(std::ffi::OsStr::new("test-namespace"))
        }),);
    }

    #[test]
    fn json_snapshot_filters_normalize_json_fields() {
        let mut base_filters = TestContext::default_filters();
        base_filters.push(("custom-value".to_string(), "[CUSTOM]".to_string()));
        let filters = json_snapshot_filters(base_filters);
        let value = serde_json::json!({
            "id": "a68e40fe-0877-48a3-913f-6339b0d198cc",
            "created_at": "2026-04-24T12:34:56.789Z",
            "duration_ms": 12345,
            "manifest_blob": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "definition_blob": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
            "run_dir": "[STORAGE_DIR]/scratch/20260424-01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "message": "custom-value"
        });
        let rendered = serde_json::to_string_pretty(&value).expect("json should render");

        assert_eq!(
            apply_filters(&rendered, &filters),
            r#"{
  "id": "[EVENT_ID]",
  "created_at": "[TIMESTAMP]",
  "duration_ms": "[DURATION_MS]",
  "manifest_blob": "[BLOB_ID]",
  "definition_blob": "[BLOB_ID]",
  "run_dir": "[RUN_DIR]",
  "message": "[CUSTOM]"
}"#
        );
    }

    #[test]
    fn fabro_json_snapshot_accepts_default_filters_and_extra_filters() {
        crate::fabro_json_snapshot!(
            vec![("custom-value".to_string(), "[CUSTOM]".to_string())],
            serde_json::json!({
                "created_at": "2026-04-24T12:34:56Z",
                "message": "custom-value"
            }),
            @r#"
        {
          "created_at": "[TIMESTAMP]",
          "message": "[CUSTOM]"
        }
        "#
        );
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "Regression test: spawn /usr/bin/env synchronously and inspect stdout to assert the harness's env isolation."
    )]
    fn apply_test_isolation_strips_ambient_credentials() {
        let home = tempfile::tempdir().expect("temp home should be created");
        let mut cmd = std::process::Command::new("/usr/bin/env");
        apply_test_isolation_with_lookup(&mut cmd, home.path(), |name| match name {
            EnvVars::PATH => Some(std::ffi::OsString::from("/usr/bin:/bin")),
            EnvVars::LLVM_PROFILE_FILE => Some(std::ffi::OsString::from("/tmp/coverage.profraw")),
            EnvVars::GITHUB_TOKEN => Some(std::ffi::OsString::from("sentinel-should-not-leak")),
            EnvVars::ANTHROPIC_API_KEY => {
                Some(std::ffi::OsString::from("sentinel-also-should-not-leak"))
            }
            _ => None,
        });
        let output = cmd.output().expect("/usr/bin/env should execute");
        assert!(output.status.success(), "env exited non-zero");
        let env_output = String::from_utf8(output.stdout).expect("env stdout should be UTF-8");

        assert!(
            !env_output
                .lines()
                .any(|line| line.starts_with("GITHUB_TOKEN=")),
            "GITHUB_TOKEN leaked into child env:\n{env_output}"
        );
        assert!(
            !env_output
                .lines()
                .any(|line| line.starts_with("ANTHROPIC_API_KEY=")),
            "ANTHROPIC_API_KEY leaked into child env:\n{env_output}"
        );
        assert!(
            env_output.lines().any(|line| line.starts_with("PATH=")),
            "PATH should be preserved so subprocess can find git and friends"
        );
        assert!(
            env_output
                .lines()
                .any(|line| line.starts_with("FABRO_NO_UPGRADE_CHECK=true")),
            "harness-set env vars should still be present"
        );
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
        let (_, run_id, paths) = session_paths_for_run_id(Some("nextest-run-123"));
        assert_eq!(run_id, "nextest-run-123");
        assert!(paths.root.ends_with(Path::new("fx").join("n-nextestrun12")));
        assert_eq!(paths.server.storage_dir, paths.root.join("storage"));
        assert_eq!(paths.server.socket_path, paths.root.join("fabro.sock"));
    }

    #[test]
    fn session_paths_fall_back_to_process_storage_dir() {
        let (_, run_id, paths) = session_paths_for_run_id(None);
        assert_eq!(run_id, format!("process-{}", current_pid()));
        assert!(
            paths
                .root
                .ends_with(Path::new("fx").join(format!("p-{}", current_pid())))
        );
        assert_eq!(paths.server.storage_dir, paths.root.join("storage"));
        assert_eq!(paths.server.socket_path, paths.root.join("fabro.sock"));
    }

    #[test]
    fn session_lock_path_lives_outside_session_root() {
        let root = Path::new("/tmp/fx/n-session");
        let lock_path = session_lock_path(root);

        assert!(
            !lock_path.starts_with(root),
            "session lock must survive remove_dir_all({})",
            root.display()
        );
    }

    #[test]
    fn parse_etime_accepts_bsd_elapsed_time_formats() {
        assert_eq!(parse_etime("00:42"), Some(42));
        assert_eq!(parse_etime("01:23:45"), Some(5_025));
        assert_eq!(parse_etime("2-03:04:05"), Some(183_845));
    }

    #[test]
    fn parse_etime_rejects_malformed_elapsed_time() {
        assert_eq!(parse_etime(""), None);
        assert_eq!(parse_etime("abc"), None);
        assert_eq!(parse_etime("01:02:03:04"), None);
        assert_eq!(parse_etime("2-not-time"), None);
    }

    #[test]
    fn parse_tmp_daemon_ps_line_handles_padded_rows() {
        let line = "43640       12:14 fabro server unix:/tmp/.tmppL8cKh/fabro.sock";

        assert_eq!(
            parse_tmp_daemon_ps_line(line),
            Some((43_640, 734, "fabro server unix:/tmp/.tmppL8cKh/fabro.sock"))
        );
    }

    #[test]
    fn tmp_daemon_socket_regex_matches_only_test_tmp_unix_daemons() {
        let re = tmp_daemon_socket_re();

        assert!(re.is_match("fabro server unix:/tmp/.tmpAbC/fabro.sock"));
        assert!(!re.is_match("fabro server unix:/tmp/.ft-foo-XYZ/test.sock"));
        assert!(!re.is_match("sh -c fabro server unix:/tmp/.tmpAbC/fabro.sock"));
        assert!(!re.is_match("fabro server unix:/Users/me/.fabro/fabro.sock"));
        assert!(!re.is_match("fabro server unix:/tmp/notmatching/fabro.sock"));
        assert!(!re.is_match("fabro server tcp:127.0.0.1:32276"));
    }

    #[test]
    fn stamp_is_recent_checks_missing_and_fresh_stamps() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let missing = temp.path().join("missing.stamp");
        assert!(!stamp_is_recent(&missing, Duration::from_mins(1)));

        let fresh = temp.path().join("fresh.stamp");
        File::create(&fresh).expect("stamp should be created");
        assert!(stamp_is_recent(&fresh, Duration::from_mins(1)));
    }

    #[test]
    fn run_and_create_commands_include_test_labels() {
        let context_root = tempfile::tempdir().expect("failed to create temp dir");
        let context = TestContext {
            temp_dir:             context_root.path().join("temp"),
            home_dir:             context_root.path().join("home"),
            storage_dir:          context_root.path().join("storage"),
            test_case_id:         "case-123".to_string(),
            test_run_id:          "run-cmd-labels".to_string(),
            session_root:         context_root.path().join("session"),
            fabro_bin:            context_root.path().join("fabro"),
            filters:              Vec::new(),
            active_socket_path:   context_root.path().join("fabro.sock"),
            isolated_server:      None,
            managed_storage_dirs: Vec::new(),
            _context_root:        context_root,
        };

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

    #[test]
    fn stop_test_server_timeout_is_short() {
        assert!(
            test_server_stop_timeout() <= std::time::Duration::from_secs(1),
            "test harness should SIGKILL quickly — no real work to preserve"
        );
    }
}
