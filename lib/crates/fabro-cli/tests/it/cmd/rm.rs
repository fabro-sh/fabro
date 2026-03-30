use fabro_test::{fabro_snapshot, test_context};

use super::support::{setup_completed_dry_run, setup_created_dry_run};

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
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -f, --force                      Force removal of active runs
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn rm_deletes_completed_run() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
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
fn rm_rejects_submitted_run_without_force() {
    let context = test_context!();
    let run = setup_created_dry_run(&context);
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
    let run = setup_created_dry_run(&context);
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
fn rm_partial_failure_reports_which_identifiers_failed() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
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
