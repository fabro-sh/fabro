use std::path::PathBuf;

use fabro_config::mcp::McpTransport;
use fabro_test::{fabro_snapshot, test_context};
use fabro_types::Settings;
use predicates::prelude::*;

use super::support::run_state;
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.settings();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Inspect merged configuration

    Usage: fabro settings [OPTIONS] [WORKFLOW]

    Arguments:
      [WORKFLOW]  Optional workflow name, .fabro path, or .toml run config to overlay

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn old_config_show_command_is_rejected() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["config", "show"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: unrecognized subcommand 'config'

    Usage: fabro [OPTIONS] <COMMAND>

    For more information, try '--help'.
    ");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_settings(stdout: &[u8]) -> Settings {
    serde_yaml::from_slice(stdout).expect("stdout should be valid YAML Settings")
}

/// Set up home config and project config for settings command tests.
/// Uses `context.home_dir` for the home directory. Returns project tempdir.
fn setup_settings_fixture(context: &fabro_test::TestContext) -> tempfile::TempDir {
    context.write_home(
        ".fabro/settings.toml",
        r#"
verbose = true

[llm]
model = "cli-model"
provider = "openai"

[vars]
cli_only = "1"
shared = "cli"

[checkpoint]
exclude_globs = ["cli-only", "shared"]

[[hooks]]
name = "shared"
event = "run_start"
command = "echo cli"

[mcp_servers.shared]
type = "stdio"
command = ["echo", "cli"]

[sandbox]
provider = "daytona"

[sandbox.daytona]
labels = { cli_only = "1", shared = "cli" }

[sandbox.env]
CLI_ONLY = "1"
SHARED = "cli"
"#,
    );

    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("fabro.toml"),
        r#"
version = 1

[fabro]
root = "fabro"

[llm]
model = "project-model"

[vars]
project_only = "1"
shared = "project"

[[hooks]]
name = "project"
event = "run_complete"
command = "echo project"
"#,
    )
    .unwrap();

    let workflow_dir = project.path().join("fabro").join("workflows").join("demo");
    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::write(
        workflow_dir.join("workflow.toml"),
        r#"
version = 1
goal = "demo goal"

[llm]
model = "run-model"
provider = "anthropic"

[vars]
run_only = "1"
shared = "run"

[checkpoint]
exclude_globs = ["run-only", "shared"]

[[hooks]]
name = "shared"
event = "run_start"
command = "echo run"

[[hooks]]
name = "run-only"
event = "run_complete"
command = "echo run-only"

[mcp_servers.shared]
type = "stdio"
command = ["echo", "run"]

[mcp_servers.run_only]
type = "stdio"
command = ["echo", "run-only"]

[sandbox.daytona]
labels = { run_only = "1", shared = "run" }

[sandbox.env]
RUN_ONLY = "1"
SHARED = "run"
"#,
    )
    .unwrap();

    std::fs::write(
        project.path().join("standalone.fabro"),
        "digraph Test { start -> end }",
    )
    .unwrap();

    project
}

/// Set up an external workflow fixture with a custom storage_dir in settings.toml.
/// Returns (project_tempdir, storage_dir_path).
fn setup_external_workflow_fixture(
    context: &mut fabro_test::TestContext,
) -> (tempfile::TempDir, PathBuf) {
    let storage_dir = context.home_dir.join("fabro-data");
    context.manage_storage_dir(&storage_dir);

    context.write_home(
        ".fabro/settings.toml",
        format!(
            r#"
storage_dir = "{}"
auto_approve = true

[setup]
commands = ["cli-setup"]
"#,
            storage_dir.display()
        ),
    );

    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("fabro.toml"),
        r#"
version = 1

[setup]
commands = ["project-setup"]

[sandbox]
preserve = true
"#,
    )
    .unwrap();

    std::fs::write(
        project.path().join("workflow.fabro"),
        r#"
digraph Test {
  start [shape=Mdiamond, label="Start"]
  exit [shape=Msquare, label="Exit"]
  start -> exit
}
"#,
    )
    .unwrap();

    std::fs::write(
        project.path().join("workflow.toml"),
        r#"
version = 1
goal = "Ship it"
graph = "workflow.fabro"

[llm]
model = "claude-sonnet-4-6"

[setup]
commands = ["workflow-setup"]
"#,
    )
    .unwrap();

    (project, storage_dir)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn settings_merges_cli_and_project_defaults() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);

    let output = context
        .settings()
        .current_dir(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let cfg = parse_settings(&output);
    let llm = cfg.llm.as_ref().expect("llm config");
    assert_eq!(llm.model.as_deref(), Some("project-model"));
    assert_eq!(llm.provider.as_deref(), Some("openai"));
    assert_eq!(cfg.goal.as_deref(), None);
    assert_eq!(cfg.fabro.as_ref().map(|f| f.root.as_str()), Some("fabro"));

    let vars = cfg.vars.as_ref().expect("vars");
    assert_eq!(vars.get("cli_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("project_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("shared").map(String::as_str), Some("project"));

    let sandbox = cfg.sandbox.as_ref().expect("sandbox");
    let labels = sandbox
        .daytona
        .as_ref()
        .and_then(|d| d.labels.as_ref())
        .expect("daytona labels");
    assert_eq!(labels.get("cli_only").map(String::as_str), Some("1"));
    assert_eq!(labels.get("shared").map(String::as_str), Some("cli"));
}

#[test]
fn settings_workflow_name_applies_run_overlay_and_deep_merges() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);

    let output = context
        .settings()
        .current_dir(project.path())
        .args(["demo"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let cfg = parse_settings(&output);
    let llm = cfg.llm.as_ref().expect("llm config");
    assert_eq!(cfg.goal.as_deref(), Some("demo goal"));
    assert_eq!(llm.model.as_deref(), Some("run-model"));
    assert_eq!(llm.provider.as_deref(), Some("anthropic"));

    let vars = cfg.vars.as_ref().expect("vars");
    assert_eq!(vars.get("cli_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("project_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("run_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("shared").map(String::as_str), Some("run"));

    assert_eq!(
        cfg.checkpoint.exclude_globs,
        vec![
            "cli-only".to_string(),
            "run-only".to_string(),
            "shared".to_string()
        ]
    );

    assert_eq!(cfg.hooks.len(), 3);
    let shared_hook = cfg
        .hooks
        .iter()
        .find(|hook| hook.name.as_deref() == Some("shared"))
        .expect("shared hook");
    assert_eq!(shared_hook.command.as_deref(), Some("echo run"));
    assert!(
        cfg.hooks
            .iter()
            .any(|hook| hook.name.as_deref() == Some("project"))
    );
    assert!(
        cfg.hooks
            .iter()
            .any(|hook| hook.name.as_deref() == Some("run-only"))
    );

    match &cfg.mcp_servers["shared"].transport {
        McpTransport::Stdio { command, .. } => assert_eq!(command, &vec!["echo", "run"]),
        other => panic!("unexpected MCP transport: {other:?}"),
    }
    assert!(cfg.mcp_servers.contains_key("run_only"));

    let sandbox = cfg.sandbox.as_ref().expect("sandbox");
    let labels = sandbox
        .daytona
        .as_ref()
        .and_then(|d| d.labels.as_ref())
        .expect("daytona labels");
    assert_eq!(labels.get("cli_only").map(String::as_str), Some("1"));
    assert_eq!(labels.get("run_only").map(String::as_str), Some("1"));
    assert_eq!(labels.get("shared").map(String::as_str), Some("run"));

    let env = sandbox.env.as_ref().expect("sandbox env");
    assert_eq!(env.get("CLI_ONLY").map(String::as_str), Some("1"));
    assert_eq!(env.get("RUN_ONLY").map(String::as_str), Some("1"));
    assert_eq!(env.get("SHARED").map(String::as_str), Some("run"));
}

#[test]
fn settings_explicit_workflow_path_uses_workflow_project_layers() {
    let mut context = test_context!();
    let (project, _storage_dir) = setup_external_workflow_fixture(&mut context);
    let cwd = tempfile::tempdir().unwrap();
    let workflow = project.path().join("workflow.toml");

    // Remove FABRO_STORAGE_DIR so the CLI uses storage_dir from settings.toml
    let output = context
        .settings()
        .env_remove("FABRO_STORAGE_DIR")
        .current_dir(cwd.path())
        .args([workflow.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let cfg = parse_settings(&output);
    assert_eq!(cfg.auto_approve, Some(true));
    assert_eq!(
        cfg.setup.as_ref().expect("setup config").commands,
        vec![
            "workflow-setup".to_string(),
            "project-setup".to_string(),
            "cli-setup".to_string(),
        ]
    );
    assert_eq!(
        cfg.sandbox.as_ref().expect("sandbox config").preserve,
        Some(true)
    );
}

#[test]
fn create_explicit_workflow_path_uses_project_config_relative_to_workflow() {
    let mut context = test_context!();
    let (project, storage_dir) = setup_external_workflow_fixture(&mut context);
    let cwd = tempfile::tempdir().unwrap();
    let workflow = project.path().join("workflow.toml");
    let run_id = unique_run_id();

    // Remove FABRO_STORAGE_DIR so the CLI uses storage_dir from settings.toml
    context
        .command()
        .env_remove("FABRO_STORAGE_DIR")
        .current_dir(cwd.path())
        .args([
            "create",
            "--dry-run",
            "--model",
            "gpt-5.2",
            "--run-id",
            run_id.as_str(),
            workflow.to_str().unwrap(),
        ])
        .assert()
        .success();

    let runs_dir = storage_dir.join("scratch");
    let run_dir = std::fs::read_dir(&runs_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(&run_id))
        })
        .unwrap_or_else(|| {
            panic!(
                "expected run directory for {run_id} under {}",
                runs_dir.display()
            )
        });

    let state = run_state(&run_dir);
    let run_record =
        serde_json::to_value(state.run.as_ref().expect("run record should exist")).unwrap();
    assert_eq!(run_record["settings"]["auto_approve"].as_bool(), Some(true));
    assert_eq!(
        run_record["settings"]["storage_dir"].as_str(),
        Some(storage_dir.to_str().unwrap())
    );
    assert_eq!(
        run_record["settings"]["sandbox"]["preserve"].as_bool(),
        Some(true)
    );
    assert_eq!(
        run_record["settings"]["llm"]["model"].as_str(),
        Some("gpt-5.2")
    );
    assert_eq!(
        run_record["settings"]["setup"]["commands"],
        serde_json::json!(["workflow-setup", "project-setup", "cli-setup"])
    );
}

#[test]
fn settings_fabro_path_matches_ambient_defaults() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);

    let ambient = context
        .settings()
        .current_dir(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let graph = context
        .settings()
        .current_dir(project.path())
        .args(["standalone.fabro"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(parse_settings(&graph), parse_settings(&ambient));
}

#[test]
fn settings_missing_run_config_errors() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);

    let mut cmd = context.settings();
    cmd.current_dir(project.path());
    cmd.args(["missing.toml"]);
    let output = cmd.output().expect("command should execute");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).trim().is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error: Workflow not found:"),
        "stderr should report missing workflow path, got:\n{stderr}"
    );
    assert!(
        stderr.contains("missing.toml"),
        "stderr should include missing workflow filename, got:\n{stderr}"
    );
}

#[test]
fn settings_legacy_cli_config_warns_and_ignores_it() {
    let context = test_context!();
    let project = tempfile::tempdir().unwrap();

    context.write_home(
        ".fabro/cli.toml",
        r#"
verbose = true

[llm]
model = "legacy-model"
"#,
    );

    let assert = context
        .settings()
        .current_dir(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("ignoring legacy config file"))
        .stderr(predicate::str::contains("Rename it to"));

    let cfg = parse_settings(&assert.get_output().stdout);
    assert_eq!(cfg.verbose, None);
    assert_eq!(cfg.llm, None);
}

#[test]
fn settings_user_config_wins_over_legacy_cli_config() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);
    context.write_home(
        ".fabro/cli.toml",
        r#"
[llm]
model = "legacy-model"

[vars]
shared = "legacy"
"#,
    );

    let assert = context
        .settings()
        .current_dir(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("ignoring legacy config file"));

    let cfg = parse_settings(&assert.get_output().stdout);
    let llm = cfg.llm.as_ref().expect("llm config");
    assert_eq!(llm.model.as_deref(), Some("project-model"));
    assert_eq!(
        cfg.vars
            .as_ref()
            .and_then(|vars| vars.get("shared").map(String::as_str)),
        Some("project")
    );
}

#[test]
fn settings_uses_fabro_home_for_home_config_resolution() {
    let context = test_context!();
    let fabro_home = tempfile::tempdir().unwrap();

    std::fs::write(
        fabro_home.path().join("settings.toml"),
        r#"
verbose = true

[llm]
model = "from-fabro-home"
"#,
    )
    .unwrap();

    let output = context
        .settings()
        .arg("--json")
        .env("FABRO_HOME", fabro_home.path())
        .env_remove("FABRO_STORAGE_DIR")
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "settings command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let cfg: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cfg["verbose"].as_bool(), Some(true));
    assert_eq!(cfg["llm"]["model"].as_str(), Some("from-fabro-home"));
}

#[test]
fn settings_rejects_server_url_flag() {
    let context = test_context!();
    context
        .command()
        .args(["--server-url", "https://cli.example.com", "settings"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unexpected argument '--server-url' found",
        ));
}
