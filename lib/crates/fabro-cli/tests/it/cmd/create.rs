use fabro_test::{fabro_snapshot, test_context};

use crate::support::{fabro_json_snapshot, read_json};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["create", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Create a workflow run (allocate run dir, persist spec)

    Usage: fabro create [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Path to a .fabro workflow file or .toml task config

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --dry-run                    Execute with simulated LLM backend
          --auto-approve               Auto-approve all human gates
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --goal <GOAL>                Override the workflow goal (exposed as $goal in prompts)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --goal-file <GOAL_FILE>      Read the workflow goal from a file
          --model <MODEL>              Override default LLM model
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --provider <PROVIDER>        Override default LLM provider
      -v, --verbose                    Enable verbose output
          --sandbox <SANDBOX>          Sandbox for agent tools [possible values: local, docker, daytona]
          --label <KEY=VALUE>          Attach a label to this run (repeatable, format: KEY=VALUE)
          --no-retro                   Skip retro generation after the run
          --preserve-sandbox           Keep the sandbox alive after the run finishes (for debugging)
      -d, --detach                     Run the workflow in the background and print the run ID
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn create_persists_directory_workflow_slug_and_cached_graph() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
    let workflow_path = context.temp_dir.join("sluggy/workflow.fabro");

    context.write_temp(
        "sluggy/workflow.fabro",
        "\
digraph BarBaz {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    let run_record = read_json(run_dir.join("run.json"));
    let cached_graph = std::fs::read_to_string(run_dir.join("workflow.fabro")).unwrap();
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "workflow_slug": run_record["workflow_slug"],
            "graph_name": run_record["graph"]["name"],
            "cached_graph_lines": cached_graph.lines().collect::<Vec<_>>(),
        }),
        @r#"
        {
          "workflow_slug": "sluggy",
          "graph_name": "BarBaz",
          "cached_graph_lines": [
            "digraph BarBaz {",
            "  start [shape=Mdiamond, label=\"Start\"]",
            "  exit  [shape=Msquare, label=\"Exit\"]",
            "  start -> exit",
            "}"
          ]
        }
        "#
    );
}

#[test]
fn create_persists_file_stem_slug_for_standalone_file() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAB";
    let workflow_path = context.temp_dir.join("alpha.fabro");

    context.write_temp(
        "alpha.fabro",
        "\
digraph FooWorkflow {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    let run_record = read_json(run_dir.join("run.json"));
    let cached_graph = std::fs::read_to_string(run_dir.join("workflow.fabro")).unwrap();
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "workflow_slug": run_record["workflow_slug"],
            "graph_name": run_record["graph"]["name"],
            "cached_graph_lines": cached_graph.lines().collect::<Vec<_>>(),
        }),
        @r#"
        {
          "workflow_slug": "alpha",
          "graph_name": "FooWorkflow",
          "cached_graph_lines": [
            "digraph FooWorkflow {",
            "  start [shape=Mdiamond, label=\"Start\"]",
            "  exit  [shape=Msquare, label=\"Exit\"]",
            "  start -> exit",
            "}"
          ]
        }
        "#
    );
}
