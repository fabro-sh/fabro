use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use insta::assert_snapshot;
use serde_json::json;

use super::support::{
    compact_git_inspect, compact_inspect, run_success, setup_seeded_completed_dry_run,
    setup_seeded_created_dry_run, setup_seeded_git_backed_changed_run,
};
use crate::support::unique_run_id;

fn remote_run_summary(run_id: &str, status: &serde_json::Value) -> serde_json::Value {
    json!({
        "run_id": run_id,
        "workflow_name": "Nightly Build",
        "workflow_slug": "nightly-build",
        "goal": "Inspect remote state",
        "title": "Inspect remote state",
        "labels": {},
        "source_directory": "/srv/repo",
        "repository": { "name": "repo" },
        "start_time": "2026-04-19T12:00:00Z",
        "created_at": "2026-04-19T12:00:00Z",
        "status": status,
        "pending_control": null,
        "duration_ms": null,
        "elapsed_secs": null,
        "total_usd_micros": null
    })
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["inspect", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Show detailed information about a workflow run

    Usage: fabro inspect [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name (most recent run)

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
fn inspect_resolves_selector_via_server_endpoint() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let summary = remote_run_summary(
        run_id.as_str(),
        &json!({
            "kind": "succeeded",
            "reason": "completed"
        }),
    );

    let resolve_run = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", "nightly-build");
        then.status(200)
            .header("content-type", "application/json")
            .body(summary.to_string());
    });
    let run_state = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{}/state", run_id.as_str()));
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"stages": {}}"#);
    });

    let mut cmd = context.command();
    cmd.args([
        "inspect",
        "--server",
        &format!("{}/api/v1", server.base_url()),
        "nightly-build",
    ]);

    fabro_snapshot!(context.filters(), cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    [
      {
        "run_id": "[ULID]",
        "status": {
          "kind": "succeeded",
          "reason": "completed"
        },
        "run_spec": null,
        "start_record": null,
        "conclusion": null,
        "checkpoint": null,
        "sandbox": null
      }
    ]
    ----- stderr -----
    "###);

    resolve_run.assert();
    run_state.assert();
}

#[test]
fn inspect_created_run_shows_run_spec_without_start_or_conclusion() {
    let context = test_context!();
    let run = setup_seeded_created_dry_run(&context);
    let output = run_success(&context, &["inspect", &run.run_id]);

    assert_snapshot!(serde_json::to_string_pretty(&compact_inspect(&output)).unwrap(), @r#"
    [
      {
        "run_id": "[ULID]",
        "status": {
          "kind": "submitted"
        },
        "run_spec": {
          "goal": {
            "type": "inline",
            "value": "Run tests and report results"
          },
          "workflow_name": "Simple",
          "workflow_slug": "simple",
          "sandbox_provider": "local",
          "dry_run": true,
          "provenance": {
            "server_version": "[VERSION]",
            "client_name": "fabro-cli",
            "client_version": "[VERSION]",
            "subject_auth_method": "dev_token"
          }
        },
        "start_record": null,
        "conclusion": null,
        "checkpoint": null,
        "sandbox": null
      }
    ]
    "#);
}

#[test]
fn inspect_completed_run_shows_run_start_conclusion_checkpoint() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let output = run_success(&context, &["inspect", &run.run_id]);

    assert_snapshot!(serde_json::to_string_pretty(&compact_inspect(&output)).unwrap(), @r#"
    [
      {
        "run_id": "[ULID]",
        "status": {
          "kind": "succeeded",
          "reason": "completed"
        },
        "run_spec": {
          "goal": {
            "type": "inline",
            "value": "Run tests and report results"
          },
          "workflow_name": "Simple",
          "workflow_slug": "simple",
          "sandbox_provider": "local",
          "dry_run": true,
          "provenance": {
            "server_version": "[VERSION]",
            "client_name": "fabro-cli",
            "client_version": "[VERSION]",
            "subject_auth_method": "dev_token"
          }
        },
        "start_record": {
          "has_start_time": true
        },
        "conclusion": {
          "status": "succeeded",
          "duration_ms": "[DURATION_MS]",
          "stage_count": null
        },
        "checkpoint": {
          "current_node": "report",
          "completed_nodes": [
            "start",
            "run_tests",
            "report"
          ],
          "next_node_id": "exit"
        },
        "sandbox": {
          "provider": "local"
        }
      }
    ]
    "#);
}

#[test]
fn inspect_json_omits_run_dir() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let output = run_success(&context, &["inspect", &run.run_id]);
    let items: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("inspect output should parse");
    let first = items
        .as_array()
        .and_then(|items| items.first())
        .expect("inspect output should contain one item");
    assert!(
        first.get("run_dir").is_none(),
        "inspect JSON should not expose run_dir"
    );
}

#[test]
fn inspect_completed_run_reads_store_without_disk_metadata_files() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let output = run_success(&context, &["inspect", &run.run_id]);

    assert_snapshot!(serde_json::to_string_pretty(&compact_inspect(&output)).unwrap(), @r#"
    [
      {
        "run_id": "[ULID]",
        "status": {
          "kind": "succeeded",
          "reason": "completed"
        },
        "run_spec": {
          "goal": {
            "type": "inline",
            "value": "Run tests and report results"
          },
          "workflow_name": "Simple",
          "workflow_slug": "simple",
          "sandbox_provider": "local",
          "dry_run": true,
          "provenance": {
            "server_version": "[VERSION]",
            "client_name": "fabro-cli",
            "client_version": "[VERSION]",
            "subject_auth_method": "dev_token"
          }
        },
        "start_record": {
          "has_start_time": true
        },
        "conclusion": {
          "status": "succeeded",
          "duration_ms": "[DURATION_MS]",
          "stage_count": null
        },
        "checkpoint": {
          "current_node": "report",
          "completed_nodes": [
            "start",
            "run_tests",
            "report"
          ],
          "next_node_id": "exit"
        },
        "sandbox": {
          "provider": "local"
        }
      }
    ]
    "#);
}

#[test]
fn inspect_git_backed_run_exposes_checkpoint_and_sandbox_state() {
    let context = test_context!();
    let setup = setup_seeded_git_backed_changed_run(&context);
    let output = run_success(&context, &["inspect", &setup.run.run_id]);

    assert_snapshot!(
        serde_json::to_string_pretty(&compact_git_inspect(&output)).unwrap(),
        @r#"
    [
      {
        "run_id": "[ULID]",
        "status": {
          "kind": "succeeded",
          "reason": "completed"
        },
        "run_spec": {
          "goal": {
            "type": "inline",
            "value": "Edit a tracked file"
          },
          "workflow_name": "Flow",
          "workflow_slug": "flow",
          "llm_provider": "openai",
          "sandbox_provider": "local",
          "provenance": {
            "server_version": "[VERSION]",
            "client_name": "fabro-cli",
            "client_version": "[VERSION]",
            "subject_auth_method": "dev_token"
          }
        },
        "start_record": {
          "has_start_time": true,
          "run_branch": "fabro/run/[ULID]",
          "base_sha": "[SHA]"
        },
        "conclusion": {
          "status": "succeeded",
          "duration_ms": "[DURATION_MS]",
          "final_git_commit_sha": "[SHA]",
          "stage_count": null
        },
        "checkpoint": {
          "current_node": "step_two",
          "completed_nodes": [
            "start",
            "step_one",
            "step_two"
          ],
          "next_node_id": "exit",
          "git_commit_sha": "[SHA]"
        },
        "sandbox": {
          "provider": "local",
          "working_directory": "[WORKTREE]"
        }
      }
    ]
    "#
    );
}
