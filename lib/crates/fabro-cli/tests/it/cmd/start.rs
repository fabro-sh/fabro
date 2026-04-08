use fabro_test::{fabro_snapshot, test_context};

use crate::support::{example_fixture, fabro_json_snapshot, unique_run_id};

use super::support::{output_stdout, resolve_run, wait_for_status, write_gated_workflow};

const SHARED_DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["start", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Start a created workflow run on the server

    Usage: fabro start [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn start_by_run_id_starts_created_run() {
    let context = test_context!();
    let run_id = unique_run_id();

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["start", &run_id])
        .assert()
        .success();
    context
        .command()
        .args(["wait", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();

    let output = context
        .command()
        .args(["wait", "--json", &run_id])
        .output()
        .expect("wait should execute");
    assert!(output.status.success(), "wait should succeed");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("wait JSON");
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "status": value["status"],
        }),
        @r#"
        {
          "status": "succeeded"
        }
        "#
    );
}

#[test]
fn start_by_run_id_starts_created_run_without_run_json_or_status_json() {
    let context = test_context!();
    let run_id = unique_run_id();

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["start", &run_id])
        .assert()
        .success();
    let output = context
        .command()
        .args(["wait", "--json", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .output()
        .expect("wait should execute");
    assert!(output.status.success(), "wait should succeed");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("wait JSON");
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "status": value["status"],
        }),
        @r#"
        {
          "status": "succeeded"
        }
        "#
    );
}

#[test]
fn start_rejects_already_active_or_completed_run() {
    let context = test_context!();
    let gate = write_gated_workflow(&context.temp_dir.join("slow.fabro"), "slow", "Run slowly");

    let mut create_cmd = context.command();
    create_cmd.env("OPENAI_API_KEY", "test");
    create_cmd.args([
        "create",
        "--provider",
        "openai",
        "--sandbox",
        "local",
        "--no-retro",
        "slow.fabro",
    ]);
    let create_output = create_cmd.output().expect("command should execute");
    assert!(
        create_output.status.success(),
        "create failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&create_output.stdout),
        String::from_utf8_lossy(&create_output.stderr)
    );
    let run_id = output_stdout(&create_output).trim().to_string();
    let run = resolve_run(&context, &run_id);

    let mut start_cmd = context.command();
    start_cmd.env("OPENAI_API_KEY", "test");
    start_cmd.args(["start", &run_id]);
    start_cmd.assert().success();

    wait_for_status(&run.run_dir, &["running"]);

    let mut active_cmd = context.command();
    active_cmd.args(["start", &run_id]);
    fabro_snapshot!(context.filters(), active_cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: an engine process is still running for this run — cannot start
    ");

    gate.release();
    wait_for_status(&run.run_dir, &["succeeded"]);

    let mut completed_cmd = context.command();
    completed_cmd.args(["start", &run_id]);
    fabro_snapshot!(context.filters(), completed_cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: cannot start run: status is Succeeded, expected submitted
    ");
}

#[test]
fn start_runs_under_server_ownership_without_launcher_record() {
    let context = test_context!();
    let gate = write_gated_workflow(
        &context.temp_dir.join("owned-by-server.fabro"),
        "owned-by-server",
        "Run under daemon ownership",
    );

    let output = context
        .command()
        .args([
            "create",
            "--provider",
            "openai",
            "--sandbox",
            "local",
            "--no-retro",
            "owned-by-server.fabro",
        ])
        .env("OPENAI_API_KEY", "test")
        .output()
        .expect("create should execute");
    assert!(
        output.status.success(),
        "create failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let run_id = output_stdout(&output).trim().to_string();
    let run = resolve_run(&context, &run_id);

    context
        .command()
        .args(["start", &run_id])
        .env("OPENAI_API_KEY", "test")
        .assert()
        .success();

    wait_for_status(&run.run_dir, &["running"]);
    assert!(
        !context
            .storage_dir
            .join("launchers")
            .join(format!("{run_id}.json"))
            .exists(),
        "server-owned execution should not create a launcher record"
    );

    gate.release();
    wait_for_status(&run.run_dir, &["succeeded"]);
}
