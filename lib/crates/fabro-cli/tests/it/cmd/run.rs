use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

use crate::support::{
    example_fixture, fabro_json_snapshot, read_json, read_jsonl, run_output_filters,
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
    Graph: [FIXTURES]/simple.fabro
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
            "--sandbox",
            "local",
            "--run-id",
            run_id,
            example_fixture("simple.fabro").to_str().unwrap(),
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
    fabro_json_snapshot!(context, &progress, @r#"
    [
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.initializing",
        "properties": {
          "provider": "local"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.ready",
        "properties": {
          "provider": "local",
          "duration_ms": "[DURATION_MS]",
          "name": null,
          "cpu": null,
          "memory": null,
          "url": null
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.initialized",
        "properties": {
          "working_directory": "[TEMP_DIR]"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "run.started",
        "properties": {
          "name": "Simple",
          "goal": "Run tests and report results"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.started",
        "node_id": "start",
        "node_label": "Start",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 0,
          "handler_type": "start"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.completed",
        "node_id": "start",
        "node_label": "Start",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 0,
          "duration_ms": "[DURATION_MS]",
          "status": "success",
          "preferred_label": null,
          "suggested_next_ids": [],
          "usage": null,
          "notes": "[Simulated] start",
          "files_touched": []
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "edge.selected",
        "properties": {
          "from_node": "start",
          "to_node": "run_tests",
          "label": null,
          "condition": null,
          "reason": "unconditional",
          "stage_status": "success",
          "is_jump": false
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "checkpoint.completed",
        "node_id": "start",
        "node_label": "start",
        "properties": {
          "status": "success"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.started",
        "node_id": "run_tests",
        "node_label": "Run Tests",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 1,
          "handler_type": "agent"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.completed",
        "node_id": "run_tests",
        "node_label": "Run Tests",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 1,
          "duration_ms": "[DURATION_MS]",
          "status": "success",
          "preferred_label": null,
          "suggested_next_ids": [],
          "usage": null,
          "notes": "[Simulated] run_tests",
          "files_touched": []
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "edge.selected",
        "properties": {
          "from_node": "run_tests",
          "to_node": "report",
          "label": null,
          "condition": null,
          "reason": "unconditional",
          "stage_status": "success",
          "is_jump": false
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "checkpoint.completed",
        "node_id": "run_tests",
        "node_label": "run_tests",
        "properties": {
          "status": "success"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.started",
        "node_id": "report",
        "node_label": "Report",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 2,
          "handler_type": "agent"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.completed",
        "node_id": "report",
        "node_label": "Report",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 2,
          "duration_ms": "[DURATION_MS]",
          "status": "success",
          "preferred_label": null,
          "suggested_next_ids": [],
          "usage": null,
          "notes": "[Simulated] report",
          "files_touched": []
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "edge.selected",
        "properties": {
          "from_node": "report",
          "to_node": "exit",
          "label": null,
          "condition": null,
          "reason": "unconditional",
          "stage_status": "success",
          "is_jump": false
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "checkpoint.completed",
        "node_id": "report",
        "node_label": "report",
        "properties": {
          "status": "success"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.started",
        "node_id": "exit",
        "node_label": "Exit",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 3,
          "handler_type": "exit"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.completed",
        "node_id": "exit",
        "node_label": "Exit",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 3,
          "duration_ms": "[DURATION_MS]",
          "status": "success",
          "preferred_label": null,
          "suggested_next_ids": [],
          "usage": null,
          "notes": null,
          "files_touched": []
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "run.completed",
        "properties": {
          "duration_ms": "[DURATION_MS]",
          "artifact_count": 0,
          "status": "success"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.cleanup.started",
        "properties": {
          "provider": "local"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.cleanup.completed",
        "properties": {
          "provider": "local",
          "duration_ms": "[DURATION_MS]"
        }
      }
    ]
    "#);

    let live_path = run_dir.join("live.json");
    let live_content = read_json(&live_path);
    fabro_json_snapshot!(context, &live_content, @r#"
    {
      "id": "[EVENT_ID]",
      "ts": "[TIMESTAMP]",
      "run_id": "[ULID]",
      "event": "sandbox.cleanup.completed",
      "properties": {
        "provider": "local",
        "duration_ms": "[DURATION_MS]"
      }
    }
    "#);

    assert_eq!(live_content, *progress.last().unwrap());
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
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    let run_record = read_json(run_dir.join("run.json"));
    assert_eq!(run_record["run_id"].as_str(), Some(run_id));
}

#[test]
fn json_run_implies_auto_approve_for_human_gates() {
    let context = test_context!();
    let workflow = context.temp_dir.join("human-gate.fabro");
    context.write_temp(
        "human-gate.fabro",
        r#"digraph HumanGate {
  graph [goal="Route through the default approval path"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare, label="Exit"]
  approve [shape=hexagon, label="Approve?"]
  ship   [shape=parallelogram, script="echo shipped"]
  revise [shape=parallelogram, script="echo revised"]
  start -> approve
  approve -> ship   [label="[A] Approve"]
  approve -> revise [label="[R] Revise"]
  ship -> exit
  revise -> exit
}
"#,
    );

    let output = context
        .command()
        .args([
            "--json",
            "run",
            "--sandbox",
            "local",
            "--no-retro",
            workflow.to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let progress: Vec<Value> = String::from_utf8(output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("run JSON output should be JSONL"))
        .collect();
    fabro_json_snapshot!(context, &progress, @r#"
    [
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.initializing",
        "properties": {
          "provider": "local"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.ready",
        "properties": {
          "provider": "local",
          "duration_ms": "[DURATION_MS]",
          "name": null,
          "cpu": null,
          "memory": null,
          "url": null
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.initialized",
        "properties": {
          "working_directory": "[TEMP_DIR]"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "run.notice",
        "properties": {
          "level": "warn",
          "code": "dry_run_no_llm",
          "message": "No LLM providers configured. Running in dry-run mode."
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "run.started",
        "properties": {
          "name": "HumanGate",
          "goal": "Route through the default approval path"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.started",
        "node_id": "start",
        "node_label": "Start",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 0,
          "handler_type": "start"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.completed",
        "node_id": "start",
        "node_label": "Start",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 0,
          "duration_ms": "[DURATION_MS]",
          "status": "success",
          "preferred_label": null,
          "suggested_next_ids": [],
          "usage": null,
          "notes": "[Simulated] start",
          "files_touched": []
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "edge.selected",
        "properties": {
          "from_node": "start",
          "to_node": "approve",
          "label": null,
          "condition": null,
          "reason": "unconditional",
          "stage_status": "success",
          "is_jump": false
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "checkpoint.completed",
        "node_id": "start",
        "node_label": "start",
        "properties": {
          "status": "success"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.started",
        "node_id": "approve",
        "node_label": "Approve?",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 1,
          "handler_type": "human"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.completed",
        "node_id": "approve",
        "node_label": "Approve?",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 1,
          "duration_ms": "[DURATION_MS]",
          "status": "success",
          "preferred_label": "[A] Approve",
          "suggested_next_ids": [
            "ship"
          ],
          "usage": null,
          "notes": "[Simulated] approve",
          "files_touched": []
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "edge.selected",
        "properties": {
          "from_node": "approve",
          "to_node": "ship",
          "label": "[A] Approve",
          "condition": null,
          "reason": "preferred_label",
          "preferred_label": "[A] Approve",
          "suggested_next_ids": [
            "ship"
          ],
          "stage_status": "success",
          "is_jump": false
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "checkpoint.completed",
        "node_id": "approve",
        "node_label": "approve",
        "properties": {
          "status": "success"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.started",
        "node_id": "ship",
        "node_label": "ship",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 2,
          "handler_type": "command",
          "script": "echo shipped"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.completed",
        "node_id": "ship",
        "node_label": "ship",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 2,
          "duration_ms": "[DURATION_MS]",
          "status": "success",
          "preferred_label": null,
          "suggested_next_ids": [],
          "usage": null,
          "notes": "[Simulated] Command skipped: echo shipped",
          "files_touched": []
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "edge.selected",
        "properties": {
          "from_node": "ship",
          "to_node": "exit",
          "label": null,
          "condition": null,
          "reason": "unconditional",
          "stage_status": "success",
          "is_jump": false
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "checkpoint.completed",
        "node_id": "ship",
        "node_label": "ship",
        "properties": {
          "status": "success"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.started",
        "node_id": "exit",
        "node_label": "Exit",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 3,
          "handler_type": "exit"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "stage.completed",
        "node_id": "exit",
        "node_label": "Exit",
        "properties": {
          "max_attempts": 1,
          "attempt": 1,
          "index": 3,
          "duration_ms": "[DURATION_MS]",
          "status": "success",
          "preferred_label": null,
          "suggested_next_ids": [],
          "usage": null,
          "notes": null,
          "files_touched": []
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "run.completed",
        "properties": {
          "duration_ms": "[DURATION_MS]",
          "artifact_count": 0,
          "status": "success"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.cleanup.started",
        "properties": {
          "provider": "local"
        }
      },
      {
        "id": "[EVENT_ID]",
        "ts": "[TIMESTAMP]",
        "run_id": "[ULID]",
        "event": "sandbox.cleanup.completed",
        "properties": {
          "provider": "local",
          "duration_ms": "[DURATION_MS]"
        }
      }
    ]
    "#);

    let run = context.single_run_dir();
    let run_json = read_json(run.join("run.json"));
    assert_eq!(
        run_json.pointer("/settings/auto_approve"),
        Some(&serde_json::json!(true))
    );
}

#[test]
fn detach_prints_ulid_and_exits() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args([
        "--detach",
        "--dry-run",
        "--auto-approve",
        example_fixture("simple.fabro").to_str().unwrap(),
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
            example_fixture("simple.fabro").to_str().unwrap(),
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
