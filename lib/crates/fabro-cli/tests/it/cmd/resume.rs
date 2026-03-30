use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["resume", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Resume an interrupted workflow run

    Usage: fabro resume [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID or unambiguous prefix

    Options:
      -d, --detach                     Run in the background and print the run ID
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
fn resume_requires_run_arg() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["resume"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <RUN>

    Usage: fabro resume --no-upgrade-check --storage-dir <STORAGE_DIR> <RUN>

    For more information, try '--help'.
    ");
}
