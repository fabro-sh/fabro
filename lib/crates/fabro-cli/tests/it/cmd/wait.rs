use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::json;

use super::support::{setup_completed_fast_dry_run, setup_created_fast_dry_run};
use crate::support::unique_run_id;

fn remote_run_summary(run_id: &str, status: &str) -> serde_json::Value {
    json!({
        "run_id": run_id,
        "workflow_name": "Blocked Remote Workflow",
        "workflow_slug": "blocked-remote-workflow",
        "goal": "Wait for approval",
        "labels": {},
        "host_repo_path": "/srv/repo",
        "start_time": "2026-04-19T12:00:00Z",
        "status": status,
        "status_reason": null,
        "duration_ms": null,
        "total_usd_micros": null
    })
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["wait", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Block until a workflow run completes

    Usage: fabro wait [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name (most recent run)

    Options:
          --json               Output as JSON [env: FABRO_JSON=]
          --server <SERVER>    Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug              Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --timeout <SECONDS>  Maximum time to wait in seconds
          --interval <MS>      Poll interval in milliseconds [default: 1000]
          --no-upgrade-check   Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet              Suppress non-essential output [env: FABRO_QUIET=]
          --verbose            Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help               Print help
    ----- stderr -----
    ");
}

#[test]
fn wait_completed_run_prints_success_summary() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["wait", &run.run_id]);

    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Succeeded [ULID]  [DURATION]
    ");
}

#[test]
fn wait_completed_run_reads_store_without_status_or_conclusion_files() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["wait", &run.run_id]);

    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Succeeded [ULID]  [DURATION]
    ");
}

#[test]
fn wait_completed_run_json_outputs_status_and_duration() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["wait", "--json", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    {
      "run_id": "[ULID]",
      "status": "succeeded",
      "duration_ms": [DURATION_MS]
    }
    ----- stderr -----
    "###);
}

#[test]
fn wait_submitted_run_times_out() {
    let context = test_context!();
    let run = setup_created_fast_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["wait", "--timeout", "1", "--interval", "10", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Timed out after 1s waiting for run '[ULID]'
    ");
}

#[test]
fn wait_blocked_run_times_out_without_treating_it_as_terminal() {
    let context = test_context!();
    let run_id = unique_run_id();
    let server = MockServer::start();
    let summary = remote_run_summary(run_id.as_str(), "blocked");

    let list_runs = server.mock(|when, then| {
        when.method("GET").path("/api/v1/runs");
        then.status(200)
            .header("content-type", "application/json")
            .body(json!([summary.clone()]).to_string());
    });
    let retrieve_run = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{}", run_id.as_str()));
        then.status(200)
            .header("content-type", "application/json")
            .body(summary.to_string());
    });
    let run_state = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{}/state", run_id.as_str()));
        then.status(200)
            .header("content-type", "application/json")
            .body("{}");
    });

    let mut cmd = context.command();
    cmd.args([
        "wait",
        "--server",
        &format!("{}/api/v1", server.base_url()),
        "--timeout",
        "1",
        "--interval",
        "10",
        run_id.as_str(),
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Timed out after 1s waiting for run '[ULID]'
    ");
    list_runs.assert();
    assert!(
        retrieve_run.calls() > 0,
        "wait should keep polling the blocked run summary until timeout"
    );
    assert_eq!(
        run_state.calls(),
        0,
        "wait should not fetch run state when the run never becomes terminal"
    );
}
