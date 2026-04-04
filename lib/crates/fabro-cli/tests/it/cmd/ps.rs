use fabro_test::{fabro_snapshot, test_context};

use super::support::{fixture, run_success, setup_completed_dry_run, setup_created_dry_run};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["ps", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List workflow runs

    Usage: fabro ps [OPTIONS]

    Options:
          --before <BEFORE>            Only include runs started before this date (YYYY-MM-DD prefix match)
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --workflow <WORKFLOW>        Filter by workflow name (substring match)
          --label <KEY=VALUE>          Filter by label (KEY=VALUE, repeatable, AND semantics)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --orphans                    Include orphan directories (no run.json)
      -a, --all                        Show all runs, not just running (like docker ps -a)
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -q, --quiet                      Only display run IDs
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn ps_default_excludes_non_running_runs() {
    let context = test_context!();
    setup_completed_dry_run(&context);
    let cmd = context.ps();

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    No running processes found. Use -a to show all runs.
    ");
}

#[test]
fn ps_all_json_lists_created_and_completed_runs() {
    let context = test_context!();
    setup_completed_dry_run(&context);
    setup_created_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    filters.push((r"\d{8}-dry-run-".to_string(), "[DATE]-dry-run-".to_string()));
    let mut cmd = context.ps();
    cmd.args(["-a", "--json"]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    [
      {
        "run_id": "[ULID]",
        "dir_name": "20260404-[ULID]",
        "workflow_name": "Simple",
        "workflow_slug": "simple",
        "status": "submitted",
        "status_reason": null,
        "start_time": "[TIMESTAMP]",
        "labels": {},
        "duration_ms": null,
        "total_cost": null,
        "host_repo_path": "[TEMP_DIR]",
        "goal": "Run tests and report results"
      },
      {
        "run_id": "[ULID]",
        "dir_name": "20260404-[ULID]",
        "workflow_name": "Simple",
        "workflow_slug": "simple",
        "status": "succeeded",
        "status_reason": "completed",
        "start_time": "[TIMESTAMP]",
        "labels": {},
        "duration_ms": [DURATION_MS],
        "total_cost": null,
        "host_repo_path": "[TEMP_DIR]",
        "goal": "Run tests and report results"
      }
    ]
    ----- stderr -----
    "#);
}

#[test]
fn ps_quiet_outputs_run_ids_only() {
    let context = test_context!();
    setup_completed_dry_run(&context);
    setup_created_dry_run(&context);
    let mut cmd = context.ps();
    cmd.args(["-a", "--quiet"]);

    fabro_snapshot!(context.filters(), cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    [ULID]
    [ULID]
    ----- stderr -----
    "###);
}

#[test]
fn ps_filters_by_workflow_and_label() {
    let context = test_context!();
    let simple = fixture("simple.fabro");
    let branching = fixture("branching.fabro");

    run_success(
        &context,
        &[
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--label",
            "suite=alpha",
            simple.to_str().unwrap(),
        ],
    );
    run_success(
        &context,
        &[
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--label",
            "suite=beta",
            branching.to_str().unwrap(),
        ],
    );

    let mut filters = context.filters();
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    filters.push((r"\d{8}-dry-run-".to_string(), "[DATE]-dry-run-".to_string()));
    let mut cmd = context.ps();
    cmd.args([
        "-a",
        "--json",
        "--workflow",
        "Simple",
        "--label",
        "suite=alpha",
    ]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    [
      {
        "run_id": "[ULID]",
        "dir_name": "20260404-[ULID]",
        "workflow_name": "Simple",
        "workflow_slug": "simple",
        "status": "succeeded",
        "status_reason": "completed",
        "start_time": "[TIMESTAMP]",
        "labels": {
          "suite": "alpha"
        },
        "duration_ms": [DURATION_MS],
        "total_cost": null,
        "host_repo_path": "[TEMP_DIR]",
        "goal": "Run tests and report results"
      }
    ]
    ----- stderr -----
    "#);
}
