use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;

use super::support::setup_created_fast_dry_run;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["system", "prune", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Delete old workflow runs

    Usage: fabro system prune [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --before <BEFORE>            Only include runs started before this date (YYYY-MM-DD prefix match)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --workflow <WORKFLOW>        Filter by workflow name (substring match)
          --label <KEY=VALUE>          Filter by label (KEY=VALUE, repeatable, AND semantics)
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --orphans                    Include orphan directories (no matching durable run)
          --older-than <DURATION>      Only prune runs older than this duration (e.g. 24h, 7d). Default: 24h when no explicit filters are set
          --yes                        Actually delete (default is dry-run)
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn system_prune_dry_run_lists_matching_runs_without_deleting() {
    let context = test_context!();
    let server = MockServer::start();
    let prune_mock = server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/system/prune/runs")
            .json_body_obj(&serde_json::json!({
                "dry_run": true,
                "labels": {
                    "fabro_test_case": context.test_case_id(),
                },
                "orphans": false,
                "workflow": "Simple",
            }));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "dry_run": true,
                    "runs": [
                        {
                            "run_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                            "dir_name": "20260407-01ARZ3NDEKTSV4RRFFQ69G5FAV",
                            "workflow_name": "Simple",
                            "size_bytes": 1024,
                        }
                    ],
                    "total_count": 1,
                    "total_size_bytes": 1024,
                })
                .to_string(),
            );
    });
    let mut filters = context.filters();
    filters.push((r"\b\d{8}-\[ULID\]".to_string(), "[DATE]-[ULID]".to_string()));
    filters.push((
        r"\b\d+(\.\d+)?\s(?:[KMGT]?B|B)\b".to_string(),
        "[SIZE]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args([
        "system",
        "prune",
        "--server",
        &format!("{}/api/v1", server.base_url()),
        "--workflow",
        "Simple",
        "--label",
        &context.test_case_label(),
    ]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    would delete: [DATE]-[ULID] (Simple)
    ----- stderr -----

    1 run(s) would be deleted ([SIZE] freed). Pass --yes to confirm.
    ");
    prune_mock.assert();
}

#[test]
fn system_prune_yes_uses_server_target_and_reports_deleted_runs() {
    let context = test_context!();
    let server = MockServer::start();
    let prune_mock = server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/system/prune/runs")
            .json_body_obj(&serde_json::json!({
                "dry_run": false,
                "labels": {
                    "fabro_test_case": context.test_case_id(),
                },
                "orphans": false,
                "workflow": "Simple",
            }));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "dry_run": false,
                    "deleted_count": 1,
                    "freed_bytes": 1024,
                    "total_count": 1,
                    "total_size_bytes": 1024,
                })
                .to_string(),
            );
    });
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?\s(?:[KMGT]?B|B)\b".to_string(),
        "[SIZE]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args([
        "system",
        "prune",
        "--server",
        &format!("{}/api/v1", server.base_url()),
        "--workflow",
        "Simple",
        "--label",
        &context.test_case_label(),
        "--yes",
    ]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    1 run(s) deleted ([SIZE] freed).
    ");
    prune_mock.assert();
}

#[test]
fn system_prune_does_not_delete_active_or_submitted_runs() {
    let context = test_context!();
    let run = setup_created_fast_dry_run(&context);
    let mut cmd = context.command();
    cmd.args([
        "system",
        "prune",
        "--workflow",
        "Simple",
        "--label",
        &context.test_case_label(),
        "--yes",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    No matching runs to prune.
    ");
    assert!(
        run.run_dir.exists(),
        "submitted run should not be deleted by system prune"
    );
}
