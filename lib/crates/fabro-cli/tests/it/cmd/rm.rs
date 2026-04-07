use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::Value;

use crate::support::unique_run_id;

use super::support::{
    setup_completed_fast_dry_run, setup_created_fast_dry_run, setup_local_sandbox_run,
};

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
    let run = setup_completed_fast_dry_run(&context);
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
    let run = setup_created_fast_dry_run(&context);
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
    cannot remove active run [ULID] (status: submitted, use -f to force)
    error: some runs could not be removed
    ");
}

#[test]
fn rm_force_deletes_submitted_run() {
    let context = test_context!();
    let run = setup_created_fast_dry_run(&context);
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
    let _ = std::fs::remove_file(setup.run.run_dir.join("sandbox.json"));

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
        "run directory should be deleted even without sandbox.json"
    );
}

#[test]
fn rm_partial_failure_reports_which_identifiers_failed() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
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
    error: some runs could not be removed
    ");
    assert!(
        !run.run_dir.exists(),
        "existing run should still be removed"
    );
}

#[test]
fn rm_partial_failure_json_includes_removed_and_errors() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);

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
    let list_mock = server.mock(|when, then| {
        when.method("GET").path("/api/v1/runs");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!([
                    {
                        "run_id": run_id,
                        "workflow_name": "Remote Workflow",
                        "workflow_slug": "remote-workflow",
                        "goal": "Remote goal",
                        "labels": {},
                        "host_repo_path": null,
                        "start_time": "2026-04-05T12:00:00Z",
                        "status": "succeeded",
                        "status_reason": null,
                        "duration_ms": 123,
                        "total_usd_micros": null
                    }
                ])
                .to_string(),
            );
    });
    let delete_mock = server.mock(|when, then| {
        when.method("DELETE").path(format!("/api/v1/runs/{run_id}"));
        then.status(204);
    });
    context.write_home(
        ".fabro/settings.toml",
        format!("[server]\ntarget = \"{}/api/v1\"\n", server.base_url()),
    );

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
    list_mock.assert();
    delete_mock.assert();
}
