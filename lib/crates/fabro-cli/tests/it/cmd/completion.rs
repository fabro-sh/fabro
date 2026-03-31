use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["completion", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Generate shell completions

    Usage: fabro completion [OPTIONS] <SHELL>

    Arguments:
      <SHELL>  Shell to generate completions for [possible values: bash, elvish, fish, powershell, zsh]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn generates_zsh_completions() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["completion", "zsh"]);
    cmd.assert().success();
}

#[test]
fn generates_fish_completions() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["completion", "fish"]);
    cmd.assert().success();
}
