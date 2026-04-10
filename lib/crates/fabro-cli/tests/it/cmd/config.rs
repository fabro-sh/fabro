use std::path::PathBuf;

use fabro_config::parse_settings_layer;
use fabro_test::{fabro_snapshot, test_context};
use fabro_types::settings::SettingsLayer;
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

    Usage: fabro [OPTIONS] <COMMAND>

    For more information, try '--help'.
    ");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_settings(stdout: &[u8]) -> SettingsLayer {
    serde_yaml::from_slice(stdout).expect("stdout should be valid YAML SettingsLayer")
}

fn resolve_cli(settings: &SettingsLayer) -> fabro_types::settings::CliSettings {
    fabro_config::resolve_cli_from_file(settings).expect("cli settings should resolve")
}

fn resolve_project(settings: &SettingsLayer) -> fabro_types::settings::ProjectSettings {
    fabro_config::resolve_project_from_file(settings).expect("project settings should resolve")
}

fn resolve_run(settings: &SettingsLayer) -> fabro_types::settings::RunSettings {
    fabro_config::resolve_run_from_file(settings).expect("run settings should resolve")
}

fn resolve_server(settings: &SettingsLayer) -> fabro_types::settings::ServerSettings {
    fabro_config::resolve_server_from_file(settings).expect("server settings should resolve")
}

fn run_goal_inline(settings: &SettingsLayer) -> Option<String> {
    match resolve_run(settings).goal {
        Some(fabro_types::settings::run::RunGoal::Inline(value)) => Some(value.as_source()),
        _ => None,
    }
}

fn run_model_name(settings: &SettingsLayer) -> Option<String> {
    resolve_run(settings)
        .model
        .name
        .as_ref()
        .map(|value| value.as_source())
}

fn run_model_provider(settings: &SettingsLayer) -> Option<String> {
    resolve_run(settings)
        .model
        .provider
        .as_ref()
        .map(|value| value.as_source())
}

fn run_inputs(settings: &SettingsLayer) -> &std::collections::HashMap<String, toml::Value> {
    settings
        .run
        .as_ref()
        .and_then(|run| run.inputs.as_ref())
        .expect("run.inputs")
}

fn run_sandbox(settings: &SettingsLayer) -> &fabro_types::settings::run::RunSandboxLayer {
    settings
        .run
        .as_ref()
        .and_then(|run| run.sandbox.as_ref())
        .expect("run.sandbox")
}

fn run_checkpoint(settings: &SettingsLayer) -> &fabro_types::settings::run::RunCheckpointLayer {
    settings
        .run
        .as_ref()
        .and_then(|run| run.checkpoint.as_ref())
        .expect("run.checkpoint")
}

fn run_hooks(settings: &SettingsLayer) -> &[fabro_types::settings::run::HookEntry] {
    settings
        .run
        .as_ref()
        .map(|run| run.hooks.as_slice())
        .unwrap_or(&[])
}

fn run_agent_mcps(
    settings: &SettingsLayer,
) -> &std::collections::HashMap<String, fabro_types::settings::run::McpEntryLayer> {
    settings
        .run
        .as_ref()
        .and_then(|run| run.agent.as_ref())
        .map(|agent| &agent.mcps)
        .expect("run.agent.mcps")
}

fn auto_approve_enabled(settings: &SettingsLayer) -> bool {
    resolve_run(settings).execution.approval == fabro_types::settings::run::ApprovalMode::Auto
}

fn run_prepare_commands(settings: &SettingsLayer) -> Vec<String> {
    resolve_run(settings).prepare.commands
}

fn server_storage_root(settings: &SettingsLayer) -> String {
    resolve_server(settings).storage.root.as_source()
}

fn server_settings_fixture() -> SettingsLayer {
    parse_settings_layer(
        r#"
_version = 1

[server.storage]
root = "/srv/fabro-server"

[run.model]
name = "server-model"
provider = "openai"

[run.inputs]
server_only = "1"
shared = "server"
"#,
    )
    .expect("server settings fixture should parse")
}

fn server_settings_body(settings: &SettingsLayer) -> String {
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
"#,
    );

    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("fabro.toml"),
        r#"
_version = 1

[project]
directory = "fabro"

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
    .unwrap();

    let workflow_dir = project.path().join("fabro").join("workflows").join("demo");
    std::fs::create_dir_all(&workflow_dir).unwrap();
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
_version = 1

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

    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("fabro.toml"),
        r#"
_version = 1

[[run.prepare.steps]]
script = "project-setup"

[run.sandbox]
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
    .unwrap();

    (project, storage_dir)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn settings_local_merges_cli_and_project_defaults() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);

    let output = context
        .settings()
        .arg("--local")
        .current_dir(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let cfg = parse_settings(&output);
    assert_eq!(run_model_name(&cfg).as_deref(), Some("project-model"));
    assert_eq!(run_model_provider(&cfg).as_deref(), Some("openai"));
    assert_eq!(run_goal_inline(&cfg).as_deref(), None);
    assert_eq!(resolve_project(&cfg).directory, "fabro");

    // v2 R22: run.inputs replaces the inherited map wholesale rather than
    // merging by key, so the project layer wipes out the CLI layer's inputs.
    let vars = run_inputs(&cfg);
    assert_eq!(vars.get("project_only").and_then(|v| v.as_str()), Some("1"));
    assert_eq!(vars.get("shared").and_then(|v| v.as_str()), Some("project"));
    assert!(
        !vars.contains_key("cli_only"),
        "run.inputs should replace across layers, not merge by key"
    );

    // v2 R71: provider-native maps such as run.sandbox.daytona.labels remain
    // sticky merge-by-key, so CLI labels persist under the project layer.
    let sandbox = run_sandbox(&cfg);
    let labels = &sandbox.daytona.as_ref().expect("daytona").labels;
    assert_eq!(labels.get("cli_only").map(String::as_str), Some("1"));
    assert_eq!(labels.get("shared").map(String::as_str), Some("cli"));
}

#[test]
fn settings_local_workflow_name_applies_run_overlay_and_deep_merges() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);

    let output = context
        .settings()
        .current_dir(project.path())
        .args(["--local", "demo"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    use fabro_types::settings::run::McpEntryLayer;

    let cfg = parse_settings(&output);
    assert_eq!(run_goal_inline(&cfg).as_deref(), Some("demo goal"));
    assert_eq!(run_model_name(&cfg).as_deref(), Some("run-model"));
    assert_eq!(run_model_provider(&cfg).as_deref(), Some("anthropic"));

    // v2 R22: run.inputs replaces wholesale, so the workflow layer wins
    // over project and cli.
    let vars = run_inputs(&cfg);
    assert_eq!(vars.get("run_only").and_then(|v| v.as_str()), Some("1"));
    assert_eq!(vars.get("shared").and_then(|v| v.as_str()), Some("run"));

    // checkpoint.exclude_globs is a security/policy list: replace by default.
    let checkpoint = run_checkpoint(&cfg);
    assert_eq!(
        checkpoint.exclude_globs,
        vec!["run-only".to_string(), "shared".to_string()]
    );

    // Hooks: id-based replacement. The "shared" hook appears in both cli and
    // workflow layers and resolves to the workflow entry; project and run-only
    // contribute the other two ids.
    let hooks = run_hooks(&cfg);
    assert!(hooks.len() >= 2);
    let shared_hook = hooks
        .iter()
        .find(|hook| hook.name.as_deref() == Some("shared"))
        .expect("shared hook");
    assert_eq!(
        shared_hook
            .script
            .as_ref()
            .map(|s| s.as_source())
            .as_deref(),
        Some("echo run")
    );
    assert!(
        hooks
            .iter()
            .any(|hook| hook.name.as_deref() == Some("run-only"))
    );

    let mcps = run_agent_mcps(&cfg);
    match mcps.get("shared").expect("shared mcp") {
        McpEntryLayer::Stdio { command, .. } => {
            let command = command.as_ref().expect("command");
            let parts: Vec<String> = command.iter().map(|c| c.as_source()).collect();
            assert_eq!(parts, vec!["echo".to_string(), "run".to_string()]);
        }
        other => panic!("unexpected MCP transport: {other:?}"),
    }
    assert!(mcps.contains_key("run_only"));

    // run.sandbox.daytona.labels stays sticky merge-by-key per R71.
    let sandbox = run_sandbox(&cfg);
    let labels = &sandbox.daytona.as_ref().expect("daytona").labels;
    assert_eq!(labels.get("run_only").map(String::as_str), Some("1"));
    assert_eq!(labels.get("shared").map(String::as_str), Some("run"));

    // run.sandbox.env stays sticky merge-by-key per R71.
    let env = &sandbox.env;
    assert_eq!(
        env.get("CLI_ONLY").map(|v| v.as_source()).as_deref(),
        Some("1")
    );
    assert_eq!(
        env.get("RUN_ONLY").map(|v| v.as_source()).as_deref(),
        Some("1")
    );
    assert_eq!(
        env.get("SHARED").map(|v| v.as_source()).as_deref(),
        Some("run")
    );
}

#[test]
fn settings_local_explicit_workflow_path_uses_workflow_project_layers() {
    let mut context = test_context!();
    let (project, _storage_dir) = setup_external_workflow_fixture(&mut context);
    let cwd = tempfile::tempdir().unwrap();
    let workflow = project.path().join("workflow.toml");

    // Remove FABRO_STORAGE_DIR so the CLI uses storage_dir from settings.toml
    let output = context
        .settings()
        .env_remove("FABRO_STORAGE_DIR")
        .current_dir(cwd.path())
        .args(["--local", workflow.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let cfg = parse_settings(&output);
    assert!(auto_approve_enabled(&cfg));
    // v2 R30: run.prepare.steps replaces the whole ordered list across layers.
    // The highest-precedence layer (workflow) wins.
    assert_eq!(
        run_prepare_commands(&cfg),
        vec!["workflow-setup".to_string()]
    );
    assert_eq!(run_sandbox(&cfg).preserve, Some(true));
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
    assert_eq!(
        run_record["settings"]["run"]["execution"]["approval"].as_str(),
        Some("auto")
    );
    assert_eq!(
        run_record["settings"]["server"]["storage"]["root"].as_str(),
        Some(storage_dir.to_str().unwrap())
    );
    assert_eq!(
        run_record["settings"]["run"]["sandbox"]["preserve"].as_bool(),
        Some(true)
    );
    assert_eq!(
        run_record["settings"]["run"]["model"]["name"].as_str(),
        Some("gpt-5.2")
    );
    // v2 R30: run.prepare.steps replaces the whole ordered list across layers.
    assert_eq!(
        run_record["settings"]["run"]["prepare"]["steps"],
        serde_json::json!([{"script": "workflow-setup"}])
    );
}

#[test]
fn settings_fabro_path_matches_ambient_defaults() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);

    let ambient = context
        .settings()
        .arg("--local")
        .current_dir(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let graph = context
        .settings()
        .current_dir(project.path())
        .args(["--local", "standalone.fabro"])
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
    cmd.args(["--local", "missing.toml"]);
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
_version = 1

[cli.output]
verbosity = "verbose"

[run.model]
name = "legacy-model"
"#,
    );

    let assert = context
        .settings()
        .arg("--local")
        .current_dir(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("ignoring legacy config file"))
        .stderr(predicate::str::contains("Rename it to"));

    let cfg = parse_settings(&assert.get_output().stdout);
    assert_eq!(
        resolve_cli(&cfg).output.verbosity,
        fabro_types::settings::cli::OutputVerbosity::Normal
    );
    assert!(
        cfg.run
            .as_ref()
            .and_then(|run| run.model.as_ref())
            .is_none()
    );
}

#[test]
fn settings_user_config_wins_over_legacy_cli_config() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);
    context.write_home(
        ".fabro/cli.toml",
        r#"
_version = 1

[run.model]
name = "legacy-model"

[run.inputs]
shared = "legacy"
"#,
    );

    let assert = context
        .settings()
        .arg("--local")
        .current_dir(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("ignoring legacy config file"));

    let cfg = parse_settings(&assert.get_output().stdout);
    assert_eq!(run_model_name(&cfg).as_deref(), Some("project-model"));
    let vars = run_inputs(&cfg);
    assert_eq!(vars.get("shared").and_then(|v| v.as_str()), Some("project"));
}

#[test]
fn settings_uses_fabro_home_for_home_config_resolution() {
    let context = test_context!();
    let fabro_home = tempfile::tempdir().unwrap();

    std::fs::write(
        fabro_home.path().join("settings.toml"),
        r#"
_version = 1

[cli.output]
verbosity = "verbose"

[run.model]
name = "from-fabro-home"
"#,
    )
    .unwrap();

    let output = context
        .settings()
        .args(["--local", "--json"])
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
    assert_eq!(cfg["cli"]["output"]["verbosity"].as_str(), Some("verbose"));
    assert_eq!(
        cfg["run"]["model"]["name"].as_str(),
        Some("from-fabro-home")
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
fn settings_rejects_local_and_server_combination() {
    let context = test_context!();
    context
        .settings()
        .args(["--local", "--server", "https://cli.example.com"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the argument '--local' cannot be used with '--server <SERVER>'",
        ));
}

#[test]
fn settings_fetches_server_settings_and_merges_with_local_config() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);
    let server = MockServer::start();
    let server_settings = server_settings_fixture();
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
    assert_eq!(run_model_name(&cfg).as_deref(), Some("project-model"));
    assert_eq!(run_model_provider(&cfg).as_deref(), Some("openai"));
    assert_eq!(server_storage_root(&cfg), "/srv/fabro-server");
    assert_eq!(
        resolve_cli(&cfg).output.verbosity,
        fabro_types::settings::cli::OutputVerbosity::Verbose
    );

    // R22: run.inputs replaces wholesale across layers. Project is the
    // highest-precedence layer that sets inputs, so project's vars win
    // and server-side vars are discarded rather than merged.
    let vars = run_inputs(&cfg);
    assert_eq!(vars.get("project_only").and_then(|v| v.as_str()), Some("1"));
    assert_eq!(vars.get("shared").and_then(|v| v.as_str()), Some("project"));
    assert!(
        !vars.contains_key("server_only"),
        "v2 merge matrix replaces run.inputs wholesale; server_only should be dropped"
    );
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
    let cli_server_settings = server_settings_fixture();
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
