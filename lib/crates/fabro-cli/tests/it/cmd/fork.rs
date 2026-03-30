use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["fork", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Fork a workflow run from an earlier checkpoint into a new run

    Usage: fabro fork [OPTIONS] <RUN_ID> [TARGET]

    Arguments:
      <RUN_ID>  Run ID (or unambiguous prefix)
      [TARGET]  Target checkpoint: node name, node@visit, or @ordinal (omit to fork from latest)

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --list                       Show the checkpoint timeline instead of forking
          --no-push                    Skip pushing new branches to the remote
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
