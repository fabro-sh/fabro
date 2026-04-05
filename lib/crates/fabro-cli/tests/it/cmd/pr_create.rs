use fabro_test::{fabro_snapshot, test_context};

use super::support::{setup_completed_fast_dry_run, setup_created_fast_dry_run};

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
          --json                       Output as JSON [env: FABRO_JSON=]
          --model <MODEL>              LLM model for generating PR description
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Fabro API server URL (overrides server.base_url from user.toml when supported) [env: FABRO_SERVER_URL=]
      -h, --help                       Print help
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
fn pr_create_uses_store_run_record_without_run_json() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let _ = std::fs::remove_file(run.run_dir.join("run.json"));
    let _ = std::fs::remove_file(run.run_dir.join("start.json"));
    let _ = std::fs::remove_file(run.run_dir.join("conclusion.json"));

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
