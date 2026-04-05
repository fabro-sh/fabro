use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

use crate::support::{example_fixture, fabro_json_snapshot, run_output_filters, unique_run_id};

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
    Run:       [STORAGE_DIR]/runs/20260404-[ULID]

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

    context.find_run_dir(&run_id);
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

    let progress: Vec<Value> = String::from_utf8(output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("run JSON output should be JSONL"))
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
          "run_dir": "[RUN_DIR]",
          "settings": {
            "auto_approve": true,
            "goal": "Route through the default approval path",
            "llm": {
              "fallbacks": null,
              "model": "claude-sonnet-4-6",
              "provider": "anthropic"
            },
            "mode": "standalone",
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
            "current.preamble": "Goal: Route through the default approval path/n",
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
            "current.preamble": "Goal: Route through the default approval path/n",
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
      },
      {
        "event": "sandbox.cleanup.started",
        "id": "[EVENT_ID]",
        "properties": {
          "provider": "local"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
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
            "detach_log_exists": run_dir.join("detach.log").exists(),
        }),
        @r#"
    {
      "run_dir": "[RUN_DIR]",
      "launcher_log_exists": false,
      "detach_log_exists": false
    }
    "#
    );
}
