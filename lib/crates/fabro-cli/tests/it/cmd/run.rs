use fabro_test::{fabro_snapshot, test_context};

use crate::support::{
    compact_progress_event, example_fixture, fabro_json_snapshot, read_json, read_jsonl,
    run_output_filters,
};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Launch a workflow run

    Usage: fabro run [OPTIONS] <WORKFLOW>

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
fn dry_run_simple() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(example_fixture("simple.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Simple (4 nodes, 3 edges)
    Graph: ../../../test/simple.fabro
    Goal: Run tests and report results

        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Run Tests  [TIME]
        ✓ Report  [TIME]
        ✓ Exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [DRY_RUN_DIR]

    === Output ===
    [Simulated] Response for stage: report
    ");
}

#[test]
fn dry_run_writes_jsonl_and_live_json() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FB8";

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "../../../test/simple.fabro",
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    let jsonl_path = run_dir.join("progress.jsonl");
    let progress = read_jsonl(&jsonl_path);
    assert!(
        !progress.is_empty(),
        "progress.jsonl should have at least one line"
    );
    let progress_summary: Vec<_> = progress.iter().map(compact_progress_event).collect();
    fabro_json_snapshot!(context, &progress_summary, @r#"
    [
      {
        "event": "Sandbox.Initializing",
        "sandbox_provider": "local"
      },
      {
        "event": "Sandbox.Ready",
        "sandbox_provider": "local"
      },
      {
        "event": "SandboxInitialized"
      },
      {
        "event": "WorkflowRunStarted",
        "workflow_name": "Simple",
        "goal": "Run tests and report results"
      },
      {
        "event": "StageStarted",
        "node_id": "start",
        "node_label": "Start",
        "handler_type": "start",
        "stage_index": 0
      },
      {
        "event": "StageCompleted",
        "node_id": "start",
        "node_label": "Start",
        "stage_index": 0,
        "status": "success"
      },
      {
        "event": "EdgeSelected",
        "from_node_id": "start",
        "to_node_id": "run_tests",
        "reason": "unconditional"
      },
      {
        "event": "CheckpointCompleted",
        "node_id": "start",
        "node_label": "start",
        "status": "success"
      },
      {
        "event": "StageStarted",
        "node_id": "run_tests",
        "node_label": "Run Tests",
        "handler_type": "agent",
        "stage_index": 1
      },
      {
        "event": "StageCompleted",
        "node_id": "run_tests",
        "node_label": "Run Tests",
        "stage_index": 1,
        "status": "success"
      },
      {
        "event": "EdgeSelected",
        "from_node_id": "run_tests",
        "to_node_id": "report",
        "reason": "unconditional"
      },
      {
        "event": "CheckpointCompleted",
        "node_id": "run_tests",
        "node_label": "run_tests",
        "status": "success"
      },
      {
        "event": "StageStarted",
        "node_id": "report",
        "node_label": "Report",
        "handler_type": "agent",
        "stage_index": 2
      },
      {
        "event": "StageCompleted",
        "node_id": "report",
        "node_label": "Report",
        "stage_index": 2,
        "status": "success"
      },
      {
        "event": "EdgeSelected",
        "from_node_id": "report",
        "to_node_id": "exit",
        "reason": "unconditional"
      },
      {
        "event": "CheckpointCompleted",
        "node_id": "report",
        "node_label": "report",
        "status": "success"
      },
      {
        "event": "StageStarted",
        "node_id": "exit",
        "node_label": "Exit",
        "handler_type": "exit",
        "stage_index": 3
      },
      {
        "event": "StageCompleted",
        "node_id": "exit",
        "node_label": "Exit",
        "stage_index": 3,
        "status": "success"
      },
      {
        "event": "WorkflowRunCompleted",
        "status": "success",
        "artifact_count": 0
      },
      {
        "event": "Sandbox.CleanupStarted",
        "sandbox_provider": "local"
      },
      {
        "event": "Sandbox.CleanupCompleted",
        "sandbox_provider": "local"
      }
    ]
    "#);

    let live_path = run_dir.join("live.json");
    let live_content = read_json(&live_path);
    let live_summary = compact_progress_event(&live_content);
    fabro_json_snapshot!(context, &live_summary, @r#"
    {
      "event": "Sandbox.CleanupCompleted",
      "sandbox_provider": "local"
    }
    "#);

    assert_eq!(live_summary, progress_summary.last().cloned().unwrap());
}

#[test]
fn run_id_passthrough_uses_provided_ulid() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "../../../test/simple.fabro",
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    let run_record = read_json(run_dir.join("run.json"));
    assert_eq!(run_record["run_id"].as_str(), Some(run_id));
}

#[test]
fn detach_prints_ulid_and_exits() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args([
        "--detach",
        "--dry-run",
        "--auto-approve",
        "../../../test/simple.fabro",
    ]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    [ULID]
    ----- stderr -----
    ");
}

#[test]
fn detach_creates_run_dir_with_detach_log() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FB9";

    context
        .run_cmd()
        .args([
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "../../../test/simple.fabro",
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "run_dir": run_dir,
            "launcher_log_exists": context.storage_dir.join("launchers").join(format!("{run_id}.log")).exists(),
            "detach_log_exists": run_dir.join("detach.log").exists(),
        }),
        @r#"
        {
          "run_dir": "[DRY_RUN_DIR]",
          "launcher_log_exists": true,
          "detach_log_exists": false
        }
        "#
    );
}
