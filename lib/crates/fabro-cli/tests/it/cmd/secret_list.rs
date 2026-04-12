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
fn secret_list_json_returns_metadata_only() {
    let context = test_context!();
    context
        .command()
        .args(["secret", "set", "ANTHROPIC_API_KEY", "test-value"])
        .assert()
        .success();

    let output = context
        .command()
        .args(["--json", "secret", "list"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("secret list should parse");
    let array = value.as_array().expect("secret list should be an array");
    let entry = array
        .iter()
        .find(|entry| entry["name"] == "ANTHROPIC_API_KEY")
        .expect("secret list should include the saved key");
    assert_eq!(entry["type"], "environment");
    assert!(entry.get("updated_at").is_some());
    assert!(entry.get("value").is_none());
}
