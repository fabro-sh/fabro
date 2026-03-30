use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["rewind", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Rewind a workflow run to an earlier checkpoint

    Usage: fabro rewind [OPTIONS] <RUN_ID> [TARGET]

    Arguments:
      <RUN_ID>  Run ID (or unambiguous prefix)
      [TARGET]  Target checkpoint: node name, node@visit, or @ordinal (omit with --list)

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --list                       Show the checkpoint timeline instead of rewinding
          --no-push                    Skip force-pushing rewound refs to the remote
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
