use fabro_test::{fabro_snapshot, test_context};

use super::support::setup_completed_dry_run;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["pr", "view", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    View pull request details

    Usage: fabro pr view [OPTIONS] <RUN_ID>

    Arguments:
      <RUN_ID>  Run ID or prefix

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
fn pr_view_missing_pull_request_json_errors() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["pr", "view", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: No pull_request.json found in run directory. Create one first with: fabro pr create [ULID]
      > No such file or directory (os error 2)
    ");
}
