use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::Value;

use super::support::{conflict_error_body, remote_run_summary_json, ulid_filter};
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["deny", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Deny pending workflow runs

    Usage: fabro deny [OPTIONS] <RUNS>...

    Arguments:
      <RUNS>...  Run IDs or workflow names to deny

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --reason <REASON>   Reason for denying execution
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn deny_requires_at_least_one_run() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["deny"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <RUNS>...

    Usage: fabro deny --no-upgrade-check <RUNS>...

    For more information, try '--help'.
    ");
}

#[test]
fn deny_resolves_selector_posts_endpoint_and_prints_short_run_id() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let resolve = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", "nightly-build");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Nightly Build",
                "nightly-build",
                "Nightly run",
                &serde_json::json!({ "kind": "pending", "reason": "approval_required" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    let deny = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/deny"))
            .json_body(serde_json::json!({}));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Nightly Build",
                "nightly-build",
                "Nightly run",
                &serde_json::json!({ "kind": "failed", "reason": "approval_denied" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    context.set_http_target(&server.base_url());

    let mut filters = context.filters();
    filters.push(ulid_filter());
    let mut cmd = context.command();
    cmd.args(["deny", "nightly-build"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");

    resolve.assert();
    deny.assert();
}

#[test]
fn deny_reason_sends_request_body() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let resolve = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", "nightly-build");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Nightly Build",
                "nightly-build",
                "Nightly run",
                &serde_json::json!({ "kind": "pending", "reason": "approval_required" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    let deny = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/deny"))
            .json_body(serde_json::json!({ "reason": "Needs review" }));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Nightly Build",
                "nightly-build",
                "Nightly run",
                &serde_json::json!({ "kind": "failed", "reason": "approval_denied" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    context.set_http_target(&server.base_url());

    let mut filters = context.filters();
    filters.push(ulid_filter());
    let mut cmd = context.command();
    cmd.args(["deny", "--reason", "Needs review", "nightly-build"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");

    resolve.assert();
    deny.assert();
}

#[test]
fn deny_json_success_shape() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let resolve = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", "nightly-build");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Nightly Build",
                "nightly-build",
                "Nightly run",
                &serde_json::json!({ "kind": "pending", "reason": "approval_required" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    let deny = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{run_id}/deny"))
            .json_body(serde_json::json!({}));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &run_id,
                "Nightly Build",
                "nightly-build",
                "Nightly run",
                &serde_json::json!({ "kind": "failed", "reason": "approval_denied" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    context.set_http_target(&server.base_url());

    let output = context
        .command()
        .args(["--json", "deny", "nightly-build"])
        .output()
        .expect("deny --json should execute");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("deny JSON should parse");
    assert_eq!(
        value["denied"],
        Value::Array(vec![Value::String(run_id.clone())])
    );
    assert_eq!(value["errors"], Value::Array(vec![]));
    resolve.assert();
    deny.assert();
}

#[test]
fn deny_partial_error_attempts_remaining_runs_and_reports_json() {
    let context = test_context!();
    let server = MockServer::start();
    let stale_run = unique_run_id();
    let good_run = unique_run_id();
    let stale_resolve = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", "stale");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &stale_run,
                "Stale",
                "stale",
                "Stale run",
                &serde_json::json!({ "kind": "succeeded", "reason": "completed" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    let good_resolve = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", "nightly");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &good_run,
                "Nightly",
                "nightly",
                "Nightly run",
                &serde_json::json!({ "kind": "pending", "reason": "approval_required" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    let stale_deny = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{stale_run}/deny"))
            .json_body(serde_json::json!({ "reason": "Needs review" }));
        then.status(409)
            .header("Content-Type", "application/json")
            .json_body(conflict_error_body("Run is not pending approval."));
    });
    let good_deny = server.mock(|when, then| {
        when.method("POST")
            .path(format!("/api/v1/runs/{good_run}/deny"))
            .json_body(serde_json::json!({ "reason": "Needs review" }));
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(remote_run_summary_json(
                &good_run,
                "Nightly",
                "nightly",
                "Nightly run",
                &serde_json::json!({ "kind": "failed", "reason": "approval_denied" }),
                "2026-04-05T12:00:00Z",
            ));
    });
    context.set_http_target(&server.base_url());

    let output = context
        .command()
        .args([
            "--json",
            "deny",
            "--reason",
            "Needs review",
            "stale",
            "nightly",
        ])
        .output()
        .expect("deny should execute");

    assert!(!output.status.success(), "mixed batch should exit non-zero");
    let value: Value = serde_json::from_slice(&output.stdout).expect("deny JSON should parse");
    assert_eq!(
        value["denied"],
        Value::Array(vec![Value::String(good_run.clone())])
    );
    assert_eq!(value["errors"][0]["identifier"], "stale");
    assert!(
        value["errors"][0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("Run is not pending approval")),
        "{value}"
    );
    stale_resolve.assert();
    good_resolve.assert();
    stale_deny.assert();
    good_deny.assert();
}
