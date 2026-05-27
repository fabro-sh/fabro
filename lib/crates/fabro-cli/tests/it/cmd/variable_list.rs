use fabro_test::{fabro_json_snapshot, fabro_snapshot, test_context};
use serde_json::Value;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.variable();
    cmd.args(["list", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List variables

    Usage: fabro variable list [OPTIONS]

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
fn variable_list_json_returns_full_variables() {
    let context = test_context!();
    let name = format!("LIST_JSON_{}", context.test_case_id());
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
        .args(["--json", "list"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("variable list should parse");
    let entry = value
        .as_array()
        .expect("variable list should be an array")
        .iter()
        .find(|entry| entry["name"] == name)
        .expect("variable list should include the saved variable");
    fabro_json_snapshot!(context, entry, @r#"
    {
      "name": "LIST_JSON_[TEST_CASE]",
      "value": "staging",
      "description": "Deployment target",
      "created_at": "[TIMESTAMP]",
      "updated_at": "[TIMESTAMP]"
    }
    "#);
}

#[test]
fn variable_list_alias_ls_includes_values() {
    let context = test_context!();
    let name = format!("LIST_ALIAS_{}", context.test_case_id());
    context
        .variable()
        .args(["set", &name, "visible-value"])
        .assert()
        .success();

    context
        .variable()
        .args(["ls"])
        .assert()
        .success()
        .stdout(predicates::str::contains(&name))
        .stdout(predicates::str::contains("visible-value"))
        .stdout(predicates::str::contains("VALUE"));
}
