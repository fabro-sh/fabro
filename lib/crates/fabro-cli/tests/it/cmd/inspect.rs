use insta::assert_snapshot;

use fabro_test::{fabro_snapshot, test_context};

use super::support::{
    compact_git_inspect, compact_inspect, run_success, setup_completed_fast_dry_run,
    setup_created_fast_dry_run, setup_git_backed_changed_run,
};

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
fn inspect_created_run_shows_run_record_without_start_or_conclusion() {
    let context = test_context!();
    let run = setup_created_fast_dry_run(&context);
    let output = run_success(&context, &["inspect", &run.run_id]);

    assert_snapshot!(serde_json::to_string_pretty(&compact_inspect(&output)).unwrap(), @r###"
    [
      {
        "run_id": "[ULID]",
        "status": "submitted",
        "run_record": {
          "goal": "Run tests and report results",
          "workflow_name": "Simple",
          "workflow_slug": "simple",
          "sandbox_provider": "local",
          "dry_run": true
        },
        "start_record": null,
        "conclusion": null,
        "checkpoint": null,
        "sandbox": null
      }
    ]
    "###);
}

#[test]
fn inspect_completed_run_shows_run_start_conclusion_checkpoint() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let output = run_success(&context, &["inspect", &run.run_id]);

    assert_snapshot!(serde_json::to_string_pretty(&compact_inspect(&output)).unwrap(), @r#"
    [
      {
        "run_id": "[ULID]",
        "status": "succeeded",
        "run_record": {
          "goal": "Run tests and report results",
          "workflow_name": "Simple",
          "workflow_slug": "simple",
          "sandbox_provider": "local",
          "dry_run": true
        },
        "start_record": {
          "has_start_time": true
        },
        "conclusion": {
          "status": "success",
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
    let run = setup_completed_fast_dry_run(&context);
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
    let run = setup_completed_fast_dry_run(&context);
    for name in [
        "run.json",
        "start.json",
        "conclusion.json",
        "checkpoint.json",
        "sandbox.json",
    ] {
        let _ = std::fs::remove_file(run.run_dir.join(name));
    }
    let output = run_success(&context, &["inspect", &run.run_id]);

    assert_snapshot!(serde_json::to_string_pretty(&compact_inspect(&output)).unwrap(), @r#"
    [
      {
        "run_id": "[ULID]",
        "status": "succeeded",
        "run_record": {
          "goal": "Run tests and report results",
          "workflow_name": "Simple",
          "workflow_slug": "simple",
          "sandbox_provider": "local",
          "dry_run": true
        },
        "start_record": {
          "has_start_time": true
        },
        "conclusion": {
          "status": "success",
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
    let setup = setup_git_backed_changed_run(&context);
    let output = run_success(&context, &["inspect", &setup.run.run_id]);

    assert_snapshot!(
        serde_json::to_string_pretty(&compact_git_inspect(&output)).unwrap(),
        @r#"
    [
      {
        "run_id": "[ULID]",
        "status": "succeeded",
        "run_record": {
          "goal": "Edit a tracked file",
          "workflow_name": "Flow",
          "workflow_slug": "flow",
          "llm_provider": "openai",
          "sandbox_provider": "local"
        },
        "start_record": {
          "has_start_time": true,
          "run_branch": "fabro/run/[ULID]",
          "base_sha": "[SHA]"
        },
        "conclusion": {
          "status": "success",
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
