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
          --json                   Output as JSON [env: FABRO_JSON=]
          --server <SERVER>        Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug                  Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -I, --input <KEY=VALUE>      Override a workflow input value (repeatable, format: KEY=VALUE)
          --goal <GOAL>            Override the workflow goal (available as {{ goal }} in prompts)
          --no-upgrade-check       Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --goal-file <GOAL_FILE>  Read the workflow goal from a file
          --quiet                  Suppress non-essential output [env: FABRO_QUIET=]
          --model <MODEL>          Override default LLM model
          --provider <PROVIDER>    Override default LLM provider
      -v, --verbose                Enable verbose output
          --sandbox <SANDBOX>      Sandbox for agent tools [possible values: local, docker, daytona]
      -h, --help                   Print help
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
    Graph: [FIXTURES]/invalid.fabro
    error: Pipeline must have exactly one start node (shape=Mdiamond or id start/Start) (start_node)
    error [node: exit]: Exit node 'exit' has 1 outgoing edge(s) but must have none (exit_no_outgoing)
      × Validation failed
    ");
}

#[test]
fn preflight_rejects_unbound_template_inputs() {
    let context = test_context!();
    let workflow = fixture("templated_unbound.fabro");
    let mut cmd = context.command();
    cmd.args(["preflight", workflow.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Workflow: TemplatedUnbound (3 nodes, 2 edges)
    Graph: [FIXTURES]/templated_unbound.fabro
    Goal: Demo

    error: [FIXTURES]/templated_unbound.fabro:2:26: undefined template variable `inputs.app_dir` in graph attribute `goal` (template_undefined_variable)
    error: [FIXTURES]/templated_unbound.fabro:7:44: undefined template variable `inputs.app_dir` in node `work` attribute `prompt` [node: work] (template_undefined_variable)
      × Validation failed
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
