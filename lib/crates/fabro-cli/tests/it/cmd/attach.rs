use fabro_test::{fabro_snapshot, test_context};

use crate::support::{example_fixture, run_output_filters};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["attach", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Attach to a running or finished workflow run

    Usage: fabro attach [OPTIONS] <RUN>

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
fn attach_requires_run_arg() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.arg("attach");
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <RUN>

    Usage: fabro attach --no-upgrade-check --storage-dir <STORAGE_DIR> <RUN>

    For more information, try '--help'.
    ");
}

#[test]
fn attach_replays_completed_detached_run() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAQ";

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--detach",
            "--run-id",
            run_id,
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["wait", run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let mut cmd = context.command();
    cmd.args(["attach", run_id]);
    cmd.timeout(std::time::Duration::from_secs(10));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Run Tests  [TIME]
        ✓ Report  [TIME]
        ✓ Exit  [TIME]
    ");
}
