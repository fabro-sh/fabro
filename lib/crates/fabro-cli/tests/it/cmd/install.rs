use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.install();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Set up the Fabro environment (LLMs, certs, GitHub)

    Usage: fabro install [OPTIONS]

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --web-url <WEB_URL>          Base URL for the web UI (used for OAuth callback URLs) [default: http://localhost:5173]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}
