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

fn parse_settings_json(stdout: &[u8]) -> serde_json::Value {
    serde_json::from_slice(stdout).expect("stdout should be valid JSON settings")
}

fn run_goal_inline(settings: &serde_json::Value) -> Option<&str> {
    let goal = settings.get("run")?.get("goal")?;
    (goal.get("type")?.as_str() == Some("inline"))
        .then(|| goal.get("value")?.as_str())
        .flatten()
}

fn run_model_name(settings: &serde_json::Value) -> Option<&str> {
    settings.get("run")?.get("model")?.get("name")?.as_str()
}

fn run_model_provider(settings: &serde_json::Value) -> Option<&str> {
    settings.get("run")?.get("model")?.get("provider")?.as_str()
}

fn run_inputs(settings: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
    settings
        .get("run")
        .and_then(|run| run.get("inputs"))
        .and_then(serde_json::Value::as_object)
        .expect("run.inputs")
}

fn run_sandbox(settings: &serde_json::Value) -> &serde_json::Value {
    settings
        .get("run")
        .and_then(|run| run.get("sandbox"))
        .expect("run.sandbox")
}

fn run_checkpoint(settings: &serde_json::Value) -> &serde_json::Value {
    settings
        .get("run")
        .and_then(|run| run.get("checkpoint"))
        .expect("run.checkpoint")
}

fn run_hooks(settings: &serde_json::Value) -> &[serde_json::Value] {
    settings
        .get("run")
        .and_then(|run| run.get("hooks"))
        .and_then(serde_json::Value::as_array)
        .expect("run.hooks")
}

fn run_agent_mcps(settings: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
    settings
        .get("run")
        .and_then(|run| run.get("agent"))
        .and_then(|agent| agent.get("mcps"))
        .and_then(serde_json::Value::as_object)
        .expect("run.agent.mcps")
}

fn auto_approve_enabled(settings: &serde_json::Value) -> bool {
    settings
        .get("run")
        .and_then(|run| run.get("execution"))
        .and_then(|execution| execution.get("approval"))
        .and_then(serde_json::Value::as_str)
        == Some("auto")
}

fn run_prepare_commands(settings: &serde_json::Value) -> Vec<String> {
    settings
        .get("run")
        .and_then(|run| run.get("prepare"))
        .and_then(|prepare| prepare.get("commands"))
        .and_then(serde_json::Value::as_array)
        .expect("run.prepare.commands")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("command should be a string")
                .to_string()
        })
        .collect()
}

fn server_storage_root(settings: &serde_json::Value) -> &str {
    settings
        .get("server")
        .and_then(|server| server.get("storage"))
        .and_then(|storage| storage.get("root"))
        .and_then(serde_json::Value::as_str)
        .expect("server.storage.root")
}

fn server_settings_layer_fixture() -> SettingsLayer {
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

fn resolved_server_settings_fixture() -> serde_json::Value {
    let settings = fabro_config::resolve(&server_settings_layer_fixture())
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
"#,
    );

    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".fabro")).unwrap();
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
    .unwrap();

    let workflow_dir = project.path().join(".fabro").join("workflows").join("demo");
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
    std::fs::create_dir_all(project.path().join(".fabro")).unwrap();
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
    assert!(cfg.get("_version").is_none());
    assert_eq!(cfg["project"]["directory"].as_str(), Some("."));
    assert_eq!(cfg["workflow"]["graph"].as_str(), Some("workflow.fabro"));
    assert_eq!(cfg["run"]["execution"]["approval"].as_str(), Some("prompt"));
    assert_eq!(cfg["run"]["sandbox"]["provider"].as_str(), Some("daytona"));
    assert_eq!(run_model_name(&cfg), Some("project-model"));
    assert_eq!(run_model_provider(&cfg), Some("openai"));
    assert_eq!(run_goal_inline(&cfg), None);

    // v2 R22: run.inputs replaces the inherited map wholesale rather than
    // merging by key, so the project layer wipes out the CLI layer's inputs.
    let vars = run_inputs(&cfg);
    assert_eq!(
        vars.get("project_only").and_then(serde_json::Value::as_str),
        Some("1")
    );
    assert_eq!(
        vars.get("shared").and_then(serde_json::Value::as_str),
        Some("project")
    );
    assert!(
        !vars.contains_key("cli_only"),
        "run.inputs should replace across layers, not merge by key"
    );

    // v2 R71: provider-native maps such as run.sandbox.daytona.labels remain
    // sticky merge-by-key, so CLI labels persist under the project layer.
    let sandbox = run_sandbox(&cfg);
    let labels = &sandbox["daytona"]["labels"];
    assert_eq!(labels["cli_only"].as_str(), Some("1"));
    assert_eq!(labels["shared"].as_str(), Some("cli"));
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

    let cfg = parse_settings(&output);
    assert_eq!(run_goal_inline(&cfg), Some("demo goal"));
    assert_eq!(run_model_name(&cfg), Some("run-model"));
    assert_eq!(run_model_provider(&cfg), Some("anthropic"));

    // v2 R22: run.inputs replaces wholesale, so the workflow layer wins
    // over project and cli.
    let vars = run_inputs(&cfg);
    assert_eq!(vars.get("run_only").and_then(|v| v.as_str()), Some("1"));
    assert_eq!(vars.get("shared").and_then(|v| v.as_str()), Some("run"));

    // checkpoint.exclude_globs is a security/policy list: replace by default.
    let checkpoint = run_checkpoint(&cfg);
    assert_eq!(
        checkpoint["exclude_globs"],
        serde_json::json!(["run-only", "shared"])
    );

    // Hooks: id-based replacement. The "shared" hook appears in both cli and
    // workflow layers and resolves to the workflow entry; project and run-only
    // contribute the other two ids.
    let hooks = run_hooks(&cfg);
    assert!(hooks.len() >= 2);
    let shared_hook = hooks
        .iter()
        .find(|hook| hook["name"].as_str() == Some("shared"))
        .expect("shared hook");
    assert_eq!(shared_hook["command"].as_str(), Some("echo run"));
    assert!(
        hooks
            .iter()
            .any(|hook| hook["name"].as_str() == Some("run-only"))
    );

    let mcps = run_agent_mcps(&cfg);
    let shared = mcps.get("shared").expect("shared mcp");
    assert_eq!(shared["transport"]["type"].as_str(), Some("stdio"));
    assert_eq!(
        shared["transport"]["command"],
        serde_json::json!(["echo", "run"])
    );
    assert!(mcps.contains_key("run_only"));

    // run.sandbox.daytona.labels stays sticky merge-by-key per R71.
    let sandbox = run_sandbox(&cfg);
    let labels = &sandbox["daytona"]["labels"];
    assert_eq!(labels["run_only"].as_str(), Some("1"));
    assert_eq!(labels["shared"].as_str(), Some("run"));

    // run.sandbox.env stays sticky merge-by-key per R71.
    let env = &sandbox["env"];
    assert_eq!(env["CLI_ONLY"].as_str(), Some("1"));
    assert_eq!(env["RUN_ONLY"].as_str(), Some("1"));
    assert_eq!(env["SHARED"].as_str(), Some("run"));
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
    assert_eq!(run_prepare_commands(&cfg), vec![
        "workflow-setup".to_string()
    ]);
    assert_eq!(run_sandbox(&cfg)["preserve"].as_bool(), Some(true));
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
        stderr.contains("workflow not found:"),
        "stderr should report missing workflow path, got:\n{stderr}"
    );
    assert!(
        stderr.contains("missing.toml"),
        "stderr should include missing workflow filename, got:\n{stderr}"
    );
}

#[test]
fn settings_legacy_cli_config_is_silently_ignored() {
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
        .success();

    assert!(
        assert.get_output().stderr.is_empty(),
        "settings should not warn about legacy config files: {}",
        String::from_utf8_lossy(&assert.get_output().stderr)
    );

    let cfg = parse_settings(&assert.get_output().stdout);
    assert_eq!(cfg["cli"]["output"]["verbosity"].as_str(), Some("normal"));
    assert!(
        cfg["run"]["model"].get("name").is_none(),
        "resolved dense settings should omit an unset run.model.name"
    );
}

#[test]
fn settings_legacy_user_config_is_silently_ignored() {
    let context = test_context!();
    let project = tempfile::tempdir().unwrap();

    context.write_home(
        ".fabro/user.toml",
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
        .success();

    assert!(
        assert.get_output().stderr.is_empty(),
        "settings should not warn about legacy config files: {}",
        String::from_utf8_lossy(&assert.get_output().stderr)
    );

    let cfg = parse_settings(&assert.get_output().stdout);
    assert_eq!(cfg["cli"]["output"]["verbosity"].as_str(), Some("normal"));
    assert!(
        cfg["run"]["model"].get("name").is_none(),
        "resolved dense settings should omit an unset run.model.name"
    );
}

#[test]
fn settings_legacy_server_config_is_silently_ignored() {
    let context = test_context!();
    let project = tempfile::tempdir().unwrap();

    context.write_home(
        ".fabro/server.toml",
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
        .success();

    assert!(
        assert.get_output().stderr.is_empty(),
        "settings should not warn about legacy config files: {}",
        String::from_utf8_lossy(&assert.get_output().stderr)
    );

    let cfg = parse_settings(&assert.get_output().stdout);
    assert_eq!(cfg["cli"]["output"]["verbosity"].as_str(), Some("normal"));
    assert!(
        cfg["run"]["model"].get("name").is_none(),
        "resolved dense settings should omit an unset run.model.name"
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
        .success();

    assert!(
        assert.get_output().stderr.is_empty(),
        "settings should not warn about legacy config files: {}",
        String::from_utf8_lossy(&assert.get_output().stderr)
    );

    let cfg = parse_settings(&assert.get_output().stdout);
    assert_eq!(run_model_name(&cfg), Some("project-model"));
    let vars = run_inputs(&cfg);
    assert_eq!(
        vars.get("shared").and_then(serde_json::Value::as_str),
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

    let cfg = parse_settings_json(&output.stdout);
    assert!(cfg.get("_version").is_none());
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
fn settings_rejects_workflow_without_local() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);

    context
        .settings()
        .current_dir(project.path())
        .arg("demo")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "WORKFLOW requires --local; use `fabro settings --local WORKFLOW`",
        ));
}

#[test]
fn settings_fetches_server_resolved_settings() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);
    let server = MockServer::start();
    let server_settings = resolved_server_settings_fixture();
    let mock = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/settings")
            .query_param("view", "resolved");
        then.status(200)
            .header("Content-Type", "application/json")
            .header("X-Fabro-Settings-View", "resolved")
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
    assert!(cfg.get("_version").is_none());
    assert_eq!(cfg["project"]["directory"].as_str(), Some("."));
    assert_eq!(cfg["workflow"]["graph"].as_str(), Some("workflow.fabro"));
    assert_eq!(cfg["run"]["execution"]["approval"].as_str(), Some("prompt"));
    assert_eq!(run_model_name(&cfg), Some("server-model"));
    assert_eq!(run_model_provider(&cfg), Some("openai"));
    assert_eq!(server_storage_root(&cfg), "/srv/fabro-server");
    assert_eq!(cfg["cli"]["output"]["verbosity"].as_str(), Some("normal"));

    // Server-backed mode now returns the selected server's own dense resolved
    // settings; local project/user overlays are not merged into the output.
    let vars = run_inputs(&cfg);
    assert_eq!(
        vars.get("server_only").and_then(serde_json::Value::as_str),
        Some("1")
    );
    assert_eq!(
        vars.get("shared").and_then(serde_json::Value::as_str),
        Some("server")
    );
    assert!(
        !vars.contains_key("project_only"),
        "server-backed settings output must not include local workflow/project overlays"
    );
}

#[test]
fn settings_cli_server_target_overrides_configured_server_target() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);
    let configured_server = MockServer::start();
    let configured_mock = configured_server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/settings")
            .query_param("view", "resolved");
        then.status(500)
            .body("configured-server-should-not-be-used");
    });
    let cli_server = MockServer::start();
    let cli_server_settings = resolved_server_settings_fixture();
    let cli_mock = cli_server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/settings")
            .query_param("view", "resolved");
        then.status(200)
            .header("Content-Type", "application/json")
            .header("X-Fabro-Settings-View", "resolved")
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
fn settings_errors_when_server_lacks_resolved_view_marker() {
    let context = test_context!();
    let project = setup_settings_fixture(&context);
    let server = MockServer::start();
    let server_settings = resolved_server_settings_fixture();
    let mock = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/settings")
            .query_param("view", "resolved");
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
"#,
            server.base_url()
        ),
    );

    context
        .settings()
        .current_dir(project.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "server does not support resolved settings view; upgrade the server or use --local",
        ));

    mock.assert();
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
