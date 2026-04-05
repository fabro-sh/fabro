use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.pr();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Pull request operations

    Usage: fabro pr [OPTIONS] <COMMAND>

    Commands:
      create  Create a pull request from a completed run
      list    List pull requests from workflow runs
      view    View pull request details
      merge   Merge a pull request
      close   Close a pull request
      help    Print this message or the help of the given subcommand(s)

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Fabro API server URL (overrides server.base_url from user.toml when supported) [env: FABRO_SERVER_URL=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
