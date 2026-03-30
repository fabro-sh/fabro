use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["__detached", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Internal: run the engine process (reads run.json from run dir)

    Usage: fabro __detached [OPTIONS] --run-dir <RUN_DIR> --launcher-path <LAUNCHER_PATH>

    Options:
          --debug                          Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --run-dir <RUN_DIR>              Run directory
          --launcher-path <LAUNCHER_PATH>  Launcher metadata path
          --no-upgrade-check               Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                          Suppress non-essential output [env: FABRO_QUIET=]
          --resume                         Resume from checkpoint instead of fresh start
          --verbose                        Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>      Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                           Print help
    ----- stderr -----
    ");
}
