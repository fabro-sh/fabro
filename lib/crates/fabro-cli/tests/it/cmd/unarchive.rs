use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::Value;

use super::support::{setup_completed_fast_dry_run, setup_created_fast_dry_run};
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
    cmd.args(["unarchive", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Restore archived runs to their prior terminal status

    Usage: fabro unarchive [OPTIONS] <RUNS>...

    Arguments:
      <RUNS>...  Run IDs or workflow names to unarchive

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
fn unarchive_requires_at_least_one_id() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["unarchive"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <RUNS>...

    Usage: fabro unarchive --no-upgrade-check <RUNS>...

    For more information, try '--help'.
    ");
}

#[test]
fn unarchive_archived_run_restores_prior_terminal_status() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);

    // Archive first.
    let archive = context
        .command()
        .args(["archive", &run.run_id])
        .output()
        .expect("archive should execute");
    assert!(archive.status.success());

    // Unarchive.
    let mut filters = context.filters();
    filters.push(ulid_filter());
    let mut cmd = context.command();
    cmd.args(["unarchive", &run.run_id]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");

    // `ps -a` now shows it as succeeded.
    let output = context
        .ps()
        .args(["-a", "--json", "--label", &context.test_case_label()])
        .output()
        .expect("ps -a should execute");
    assert!(output.status.success());
    let runs: Vec<Value> = serde_json::from_slice(&output.stdout).expect("ps JSON should parse");
    assert_eq!(runs.len(), 1, "ps -a should show the unarchived run");
    assert_eq!(runs[0]["status"]["kind"], "succeeded");
    assert_eq!(runs[0]["status"]["reason"], "completed");
}

#[test]
fn unarchive_on_non_archived_terminal_is_idempotent() {
    // Unarchiving a succeeded (not-archived) run returns success with no event.
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);

    let output = context
        .command()
        .args(["unarchive", &run.run_id])
        .output()
        .expect("unarchive should execute");
    assert!(
        output.status.success(),
        "unarchive on already-succeeded should succeed\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unarchive_on_active_run_rejects_with_not_archived_message() {
    let context = test_context!();
    let run = setup_created_fast_dry_run(&context);

    let output = context
        .command()
        .args(["unarchive", &run.run_id])
        .output()
        .expect("unarchive should execute");
    assert!(!output.status.success(), "unarchive on submitted must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is not archived"),
        "expected 'is not archived' in stderr, got: {stderr}"
    );
}

#[test]
fn unarchive_unknown_id_renders_clean_error() {
    let context = test_context!();
    let fake_id = unique_run_id();
    let output = context
        .command()
        .args(["unarchive", &fake_id])
        .output()
        .expect("unarchive should execute");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&fake_id) || stderr.contains("No run found"),
        "expected unknown-id error in stderr, got: {stderr}"
    );
}

#[test]
fn unarchive_json_output_shape() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    context
        .command()
        .args(["archive", &run.run_id])
        .output()
        .expect("archive should execute");

    let output = context
        .command()
        .args(["--json", "unarchive", &run.run_id])
        .output()
        .expect("unarchive --json should execute");
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("unarchive JSON should parse");
    assert_eq!(
        value["unarchived"],
        Value::Array(vec![Value::String(run.run_id.clone())])
    );
    assert_eq!(value["errors"], Value::Array(vec![]));
}

#[test]
fn unarchive_mixed_batch_aggregates_errors() {
    let context = test_context!();
    let archived_run = setup_completed_fast_dry_run(&context);
    context
        .command()
        .args(["archive", &archived_run.run_id])
        .output()
        .expect("archive should execute");
    let active_run = setup_created_fast_dry_run(&context);

    let output = context
        .command()
        .args([
            "--json",
            "unarchive",
            &archived_run.run_id,
            &active_run.run_id,
        ])
        .output()
        .expect("unarchive should execute");
    assert!(!output.status.success(), "mixed batch should exit non-zero");
    let value: Value = serde_json::from_slice(&output.stdout).expect("unarchive JSON should parse");
    assert_eq!(
        value["unarchived"],
        Value::Array(vec![Value::String(archived_run.run_id.clone())])
    );
    let errors = value["errors"].as_array().expect("errors should be array");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["identifier"], active_run.run_id);
}

#[test]
fn unarchive_resolves_selector_via_server_endpoint() {
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
                    "host_repo_path": null,
                    "repository": { "name": "unknown" },
                    "start_time": "2026-04-05T12:00:00Z",
                    "created_at": "2026-04-05T12:00:00Z",
                    "status": {
                        "kind": "archived",
                        "prior": {
                            "kind": "succeeded",
                            "reason": "completed"
                        }
                    },
                    "pending_control": null,
                    "duration_ms": 123,
                    "elapsed_secs": 0,
                    "total_usd_micros": null
                })
                .to_string(),
            );
    });
    let unarchive_mock = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/unarchive"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "id": run_id,
                    "status": {
                        "kind": "succeeded",
                        "reason": "completed"
                    },
                    "error": null,
                    "queue_position": null,
                    "pending_control": null,
                    "created_at": "2026-04-05T12:00:00Z"
                })
                .to_string(),
            );
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            "_version = 1\n\n[cli.target]\ntype = \"http\"\nurl = \"{}/api/v1\"\n",
            server.base_url()
        ),
    );

    let mut filters = context.filters();
    filters.push(ulid_filter());
    let mut cmd = context.command();
    cmd.args(["unarchive", "nightly-build"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");

    resolve_mock.assert();
    unarchive_mock.assert();
}
