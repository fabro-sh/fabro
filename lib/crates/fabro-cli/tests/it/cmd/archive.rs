use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::Value;

use super::support::{setup_seeded_completed_dry_run, setup_seeded_created_dry_run};
use crate::support::unique_run_id;

fn ulid_filter() -> (String, String) {
    (
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    )
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["archive", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @r"
    success: true
    exit_code: 0
    ----- stdout -----
    Mark terminal runs as archived (reviewed, no further action needed). Archived runs are hidden from default listings

    Usage: fabro archive [OPTIONS] <RUNS>...

    Arguments:
      <RUNS>...  Run IDs or workflow names to archive

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
fn archive_requires_at_least_one_id() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["archive"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <RUNS>...

    Usage: fabro archive --no-upgrade-check <RUNS>...

    For more information, try '--help'.
    ");
}

#[test]
fn archive_succeeded_run_hides_it_from_default_ps() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push(ulid_filter());

    let mut cmd = context.command();
    cmd.args(["archive", &run.run_id]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");

    // Default `ps` filters out archived runs.
    let mut ps = context.ps();
    ps.args(["--json", "--label", &context.test_case_label()]);
    fabro_snapshot!(context.filters(), ps, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    []
    ----- stderr -----
    "#);

    // `ps -a` surfaces it with status `archived`.
    let output = context
        .ps()
        .args(["-a", "--json", "--label", &context.test_case_label()])
        .output()
        .expect("ps -a should execute");
    assert!(output.status.success());
    let runs: Vec<Value> = serde_json::from_slice(&output.stdout).expect("ps JSON should parse");
    assert_eq!(runs.len(), 1, "ps -a should show the archived run");
    assert_eq!(runs[0]["status"]["kind"], "archived");
    assert_eq!(runs[0]["status"]["prior"]["kind"], "succeeded");
    assert_eq!(runs[0]["status"]["prior"]["reason"], "completed");
    assert_eq!(runs[0]["run_id"], run.run_id);
}

#[test]
fn archive_running_run_rejects_with_must_be_terminal_message() {
    // A `create`d run is in `submitted` — not yet terminal.
    let context = test_context!();
    let run = setup_seeded_created_dry_run(&context);
    let output = context
        .command()
        .args(["archive", &run.run_id])
        .output()
        .expect("archive should execute");
    assert!(!output.status.success(), "archive on submitted must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("must be terminal"),
        "expected 'must be terminal' in stderr, got: {stderr}"
    );
}

#[test]
fn archive_already_archived_is_idempotent() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);

    let first = context
        .command()
        .args(["archive", &run.run_id])
        .output()
        .expect("archive should execute");
    assert!(first.status.success(), "first archive should succeed");

    let second = context
        .command()
        .args(["archive", &run.run_id])
        .output()
        .expect("archive should execute");
    assert!(
        second.status.success(),
        "second archive on already-archived should succeed\nstderr:\n{}",
        String::from_utf8_lossy(&second.stderr)
    );
}

#[test]
fn archive_unknown_id_renders_clean_error() {
    let context = test_context!();
    let fake_id = unique_run_id();
    let output = context
        .command()
        .args(["archive", &fake_id])
        .output()
        .expect("archive should execute");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&fake_id) || stderr.contains("No run found"),
        "expected unknown-id error in stderr, got: {stderr}"
    );
}

#[test]
fn archive_json_output_shape() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);

    let output = context
        .command()
        .args(["--json", "archive", &run.run_id])
        .output()
        .expect("archive --json should execute");
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("archive JSON should parse");
    assert_eq!(
        value["archived"],
        Value::Array(vec![Value::String(run.run_id.clone())])
    );
    assert_eq!(value["errors"], Value::Array(vec![]));
}

#[test]
fn archive_mixed_batch_aggregates_errors() {
    let context = test_context!();
    let good = setup_seeded_completed_dry_run(&context);
    let bad = unique_run_id();

    let output = context
        .command()
        .args(["--json", "archive", &good.run_id, &bad])
        .output()
        .expect("archive should execute");
    assert!(!output.status.success(), "mixed batch should exit non-zero");
    let value: Value = serde_json::from_slice(&output.stdout).expect("archive JSON should parse");
    assert_eq!(
        value["archived"],
        Value::Array(vec![Value::String(good.run_id.clone())])
    );
    let errors = value["errors"].as_array().expect("errors should be array");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["identifier"], bad);
}

#[test]
fn archive_resolves_selector_via_server_endpoint() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let resolve_mock = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", "nightly-build");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "run_id": run_id,
                    "workflow_name": "Nightly Build",
                    "workflow_slug": "nightly-build",
                    "goal": "Nightly run",
                    "title": "Nightly run",
                    "labels": {},
                    "source_directory": null,
                    "repository": { "name": "unknown" },
                    "start_time": "2026-04-05T12:00:00Z",
                    "created_at": "2026-04-05T12:00:00Z",
                    "status": {
                        "kind": "succeeded",
                        "reason": "completed"
                    },
                    "pending_control": null,
                    "duration_ms": 123,
                    "elapsed_secs": 0,
                    "total_usd_micros": null
                })
                .to_string(),
            );
    });
    let archive_mock = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/archive"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "id": run_id,
                    "status": {
                        "kind": "archived",
                        "prior": {
                            "kind": "succeeded",
                            "reason": "completed"
                        }
                    },
                    "error": null,
                    "queue_position": null,
                    "pending_control": null,
                    "created_at": "2026-04-05T12:00:00Z"
                })
                .to_string(),
            );
    });
    context.set_http_target(&server.base_url());

    let mut filters = context.filters();
    filters.push(ulid_filter());
    let mut cmd = context.command();
    cmd.args(["archive", "nightly-build"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");

    resolve_mock.assert();
    archive_mock.assert();
}
