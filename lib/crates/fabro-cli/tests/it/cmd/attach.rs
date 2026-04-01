use std::time::Duration;

use fabro_test::{fabro_snapshot, run_and_format, test_context};
use serde_json::Value;

use crate::support::{example_fixture, fabro_json_snapshot, run_output_filters};

use super::support::{output_stdout, resolve_run, wait_for_status, write_gated_workflow};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["attach", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Attach to a running or finished workflow run

    Usage: fabro attach [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name

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
fn attach_requires_run_arg() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.arg("attach");
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <RUN>

    Usage: fabro attach --no-upgrade-check --storage-dir <STORAGE_DIR> <RUN>

    For more information, try '--help'.
    ");
}

#[test]
fn attach_replays_completed_detached_run() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAQ";

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--detach",
            "--run-id",
            run_id,
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["wait", run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let mut cmd = context.command();
    cmd.args(["attach", run_id]);
    cmd.timeout(std::time::Duration::from_secs(10));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Run Tests  [TIME]
        ✓ Report  [TIME]
        ✓ Exit  [TIME]
    ");
}

#[test]
fn attach_before_completion_streams_to_finished_state() {
    let context = test_context!();
    let gate = write_gated_workflow(&context.temp_dir.join("slow.fabro"), "slow", "Run slowly");

    let mut run_cmd = context.command();
    run_cmd.current_dir(&context.temp_dir);
    run_cmd.env("OPENAI_API_KEY", "test");
    run_cmd.args([
        "run",
        "--detach",
        "--provider",
        "openai",
        "--sandbox",
        "local",
        "--no-retro",
        "slow.fabro",
    ]);
    let run_output = run_cmd.output().expect("command should execute");
    assert!(
        run_output.status.success(),
        "run --detach failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let run_id = output_stdout(&run_output).trim().to_string();
    let run = resolve_run(&context, &run_id);
    wait_for_status(&run.run_dir, &["running"]);

    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let release_gate = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        gate.release();
    });
    let mut attach_cmd = context.command();
    attach_cmd.current_dir(&context.temp_dir);
    attach_cmd.args(["attach", &run_id]);
    let (snapshot, _output) = run_and_format(&mut attach_cmd, &filters);
    release_gate.join().expect("gate releaser should join");
    wait_for_status(&run.run_dir, &["succeeded"]);

    insta::assert_snapshot!(snapshot, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
        Sandbox: local (ready in [TIME])
        ✓ start  [DURATION]
    ");
}

#[test]
fn attach_json_errors_without_prompting_for_human_input() {
    let context = test_context!();
    let workflow = context.temp_dir.join("human-gate.fabro");
    context.write_temp(
        "human-gate.fabro",
        r#"digraph HumanGate {
  graph [goal="Wait for approval"]
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

    let run_output = context
        .command()
        .current_dir(&context.temp_dir)
        .env("OPENAI_API_KEY", "test")
        .args([
            "run",
            "--detach",
            "--no-retro",
            "--sandbox",
            "local",
            "--provider",
            "openai",
            workflow.to_str().unwrap(),
        ])
        .output()
        .expect("detached run should execute");
    assert!(
        run_output.status.success(),
        "detached run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let run_id = output_stdout(&run_output).trim().to_string();
    let cleanup_run_id = run_id.clone();
    scopeguard::defer! {
        let _ = context.command().args(["rm", "--force", &cleanup_run_id]).output();
    }
    let run_dir = context.find_run_dir(&run_id);

    let request_path = run_dir.join("runtime/interview_request.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !request_path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for interview request for {run_id}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let output = context
        .command()
        .args(["--json", "attach", &run_id])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("attach should execute");

    assert!(!output.status.success(), "attach --json should fail fast");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("--json is non-interactive"));
    assert!(
        !stderr.contains("Approve?"),
        "attach should not prompt on stderr"
    );
    assert!(
        request_path.exists(),
        "the run should still be waiting on the interview request"
    );
    assert!(
        !run_dir.join("runtime/interview_response.json").exists(),
        "attach --json should not answer the interview"
    );

    let progress: Vec<Value> = String::from_utf8(output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("attach JSON output should be JSONL"))
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
        "event": "run.started",
        "properties": {
          "name": "HumanGate",
          "goal": "Wait for approval"
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
          "notes": null,
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
      }
    ]
    "#);
}
