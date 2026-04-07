use std::time::Duration;

use fabro_test::{fabro_snapshot, run_and_format, test_context};
use httpmock::MockServer;
use serde_json::Value;

use crate::support::{example_fixture, fabro_json_snapshot, run_output_filters, unique_run_id};

use super::support::{output_stdout, resolve_run, wait_for_status, write_gated_workflow};

const SHARED_DAEMON_TIMEOUT: Duration = Duration::from_secs(30);

fn live_run_state_response() -> serde_json::Value {
    serde_json::json!({
        "run": null,
        "graph_source": null,
        "start": null,
        "status": {
            "status": "running",
            "reason": null,
            "updated_at": "2026-04-05T12:00:01Z"
        },
        "checkpoint": null,
        "checkpoints": [],
        "conclusion": null,
        "retro": null,
        "retro_prompt": null,
        "retro_response": null,
        "sandbox": null,
        "final_patch": null,
        "pull_request": null,
        "nodes": {}
    })
}

fn run_sse_body(run_id: &str) -> String {
    let completed = serde_json::json!({
        "seq": 2,
        "payload": {
            "event": "run.completed",
            "id": "evt-run-completed",
            "run_id": run_id,
            "ts": "2026-04-05T12:00:01Z",
            "properties": {
                "duration_ms": 12,
                "artifact_count": 0,
                "status": "success"
            }
        }
    });

    format!("data: {completed}\n\n")
}

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
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
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

    Usage: fabro attach --no-upgrade-check <RUN>

    For more information, try '--help'.
    ");
}

#[test]
fn attach_uses_configured_server_target_without_server_flag() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let list_mock = server.mock(|when, then| {
        when.method("GET").path("/api/v1/runs");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!([
                    {
                        "run_id": run_id,
                        "workflow_name": "Remote Workflow",
                        "workflow_slug": "remote-workflow",
                        "goal": "Remote output",
                        "labels": {},
                        "host_repo_path": null,
                        "start_time": "2026-04-05T12:00:00Z",
                        "status": "running",
                        "status_reason": null,
                        "duration_ms": 12,
                        "total_cost": null
                    }
                ])
                .to_string(),
            );
    });
    server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/events"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [{
                        "seq": 1,
                        "payload": {
                            "event": "run.running",
                            "id": "evt-run-running",
                            "run_id": run_id,
                            "ts": "2026-04-05T12:00:00Z",
                            "properties": {}
                        }
                    }],
                    "meta": { "has_more": false }
                })
                .to_string(),
            );
    });
    server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/state"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(live_run_state_response().to_string());
    });
    server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/questions"))
            .query_param("page[limit]", "100")
            .query_param("page[offset]", "0");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"data":[],"meta":{"has_more":false}}"#);
    });
    let attach_mock = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/attach"))
            .query_param("since_seq", "2");
        then.status(200)
            .header("Content-Type", "text/event-stream")
            .body(run_sse_body(run_id.as_str()));
    });
    context.write_home(
        ".fabro/settings.toml",
        format!("[server]\ntarget = \"{}/api/v1\"\n", server.base_url()),
    );

    let output = context
        .command()
        .args(["--json", "attach", &run_id])
        .output()
        .expect("attach should execute");

    assert!(
        output.status.success(),
        "attach failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    list_mock.assert();
    attach_mock.assert();
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(stdout.contains("\"event\":\"run.completed\""), "{stdout}");
}

#[test]
fn attach_replays_completed_detached_run() {
    let context = test_context!();
    let run_id = unique_run_id();

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--detach",
            "--run-id",
            run_id.as_str(),
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["wait", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();

    let mut cmd = context.command();
    cmd.args(["attach", &run_id]);
    cmd.timeout(SHARED_DAEMON_TIMEOUT);
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
fn attach_replays_from_store_without_run_json_or_progress_jsonl() {
    let context = test_context!();
    let run_id = unique_run_id();

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--detach",
            "--run-id",
            run_id.as_str(),
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["wait", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();

    let run = resolve_run(&context, &run_id);
    let _ = std::fs::remove_file(run.run_dir.join("run.json"));
    let _ = std::fs::remove_file(run.run_dir.join("progress.jsonl"));

    let mut cmd = context.command();
    cmd.args(["attach", &run_id]);
    cmd.timeout(SHARED_DAEMON_TIMEOUT);
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
        std::thread::sleep(Duration::from_secs(1));
        gate.release();
    });
    let mut attach_cmd = context.command();
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
        ✓ wait  [DURATION]
        ✓ exit  [DURATION]
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
    let deadline = std::time::Instant::now() + SHARED_DAEMON_TIMEOUT;
    loop {
        let logs_output = context
            .command()
            .args(["logs", &run_id, "--json"])
            .output()
            .expect("logs should execute");
        assert!(logs_output.status.success(), "logs should succeed");
        let log_events: Vec<Value> = String::from_utf8(logs_output.stdout)
            .expect("stdout should be UTF-8")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("log line should be valid JSON"))
            .collect();
        if log_events.iter().any(|event| {
            event["event"] == "stage.started"
                && event["node_id"] == "approve"
                && event["properties"]["handler_type"] == "human"
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for human gate to start for {run_id}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let output = context
        .command()
        .args(["--json", "attach", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .output()
        .expect("attach should execute");

    assert!(!output.status.success(), "attach --json should fail fast");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("--json is non-interactive"));
    assert!(
        !stderr.contains("Approve?"),
        "attach should not prompt on stderr"
    );
    let logs_output = context
        .command()
        .args(["logs", &run_id, "--json"])
        .output()
        .expect("logs should execute");
    assert!(logs_output.status.success(), "logs should succeed");
    let log_events: Vec<Value> = String::from_utf8(logs_output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("log line should be valid JSON"))
        .collect();
    assert!(
        log_events.iter().any(|event| {
            event["event"] == "stage.started"
                && event["node_id"] == "approve"
                && event["properties"]["handler_type"] == "human"
        }),
        "the run should still be waiting on the human gate"
    );
    assert!(
        !log_events.iter().any(|event| {
            event["node_id"] == "approve"
                && matches!(
                    event["event"].as_str(),
                    Some("stage.completed" | "stage.failed" | "interview.completed")
                )
        }),
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
        "event": "run.created",
        "id": "[EVENT_ID]",
        "properties": {
          "graph": {
            "attrs": {
              "goal": {
                "String": "Wait for approval"
              }
            },
            "edges": [
              {
                "attrs": {},
                "from": "start",
                "to": "approve"
              },
              {
                "attrs": {
                  "label": {
                    "String": "[A] Approve"
                  }
                },
                "from": "approve",
                "to": "ship"
              },
              {
                "attrs": {
                  "label": {
                    "String": "[R] Revise"
                  }
                },
                "from": "approve",
                "to": "revise"
              },
              {
                "attrs": {},
                "from": "ship",
                "to": "exit"
              },
              {
                "attrs": {},
                "from": "revise",
                "to": "exit"
              }
            ],
            "name": "HumanGate",
            "nodes": {
              "approve": {
                "attrs": {
                  "label": {
                    "String": "Approve?"
                  },
                  "shape": {
                    "String": "hexagon"
                  }
                },
                "id": "approve"
              },
              "exit": {
                "attrs": {
                  "label": {
                    "String": "Exit"
                  },
                  "shape": {
                    "String": "Msquare"
                  }
                },
                "id": "exit"
              },
              "revise": {
                "attrs": {
                  "script": {
                    "String": "echo revised"
                  },
                  "shape": {
                    "String": "parallelogram"
                  }
                },
                "id": "revise"
              },
              "ship": {
                "attrs": {
                  "script": {
                    "String": "echo shipped"
                  },
                  "shape": {
                    "String": "parallelogram"
                  }
                },
                "id": "ship"
              },
              "start": {
                "attrs": {
                  "label": {
                    "String": "Start"
                  },
                  "shape": {
                    "String": "Mdiamond"
                  }
                },
                "id": "start"
              }
            }
          },
          "host_repo_path": "[TEMP_DIR]",
          "run_dir": "[RUN_DIR]",
          "settings": {
            "goal": "Wait for approval",
            "llm": {
              "fallbacks": null,
              "model": "gpt-5.4",
              "provider": "openai"
            },
            "no_retro": true,
            "sandbox": {
              "daytona": null,
              "devcontainer": null,
              "env": null,
              "local": null,
              "preserve": null,
              "provider": "local"
            },
            "storage_dir": "[STORAGE_DIR]"
          },
          "workflow_slug": "human-gate",
          "workflow_source": "digraph HumanGate {/n  graph [goal=\"Wait for approval\"]/n  start [shape=Mdiamond, label=\"Start\"]/n  exit  [shape=Msquare, label=\"Exit\"]/n  approve [shape=hexagon, label=\"Approve?\"]/n  ship   [shape=parallelogram, script=\"echo shipped\"]/n  revise [shape=parallelogram, script=\"echo revised\"]/n  start -> approve/n  approve -> ship   [label=\"[A] Approve\"]/n  approve -> revise [label=\"[R] Revise\"]/n  ship -> exit/n  revise -> exit/n}/n",
          "working_directory": "[TEMP_DIR]"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "run.submitted",
        "id": "[EVENT_ID]",
        "properties": {},
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "run.starting",
        "id": "[EVENT_ID]",
        "properties": {
          "reason": "sandbox_initializing"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "sandbox.initializing",
        "id": "[EVENT_ID]",
        "properties": {
          "provider": "local"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "sandbox.ready",
        "id": "[EVENT_ID]",
        "properties": {
          "duration_ms": "[DURATION_MS]",
          "provider": "local"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "sandbox.initialized",
        "id": "[EVENT_ID]",
        "properties": {
          "provider": "local",
          "working_directory": "[TEMP_DIR]"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "run.started",
        "id": "[EVENT_ID]",
        "properties": {
          "goal": "Wait for approval",
          "name": "HumanGate"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "run.running",
        "id": "[EVENT_ID]",
        "properties": {},
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.started",
        "id": "[EVENT_ID]",
        "node_id": "start",
        "node_label": "Start",
        "properties": {
          "attempt": 1,
          "handler_type": "start",
          "index": 0,
          "max_attempts": 1
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.completed",
        "id": "[EVENT_ID]",
        "node_id": "start",
        "node_label": "Start",
        "properties": {
          "attempt": 1,
          "context_values": {
            "current.preamble": "Goal: Wait for approval/n",
            "current_node": "start",
            "graph.goal": "Wait for approval",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.run_id": "[ULID]",
            "internal.thread_id": null
          },
          "duration_ms": "[DURATION_MS]",
          "index": 0,
          "max_attempts": 1,
          "node_visits": {
            "start": 1
          },
          "status": "success"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "from_node": "start",
          "is_jump": false,
          "reason": "unconditional",
          "stage_status": "success",
          "to_node": "approve"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "checkpoint.completed",
        "id": "[EVENT_ID]",
        "node_id": "start",
        "node_label": "start",
        "properties": {
          "completed_nodes": [
            "start"
          ],
          "context_values": {
            "current.preamble": "Goal: Wait for approval/n",
            "current_node": "start",
            "failure_class": "",
            "failure_signature": "",
            "graph.goal": "Wait for approval",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.retry_count.start": 0,
            "internal.run_id": "[ULID]",
            "internal.thread_id": null,
            "outcome": "success"
          },
          "current_node": "start",
          "next_node_id": "approve",
          "node_outcomes": {
            "start": {
              "status": "success",
              "usage": null
            }
          },
          "node_visits": {
            "start": 1
          },
          "status": "success"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.started",
        "id": "[EVENT_ID]",
        "node_id": "approve",
        "node_label": "Approve?",
        "properties": {
          "attempt": 1,
          "handler_type": "human",
          "index": 1,
          "max_attempts": 1
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      }
    ]
    "#);
}
