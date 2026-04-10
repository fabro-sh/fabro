use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fabro_test::{apply_filters, fabro_snapshot, test_context};
use serde_json::Value;

use crate::support::{example_fixture, fabro_json_snapshot, run_output_filters, unique_run_id};

use super::support::{
    output_stdout, resolve_run, server_target, wait_for_status, write_gated_workflow,
};

const SHARED_DAEMON_TIMEOUT: Duration = Duration::from_secs(30);

fn server_endpoint(storage_dir: &Path) -> (reqwest::Client, String) {
    let target = server_target(storage_dir);
    if target.starts_with('/') {
        (
            reqwest::ClientBuilder::new()
                .unix_socket(target)
                .no_proxy()
                .build()
                .expect("test Unix-socket HTTP client should build"),
            "http://fabro".to_string(),
        )
    } else {
        (
            reqwest::ClientBuilder::new()
                .no_proxy()
                .build()
                .expect("test TCP HTTP client should build"),
            target,
        )
    }
}

async fn wait_for_server_question(client: &reqwest::Client, base_url: &str, run_id: &str) -> Value {
    let deadline = std::time::Instant::now() + SHARED_DAEMON_TIMEOUT;
    loop {
        let response = client
            .get(format!("{base_url}/api/v1/runs/{run_id}/questions"))
            .query(&[("page[limit]", "100"), ("page[offset]", "0")])
            .send()
            .await
            .expect("question request should succeed");
        assert!(
            response.status().is_success(),
            "question request failed: {}",
            response.status()
        );
        let body: Value = response
            .json()
            .await
            .expect("question response should parse");
        if let Some(question) = body["data"].as_array().and_then(|items| items.first()) {
            return question.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for a pending question"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn format_output_snapshot(output: &Output, filters: &[(String, String)]) -> String {
    let stdout = apply_filters(&String::from_utf8_lossy(&output.stdout), filters);
    let stderr = apply_filters(&String::from_utf8_lossy(&output.stderr), filters);

    format!(
        "success: {success}\nexit_code: {code}\n----- stdout -----\n{stdout}----- stderr -----\n{stderr}",
        success = output.status.success(),
        code = output.status.code().unwrap_or(-1),
        stdout = stdout,
        stderr = stderr,
    )
}

fn wait_for_output_signal(
    child: &mut std::process::Child,
    stdout: &mut impl Read,
    stderr_reader: std::thread::JoinHandle<Vec<u8>>,
    signal_rx: mpsc::Receiver<()>,
    needle: &str,
) -> std::thread::JoinHandle<Vec<u8>> {
    let deadline = Instant::now() + SHARED_DAEMON_TIMEOUT;
    let mut stderr_reader = Some(stderr_reader);

    loop {
        match signal_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(()) => {
                return stderr_reader
                    .take()
                    .expect("stderr reader should still be available");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        if let Some(status) = child.try_wait().expect("attach should stay alive") {
            let mut stdout_bytes = Vec::new();
            stdout
                .read_to_end(&mut stdout_bytes)
                .expect("attach stdout should be readable");
            let stderr_bytes = stderr_reader
                .take()
                .expect("stderr reader should still be available")
                .join()
                .expect("stderr reader should join");
            panic!(
                "attach exited before emitting {needle:?}\nstatus: {status}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes)
            );
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().expect("attach should exit after kill");
            let mut stdout_bytes = Vec::new();
            stdout
                .read_to_end(&mut stdout_bytes)
                .expect("attach stdout should be readable");
            let stderr_bytes = stderr_reader
                .take()
                .expect("stderr reader should still be available")
                .join()
                .expect("stderr reader should join");
            panic!(
                "timed out waiting for attach output {needle:?}\nstatus: {status}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes)
            );
        }
    }
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
    let mut attach_cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    attach_cmd.current_dir(&context.temp_dir);
    attach_cmd.env("NO_COLOR", "1");
    attach_cmd.env("HOME", &context.home_dir);
    attach_cmd.env("FABRO_NO_UPGRADE_CHECK", "true");
    attach_cmd.env("FABRO_SERVER_MAX_CONCURRENT_RUNS", "64");
    attach_cmd.env("FABRO_TEST_IN_MEMORY_STORE", "1");
    attach_cmd.args(["attach", &run_id]);
    attach_cmd.stdout(Stdio::piped());
    attach_cmd.stderr(Stdio::piped());
    let mut child = attach_cmd.spawn().expect("attach should spawn");
    let mut stdout = child.stdout.take().expect("attach stdout should be piped");
    let stderr = child.stderr.take().expect("attach stderr should be piped");
    let (signal_tx, signal_rx) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut stderr_bytes = Vec::new();
        let mut line = Vec::new();

        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .expect("attach stderr should be readable");
            if read == 0 {
                break;
            }
            if line
                .windows("✓ start".len())
                .any(|window| window == "✓ start".as_bytes())
            {
                let _ = signal_tx.send(());
            }
            stderr_bytes.extend_from_slice(&line);
        }

        stderr_bytes
    });
    let stderr_reader =
        wait_for_output_signal(&mut child, &mut stdout, stderr_reader, signal_rx, "✓ start");
    gate.release();
    let status = child.wait().expect("attach should exit");
    let mut stdout_bytes = Vec::new();
    stdout
        .read_to_end(&mut stdout_bytes)
        .expect("attach stdout should be readable");
    let output = Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_reader.join().expect("stderr reader should join"),
    };
    let snapshot = format_output_snapshot(&output, &filters);
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
        .map(|mut event: Value| {
            if let Some(properties) = event.get_mut("properties").and_then(Value::as_object_mut) {
                if properties.contains_key("manifest_blob") {
                    properties.insert(
                        "manifest_blob".to_string(),
                        Value::String("[BLOB_ID]".to_string()),
                    );
                }
                if properties.contains_key("definition_blob") {
                    properties.insert(
                        "definition_blob".to_string(),
                        Value::String("[BLOB_ID]".to_string()),
                    );
                }
            }
            // Strip v2-shape server/version fields that the bridge emits,
            // since the test fixture's socket path is randomised per run.
            if let Some(settings) = event
                .pointer_mut("/properties/settings")
                .and_then(Value::as_object_mut)
            {
                settings.remove("_version");
                settings.remove("server");
                settings.remove("version");
            }
            if let Some(target) = event
                .pointer_mut("/properties/settings/cli/target")
                .and_then(Value::as_object_mut)
            {
                if target.contains_key("path") {
                    target.insert(
                        "path".to_string(),
                        Value::String("[CLI_SOCKET]".to_string()),
                    );
                }
            }
            event
        })
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
          "manifest_blob": "[BLOB_ID]",
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
            "run": {
              "execution": {
                "retros": false
              },
              "goal": "Wait for approval",
              "model": {
                "name": "gpt-5.4",
                "provider": "openai"
              },
              "sandbox": {
                "provider": "local"
              }
            },
            "cli": {
              "target": {
                "path": "[CLI_SOCKET]",
                "type": "unix"
              }
            }
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
        "properties": {
          "definition_blob": "[BLOB_ID]"
        },
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
        "stage_id": "start@1",
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
        "stage_id": "start@1",
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
        "stage_id": "start@1",
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
        "stage_id": "approve@1",
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
        "stage_id": "approve@1",
        "ts": "[TIMESTAMP]"
      }
    ]
    "#);

    let run = resolve_run(&context, &run_id);
    tokio::runtime::Runtime::new()
        .expect("test runtime should build")
        .block_on(async {
            let (client, base_url) = server_endpoint(&context.storage_dir);
            let question = wait_for_server_question(&client, &base_url, &run_id).await;
            let question_id = question["id"]
                .as_str()
                .expect("question id should be present");

            let response = client
                .post(format!(
                    "{base_url}/api/v1/runs/{run_id}/questions/{question_id}/answer"
                ))
                .json(&serde_json::json!({ "selected_option_key": "A" }))
                .send()
                .await
                .expect("answer submission should succeed");
            assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        });
    wait_for_status(&run.run_dir, &["succeeded"]);
}
