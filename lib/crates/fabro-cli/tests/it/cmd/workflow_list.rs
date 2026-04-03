use fabro_test::{fabro_snapshot, test_context};

use super::support::{add_project_workflow, add_user_workflow, setup_project_fixture};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["workflow", "list", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List available workflows

    Usage: fabro workflow list [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn workflow_list_errors_without_project_config() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["workflow", "list"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: No fabro.toml found in [TEMP_DIR] or any parent directory
    ");
}

#[test]
fn workflow_list_shows_project_and_user_sections() {
    let context = test_context!();
    let project = setup_project_fixture(&context);
    add_project_workflow(
        &project,
        "project-alpha",
        "Project alpha goal",
        "digraph ProjectAlpha {\n  graph [goal=\"Project alpha goal\"]\n  start [shape=Mdiamond]\n  exit [shape=Msquare]\n  main [label=\"Main\", prompt=\"Do project alpha\"]\n  start -> main -> exit\n}\n",
    );
    add_user_workflow(&context, "user-beta", "User beta goal");

    let mut cmd = context.command();
    cmd.current_dir(&project.project_dir);
    cmd.args(["workflow", "list"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    2 workflow(s) found

    User Workflows (~/.fabro/workflows)

      NAME           DESCRIPTION
      user-beta      User beta goal

    Project Workflows (fabro/workflows)

      NAME           DESCRIPTION
      project-alpha  Project alpha goal
    ");
}
