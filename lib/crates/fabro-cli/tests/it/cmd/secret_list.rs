use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["secret", "list", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List secret names

    Usage: fabro secret list [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --show-values                Show values alongside keys
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
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
fn secret_list_json_show_values_includes_values() {
    let context = test_context!();
    context
        .command()
        .args(["secret", "set", "ANTHROPIC_API_KEY", "test-value"])
        .assert()
        .success();

    let output = context
        .command()
        .args(["--json", "secret", "list", "--show-values"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("secret list should parse");
    assert_eq!(
        value,
        Value::Array(vec![serde_json::json!({
            "key": "ANTHROPIC_API_KEY",
            "value": "test-value",
        })])
    );
}
