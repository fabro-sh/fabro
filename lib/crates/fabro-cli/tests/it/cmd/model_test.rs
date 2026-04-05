use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Test model availability by sending a simple prompt

    Usage: fabro model test [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
      -p, --provider <PROVIDER>        Filter by provider
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -m, --model <MODEL>              Test a specific model
          --deep                       Run a multi-turn tool-use test (catches reasoning round-trip bugs)
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
fn model_test_unknown_model_errors() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test", "--model", "nonexistent-model-xyz"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Testing nonexistent-model-xyz... done
    error: Unknown model: nonexistent-model-xyz
    ");
}
