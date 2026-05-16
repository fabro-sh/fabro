use fabro_test::{fabro_snapshot, test_context};

use crate::support::LightweightCli;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../../test/{name}"))
        .canonicalize()
        .expect("fixture path should exist")
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Validate a workflow

    Usage: fabro validate [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Path to the .fabro workflow file

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
fn simple() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("simple.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Simple (4 nodes, 3 edges)
    Graph: [FIXTURES]/simple.fabro
    Validation: OK
    ");
}

#[test]
fn simple_does_not_connect_to_configured_server() {
    let cli = LightweightCli::new();
    let mut cmd = cli.command();
    cmd.env("FABRO_SERVER", "http://127.0.0.1:9")
        .arg("validate")
        .arg(fixture("simple.fabro"));

    let output = cmd.output().expect("validate should execute");
    assert!(
        output.status.success(),
        "validate should run locally without connecting to FABRO_SERVER\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn branching() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("branching.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Branch (6 nodes, 6 edges)
    Graph: [FIXTURES]/branching.fabro
    warning [node: implement]: Node 'implement' has goal_gate=true but no retry_target or fallback_retry_target (goal_gate_has_retry)
    Validation: OK
    ");
}

#[test]
fn conditions() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("conditions.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Conditions (5 nodes, 5 edges)
    Graph: [FIXTURES]/conditions.fabro
    Validation: OK
    ");
}

#[test]
fn parallel() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("parallel.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Parallel (7 nodes, 7 edges)
    Graph: [FIXTURES]/parallel.fabro
    Validation: OK
    ");
}

#[test]
fn styled() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("styled.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Styled (5 nodes, 4 edges)
    Graph: [FIXTURES]/styled.fabro
    Validation: OK
    ");
}

#[test]
fn legacy_tool() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("legacy_tool.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: LegacyTool (3 nodes, 2 edges)
    Graph: [FIXTURES]/legacy_tool.fabro
    Validation: OK
    ");
}

#[test]
fn bare_fabro_with_unbound_inputs_validates_structurally_with_warning() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templated_unbound.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: TemplatedUnbound (3 nodes, 2 edges)
    Graph: [FIXTURES]/templated_unbound.fabro
    warning: [FIXTURES]/templated_unbound.fabro:2:26: undefined template variable `inputs.app_dir` in graph attribute `goal` (template_undefined_variable)
    warning: [FIXTURES]/templated_unbound.fabro:7:44: undefined template variable `inputs.app_dir` in node `work` attribute `prompt` [node: work] (template_undefined_variable)
    Validation: OK
    ");
}

/// Regression: https://github.com/fabro-sh/fabro/issues/286
///
/// Undefined template variables in a prompt loaded via `@file` reference must
/// surface as the same warning diagnostic as an inline prompt — not a hard
/// validation error.
#[test]
fn bare_fabro_with_unbound_inputs_in_imported_prompt_validates_structurally_with_warning() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templated_unbound_imported/workflow.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: TemplatedUnboundImported (3 nodes, 2 edges)
    Graph: [FIXTURES]/templated_unbound_imported/workflow.fabro
    warning: [FIXTURES]/templated_unbound_imported/work.md:1:12: undefined template variable `inputs.app_dir` in node `work` attribute `prompt` [node: work] (template_undefined_variable)
    Validation: OK
    ");
}

#[test]
fn bare_fabro_picks_up_sibling_workflow_toml_inputs() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templated_inputs/workflow.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: TemplatedInputs (3 nodes, 2 edges)
    Graph: [FIXTURES]/templated_inputs/workflow.fabro
    Validation: OK
    ");
}

#[test]
fn invalid() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("invalid.fabro"));
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
