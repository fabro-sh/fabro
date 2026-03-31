use fabro_test::{fabro_snapshot, test_context};

use crate::support::{example_fixture, fabro_json_snapshot, read_json};

use super::support::{output_stdout, resolve_run, wait_for_status, write_sleep_workflow};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["start", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Start a created workflow run (spawn engine process)

    Usage: fabro start [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn start_by_run_id_starts_created_run() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAC";

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    context.command().args(["start", run_id]).assert().success();
    context
        .command()
        .args(["wait", run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    let status = read_json(run_dir.join("status.json"));
    let conclusion = read_json(run_dir.join("conclusion.json"));
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "status": status["status"],
            "reason": status["reason"],
            "conclusion_status": conclusion["status"],
        }),
        @r#"
        {
          "status": "succeeded",
          "reason": "completed",
          "conclusion_status": "success"
        }
        "#
    );
}

#[test]
fn start_by_workflow_name_prefers_newly_created_submitted_run() {
    let context = test_context!();
    let workflow_path = context.temp_dir.join("smoke/workflow.fabro");
    let old_run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAD";
    let new_run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAE";

    context.write_temp(
        "smoke/workflow.fabro",
        "\
digraph Smoke {
  start [shape=Mdiamond, label=\"Start\"]
  work  [label=\"Work\", prompt=\"Do the work.\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> work -> exit
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
            old_run_id,
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    context
        .command()
        .args(["start", old_run_id])
        .assert()
        .success();
    context
        .command()
        .args(["wait", old_run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            new_run_id,
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["start", "smoke"])
        .assert()
        .success();
    context
        .command()
        .args(["attach", new_run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let new_run_dir = context.find_run_dir(new_run_id);
    let status = read_json(new_run_dir.join("status.json"));
    fabro_json_snapshot!(context, &status, @r#"
    {
      "status": "succeeded",
      "reason": "completed",
      "updated_at": "[TIMESTAMP]"
    }
    "#);
}

#[test]
fn start_rejects_already_active_or_completed_run() {
    let context = test_context!();
    write_sleep_workflow(
        &context.temp_dir.join("slow.fabro"),
        "slow",
        "Run slowly",
        3,
    );

    let mut create_cmd = context.command();
    create_cmd.current_dir(&context.temp_dir);
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
    start_cmd.current_dir(&context.temp_dir);
    start_cmd.env("OPENAI_API_KEY", "test");
    start_cmd.args(["start", &run_id]);
    start_cmd.assert().success();

    wait_for_status(&run.run_dir, &["starting", "running"]);

    let mut active_cmd = context.command();
    active_cmd.current_dir(&context.temp_dir);
    active_cmd.args(["start", &run_id]);
    fabro_snapshot!(context.filters(), active_cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: an engine process is still running for this run — cannot start
    ");

    wait_for_status(&run.run_dir, &["succeeded"]);

    let mut completed_cmd = context.command();
    completed_cmd.current_dir(&context.temp_dir);
    completed_cmd.args(["start", &run_id]);
    fabro_snapshot!(context.filters(), completed_cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: cannot start run: status is Succeeded, expected submitted
    ");
}
