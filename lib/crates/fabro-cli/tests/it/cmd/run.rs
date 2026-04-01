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
        "event": "run.created",
        "id": "[EVENT_ID]",
        "properties": {
          "graph": {
            "attrs": {
              "goal": {
                "String": "Run tests and report results"
              },
              "rankdir": {
                "String": "LR"
              }
            },
            "edges": [
              {
                "attrs": {},
                "from": "start",
                "to": "run_tests"
              },
              {
                "attrs": {},
                "from": "run_tests",
                "to": "report"
              },
              {
                "attrs": {},
                "from": "report",
                "to": "exit"
              }
            ],
            "name": "Simple",
            "nodes": {
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
              "report": {
                "attrs": {
                  "label": {
                    "String": "Report"
                  },
                  "prompt": {
                    "String": "Summarize the test results"
                  }
                },
                "id": "report"
              },
              "run_tests": {
                "attrs": {
                  "label": {
                    "String": "Run Tests"
                  },
                  "prompt": {
                    "String": "Run the test suite and report results"
                  }
                },
                "id": "run_tests"
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
          "labels": {},
          "run_dir": "[STORAGE_DIR]/runs/REDACTED",
          "settings": {
            "auto_approve": true,
            "dry_run": true,
            "fabro": {
              "root": "fabro/"
            },
            "features": {
              "retros": false,
              "session_sandboxes": false
            },
            "goal": "Run tests and report results",
            "hooks": [
              {
                "blocking": true,
                "command": "cargo fmt",
                "event": "post_tool_use",
                "matcher": "write_file|edit_file|apply_patch",
                "name": "cargo-fmt",
                "sandbox": null,
                "timeout_ms": null
              }
            ],
            "llm": {
              "fallbacks": null,
              "model": "claude-sonnet-4-6",
              "provider": "anthropic"
            },
            "mode": "standalone",
            "pull_request": {
              "auto_merge": false,
              "draft": false,
              "enabled": true,
              "merge_strategy": "squash"
            },
            "sandbox": {
              "daytona": {
                "auto_stop_interval": 30,
                "labels": {
                  "repo": "fabro-sh/fabro"
                },
                "network": null,
                "skip_clone": false,
                "snapshot": {
                  "cpu": 4,
                  "disk": 20,
                  "dockerfile": "FROM ubuntu:24.04/n/nRUN apt-get update && apt-get install -y --no-install-recommends curl git ca-certificates build-essential pkg-config libssl-dev unzip python3 && rm -rf /var/lib/apt/lists/*/n/n# GitHub CLI/nRUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg && echo \"deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main\" | tee /etc/apt/sources.list.d/github-cli.list > /dev/null && apt-get update && apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/*/n/n# Rust/nRUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y/nENV PATH=\"/root/.cargo/bin:${PATH}\"/nRUN cargo install cargo-nextest --locked/nENV CARGO_INCREMENTAL=0/n/n# Bun/nRUN curl -fsSL https://bun.sh/install | bash/nENV PATH=\"/root/.bun/bin:${PATH}\"/n/nWORKDIR /root/n",
                  "memory": 8,
                  "name": "fabro-v6"
                }
              },
              "devcontainer": null,
              "env": null,
              "local": null,
              "preserve": null,
              "provider": "local"
            },
            "storage_dir": "[STORAGE_DIR]",
            "version": 1
          },
          "workflow_slug": "simple",
          "workflow_source": "digraph Simple {/n    graph [goal=\"Run tests and report results\"]/n    rankdir=LR/n/n    start [shape=Mdiamond, label=\"Start\"]/n    exit  [shape=Msquare, label=\"Exit\"]/n/n    run_tests [label=\"Run Tests\", prompt=\"Run the test suite and report results\"]/n    report    [label=\"Report\", prompt=\"Summarize the test results\"]/n/n    start -> run_tests -> report -> exit/n}/n",
          "working_directory": "[TEMP_DIR]"
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
          "cpu": null,
          "duration_ms": "[DURATION_MS]",
          "memory": null,
          "name": null,
          "provider": "local",
          "url": null
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
          "goal": "Run tests and report results",
          "name": "Simple"
        },
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
            "current.preamble": "Goal: Run tests and report results/n",
            "current_node": "start",
            "graph.goal": "Run tests and report results",
            "graph.rankdir": "LR",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.run_id": "[ULID]",
            "internal.thread_id": null
          },
          "duration_ms": "[DURATION_MS]",
          "files_touched": [],
          "index": 0,
          "max_attempts": 1,
          "node_visits": {
            "start": 1
          },
          "notes": "[Simulated] start",
          "preferred_label": null,
          "status": "success",
          "suggested_next_ids": [],
          "usage": null
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "condition": null,
          "from_node": "start",
          "is_jump": false,
          "label": null,
          "reason": "unconditional",
          "stage_status": "success",
          "to_node": "run_tests"
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
          "status": "success"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.started",
        "id": "[EVENT_ID]",
        "node_id": "run_tests",
        "node_label": "Run Tests",
        "properties": {
          "attempt": 1,
          "handler_type": "agent",
          "index": 1,
          "max_attempts": 1
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.completed",
        "id": "[EVENT_ID]",
        "node_id": "run_tests",
        "node_label": "Run Tests",
        "properties": {
          "attempt": 1,
          "context_updates": {
            "last_response": "[Simulated] Response for stage: run_tests",
            "last_stage": "run_tests",
            "response.run_tests": "[Simulated] Response for stage: run_tests"
          },
          "context_values": {
            "current.preamble": "Goal: Run tests and report results/n",
            "current_node": "run_tests",
            "failure_class": "",
            "failure_signature": "",
            "graph.goal": "Run tests and report results",
            "graph.rankdir": "LR",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.retry_count.start": 0,
            "internal.run_id": "[ULID]",
            "internal.thread_id": "start",
            "outcome": "success",
            "thread.start.current_node": "run_tests"
          },
          "duration_ms": "[DURATION_MS]",
          "files_touched": [],
          "index": 1,
          "max_attempts": 1,
          "node_visits": {
            "run_tests": 1,
            "start": 1
          },
          "notes": "[Simulated] run_tests",
          "preferred_label": null,
          "status": "success",
          "suggested_next_ids": [],
          "usage": null
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "condition": null,
          "from_node": "run_tests",
          "is_jump": false,
          "label": null,
          "reason": "unconditional",
          "stage_status": "success",
          "to_node": "report"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "checkpoint.completed",
        "id": "[EVENT_ID]",
        "node_id": "run_tests",
        "node_label": "run_tests",
        "properties": {
          "status": "success"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.started",
        "id": "[EVENT_ID]",
        "node_id": "report",
        "node_label": "Report",
        "properties": {
          "attempt": 1,
          "handler_type": "agent",
          "index": 2,
          "max_attempts": 1
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "stage.completed",
        "id": "[EVENT_ID]",
        "node_id": "report",
        "node_label": "Report",
        "properties": {
          "attempt": 1,
          "context_updates": {
            "last_response": "[Simulated] Response for stage: report",
            "last_stage": "report",
            "response.report": "[Simulated] Response for stage: report"
          },
          "context_values": {
            "current.preamble": "Goal: Run tests and report results/n/n## Completed stages/n- **run_tests**: success/n",
            "current_node": "report",
            "failure_class": "",
            "failure_signature": "",
            "graph.goal": "Run tests and report results",
            "graph.rankdir": "LR",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.retry_count.run_tests": 0,
            "internal.retry_count.start": 0,
            "internal.run_id": "[ULID]",
            "internal.thread_id": "run_tests",
            "last_response": "[Simulated] Response for stage: run_tests",
            "last_stage": "run_tests",
            "outcome": "success",
            "response.run_tests": "[Simulated] Response for stage: run_tests",
            "thread.run_tests.current_node": "report",
            "thread.start.current_node": "run_tests"
          },
          "duration_ms": "[DURATION_MS]",
          "files_touched": [],
          "index": 2,
          "max_attempts": 1,
          "node_visits": {
            "report": 1,
            "run_tests": 1,
            "start": 1
          },
          "notes": "[Simulated] report",
          "preferred_label": null,
          "status": "success",
          "suggested_next_ids": [],
          "usage": null
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "condition": null,
          "from_node": "report",
          "is_jump": false,
          "label": null,
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
        "node_id": "report",
        "node_label": "report",
        "properties": {
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
          "files_touched": [],
          "index": 3,
          "max_attempts": 1,
          "notes": null,
          "preferred_label": null,
          "status": "success",
          "suggested_next_ids": [],
          "usage": null
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

    let live_path = run_dir.join("live.json");
    let live_content = read_json(&live_path);
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
          "labels": {},
          "run_dir": "[STORAGE_DIR]/runs/20260401-[ULID]",
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
          "cpu": null,
          "duration_ms": "[DURATION_MS]",
          "memory": null,
          "name": null,
          "provider": "local",
          "url": null
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
          "files_touched": [],
          "index": 0,
          "max_attempts": 1,
          "node_visits": {
            "start": 1
          },
          "notes": null,
          "preferred_label": null,
          "status": "success",
          "suggested_next_ids": [],
          "usage": null
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "condition": null,
          "from_node": "start",
          "is_jump": false,
          "label": null,
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
          "files_touched": [],
          "index": 1,
          "max_attempts": 1,
          "node_visits": {
            "approve": 1,
            "start": 1
          },
          "notes": null,
          "preferred_label": "[A] Approve",
          "status": "success",
          "suggested_next_ids": [
            "ship"
          ],
          "usage": null
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "condition": null,
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
          "files_touched": [],
          "index": 2,
          "max_attempts": 1,
          "node_visits": {
            "approve": 1,
            "ship": 1,
            "start": 1
          },
          "notes": "Script completed: echo shipped",
          "preferred_label": null,
          "status": "success",
          "suggested_next_ids": [],
          "usage": null
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "condition": null,
          "from_node": "ship",
          "is_jump": false,
          "label": null,
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
          "files_touched": [],
          "index": 3,
          "max_attempts": 1,
          "notes": null,
          "preferred_label": null,
          "status": "success",
          "suggested_next_ids": [],
          "usage": null
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
