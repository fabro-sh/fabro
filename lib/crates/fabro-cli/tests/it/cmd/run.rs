use fabro_test::{fabro_snapshot, test_context};
use fabro_types::StatusReason;
use httpmock::MockServer;
use serde_json::Value;

use super::support::{
    output_stderr, resolve_run, run_state, wait_for_event_names, wait_for_no_process_match,
    wait_for_status, write_gated_workflow,
};
use crate::support::{example_fixture, fabro_json_snapshot, run_output_filters, unique_run_id};

fn run_status_response(run_id: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "id": run_id,
        "status": status,
        "created_at": "2026-04-05T12:00:00Z"
    })
}

fn preflight_response() -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "workflow": {
            "name": "Simple",
            "graph_path": null,
            "nodes": 4,
            "edges": 3,
            "goal": "Run tests and report results",
            "diagnostics": []
        },
        "checks": {
            "title": "Preflight",
            "sections": []
        }
    })
}

fn remote_run_state_response() -> serde_json::Value {
    serde_json::json!({
        "run": null,
        "graph_source": null,
        "start": null,
        "status": null,
        "checkpoint": {
            "timestamp": "2026-04-05T12:00:01Z",
            "current_node": "exit",
            "completed_nodes": ["report"],
            "node_retries": {},
            "context_values": {
                "response.report": "Remote output"
            },
            "node_outcomes": {},
            "next_node_id": null,
            "git_commit_sha": null,
            "loop_failure_signatures": {},
            "restart_failure_signatures": {},
            "node_visits": {}
        },
        "checkpoints": [],
        "conclusion": {
            "timestamp": "2026-04-05T12:00:01Z",
            "status": "success",
            "duration_ms": 12,
            "stages": [],
            "billing": null,
            "total_retries": 0,
        },
        "retro": null,
        "retro_prompt": null,
        "retro_response": null,
        "sandbox": null,
        "final_patch": null,
        "pull_request": null,
        "nodes": {}
    })
}

fn run_completed_event(run_id: &str) -> serde_json::Value {
    serde_json::json!({
        "seq": 1,
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
    })
}

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
          --json                   Output as JSON [env: FABRO_JSON=]
          --server <SERVER>        Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug                  Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --dry-run                Execute with simulated LLM backend
          --auto-approve           Auto-approve all human gates
          --no-upgrade-check       Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --goal <GOAL>            Override the workflow goal (exposed as $goal in prompts)
          --quiet                  Suppress non-essential output [env: FABRO_QUIET=]
          --goal-file <GOAL_FILE>  Read the workflow goal from a file
          --model <MODEL>          Override default LLM model
          --provider <PROVIDER>    Override default LLM provider
      -v, --verbose                Enable verbose output
          --sandbox <SANDBOX>      Sandbox for agent tools [possible values: local, docker, daytona]
          --label <KEY=VALUE>      Attach a label to this run (repeatable, format: KEY=VALUE)
          --no-retro               Skip retro generation after the run
          --preserve-sandbox       Keep the sandbox alive after the run finishes (for debugging)
      -d, --detach                 Run the workflow in the background and print the run ID
      -h, --help                   Print help
    ----- stderr -----
    ");
}

#[test]
fn detach_uses_explicit_server_target_and_prints_remote_run_id() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let create_mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });
    let start_mock = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/start"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "queued").to_string());
    });

    let output = context
        .run_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--detach",
            "--dry-run",
            "--auto-approve",
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    create_mock.assert();
    start_mock.assert();
    assert_eq!(output_stderr(&output), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        run_id.as_str()
    );
}

#[test]
fn detach_uses_configured_server_target_without_server_flag() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let create_mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });
    let start_mock = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/start"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "queued").to_string());
    });
    context.write_home(
        ".fabro/settings.toml",
        format!("[server]\ntarget = \"{}/api/v1\"\n", server.base_url()),
    );

    let output = context
        .run_cmd()
        .args([
            "--detach",
            "--dry-run",
            "--auto-approve",
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    create_mock.assert();
    start_mock.assert();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        run_id.as_str()
    );
}

#[test]
fn detach_rejects_storage_dir_flag() {
    let context = test_context!();
    let output = context
        .run_cmd()
        .args([
            "--storage-dir",
            "/tmp/fabro-run",
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        !output.status.success(),
        "command should reject --storage-dir"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument '--storage-dir'"));
}

#[test]
fn detach_cli_server_target_overrides_configured_server_target() {
    let context = test_context!();
    let config_server = MockServer::start();
    let config_create = config_server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(500)
            .body("configured-server-should-not-be-used");
    });
    let config_start = config_server.mock(|when, then| {
        when.method("POST").path_includes("/api/v1/runs/");
        then.status(500)
            .body("configured-server-should-not-be-used");
    });
    let cli_server = MockServer::start();
    let run_id = unique_run_id();
    let cli_create = cli_server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });
    let cli_start = cli_server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/start"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "queued").to_string());
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            "[server]\ntarget = \"{}/api/v1\"\n",
            config_server.base_url()
        ),
    );

    let output = context
        .run_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", cli_server.base_url()),
            "--detach",
            "--dry-run",
            "--auto-approve",
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    cli_create.assert();
    cli_start.assert();
    config_create.assert_calls(0);
    config_start.assert_calls(0);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        run_id.as_str()
    );
}

#[test]
fn remote_foreground_run_prints_server_backed_summary_without_local_run_dir() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    server.mock(|when, then| {
        when.method("POST").path("/api/v1/preflight");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(preflight_response().to_string());
    });
    server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });
    server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/start"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "queued").to_string());
    });
    server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/events"))
            .query_param_missing("since_seq");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [run_completed_event(run_id.as_str())],
                    "meta": { "has_more": false }
                })
                .to_string(),
            );
    });
    server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/events"))
            .query_param("since_seq", "2");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [],
                    "meta": { "has_more": false }
                })
                .to_string(),
            );
    });
    server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/questions"))
            .query_param("page[limit]", "100")
            .query_param("page[offset]", "0");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [],
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
            .body(remote_run_state_response().to_string());
    });

    let output = context
        .run_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--dry-run",
            "--auto-approve",
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = output_stderr(&output);
    assert!(stderr.contains("=== Run Result ==="), "{stderr}");
    assert!(stderr.contains("Remote output"), "{stderr}");
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.trim_start().starts_with("Run:"))
            .count(),
        1,
        "{stderr}"
    );
    assert!(!stderr.contains("=== Artifacts ==="), "{stderr}");
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
    Run:       [RUN_DIR]

    === Output ===
    [Simulated] Response for stage: report
    ");
}

#[test]
fn dry_run_persists_event_history_in_store() {
    let context = test_context!();
    let run_id = unique_run_id();

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--sandbox",
            "local",
            "--run-id",
            run_id.as_str(),
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    wait_for_event_names(&run_dir, &["run.completed", "sandbox.cleanup.completed"]);
    let output = context
        .command()
        .args(["logs", &run_id])
        .output()
        .expect("logs command should execute");
    assert!(
        output.status.success(),
        "logs failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let progress: Vec<Value> = String::from_utf8(output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("logs output should be JSONL"))
        .collect();
    assert!(
        !progress.is_empty(),
        "store-backed event history should have at least one line"
    );
    assert_eq!(
        progress.first().and_then(|event| event["event"].as_str()),
        Some("run.created")
    );
    assert_eq!(
        progress
            .first()
            .and_then(|event| event.pointer("/properties/settings/auto_approve"))
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        progress
            .iter()
            .any(|event| event["event"].as_str() == Some("run.completed")),
        "store-backed event history should include run.completed"
    );
    assert_eq!(
        progress.last().and_then(|event| event["event"].as_str()),
        Some("sandbox.cleanup.completed")
    );

    let tail_output = context
        .command()
        .args(["logs", "--tail", "1", &run_id])
        .output()
        .expect("tail logs command should execute");
    assert!(
        tail_output.status.success(),
        "tail logs failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&tail_output.stdout),
        String::from_utf8_lossy(&tail_output.stderr)
    );
    let live_content: Value = String::from_utf8(tail_output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("tail logs output should be JSON"))
        .expect("tail logs should include the latest event");
    fabro_json_snapshot!(context, &live_content, @r#"
    {
      "event": "sandbox.cleanup.completed",
      "id": "[EVENT_ID]",
      "properties": {
        "duration_ms": "[DURATION_MS]",
        "provider": "local"
      },
      "run_id": "[ULID]",
      "ts": "[TIMESTAMP]"
    }
    "#);

    assert_eq!(live_content, *progress.last().unwrap());
}

#[test]
fn run_id_passthrough_uses_provided_ulid() {
    let context = test_context!();
    let run_id = unique_run_id();

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    context.find_run_dir(&run_id);
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

    let mut progress: Vec<Value> = String::from_utf8(output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("run JSON output should be JSONL"))
        .collect();
    for event in &mut progress {
        let Some(llm) = event.pointer_mut("/properties/settings/llm") else {
            continue;
        };
        let Some(llm) = llm.as_object_mut() else {
            continue;
        };
        llm.insert(
            "model".to_string(),
            Value::String("[LLM_MODEL]".to_string()),
        );
        llm.insert(
            "provider".to_string(),
            Value::String("[LLM_PROVIDER]".to_string()),
        );
    }
    fabro_json_snapshot!(context, &progress, @r#"
    [
      {
        "event": "run.created",
        "id": "[EVENT_ID]",
        "properties": {
          "artifact_storage": "object_store_v1",
          "graph": {
            "attrs": {
              "goal": {
                "String": "Route through the default approval path"
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
          "provenance": {
            "client": {
              "name": "fabro-cli",
              "user_agent": "fabro-cli/0.176.2",
              "version": "0.176.2"
            },
            "server": {
              "version": "0.176.2"
            },
            "subject": {
              "auth_method": "disabled"
            }
          },
          "run_dir": "[RUN_DIR]",
          "settings": {
            "auto_approve": true,
            "goal": "Route through the default approval path",
            "llm": {
              "fallbacks": null,
              "model": "[LLM_MODEL]",
              "provider": "[LLM_PROVIDER]"
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
          "workflow_source": "digraph HumanGate {/n  graph [goal=\"Route through the default approval path\"]/n  start [shape=Mdiamond, label=\"Start\"]/n  exit  [shape=Msquare, label=\"Exit\"]/n  approve [shape=hexagon, label=\"Approve?\"]/n  ship   [shape=parallelogram, script=\"echo shipped\"]/n  revise [shape=parallelogram, script=\"echo revised\"]/n  start -> approve/n  approve -> ship   [label=\"[A] Approve\"]/n  approve -> revise [label=\"[R] Revise\"]/n  ship -> exit/n  revise -> exit/n}/n",
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
          "goal": "Route through the default approval path",
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
            "current.preamble": "Goal: Route through the default approval path/n",
            "current_node": "start",
            "graph.goal": "Route through the default approval path",
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
            "current_node": "start",
            "failure_class": "",
            "failure_signature": "",
            "graph.goal": "Route through the default approval path",
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
      },
      {
        "event": "interview.started",
        "id": "[EVENT_ID]",
        "node_id": "approve",
        "node_label": "approve",
        "properties": {
          "allow_freeform": false,
          "options": [
            {
              "key": "A",
              "label": "[A] Approve"
            },
            {
              "key": "R",
              "label": "[R] Revise"
            }
          ],
          "question": "Approve?",
          "question_id": "[ULID]",
          "question_type": "multiple_choice",
          "stage": "approve"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "interview.completed",
        "id": "[EVENT_ID]",
        "properties": {
          "answer": "A",
          "duration_ms": "[DURATION_MS]",
          "question": "Approve?",
          "question_id": "[ULID]"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.completed",
        "id": "[EVENT_ID]",
        "node_id": "approve",
        "node_label": "Approve?",
        "properties": {
          "attempt": 1,
          "context_updates": {
            "human.gate.label": "[A] Approve",
            "human.gate.selected": "A"
          },
          "context_values": {
            "current.preamble": "Goal: Route through the default approval path/n",
            "current_node": "approve",
            "failure_class": "",
            "failure_signature": "",
            "graph.goal": "Route through the default approval path",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.retry_count.start": 0,
            "internal.run_id": "[ULID]",
            "internal.thread_id": "start",
            "outcome": "success",
            "thread.start.current_node": "approve"
          },
          "duration_ms": "[DURATION_MS]",
          "index": 1,
          "max_attempts": 1,
          "node_visits": {
            "approve": 1,
            "start": 1
          },
          "preferred_label": "[A] Approve",
          "status": "success",
          "suggested_next_ids": [
            "ship"
          ]
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "from_node": "approve",
          "is_jump": false,
          "label": "[A] Approve",
          "preferred_label": "[A] Approve",
          "reason": "preferred_label",
          "stage_status": "success",
          "suggested_next_ids": [
            "ship"
          ],
          "to_node": "ship"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "checkpoint.completed",
        "id": "[EVENT_ID]",
        "node_id": "approve",
        "node_label": "approve",
        "properties": {
          "completed_nodes": [
            "start",
            "approve"
          ],
          "context_values": {
            "current_node": "approve",
            "failure_class": "",
            "failure_signature": "",
            "graph.goal": "Route through the default approval path",
            "human.gate.label": "[A] Approve",
            "human.gate.selected": "A",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.retry_count.approve": 0,
            "internal.retry_count.start": 0,
            "internal.run_id": "[ULID]",
            "internal.thread_id": "start",
            "outcome": "success",
            "preferred_label": "[A] Approve",
            "thread.start.current_node": "approve"
          },
          "current_node": "approve",
          "next_node_id": "ship",
          "node_outcomes": {
            "approve": {
              "context_updates": {
                "human.gate.label": "[A] Approve",
                "human.gate.selected": "A"
              },
              "preferred_label": "[A] Approve",
              "status": "success",
              "suggested_next_ids": [
                "ship"
              ],
              "usage": null
            },
            "start": {
              "status": "success",
              "usage": null
            }
          },
          "node_visits": {
            "approve": 1,
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
        "node_id": "ship",
        "node_label": "ship",
        "properties": {
          "attempt": 1,
          "handler_type": "command",
          "index": 2,
          "max_attempts": 1
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "command.started",
        "id": "[EVENT_ID]",
        "node_id": "ship",
        "node_label": "ship",
        "properties": {
          "command": "echo shipped",
          "language": "shell",
          "script": "echo shipped"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "command.completed",
        "id": "[EVENT_ID]",
        "node_id": "ship",
        "node_label": "ship",
        "properties": {
          "duration_ms": "[DURATION_MS]",
          "exit_code": 0,
          "stderr": "",
          "stdout": "shipped/n",
          "timed_out": false
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.completed",
        "id": "[EVENT_ID]",
        "node_id": "ship",
        "node_label": "ship",
        "properties": {
          "attempt": 1,
          "context_updates": {
            "command.output": "shipped/n",
            "command.stderr": ""
          },
          "context_values": {
            "current.preamble": "Goal: Route through the default approval path/n/n## Completed stages/n- **approve**: success/n/n## Context/n- human.gate.label: [A] Approve/n- human.gate.selected: A/n",
            "current_node": "ship",
            "failure_class": "",
            "failure_signature": "",
            "graph.goal": "Route through the default approval path",
            "human.gate.label": "[A] Approve",
            "human.gate.selected": "A",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.retry_count.approve": 0,
            "internal.retry_count.start": 0,
            "internal.run_id": "[ULID]",
            "internal.thread_id": "approve",
            "outcome": "success",
            "preferred_label": "[A] Approve",
            "thread.approve.current_node": "ship",
            "thread.start.current_node": "approve"
          },
          "duration_ms": "[DURATION_MS]",
          "index": 2,
          "max_attempts": 1,
          "node_visits": {
            "approve": 1,
            "ship": 1,
            "start": 1
          },
          "notes": "Script completed: echo shipped",
          "status": "success"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "from_node": "ship",
          "is_jump": false,
          "reason": "unconditional",
          "stage_status": "success",
          "to_node": "exit"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "checkpoint.completed",
        "id": "[EVENT_ID]",
        "node_id": "ship",
        "node_label": "ship",
        "properties": {
          "completed_nodes": [
            "start",
            "approve",
            "ship"
          ],
          "context_values": {
            "command.output": "shipped/n",
            "command.stderr": "",
            "current_node": "ship",
            "failure_class": "",
            "failure_signature": "",
            "graph.goal": "Route through the default approval path",
            "human.gate.label": "[A] Approve",
            "human.gate.selected": "A",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.retry_count.approve": 0,
            "internal.retry_count.ship": 0,
            "internal.retry_count.start": 0,
            "internal.run_id": "[ULID]",
            "internal.thread_id": "approve",
            "outcome": "success",
            "preferred_label": "[A] Approve",
            "thread.approve.current_node": "ship",
            "thread.start.current_node": "approve"
          },
          "current_node": "ship",
          "next_node_id": "exit",
          "node_outcomes": {
            "approve": {
              "context_updates": {
                "human.gate.label": "[A] Approve",
                "human.gate.selected": "A"
              },
              "preferred_label": "[A] Approve",
              "status": "success",
              "suggested_next_ids": [
                "ship"
              ],
              "usage": null
            },
            "ship": {
              "context_updates": {
                "command.output": "shipped/n",
                "command.stderr": ""
              },
              "notes": "Script completed: echo shipped",
              "status": "success",
              "usage": null
            },
            "start": {
              "status": "success",
              "usage": null
            }
          },
          "node_visits": {
            "approve": 1,
            "ship": 1,
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
        "node_id": "exit",
        "node_label": "Exit",
        "properties": {
          "attempt": 1,
          "handler_type": "exit",
          "index": 3,
          "max_attempts": 1
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.completed",
        "id": "[EVENT_ID]",
        "node_id": "exit",
        "node_label": "Exit",
        "properties": {
          "attempt": 1,
          "duration_ms": "[DURATION_MS]",
          "index": 3,
          "max_attempts": 1,
          "status": "success"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "run.completed",
        "id": "[EVENT_ID]",
        "properties": {
          "artifact_count": 0,
          "duration_ms": "[DURATION_MS]",
          "reason": "completed",
          "status": "success"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      }
    ]
    "#);

    assert_eq!(
        progress[0].pointer("/properties/settings/auto_approve"),
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
    let run_id = unique_run_id();

    context
        .run_cmd()
        .args([
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            example_fixture("simple.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "run_dir": run_dir,
            "launcher_log_exists": context.storage_dir.join("launchers").join(format!("{run_id}.log")).exists(),
        }),
        @r#"
    {
      "run_dir": "[RUN_DIR]",
      "launcher_log_exists": false
    }
    "#
    );
}

#[test]
fn ctrl_c_cancels_active_run_via_server() {
    let context = test_context!();
    let gate = write_gated_workflow(&context.temp_dir.join("slow.fabro"), "slow", "Run slowly");
    let run_id = unique_run_id();

    let mut run_cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    run_cmd.current_dir(&context.temp_dir);
    run_cmd.env("NO_COLOR", "1");
    run_cmd.env("HOME", &context.home_dir);
    run_cmd.env("FABRO_NO_UPGRADE_CHECK", "true");
    run_cmd.env("FABRO_TEST_IN_MEMORY_STORE", "1");
    run_cmd.env("FABRO_STORAGE_DIR", &context.storage_dir);
    run_cmd.env("FABRO_SERVER_MAX_CONCURRENT_RUNS", "64");
    run_cmd.env("OPENAI_API_KEY", "test");
    run_cmd.args([
        "run",
        "--run-id",
        run_id.as_str(),
        "--label",
        &context.test_run_label(),
        "--label",
        &context.test_case_label(),
        "--provider",
        "openai",
        "--sandbox",
        "local",
        "--no-retro",
        "slow.fabro",
    ]);
    let child = run_cmd.spawn().expect("run should spawn");

    let run = resolve_run(&context, &run_id);
    wait_for_status(&run.run_dir, &["running"]);

    let kill_status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill should execute");
    assert!(kill_status.success(), "kill -INT should succeed");

    let output = child
        .wait_with_output()
        .expect("run should exit after SIGINT");
    assert!(
        !output.status.success(),
        "run should exit non-zero after cancellation\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        output_stderr(&output)
    );

    let final_status = wait_for_status(&run.run_dir, &["failed"]);
    assert_eq!(final_status, "failed");
    assert_eq!(
        run_state(&run.run_dir)
            .status
            .and_then(|record| record.reason),
        Some(StatusReason::Cancelled)
    );
    let gate_pattern = gate.gate_path().to_string_lossy().into_owned();
    wait_for_no_process_match(&gate_pattern);
}
