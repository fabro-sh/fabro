#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use std::path::PathBuf;

use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use predicates::prelude::*;

use super::support::run_state;
use crate::support::unique_run_id;

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

    Usage: fabro [OPTIONS] [COMMAND]

    For more information, try '--help'.
    ");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_settings(stdout: &[u8]) -> serde_json::Value {
    serde_yaml::from_slice(stdout).expect("stdout should be valid YAML settings")
}

fn server_storage_root(settings: &serde_json::Value) -> &str {
    settings
        .get("server")
        .and_then(|server| server.get("server"))
        .and_then(|server| server.get("storage"))
        .and_then(|storage| storage.get("root"))
        .and_then(serde_json::Value::as_str)
        .expect("server.storage.root")
}

fn server_settings_toml_fixture() -> &'static str {
    r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.storage]
root = "/srv/fabro-server"

[run.model]
name = "server-model"
provider = "openai"

[run.inputs]
server_only = "1"
shared = "server"
"#
}

fn resolved_server_settings_fixture() -> serde_json::Value {
    let settings = fabro_config::ServerSettingsBuilder::from_toml(server_settings_toml_fixture())
        .expect("server settings fixture should resolve");
    serde_json::to_value(settings).expect("resolved settings payload should serialize")
}

fn server_settings_body(settings: &serde_json::Value) -> String {
    serde_json::to_string(settings).expect("settings payload should serialize")
}

/// Set up home config and project config for settings command tests.
/// Uses `context.home_dir` for the home directory. Returns project tempdir.
fn setup_settings_fixture(context: &fabro_test::TestContext) -> tempfile::TempDir {
    context.write_home(
        ".fabro/settings.toml",
        r#"
_version = 1

[cli.output]
verbosity = "verbose"

[run.model]
name = "cli-model"
provider = "openai"

[run.inputs]
cli_only = "1"
shared = "cli"

[run.checkpoint]
exclude_globs = ["cli-only", "shared"]

[[run.hooks]]
id = "shared"
name = "shared"
event = "run_start"
script = "echo cli"

[run.agent.mcps.shared]
type = "stdio"
command = ["echo", "cli"]

[run.sandbox]
provider = "daytona"

[run.sandbox.env]
CLI_ONLY = "1"
SHARED = "cli"

[run.sandbox.daytona]

[run.sandbox.daytona.labels]
cli_only = "1"
shared = "cli"

[server.auth]
methods = ["dev-token"]
"#,
    );

    let project = tempfile::tempdir().expect("project fixture directory should create");
    std::fs::create_dir_all(project.path().join(".fabro"))
        .expect("project .fabro directory should create");
    std::fs::write(
        project.path().join(".fabro/project.toml"),
        r#"
_version = 1

[run.model]
name = "project-model"

[run.inputs]
project_only = "1"
shared = "project"

[[run.hooks]]
id = "project"
name = "project"
event = "run_complete"
script = "echo project"
"#,
    )
    .expect("project config fixture should write");

    let workflow_dir = project.path().join(".fabro").join("workflows").join("demo");
    std::fs::create_dir_all(&workflow_dir).expect("workflow fixture directory should create");
    std::fs::write(
        workflow_dir.join("workflow.toml"),
        r#"
_version = 1

[run]
goal = "demo goal"

[run.model]
name = "run-model"
provider = "anthropic"

[run.inputs]
run_only = "1"
shared = "run"

[run.checkpoint]
exclude_globs = ["run-only", "shared"]

[[run.hooks]]
id = "shared"
name = "shared"
event = "run_start"
script = "echo run"

[[run.hooks]]
id = "run-only"
name = "run-only"
event = "run_complete"
script = "echo run-only"

[run.agent.mcps.shared]
type = "stdio"
command = ["echo", "run"]

[run.agent.mcps.run_only]
type = "stdio"
command = ["echo", "run-only"]

[run.sandbox.env]
RUN_ONLY = "1"
SHARED = "run"

[run.sandbox.daytona]

[run.sandbox.daytona.labels]
run_only = "1"
shared = "run"
"#,
    )
    .expect("workflow fixture config should write");

    std::fs::write(
        project.path().join("standalone.fabro"),
        "digraph Test { start -> end }",
    )
    .expect("standalone workflow fixture should write");

    project
}

/// Set up an external workflow fixture with a custom storage_dir in
/// settings.toml. Returns (project_tempdir, storage_dir_path).
fn setup_external_workflow_fixture(
    context: &mut fabro_test::TestContext,
) -> (tempfile::TempDir, PathBuf) {
    let storage_dir = context.home_dir.join("fabro-data");
    context.manage_storage_dir(&storage_dir);

    context.write_home(
        ".fabro/settings.toml",
        format!(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.storage]
root = "{}"

[run.execution]
approval = "auto"

[[run.prepare.steps]]
script = "cli-setup"
"#,
            storage_dir.display()
        ),
    );

    let project = tempfile::tempdir().expect("external workflow fixture directory should create");
    std::fs::create_dir_all(project.path().join(".fabro"))
        .expect("external workflow .fabro directory should create");
    std::fs::write(
        project.path().join(".fabro/project.toml"),
        r#"
_version = 1

[[run.prepare.steps]]
script = "project-setup"

[run.sandbox]
preserve = true
"#,
    )
    .expect("external workflow project config should write");

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
    .expect("external workflow graph fixture should write");

    std::fs::write(
        project.path().join("workflow.toml"),
        r#"
_version = 1

[workflow]
graph = "workflow.fabro"

[run]
goal = "Ship it"

[run.model]
name = "claude-sonnet-4-6"

[[run.prepare.steps]]
script = "workflow-setup"
"#,
    )
    .expect("external workflow config should write");

    (project, storage_dir)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn create_explicit_workflow_path_uses_project_config_relative_to_workflow() {
    let mut context = test_context!();
    let (project, storage_dir) = setup_external_workflow_fixture(&mut context);
    context.ensure_home_server_auth_methods();
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
    let run_spec =
        serde_json::to_value(state.spec.as_ref().expect("run spec should exist")).unwrap();
    assert_eq!(
        run_spec["settings"]["run"]["execution"]["approval"].as_str(),
        Some("auto")
    );
    assert_eq!(
        run_spec["settings"]["run"]["sandbox"]["preserve"].as_bool(),
        Some(true)
    );
    assert_eq!(
        run_spec["settings"]["run"]["model"]["name"].as_str(),
        Some("gpt-5.2")
    );
    // v2 R30: run.prepare.steps replaces the whole ordered list across layers.
    assert_eq!(
        run_spec["settings"]["run"]["prepare"]["commands"],
        serde_json::json!(["workflow-setup"])
    );
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

#[test]
fn settings_rejects_storage_dir_flag() {
    let context = test_context!();
    context
        .settings()
        .args(["--storage-dir", "/tmp/fabro-settings"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unexpected argument '--storage-dir' found",
        ));
}

#[test]
fn settings_rejects_local_flag() {
    let context = test_context!();
    context
        .settings()
        .arg("--local")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unexpected argument '--local' found",
        ));
}

#[test]
fn settings_rejects_workflow_argument() {
    let context = test_context!();
    context
        .settings()
        .arg("demo")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument 'demo' found"));
}

#[test]
fn settings_fetches_server_resolved_settings() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);
    let server = MockServer::start();
    let server_settings = resolved_server_settings_fixture();
    let mock = server.mock(|when, then| {
        when.method("GET").path("/api/v1/settings");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(server_settings_body(&server_settings));
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            r#"
_version = 1

[cli.target]
type = "http"
url = "{}/api/v1"

[cli.output]
verbosity = "verbose"

[run.model]
name = "cli-model"
provider = "openai"

[run.inputs]
cli_only = "1"
shared = "cli"
"#,
            server.base_url()
        ),
    );

    let output = context
        .settings()
        .current_dir(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    mock.assert();
    let cfg = parse_settings(&output);
    assert_eq!(
        cfg["user"]["cli"]["output"]["verbosity"].as_str(),
        Some("verbose")
    );
    assert_eq!(
        cfg["user"]["features"]["session_sandboxes"].as_bool(),
        Some(false)
    );
    assert_eq!(
        cfg["server"]["server"]["auth"]["methods"][0].as_str(),
        Some("dev-token")
    );
    assert_eq!(server_storage_root(&cfg), "/srv/fabro-server");
    assert_eq!(
        cfg["server"]["server"]["artifacts"]["store"]["type"].as_str(),
        Some("local")
    );
    assert!(cfg.get("run").is_none());
    assert!(cfg.get("project").is_none());
}

#[test]
fn settings_cli_server_target_overrides_configured_server_target() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);
    let configured_server = MockServer::start();
    let configured_mock = configured_server.mock(|when, then| {
        when.method("GET").path("/api/v1/settings");
        then.status(500)
            .body("configured-server-should-not-be-used");
    });
    let cli_server = MockServer::start();
    let cli_server_settings = resolved_server_settings_fixture();
    let cli_mock = cli_server.mock(|when, then| {
        when.method("GET").path("/api/v1/settings");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(server_settings_body(&cli_server_settings));
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            r#"
_version = 1

[cli.target]
type = "http"
url = "{}/api/v1"

[cli.output]
verbosity = "verbose"
"#,
            configured_server.base_url()
        ),
    );

    let output = context
        .settings()
        .current_dir(project.path())
        .args(["--server", &format!("{}/api/v1", cli_server.base_url())])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    cli_mock.assert();
    configured_mock.assert_calls(0);
    let cfg = parse_settings(&output);
    assert_eq!(server_storage_root(&cfg), "/srv/fabro-server");
}

#[test]
fn settings_unreachable_http_target_fails_clearly() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);

    context
        .settings()
        .current_dir(project.path())
        .args(["--server", "http://127.0.0.1:9"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("retrieve_server_settings")
                .or(predicate::str::contains("error sending request")),
        );
}
