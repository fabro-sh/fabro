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
          --json                       Output as JSON [env: FABRO_JSON=]
          --web-url <WEB_URL>          Base URL for the web UI (used for OAuth callback URLs) [default: http://localhost:3000]
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

#[test]
fn install_rejects_json() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "install"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json is not supported for this command"));
}
