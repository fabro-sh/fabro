use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.system();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    System maintenance commands

    Usage: fabro system [OPTIONS] <COMMAND>

    Commands:
      info    Show server runtime information
      prune   Delete old workflow runs
      df      Show disk usage
      events  Stream run events from the server
      help    Print this message or the help of the given subcommand(s)

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}
