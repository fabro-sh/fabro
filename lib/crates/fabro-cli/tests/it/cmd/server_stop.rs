#[cfg(feature = "server")]
use fabro_test::{fabro_snapshot, test_context};

#[test]
#[cfg(feature = "server")]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["server", "stop", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Stop the HTTP API server

    Usage: fabro server stop [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --timeout <TIMEOUT>          Seconds to wait for graceful shutdown before SIGKILL [default: 10]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
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
#[cfg(feature = "server")]
fn stop_when_not_running() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["server", "stop"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Server is not running
    ");
}
