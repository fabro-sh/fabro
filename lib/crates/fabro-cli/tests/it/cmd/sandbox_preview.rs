use fabro_test::{fabro_snapshot, test_context};

use super::support::setup_local_sandbox_run;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["sandbox", "preview", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Get a preview URL for a port on a run's sandbox

    Usage: fabro sandbox preview [OPTIONS] <RUN> <PORT>

    Arguments:
      <RUN>   Run ID or prefix
      <PORT>  Port number

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --signed                     Generate a signed URL (embeds auth token, no headers needed)
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --ttl <TTL>                  Signed URL expiry in seconds (default 3600, requires --signed) [default: 3600]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --open                       Open URL in browser (implies --signed)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn sandbox_preview_rejects_non_daytona_run() {
    let context = test_context!();
    let setup = setup_local_sandbox_run(&context);
    let mut cmd = context.preview();
    cmd.args([&setup.run.run_id, "3000"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Preview URLs is only supported for Daytona sandboxes (this run uses 'local')
    ");
}
