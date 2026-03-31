use std::path::PathBuf;
use std::process::Output;

use assert_cmd::Command;
use regex::Regex;

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
    Off,
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
                _ => Self::Off,
            },
        }
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

/// An isolated test context for running fabro CLI commands.
///
/// Creates temporary directories for home, storage, and working directory,
/// and provides methods to build commands with proper isolation env vars.
pub struct TestContext {
    pub temp_dir: PathBuf,
    pub home_dir: PathBuf,
    pub storage_dir: PathBuf,
    fabro_bin: PathBuf,
    filters: Vec<(String, String)>,
    _root: tempfile::TempDir,
}

impl TestContext {
    /// Create a new isolated test context.
    ///
    /// `fabro_bin` should be the path to the compiled `fabro` binary,
    /// typically obtained via `env!("CARGO_BIN_EXE_fabro")`.
    pub fn new(fabro_bin: PathBuf) -> Self {
        let root = tempfile::tempdir().expect("failed to create temp dir");
        let root_path = root.path().to_path_buf();

        let temp_dir = root_path.join("temp");
        let home_dir = root_path.join("home");
        let storage_dir = root_path.join("storage");

        std::fs::create_dir_all(&temp_dir).expect("failed to create temp_dir");
        std::fs::create_dir_all(&home_dir).expect("failed to create home_dir");
        std::fs::create_dir_all(&storage_dir).expect("failed to create storage_dir");

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
        ];

        Self {
            temp_dir,
            home_dir,
            storage_dir,
            fabro_bin,
            filters,
            _root: root,
        }
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

    /// Build a base `Command` with all isolation env vars set.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.fabro_bin);
        cmd.env("NO_COLOR", "1");
        cmd.env("HOME", &self.home_dir);
        cmd.env("FABRO_NO_UPGRADE_CHECK", "true");
        cmd.env("FABRO_STORAGE_DIR", &self.storage_dir);
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
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one run directory under {}",
            runs_dir.display()
        );
        entries.into_iter().next().unwrap()
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
#[macro_export]
macro_rules! test_context {
    () => {
        $crate::TestContext::new(std::path::PathBuf::from(env!("CARGO_BIN_EXE_fabro")))
    };
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
