use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

const DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["auth", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Manage CLI authentication state

    Usage: fabro auth [OPTIONS] <COMMAND>

    Commands:
      login   Log in to a Fabro server
      logout  Log out from a Fabro server
      status  Show offline CLI auth status
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

#[test]
fn login_help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["auth", "login", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Log in to a Fabro server

    Usage: fabro auth login [OPTIONS]

    Options:
          --json                   Output as JSON [env: FABRO_JSON=]
          --server <SERVER>        Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug                  Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --dev-token <DEV_TOKEN>  Log in with a dev-token instead of browser OAuth
          --no-browser             Print the browser URL instead of opening it automatically
          --no-upgrade-check       Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                  Suppress non-essential output [env: FABRO_QUIET=]
          --timeout <TIMEOUT>      Timeout in seconds waiting for the browser flow to complete [default: 300]
          --verbose                Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                   Print help
    ----- stderr -----
    ");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "Integration test inspects the CLI auth store fixture synchronously."
)]
fn login_with_dev_token_writes_auth_store_entry() {
    let context = test_context!();
    let settings_file = context.home_dir.join(".fabro").join("settings.toml");
    std::fs::write(&settings_file, "_version = 1\n").unwrap();

    let mut cmd = context.command();
    cmd.args([
        "auth",
        "login",
        "--server",
        "http://127.0.0.1:32276",
        "--dev-token",
        DEV_TOKEN,
    ]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Logged in to http://127.0.0.1:32276 with dev-token
    ");

    let auth_file = context.home_dir.join(".fabro").join("auth.json");
    let auth: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(auth_file).unwrap()).unwrap();
    let entry = &auth["servers"]["http://127.0.0.1:32276"];
    assert_eq!(entry["kind"], "dev-token");
    assert_eq!(entry["token"], DEV_TOKEN);

    let settings = std::fs::read_to_string(settings_file).unwrap();
    assert!(settings.contains("[cli.target]"));
    assert!(settings.contains("http://127.0.0.1:32276"));
}

#[test]
fn login_with_invalid_dev_token_fails_before_writing_auth_store() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args([
        "auth",
        "login",
        "--server",
        "http://127.0.0.1:32276",
        "--dev-token",
        "not-a-dev-token",
    ]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × invalid dev-token format
    ");

    assert!(!context.home_dir.join(".fabro").join("auth.json").exists());
}

#[test]
fn status_help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["auth", "status", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Show offline CLI auth status

    Usage: fabro auth status [OPTIONS]

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn status_json_ignores_env_dev_token() {
    let context = test_context!();
    let output = context
        .command()
        .env("FABRO_DEV_TOKEN", DEV_TOKEN)
        .args(["auth", "status", "--json"])
        .output()
        .expect("auth status should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("status JSON should parse");
    assert!(value["servers"].as_array().unwrap().is_empty());
    assert!(value.get("env_dev_token").is_none());
}
