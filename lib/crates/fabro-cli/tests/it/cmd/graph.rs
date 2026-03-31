use fabro_test::{fabro_snapshot, test_context};

use super::support::fixture;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["graph", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Render a workflow graph as SVG or PNG

    Usage: fabro graph [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>
              Path to the .fabro workflow file, .toml task config, or project workflow name

    Options:
          --format <FORMAT>
              Output format
              
              [default: svg]
              [possible values: svg, png]

          --json
              Output as JSON
              
              [env: FABRO_JSON=]

          --debug
              Enable DEBUG-level logging (default is INFO)
              
              [env: FABRO_DEBUG=]

      -o, --output <OUTPUT>
              Output file path (defaults to stdout)

      -d, --direction <DIRECTION>
              Graph layout direction (overrides the DOT file's rankdir)

              Possible values:
              - lr: Left to right
              - tb: Top to bottom

          --no-upgrade-check
              Disable automatic upgrade check
              
              [env: FABRO_NO_UPGRADE_CHECK=true]

          --quiet
              Suppress non-essential output
              
              [env: FABRO_QUIET=]

          --verbose
              Enable verbose output
              
              [env: FABRO_VERBOSE=]

          --storage-dir <STORAGE_DIR>
              Storage directory (default: ~/.fabro)
              
              [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]

      -h, --help
              Print help (see a summary with '-h')
    ----- stderr -----
    ");
}

#[test]
fn graph_invalid_workflow_fails_after_diagnostics() {
    let context = test_context!();
    let workflow = fixture("invalid.fabro");
    let mut cmd = context.command();
    cmd.args(["graph", workflow.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Pipeline must have exactly one start node (shape=Mdiamond or id start/Start) (start_node)
    error [node: exit]: Exit node 'exit' has 1 outgoing edge(s) but must have none (exit_no_outgoing)
    error: Validation failed
    ");
}
