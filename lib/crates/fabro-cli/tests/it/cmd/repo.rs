use fabro_test::{fabro_snapshot, test_context};

fn init_fabro_project(context: &fabro_test::TestContext) {
    context
        .write_temp(".fabro/project.toml", "_version = 1\n")
        .write_temp(".fabro/workflows/hello/workflow.fabro", "digraph {}")
        .write_temp(".fabro/workflows/hello/workflow.toml", "_version = 1\n");
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.repo();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Repository commands

    Usage: fabro repo [OPTIONS] <COMMAND>

    Commands:
      init    Initialize a new project
      deinit  Remove .fabro/ project directory
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
fn test_repo_deinit_removes_fabro_toml_and_dir() {
    let context = test_context!();
    context.git_init();
    init_fabro_project(&context);

    assert!(context.temp_dir.join(".fabro/project.toml").exists());
    assert!(context.temp_dir.join(".fabro").exists());

    context.repo().arg("deinit").assert().success();

    assert!(
        !context.temp_dir.join(".fabro/project.toml").exists(),
        ".fabro/project.toml should be removed"
    );
    assert!(
        !context.temp_dir.join(".fabro").exists(),
        ".fabro/ directory should be removed"
    );
}

#[test]
fn test_repo_deinit_fails_when_not_initialized() {
    let context = test_context!();
    context.git_init();

    let mut cmd = context.repo();
    cmd.arg("deinit");
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: not initialized — .fabro/project.toml not found
    ");
}
