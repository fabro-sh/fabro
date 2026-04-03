use fabro_test::{fabro_snapshot, test_context};

use super::support::{setup_completed_dry_run, setup_created_dry_run};

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
          --before <BEFORE>            Only include runs started before this date (YYYY-MM-DD prefix match)
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --workflow <WORKFLOW>        Filter by workflow name (substring match)
          --label <KEY=VALUE>          Filter by label (KEY=VALUE, repeatable, AND semantics)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --orphans                    Include orphan directories (no run.json)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --older-than <DURATION>      Only prune runs older than this duration (e.g. 24h, 7d). Default: 24h when no explicit filters are set
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --yes                        Actually delete (default is dry-run)
          --server-url <SERVER_URL>    Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn system_prune_dry_run_lists_matching_runs_without_deleting() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((r"\d{8}-dry-run-".to_string(), "[DATE]-dry-run-".to_string()));
    filters.push((
        r"\b\d+(\.\d+)?\s(?:[KMGT]?B|B)\b".to_string(),
        "[SIZE]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["system", "prune", "--workflow", "Simple"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    would delete: [DATE]-dry-run-[ULID] (Simple)
    ----- stderr -----

    1 run(s) would be deleted ([SIZE] freed). Pass --yes to confirm.
    ");
    assert!(
        run.run_dir.exists(),
        "dry-run prune should not delete the run"
    );
}

#[test]
fn system_prune_yes_deletes_matching_runs() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?\s(?:[KMGT]?B|B)\b".to_string(),
        "[SIZE]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["system", "prune", "--workflow", "Simple", "--yes"]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    1 run(s) deleted ([SIZE] freed).
    ");
    assert!(!run.run_dir.exists(), "matching run should be deleted");

    let mut ps = context.ps();
    ps.args(["-a", "--json"]);
    fabro_snapshot!(context.filters(), ps, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    []
    ----- stderr -----
    "###);
}

#[test]
fn system_prune_does_not_delete_active_or_submitted_runs() {
    let context = test_context!();
    let run = setup_created_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["system", "prune", "--workflow", "Simple", "--yes"]);

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
