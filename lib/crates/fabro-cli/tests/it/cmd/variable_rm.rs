use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.variable();
    cmd.args(["rm", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Remove a variable

    Usage: fabro variable rm [OPTIONS] <NAME>

    Arguments:
      <NAME>  Name of the variable to remove

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
fn variable_rm_json_outputs_removed_name() {
    let context = test_context!();
    let name = format!("RM_JSON_{}", context.test_case_id());
    context
        .variable()
        .args(["set", &name, "remove-me"])
        .assert()
        .success();

    let output = context
        .variable()
        .args(["--json", "rm", &name])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("variable rm should parse");
    assert_eq!(value, serde_json::json!({ "name": name }));
}

#[test]
fn variable_rm_missing_fails() {
    let context = test_context!();
    let name = format!("RM_MISSING_{}", context.test_case_id());
    let mut cmd = context.variable();
    cmd.args(["rm", &name]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × variable not found: RM_MISSING_[TEST_CASE]
    ");
}
