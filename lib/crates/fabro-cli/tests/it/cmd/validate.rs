use fabro_test::{fabro_snapshot, test_context};

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
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
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
    error: Validation failed
    ");
}
