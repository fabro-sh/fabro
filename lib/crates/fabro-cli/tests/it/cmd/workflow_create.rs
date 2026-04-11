use fabro_test::{fabro_snapshot, test_context};
use insta::assert_snapshot;
use serde_json::Value;

use super::support::setup_project_fixture;
use crate::support::fabro_json_snapshot;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["workflow", "create", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Create a new workflow

    Usage: fabro workflow create [OPTIONS] <NAME>

    Arguments:
      <NAME>  Name of the workflow

    Options:
      -g, --goal <GOAL>       Goal description for the workflow
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
fn workflow_create_writes_scaffold_files() {
    let context = test_context!();
    let project = setup_project_fixture(&context);
    let mut cmd = context.command();
    cmd.current_dir(&project.project_dir);
    cmd.args(["workflow", "create", "hello-world"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
      ✔ fabro/workflows/hello-world/workflow.fabro
      ✔ fabro/workflows/hello-world/workflow.toml

    Workflow created! Next steps:

      1. Edit the graph:  fabro/workflows/hello-world/workflow.fabro
      2. Validate:        fabro validate hello-world
      3. Run:             fabro run hello-world
    ");

    assert_snapshot!(
        std::fs::read_to_string(project.fabro_root.join("workflows/hello-world/workflow.fabro"))
            .unwrap(),
        @r###"
    digraph HelloWorld {
        graph [goal="TODO: describe the goal"]
        rankdir=LR

        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]

        main [label="Main", prompt="TODO: describe what this agent should do"]

        start -> main -> exit
    }
    "###
    );
    assert_snapshot!(
        std::fs::read_to_string(project.fabro_root.join("workflows/hello-world/workflow.toml"))
            .unwrap(),
        @r###"
    _version = 1
    "###
    );
}

#[test]
fn workflow_create_uses_explicit_goal_in_scaffold() {
    let context = test_context!();
    let project = setup_project_fixture(&context);
    let mut cmd = context.command();
    cmd.current_dir(&project.project_dir);
    cmd.args([
        "workflow",
        "create",
        "--goal",
        "Ship a polished release",
        "release-flow",
    ]);
    cmd.assert().success();

    assert_snapshot!(
        std::fs::read_to_string(project.fabro_root.join("workflows/release-flow/workflow.fabro"))
            .unwrap(),
        @r###"
    digraph ReleaseFlow {
        graph [goal="Ship a polished release"]
        rankdir=LR

        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]

        main [label="Main", prompt="TODO: describe what this agent should do"]

        start -> main -> exit
    }
    "###
    );
}

#[test]
fn workflow_create_rejects_existing_workflow() {
    let context = test_context!();
    let project = setup_project_fixture(&context);
    std::fs::create_dir_all(project.fabro_root.join("workflows/existing")).unwrap();
    std::fs::write(
        project.fabro_root.join("workflows/existing/workflow.toml"),
        "_version = 1\n",
    )
    .unwrap();

    let mut cmd = context.command();
    cmd.current_dir(&project.project_dir);
    cmd.args(["workflow", "create", "existing"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Workflow 'existing' already exists at [TEMP_DIR]/project/fabro/workflows/existing
    ");
}

#[test]
fn workflow_create_errors_without_project_config() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["workflow", "create", "hello-world"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: No fabro.toml found in [TEMP_DIR] or any parent directory
    ");
}

#[test]
fn workflow_create_json_uses_resolved_custom_root_paths() {
    let context = test_context!();
    let project_dir = context.temp_dir.join("project");
    context.write_temp(
        "project/fabro.toml",
        "_version = 1\n\n[project]\ndirectory = \"custom/fabro-data\"\n",
    );

    let output = context
        .command()
        .current_dir(&project_dir)
        .args(["--json", "workflow", "create", "hello-world"])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: Value =
        serde_json::from_slice(&output.stdout).expect("workflow create JSON should parse");
    fabro_json_snapshot!(context, &value, @r#"
    {
      "name": "hello-world",
      "created": [
        "custom/fabro-data/workflows/hello-world/workflow.fabro",
        "custom/fabro-data/workflows/hello-world/workflow.toml"
      ]
    }
    "#);

    assert!(
        project_dir
            .join("custom/fabro-data/workflows/hello-world/workflow.fabro")
            .exists()
    );
    assert!(
        project_dir
            .join("custom/fabro-data/workflows/hello-world/workflow.toml")
            .exists()
    );
}
