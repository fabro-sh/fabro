use fabro_test::{fabro_snapshot, test_context};

use super::support::setup_completed_fast_dry_run;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["artifact", "list", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List artifacts for a workflow run

    Usage: fabro artifact list [OPTIONS] <RUN_ID>

    Arguments:
      <RUN_ID>  Run ID (or prefix)

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --node <NODE>                Filter to artifacts from a specific node
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --retry <RETRY>              Filter to artifacts from a specific retry attempt
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn artifact_list_empty_run_reports_no_artifacts() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["artifact", "list", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    No artifacts found for this run.
    ----- stderr -----
    ");
}
