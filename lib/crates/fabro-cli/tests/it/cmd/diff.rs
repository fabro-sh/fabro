use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["diff", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Show the diff of changes from a workflow run

    Usage: fabro diff [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID or prefix

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --node <NODE>                Show diff for a specific node
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --stat                       Show diffstat instead of full patch (live diffs only)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --shortstat                  Show only files-changed/insertions/deletions summary (live diffs only)
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
