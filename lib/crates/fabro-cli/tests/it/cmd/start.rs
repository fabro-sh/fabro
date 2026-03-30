use fabro_test::{fabro_snapshot, test_context};

use crate::support::{example_fixture, fabro_json_snapshot, read_json};

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
    context.command().args(["start", old_run_id]).assert().success();
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

    context.command().args(["start", "smoke"]).assert().success();
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
