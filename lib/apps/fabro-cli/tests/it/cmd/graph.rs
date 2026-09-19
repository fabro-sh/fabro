use fabro_test::{fabro_snapshot, test_context};

use super::support::{fixture, read_text};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["graph", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Render a workflow graph as SVG

    Usage: fabro graph [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>
              Path to the .fabro workflow file, .toml task config, or project workflow name

    Options:
          --json
              Output as JSON
              
              [env: FABRO_JSON=]

          --server <SERVER>
              Fabro server target: http(s) URL or absolute Unix socket path
              
              [env: FABRO_SERVER=]

          --debug
              Enable DEBUG-level logging (default is INFO)
              
              [env: FABRO_DEBUG=]

          --format <FORMAT>
              Output format
              
              [default: svg]
              [possible values: svg]

          --no-upgrade-check
              Disable automatic upgrade check
              
              [env: FABRO_NO_UPGRADE_CHECK=true]

      -o, --output <OUTPUT>
              Output file path (defaults to stdout)

      -d, --direction <DIRECTION>
              Graph layout direction (overrides the DOT file's rankdir)

              Possible values:
              - lr: Left to right
              - tb: Top to bottom

          --quiet
              Suppress non-essential output
              
              [env: FABRO_QUIET=]

          --allow-invalid
              Render even when workflow validation reports errors

          --verbose
              Enable verbose output
              
              [env: FABRO_VERBOSE=]

      -h, --help
              Print help (see a summary with '-h')
    ----- stderr -----
    ");
}

#[test]
fn graph_allow_invalid_renders_after_diagnostics() {
    let context = test_context!();
    let workflow = fixture("invalid.fabro");
    let output_path = context.temp_dir.join("invalid.svg");
    let mut cmd = context.command();
    cmd.args([
        "graph",
        "--allow-invalid",
        "-o",
        output_path.to_str().unwrap(),
        workflow.to_str().unwrap(),
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    error: [FIXTURES]/invalid.fabro:1:9: the workflow has no start node (`shape=Mdiamond`, `type=start`, or an id of `start`) (attractor.no_start)
    ");

    let svg = read_text(&output_path);
    assert!(
        svg.contains("<svg") && svg.contains("Invalid"),
        "expected invalid workflow to render as SVG, got: {}",
        &svg[..svg.len().min(200)]
    );
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
    error: [FIXTURES]/invalid.fabro:1:9: the workflow has no start node (`shape=Mdiamond`, `type=start`, or an id of `start`) (attractor.no_start)
      × Validation failed
    ");
}
