use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["workflow", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Workflow operations

    Usage: fabro workflow [OPTIONS] <COMMAND>

    Commands:
      list    List available workflows
      create  Create a new workflow
      help    Print this message or the help of the given subcommand(s)

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
fn list() {
    let context = test_context!();

    context
        .write_temp(
            ".fabro/project.toml",
            "_version = 1\n\n[project]\ndirectory = \"..\"\n",
        )
        .write_temp(
            "workflows/my_test_wf/workflow.toml",
            "_version = 1\n\n[run]\ngoal = \"A test workflow\"\n",
        );

    let mut cmd = context.command();
    cmd.args(["workflow", "list"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    1 workflow(s) found

    User Workflows (~/.fabro/workflows)
      (none)

    Project Workflows (workflows)

      NAME        DESCRIPTION
      my_test_wf  A test workflow
    ");
}
