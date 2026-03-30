use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.secret();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Manage secrets in ~/.fabro/.env

    Usage: fabro secret [OPTIONS] <COMMAND>

    Commands:
      get   Get a secret value
      list  List secret names
      rm    Remove a secret
      set   Set a secret value
      help  Print this message or the help of the given subcommand(s)

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
