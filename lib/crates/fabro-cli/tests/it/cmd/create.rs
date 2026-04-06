use httpmock::MockServer;
use insta::assert_snapshot;
use serde_json::json;

use fabro_test::{fabro_snapshot, test_context};

use crate::support::{fabro_json_snapshot, unique_run_id};

use super::support::{fixture, output_stdout, resolve_run, run_count_for_test_case, run_state};

fn run_status_response(run_id: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "id": run_id,
        "status": status,
        "created_at": "2026-04-05T12:00:00Z"
    })
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["create", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Create a workflow run (allocate run dir, persist spec)

    Usage: fabro create [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Path to a .fabro workflow file or .toml task config

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --dry-run                    Execute with simulated LLM backend
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --auto-approve               Auto-approve all human gates
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --goal <GOAL>                Override the workflow goal (exposed as $goal in prompts)
          --goal-file <GOAL_FILE>      Read the workflow goal from a file
          --model <MODEL>              Override default LLM model
          --provider <PROVIDER>        Override default LLM provider
      -v, --verbose                    Enable verbose output
          --sandbox <SANDBOX>          Sandbox for agent tools [possible values: local, docker, daytona]
          --label <KEY=VALUE>          Attach a label to this run (repeatable, format: KEY=VALUE)
          --no-retro                   Skip retro generation after the run
          --preserve-sandbox           Keep the sandbox alive after the run finishes (for debugging)
      -d, --detach                     Run the workflow in the background and print the run ID
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn create_uses_explicit_server_target_and_prints_remote_run_id() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });

    let output = context
        .create_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--dry-run",
            fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    mock.assert();
    assert_eq!(output_stdout(&output).trim(), run_id.as_str());
}

#[test]
fn create_uses_configured_server_target_without_server_flag() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });
    context.write_home(
        ".fabro/settings.toml",
        format!("[server]\ntarget = \"{}/api/v1\"\n", server.base_url()),
    );

    let output = context
        .create_cmd()
        .args(["--dry-run", fixture("simple.fabro").to_str().unwrap()])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    mock.assert();
    assert_eq!(output_stdout(&output).trim(), run_id.as_str());
}

#[test]
fn create_storage_dir_suppresses_configured_server_target() {
    let context = test_context!();
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(500)
            .body("configured-server-should-not-be-used");
    });
    let local_storage = std::path::PathBuf::from(format!(
        "/tmp/fabro-create-{}",
        &context.test_case_id()[..8]
    ));
    context.write_home(
        ".fabro/settings.toml",
        format!("[server]\ntarget = \"{}/api/v1\"\n", server.base_url()),
    );

    let output = context
        .create_cmd()
        .args([
            "--storage-dir",
            local_storage.to_str().unwrap(),
            "--dry-run",
            fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    mock.assert_calls(0);
    assert!(!output_stdout(&output).trim().is_empty());
}

#[test]
fn create_cli_server_target_overrides_configured_server_target() {
    let context = test_context!();
    let config_server = MockServer::start();
    let config_mock = config_server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(500)
            .body("configured-server-should-not-be-used");
    });
    let cli_server = MockServer::start();
    let run_id = unique_run_id();
    let cli_mock = cli_server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            "[server]\ntarget = \"{}/api/v1\"\n",
            config_server.base_url()
        ),
    );

    let output = context
        .create_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", cli_server.base_url()),
            "--dry-run",
            fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    cli_mock.assert();
    config_mock.assert_calls(0);
    assert_eq!(output_stdout(&output).trim(), run_id.as_str());
}

#[test]
fn create_persists_directory_workflow_slug_and_cached_graph() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("sluggy/workflow.fabro");

    context.write_temp(
        "sluggy/workflow.fabro",
        "\
digraph BarBaz {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    let state = run_state(&run_dir);
    let run = state.run.as_ref().expect("run record should exist");
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "workflow_slug": run.workflow_slug,
            "graph_name": run.graph.name,
            "cached_graph_lines": state.graph_source.as_ref().expect("graph should exist").lines().collect::<Vec<_>>(),
        }),
        @r#"
        {
          "workflow_slug": "sluggy",
          "graph_name": "BarBaz",
          "cached_graph_lines": [
            "digraph BarBaz {",
            "  start [shape=Mdiamond, label=\"Start\"]",
            "  exit  [shape=Msquare, label=\"Exit\"]",
            "  start -> exit",
            "}"
          ]
        }
        "#
    );
}

#[test]
fn create_persists_file_stem_slug_for_standalone_file() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("alpha.fabro");

    context.write_temp(
        "alpha.fabro",
        "\
digraph FooWorkflow {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    let state = run_state(&run_dir);
    let run = state.run.as_ref().expect("run record should exist");
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "workflow_slug": run.workflow_slug,
            "graph_name": run.graph.name,
            "cached_graph_lines": state.graph_source.as_ref().expect("graph should exist").lines().collect::<Vec<_>>(),
        }),
        @r#"
        {
          "workflow_slug": "alpha",
          "graph_name": "FooWorkflow",
          "cached_graph_lines": [
            "digraph FooWorkflow {",
            "  start [shape=Mdiamond, label=\"Start\"]",
            "  exit  [shape=Msquare, label=\"Exit\"]",
            "  start -> exit",
            "}"
          ]
        }
        "#
    );
}

#[test]
fn create_persists_requested_overrides_into_store() {
    let context = test_context!();
    let workflow = fixture("simple.fabro");
    let mut cmd = context.command();
    cmd.args([
        "create",
        "--dry-run",
        "--auto-approve",
        "--goal",
        "Ship the release",
        "--model",
        "gpt-5",
        "--provider",
        "openai",
        "--sandbox",
        "local",
        "--label",
        "env=dev",
        "--label",
        "team=cli",
        "--verbose",
        "--no-retro",
        "--preserve-sandbox",
        workflow.to_str().unwrap(),
    ]);
    let output = cmd.output().expect("command should execute");
    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = output_stdout(&output);
    let run_id = stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .expect("create should print a run ID")
        .to_string();
    let run = resolve_run(&context, &run_id);
    let state = run_state(&run.run_dir);
    let run_record = state.run.as_ref().expect("run record should exist");
    let labels = json!({
        "env": run_record.labels.get("env"),
        "team": run_record.labels.get("team"),
    });
    let compact = json!({
        "workflow_slug": run_record.workflow_slug,
        "settings": {
            "goal": run_record.settings.goal,
            "dry_run": run_record.settings.dry_run,
            "auto_approve": run_record.settings.auto_approve,
            "no_retro": run_record.settings.no_retro,
            "verbose": run_record.settings.verbose,
            "llm": {
                "model": run_record.settings.llm.as_ref().and_then(|llm| llm.model.clone()),
                "provider": run_record.settings.llm.as_ref().and_then(|llm| llm.provider.clone()),
            },
            "sandbox": {
                "provider": run_record.settings.sandbox.as_ref().and_then(|sandbox| sandbox.provider.clone()),
                "preserve": run_record.settings.sandbox.as_ref().and_then(|sandbox| sandbox.preserve),
            },
        },
        "labels": labels,
    });

    assert_snapshot!(serde_json::to_string_pretty(&compact).unwrap(), @r###"
    {
      "workflow_slug": "simple",
      "settings": {
        "goal": "Ship the release",
        "dry_run": true,
        "auto_approve": true,
        "no_retro": true,
        "verbose": true,
        "llm": {
          "model": "gpt-5",
          "provider": "openai"
        },
        "sandbox": {
          "provider": "local",
          "preserve": true
        }
      },
      "labels": {
        "env": "dev",
        "team": "cli"
      }
    }
    "###);
}

#[test]
fn create_json_implies_auto_approve() {
    let context = test_context!();
    let workflow = fixture("simple.fabro");
    let output = context
        .command()
        .args(["--json", "create", "--dry-run", workflow.to_str().unwrap()])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("create JSON should parse");
    let run_id = value["run_id"]
        .as_str()
        .expect("create JSON should include run_id");
    let run = resolve_run(&context, run_id);

    assert_eq!(
        run_state(&run.run_dir)
            .run
            .as_ref()
            .expect("run record should exist")
            .settings
            .auto_approve,
        Some(true)
    );
}

#[test]
fn create_invalid_workflow_fails_without_creating_run() {
    let context = test_context!();
    let workflow = fixture("invalid.fabro");
    let initial_run_count = run_count_for_test_case(&context);
    let mut cmd = context.create_cmd();
    cmd.arg(workflow.to_str().unwrap());

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Validation failed
    ");

    let run_count = run_count_for_test_case(&context);
    assert_eq!(
        run_count, initial_run_count,
        "invalid create should not persist a run for this test case"
    );
}
