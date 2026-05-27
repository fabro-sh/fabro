use fabro_test::{fabro_json_snapshot, fabro_snapshot, test_context};
use serde_json::Value;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.variable();
    cmd.args(["get", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Get a variable value

    Usage: fabro variable get [OPTIONS] <NAME>

    Arguments:
      <NAME>  Name of the variable to get

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
fn variable_get_plain_outputs_raw_value() {
    let context = test_context!();
    let name = format!("GET_RAW_{}", context.test_case_id());
    context
        .variable()
        .args(["set", &name, "staging"])
        .assert()
        .success();

    context
        .variable()
        .args(["get", &name])
        .assert()
        .success()
        .stdout("staging\n");
}

#[test]
fn variable_get_json_returns_full_variable() {
    let context = test_context!();
    let name = format!("GET_JSON_{}", context.test_case_id());
    context
        .variable()
        .args(["set", &name, "json-value", "--description", "Readable"])
        .assert()
        .success();

    let output = context
        .variable()
        .args(["--json", "get", &name])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("variable get should parse");
    fabro_json_snapshot!(context, &value, @r#"
    {
      "name": "GET_JSON_[TEST_CASE]",
      "value": "json-value",
      "description": "Readable",
      "created_at": "[TIMESTAMP]",
      "updated_at": "[TIMESTAMP]"
    }
    "#);
}

#[test]
fn variable_get_missing_fails() {
    let context = test_context!();
    let name = format!("GET_MISSING_{}", context.test_case_id());
    let mut cmd = context.variable();
    cmd.args(["get", &name]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × variable not found: GET_MISSING_[TEST_CASE]
    ");
}
