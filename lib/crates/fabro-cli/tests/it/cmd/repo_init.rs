use insta::assert_snapshot;

use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["repo", "init", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Initialize a new project

    Usage: fabro repo init [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn repo_init_creates_fabro_toml_and_hello_workflow() {
    let context = test_context!();
    context.git_init();

    let mut cmd = context.command();
    cmd.args(["repo", "init"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
      ✔ fabro.toml
      ✔ fabro/workflows/hello/workflow.fabro
      ✔ fabro/workflows/hello/workflow.toml

    Project initialized! Run a workflow with:

      fabro run hello

      ! No git remote found — skipping GitHub App check
      Run `git remote add origin <url>` then `fabro install` to set up the GitHub App
    ");

    assert_snapshot!(
        std::fs::read_to_string(context.temp_dir.join("fabro.toml")).unwrap(),
        @r###"
    # Fabro project configuration
    # https://docs.fabro.computer/getting-started/quick-start

    version = 1

    [fabro]
    root = "fabro/"

    # Disable retrospective analysis after workflow runs:
    # retro = false

    # Auto-create pull requests on successful workflow runs.
    [pull_request]
    enabled = true
    draft = true
    # auto_merge = true
    "###
    );
    assert_snapshot!(
        std::fs::read_to_string(context.temp_dir.join("fabro/workflows/hello/workflow.fabro"))
            .unwrap(),
        @r###"
    digraph Hello {
        graph [goal="Say hello and demonstrate a basic Fabro workflow"]
        rankdir=LR

        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]

        greet [label="Greet", prompt="Say hello! Introduce yourself and explain that this is a test of the Fabro workflow engine."]

        start -> greet -> exit
    }
    "###
    );
    assert_snapshot!(
        std::fs::read_to_string(context.temp_dir.join("fabro/workflows/hello/workflow.toml"))
            .unwrap(),
        @r###"
    version = 1
    graph = "workflow.fabro"

    [sandbox]
    provider = "local"
    "###
    );
}

#[test]
fn repo_init_rejects_already_initialized_repo() {
    let context = test_context!();
    context.git_init();
    std::fs::write(context.temp_dir.join("fabro.toml"), "version = 1\n").unwrap();

    let mut cmd = context.command();
    cmd.args(["repo", "init"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: already initialized — fabro.toml exists at [TEMP_DIR]/fabro.toml
    ");
}

#[test]
fn repo_init_errors_outside_git_repo() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["repo", "init"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: not a git repository — run `git init` first
    ");
}
