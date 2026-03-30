use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.preview();
    cmd.arg("--help");
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
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --signed                     Generate a signed URL (embeds auth token, no headers needed)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --ttl <TTL>                  Signed URL expiry in seconds (default 3600, requires --signed) [default: 3600]
          --open                       Open URL in browser (implies --signed)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
