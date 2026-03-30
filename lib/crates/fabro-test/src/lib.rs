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
    (r"\\([\w\d])", "/$1"),
];

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
                regex::escape(temp_dir.to_str().unwrap()),
                "[TEMP_DIR]".to_string(),
            ),
            (
                regex::escape(home_dir.to_str().unwrap()),
                "[HOME_DIR]".to_string(),
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
        let mut filters: Vec<(String, String)> = INSTA_FILTERS
            .iter()
            .map(|(pat, rep)| ((*pat).to_string(), (*rep).to_string()))
            .collect();
        filters.extend(self.filters.clone());
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

    /// Build a `config` subcommand.
    pub fn config(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("config");
        cmd
    }

    /// Build a `cp` subcommand.
    pub fn cp(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("cp");
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

    /// Build a `preview` subcommand.
    pub fn preview(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("preview");
        cmd
    }

    /// Build a `repo` subcommand.
    pub fn repo(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("repo");
        cmd
    }

    /// Build an `ssh` subcommand.
    pub fn ssh(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("ssh");
        cmd
    }

    /// Build a `system` subcommand.
    pub fn system(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("system");
        cmd
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
