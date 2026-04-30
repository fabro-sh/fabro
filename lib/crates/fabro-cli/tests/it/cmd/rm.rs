use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::Value;

use super::support::{
    setup_local_sandbox_run, setup_seeded_completed_dry_run, setup_seeded_created_dry_run,
};
use crate::support::unique_run_id;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["rm", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Remove one or more workflow runs

    Usage: fabro rm [OPTIONS] <RUNS>...

    Arguments:
      <RUNS>...  Run IDs or workflow names to remove

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -f, --force             Force removal of active runs
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn rm_deletes_completed_run() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["rm", &run.run_id]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");
    assert!(!run.run_dir.exists(), "run directory should be deleted");

    let mut ps = context.ps();
    ps.args(["-a", "--json", "--label", &context.test_case_label()]);
    fabro_snapshot!(context.filters(), ps, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    []
    ----- stderr -----
    "###);
}

#[test]
fn rm_rejects_submitted_run_without_force() {
    let context = test_context!();
    let run = setup_seeded_created_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["rm", &run.run_id]);
    fabro_snapshot!(filters, cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    cannot remove active run [ULID] (status: submitted, use force=true or --force to force)
      × some runs could not be removed
    ");
}

#[test]
fn rm_force_deletes_submitted_run() {
    let context = test_context!();
    let run = setup_seeded_created_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["rm", "--force", &run.run_id]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");
    assert!(!run.run_dir.exists(), "run directory should be deleted");

    let mut ps = context.ps();
    ps.args(["-a", "--json", "--label", &context.test_case_label()]);
    fabro_snapshot!(context.filters(), ps, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    []
    ----- stderr -----
    "###);
}

#[test]
fn rm_force_deletes_run_without_sandbox_json_when_store_has_sandbox() {
    let context = test_context!();
    let setup = setup_local_sandbox_run(&context);

    let mut filters = context.filters();
    filters.push((
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["rm", "--force", &setup.run.run_id]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");
    assert!(
        !setup.run.run_dir.exists(),
        "run directory should be deleted"
    );
}

#[test]
fn rm_force_removes_active_run() {
    let context = test_context!();
    let run_id = unique_run_id();
    let server = MockServer::start();
    let resolve_mock = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", &run_id);
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "run_id": run_id,
                    "workflow_name": "Active Workflow",
                    "workflow_slug": "active-workflow",
                    "goal": "Active goal",
                    "title": "Active goal",
                    "labels": {},
                    "source_directory": null,
                    "repository": { "name": "unknown" },
                    "start_time": "2026-04-05T12:00:00Z",
                    "created_at": "2026-04-05T12:00:00Z",
                    "status": {
                        "kind": "running"
                    },
                    "pending_control": null,
                    "duration_ms": 123,
                    "elapsed_secs": 0,
                    "total_usd_micros": null
                })
                .to_string(),
            );
    });
    let delete_mock = server.mock(|when, then| {
        when.method("DELETE")
            .path(format!("/api/v1/runs/{run_id}"))
            .query_param("force", "true");
        then.status(204);
    });
    context.set_http_target(&server.base_url());

    let mut filters = context.filters();
    filters.push((
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["rm", "--force", &run_id]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");
    resolve_mock.assert();
    delete_mock.assert();
}

#[test]
fn rm_without_force_uses_resolve_then_surfaces_server_conflict() {
    let context = test_context!();
    let run_id = unique_run_id();
    let server = MockServer::start();
    let resolve_mock = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", &run_id);
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "run_id": run_id,
                    "workflow_name": "Active Workflow",
                    "workflow_slug": "active-workflow",
                    "goal": "Active goal",
                    "title": "Active goal",
                    "labels": {},
                    "source_directory": null,
                    "repository": { "name": "unknown" },
                    "start_time": "2026-04-05T12:00:00Z",
                    "created_at": "2026-04-05T12:00:00Z",
                    "status": {
                        "kind": "running"
                    },
                    "pending_control": null,
                    "duration_ms": 123,
                    "elapsed_secs": 0,
                    "total_usd_micros": null
                })
                .to_string(),
            );
    });
    let delete_mock = server.mock(|when, then| {
        when.method("DELETE").path(format!("/api/v1/runs/{run_id}"));
        then.status(409)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "errors": [{
                        "status": "409",
                        "title": "Conflict",
                        "detail": format!(
                            "cannot remove active run {} (status: running, use force=true or --force to force)",
                            &run_id[..12],
                        ),
                    }]
                })
                .to_string(),
            );
    });
    context.set_http_target(&server.base_url());

    let mut filters = context.filters();
    filters.push((
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["rm", &run_id]);
    fabro_snapshot!(filters, cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    cannot remove active run [ULID] (status: running, use force=true or --force to force)
      × some runs could not be removed
    ");
    resolve_mock.assert();
    delete_mock.assert();
}

#[test]
fn rm_partial_failure_reports_which_identifiers_failed() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["rm", &run.run_id, "does-not-exist"]);
    fabro_snapshot!(filters, cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    [ULID]
    error: does-not-exist: No run found matching 'does-not-exist' (tried run ID prefix and workflow name)
      × some runs could not be removed
    ");
    assert!(
        !run.run_dir.exists(),
        "existing run should still be removed"
    );
}

#[test]
fn rm_partial_failure_json_includes_removed_and_errors() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);

    let output = context
        .command()
        .args(["--json", "rm", &run.run_id, "does-not-exist"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("rm JSON should parse");
    assert_eq!(
        value["removed"],
        Value::Array(vec![Value::String(run.run_id.clone())])
    );
    assert_eq!(value["errors"][0]["identifier"], "does-not-exist");
    assert!(
        value["errors"][0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("does-not-exist"))
    );
    assert!(
        !run.run_dir.exists(),
        "existing run should still be removed"
    );
}

#[test]
fn rm_uses_configured_server_target_without_local_run_dir() {
    let context = test_context!();
    let run_id = unique_run_id();
    let server = MockServer::start();
    let resolve_mock = server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", &run_id);
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "run_id": run_id,
                    "workflow_name": "Remote Workflow",
                    "workflow_slug": "remote-workflow",
                    "goal": "Remote goal",
                    "title": "Remote goal",
                    "labels": {},
                    "source_directory": null,
                    "repository": { "name": "unknown" },
                    "start_time": "2026-04-05T12:00:00Z",
                    "created_at": "2026-04-05T12:00:00Z",
                    "status": {
                        "kind": "succeeded",
                        "reason": "completed"
                    },
                    "duration_ms": 123,
                    "total_usd_micros": null
                })
                .to_string(),
            );
    });
    let delete_mock = server.mock(|when, then| {
        when.method("DELETE").path(format!("/api/v1/runs/{run_id}"));
        then.status(204);
    });
    context.set_http_target(&server.base_url());

    let output = context
        .command()
        .args(["rm", &run_id])
        .output()
        .expect("rm should execute");

    assert!(
        output.status.success(),
        "rm failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    resolve_mock.assert();
    delete_mock.assert();
}
