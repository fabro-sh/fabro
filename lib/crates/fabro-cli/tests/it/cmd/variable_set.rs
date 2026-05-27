use fabro_test::{fabro_json_snapshot, fabro_snapshot, test_context};
use serde_json::Value;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.variable();
    cmd.args(["set", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Set a variable value

    Usage: fabro variable set [OPTIONS] <NAME> [VALUE]

    Arguments:
      <NAME>   Name of the variable
      [VALUE]  Value to store

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --value-stdin                Read the variable value from stdin
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --description <DESCRIPTION>  Optional human-readable description
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn variable_set_json_returns_full_variable() {
    let context = test_context!();
    let name = format!("SET_JSON_{}", context.test_case_id());
    let output = context
        .variable()
        .args([
            "--json",
            "set",
            &name,
            "json-value",
            "--description",
            "Deployment target",
        ])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("variable set should parse");
    fabro_json_snapshot!(context, &value, @r#"
    {
      "name": "SET_JSON_[TEST_CASE]",
      "value": "json-value",
      "description": "Deployment target",
      "created_at": "[TIMESTAMP]",
      "updated_at": "[TIMESTAMP]"
    }
    "#);
}

#[test]
fn variable_set_update_preserves_description_when_omitted() {
    let context = test_context!();
    let name = format!("SET_PRESERVE_{}", context.test_case_id());
    context
        .variable()
        .args([
            "set",
            &name,
            "staging",
            "--description",
            "Deployment target",
        ])
        .assert()
        .success();

    let output = context
        .variable()
        .args(["--json", "set", &name, "production"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("variable set should parse");
    assert_eq!(value["value"], "production");
    assert_eq!(value["description"], "Deployment target");
}

#[test]
fn variable_set_accepts_explicit_empty_value() {
    let context = test_context!();
    let name = format!("SET_EMPTY_{}", context.test_case_id());
    context
        .variable()
        .args(["set", &name, ""])
        .assert()
        .success();

    context
        .variable()
        .args(["get", &name])
        .assert()
        .success()
        .stdout("\n");
}

#[test]
fn variable_set_accepts_empty_stdin_value() {
    let context = test_context!();
    let name = format!("SET_STDIN_EMPTY_{}", context.test_case_id());
    context
        .variable()
        .args(["set", &name, "--value-stdin"])
        .write_stdin("\n")
        .assert()
        .success();

    context
        .variable()
        .args(["get", &name])
        .assert()
        .success()
        .stdout("\n");
}

#[test]
fn variable_set_invalid_name_fails() {
    let context = test_context!();
    let mut cmd = context.variable();
    cmd.args(["set", "1BAD", "value"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × invalid variable name
    ");
}

#[test]
fn variable_set_requires_value_or_stdin() {
    let context = test_context!();
    let name = format!("SET_MISSING_VALUE_{}", context.test_case_id());
    let mut cmd = context.variable();
    cmd.args(["set", &name]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × variable value required: pass <VALUE> or use --value-stdin
    ");
}
