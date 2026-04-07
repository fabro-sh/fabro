use fabro_store::EventEnvelope;
use fabro_test::{fabro_snapshot, test_context};
use fabro_types::{EventBody, RunEvent};

use super::support::{run_events, run_state, server_target};
use crate::support::{fabro_json_snapshot, unique_run_id};

const SHARED_DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn stored_worker_events(run_dir: &std::path::Path) -> Vec<RunEvent> {
    run_events(run_dir).iter().map(run_event).collect()
}

fn run_event(event: &EventEnvelope) -> RunEvent {
    RunEvent::try_from(&event.payload).expect("stored event should parse")
}

fn assert_worker_succeeded(run_dir: &std::path::Path, stdout: &[u8]) {
    assert!(
        stdout.is_empty(),
        "worker should not emit event transport on stdout"
    );
    let events = stored_worker_events(run_dir);
    assert!(events.iter().any(|event| matches!(
        &event.body,
        EventBody::RunCompleted(props) if props.status == "success"
    )));
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["__run-worker", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Internal: execute a single workflow run locally

    Usage: fabro __run-worker [OPTIONS] --server <SERVER> --run-dir <RUN_DIR> --run-id <RUN_ID> --mode <MODE>

    Options:
          --json               Output as JSON [env: FABRO_JSON=]
          --server <SERVER>    Fabro server target: http(s) URL or absolute Unix socket path
          --debug              Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --run-dir <RUN_DIR>  Run scratch directory
          --no-upgrade-check   Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --run-id <RUN_ID>    Run ID
          --mode <MODE>        Worker mode [possible values: start, resume]
          --quiet              Suppress non-essential output [env: FABRO_QUIET=]
          --verbose            Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help               Print help
    ----- stderr -----
    ");
}

#[test]
fn runner_uses_cached_graph_after_source_deleted() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_temp(
        "workflow.fabro",
        "\
digraph CachedGraph {
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
    let server = server_target(&context.storage_dir);
    std::fs::remove_file(&workflow_path).unwrap();

    let output = context
        .command()
        .args([
            "__run-worker",
            "--server",
            server.as_str(),
            "--run-dir",
            run_dir.to_str().unwrap(),
            "--run-id",
            run_id.as_str(),
            "--mode",
            "start",
        ])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_worker_succeeded(&run_dir, &output);
}

#[test]
fn runner_uses_snapshotted_app_id_for_github_credentials() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_home(
        ".fabro/settings.toml",
        "\
version = 1

[git]
app_id = \"snapshotted-app-id\"
",
    );
    context.write_temp(
        "workflow.fabro",
        "\
digraph GitHubApp {
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
            "app_id": run.settings.git.clone().and_then(|git| git.app_id),
        }),
        @r#"
        {
          "app_id": "snapshotted-app-id"
        }
        "#
    );

    context.write_home(".fabro/settings.toml", "version = 1\n");

    let server = server_target(&context.storage_dir);
    let mut cmd = context.command();
    cmd.env("GITHUB_APP_PRIVATE_KEY", "%%%not-base64%%%");
    cmd.args([
        "__run-worker",
        "--server",
        server.as_str(),
        "--run-dir",
        run_dir.to_str().unwrap(),
        "--run-id",
        run_id.as_str(),
        "--mode",
        "start",
    ]);
    cmd.timeout(SHARED_DAEMON_TIMEOUT);
    let assert = cmd.assert().success();
    assert_worker_succeeded(&run_dir, &assert.get_output().stdout);
}

#[test]
fn runner_runs_without_run_json_when_run_id_is_explicit() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_temp(
        "workflow.fabro",
        "\
digraph DetachedStoreOnly {
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
    let server = server_target(&context.storage_dir);
    let output = context
        .command()
        .args([
            "__run-worker",
            "--server",
            server.as_str(),
            "--run-dir",
            run_dir.to_str().unwrap(),
            "--run-id",
            run_id.as_str(),
            "--mode",
            "start",
        ])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_worker_succeeded(&run_dir, &output);
}

#[test]
fn runner_resume_rejects_completed_run_without_mutating_it() {
    let context = test_context!();
    context.write_temp(
        "workflow.fabro",
        "\
digraph Test {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    let run = context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--detach",
            context.temp_dir.join("workflow.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();
    let run_id = String::from_utf8(run.get_output().stdout.clone())
        .unwrap()
        .trim()
        .to_string();
    let run_dir = context.find_run_dir(&run_id);
    let server = server_target(&context.storage_dir);

    context
        .command()
        .args(["wait", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();

    let inspect_before = context
        .command()
        .args(["inspect", &run_id])
        .assert()
        .success();
    let before: serde_json::Value =
        serde_json::from_slice(&inspect_before.get_output().stdout).unwrap();
    let before_summary = serde_json::json!({
        "run_dir": before[0]["run_dir"],
        "start_time": before[0]["start_record"]["start_time"],
        "conclusion_timestamp": before[0]["conclusion"]["timestamp"],
        "conclusion_status": before[0]["conclusion"]["status"],
    });
    fabro_json_snapshot!(context, &before_summary, @r#"
    {
      "run_dir": null,
      "start_time": "[TIMESTAMP]",
      "conclusion_timestamp": "[TIMESTAMP]",
      "conclusion_status": "success"
    }
    "#);

    let mut cmd = context.command();
    cmd.args([
        "__run-worker",
        "--server",
        &server,
        "--run-dir",
        run_dir.to_str().unwrap(),
        "--run-id",
        &run_id,
        "--mode",
        "resume",
    ]);
    cmd.timeout(SHARED_DAEMON_TIMEOUT);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Precondition failed: run already finished successfully — nothing to resume
    ");

    let inspect_after = context
        .command()
        .args(["inspect", &run_id])
        .assert()
        .success();
    let after: serde_json::Value =
        serde_json::from_slice(&inspect_after.get_output().stdout).unwrap();
    let after_summary = serde_json::json!({
        "run_dir": after[0]["run_dir"],
        "start_time": after[0]["start_record"]["start_time"],
        "conclusion_timestamp": after[0]["conclusion"]["timestamp"],
        "conclusion_status": after[0]["conclusion"]["status"],
    });

    assert_eq!(after_summary, before_summary);
}
