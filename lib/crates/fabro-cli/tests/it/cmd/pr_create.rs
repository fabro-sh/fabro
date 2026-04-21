use fabro_test::{fabro_snapshot, test_context};

use super::support::{setup_completed_fast_dry_run, setup_created_fast_dry_run, setup_failed_run};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["pr", "create", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Create a pull request from a completed run

    Usage: fabro pr create [OPTIONS] <RUN_ID>

    Arguments:
      <RUN_ID>  Run ID or prefix

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --model <MODEL>     LLM model for generating PR description
      -f, --force             Create PR even if the run status is not success/partial_success
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn pr_create_unfinished_run_errors_before_network() {
    let context = test_context!();
    let run = setup_created_fast_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["pr", "create", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Failed to load start record from store
    ");
}

#[test]
fn pr_create_completed_dry_run_without_run_branch_errors() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["pr", "create", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Run has no run_branch — was it run with git push enabled?
    ");
}

#[test]
fn pr_create_uses_store_run_spec_without_run_json() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);

    let mut cmd = context.command();
    cmd.args(["pr", "create", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Run has no run_branch — was it run with git push enabled?
    ");
}

#[test]
fn pr_create_failed_run_rejects_without_force() {
    let context = test_context!();
    let run = setup_failed_run(&context);
    let mut cmd = context.command();
    cmd.args(["pr", "create", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Run status is 'fail', expected success or partial_success
    ");
}

#[test]
fn pr_create_failed_run_proceeds_with_force() {
    let context = test_context!();
    let run = setup_failed_run(&context);
    let mut cmd = context.command();
    cmd.args(["pr", "create", "--force", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Run has no run_branch — was it run with git push enabled?
    ");
}
