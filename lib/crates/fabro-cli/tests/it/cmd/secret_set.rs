use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["secret", "set", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Set a secret value

    Usage: fabro secret set [OPTIONS] <KEY> <VALUE>

    Arguments:
      <KEY>    Name of the secret
      <VALUE>  Value to store

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --type <TYPE>                [default: environment] [possible values: environment, file]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --description <DESCRIPTION>  
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
