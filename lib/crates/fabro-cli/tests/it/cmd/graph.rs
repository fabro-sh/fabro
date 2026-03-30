use fabro_test::{fabro_snapshot, test_context};

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
          --debug
              Enable DEBUG-level logging (default is INFO)
              
              [env: FABRO_DEBUG=]

          --format <FORMAT>
              Output format
              
              [default: svg]
              [possible values: svg, png]

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
