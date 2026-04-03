use insta::assert_snapshot;
use serde_json::json;

use fabro_test::{fabro_snapshot, test_context};

use crate::support::fabro_json_snapshot;

use super::support::{fixture, output_stdout, resolve_run, run_snapshot};

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
          --dry-run                    Execute with simulated LLM backend
          --json                       Output as JSON [env: FABRO_JSON=]
          --auto-approve               Auto-approve all human gates
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --goal <GOAL>                Override the workflow goal (exposed as $goal in prompts)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --goal-file <GOAL_FILE>      Read the workflow goal from a file
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --model <MODEL>              Override default LLM model
          --provider <PROVIDER>        Override default LLM provider
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
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
    let snapshot = run_snapshot(&run_dir);
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "workflow_slug": snapshot.run.workflow_slug,
            "graph_name": snapshot.run.graph.name,
            "cached_graph_lines": snapshot.graph.expect("graph should exist").lines().collect::<Vec<_>>(),
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
    let snapshot = run_snapshot(&run_dir);
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "workflow_slug": snapshot.run.workflow_slug,
            "graph_name": snapshot.run.graph.name,
            "cached_graph_lines": snapshot.graph.expect("graph should exist").lines().collect::<Vec<_>>(),
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

#[test]
fn create_persists_requested_overrides_into_store() {
    let context = test_context!();
    let workflow = fixture("simple.fabro");
    let mut cmd = context.command();
    cmd.args([
        "create",
        "--dry-run",
        "--auto-approve",
        "--goal",
        "Ship the release",
        "--model",
        "gpt-5",
        "--provider",
        "openai",
        "--sandbox",
        "local",
        "--label",
        "env=dev",
        "--label",
        "team=cli",
        "--verbose",
        "--no-retro",
        "--preserve-sandbox",
        workflow.to_str().unwrap(),
    ]);
    let output = cmd.output().expect("command should execute");
    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = output_stdout(&output);
    let run_id = stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .expect("create should print a run ID")
        .to_string();
    let run = resolve_run(&context, &run_id);
    let snapshot = run_snapshot(&run.run_dir);
    let labels = json!({
        "env": snapshot.run.labels.get("env"),
        "team": snapshot.run.labels.get("team"),
    });
    let compact = json!({
        "workflow_slug": snapshot.run.workflow_slug,
        "settings": {
            "goal": snapshot.run.settings.goal,
            "dry_run": snapshot.run.settings.dry_run,
            "auto_approve": snapshot.run.settings.auto_approve,
            "no_retro": snapshot.run.settings.no_retro,
            "verbose": snapshot.run.settings.verbose,
            "llm": {
                "model": snapshot.run.settings.llm.as_ref().and_then(|llm| llm.model.clone()),
                "provider": snapshot.run.settings.llm.as_ref().and_then(|llm| llm.provider.clone()),
            },
            "sandbox": {
                "provider": snapshot.run.settings.sandbox.as_ref().and_then(|sandbox| sandbox.provider.clone()),
                "preserve": snapshot.run.settings.sandbox.as_ref().and_then(|sandbox| sandbox.preserve),
            },
        },
        "labels": labels,
    });

    assert_snapshot!(serde_json::to_string_pretty(&compact).unwrap(), @r###"
    {
      "workflow_slug": "simple",
      "settings": {
        "goal": "Ship the release",
        "dry_run": true,
        "auto_approve": true,
        "no_retro": true,
        "verbose": true,
        "llm": {
          "model": "gpt-5",
          "provider": "openai"
        },
        "sandbox": {
          "provider": "local",
          "preserve": true
        }
      },
      "labels": {
        "env": "dev",
        "team": "cli"
      }
    }
    "###);
}

#[test]
fn create_json_implies_auto_approve() {
    let context = test_context!();
    let workflow = fixture("simple.fabro");
    let output = context
        .command()
        .args(["--json", "create", "--dry-run", workflow.to_str().unwrap()])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("create JSON should parse");
    let run_id = value["run_id"]
        .as_str()
        .expect("create JSON should include run_id");
    let run = resolve_run(&context, run_id);

    assert_eq!(
        run_snapshot(&run.run_dir).run.settings.auto_approve,
        Some(true)
    );
}

#[test]
fn create_invalid_workflow_fails_without_creating_run() {
    let context = test_context!();
    let workflow = fixture("invalid.fabro");
    let mut cmd = context.command();
    cmd.args(["create", workflow.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Validation failed
    ");

    let runs_dir = context.storage_dir.join("runs");
    let run_count = std::fs::read_dir(&runs_dir)
        .ok()
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    assert_eq!(run_count, 0, "invalid create should not persist a run");
}
