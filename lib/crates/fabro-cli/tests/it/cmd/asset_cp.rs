use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["asset", "cp", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Copy assets from a workflow run

    Usage: fabro asset cp [OPTIONS] <SOURCE> [DEST]

    Arguments:
      <SOURCE>  Source: RUN_ID (all assets) or RUN_ID:path (specific asset)
      [DEST]    Destination directory (defaults to current directory) [default: .]

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --node <NODE>                Filter to assets from a specific node
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --retry <RETRY>              Filter to assets from a specific retry attempt
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --tree                       Preserve {node_slug}/retry_{N}/ directory structure
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
