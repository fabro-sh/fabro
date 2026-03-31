use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

use super::support::fixture;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["preflight", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Validate run configuration without executing

    Usage: fabro preflight [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Path to a .fabro workflow file or .toml task config

    Options:
          --goal <GOAL>                Override the workflow goal (exposed as $goal in prompts)
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --goal-file <GOAL_FILE>      Read the workflow goal from a file
          --model <MODEL>              Override default LLM model
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --provider <PROVIDER>        Override default LLM provider
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
      -v, --verbose                    Enable verbose output
          --sandbox <SANDBOX>          Sandbox for agent tools [possible values: local, docker, daytona]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn preflight_invalid_workflow_fails_with_validation_output() {
    let context = test_context!();
    let workflow = fixture("invalid.fabro");
    let mut cmd = context.command();
    cmd.args(["preflight", workflow.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Workflow: Invalid (2 nodes, 1 edges)
    Graph: ../../../test/invalid.fabro
    error: Pipeline must have exactly one start node (shape=Mdiamond or id start/Start) (start_node)
    error [node: exit]: Exit node 'exit' has 1 outgoing edge(s) but must have none (exit_no_outgoing)
    error: Validation failed
    ");
}

#[test]
fn preflight_invalid_workflow_json_emits_diagnostics() {
    let context = test_context!();
    let workflow = fixture("invalid.fabro");
    let output = context
        .command()
        .args(["--json", "preflight", workflow.to_str().unwrap()])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("preflight --json should parse");
    assert_eq!(value["workflow"]["name"], "Invalid");
    assert!(
        value["workflow"]["diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| !diagnostics.is_empty())
    );
    assert_eq!(value["checks"]["title"], "Run Preflight");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Validation failed"));
}
