use fabro_test::{fabro_snapshot, test_context};

use super::support::{setup_completed_fast_dry_run, setup_created_fast_dry_run};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["wait", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Block until a workflow run completes

    Usage: fabro wait [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name (most recent run)

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --timeout <SECONDS>          Maximum time to wait in seconds
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --interval <MS>              Poll interval in milliseconds [default: 1000]
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
fn wait_completed_run_prints_success_summary() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["wait", &run.run_id]);

    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Succeeded [ULID]  [DURATION]
    ");
}

#[test]
fn wait_completed_run_reads_store_without_status_or_conclusion_files() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let _ = std::fs::remove_file(run.run_dir.join("conclusion.json"));
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["wait", &run.run_id]);

    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Succeeded [ULID]  [DURATION]
    ");
}

#[test]
fn wait_completed_run_json_outputs_status_and_duration() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["wait", "--json", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    {
      "run_id": "[ULID]",
      "status": "succeeded",
      "duration_ms": [DURATION_MS]
    }
    ----- stderr -----
    "###);
}

#[test]
fn wait_submitted_run_times_out() {
    let context = test_context!();
    let run = setup_created_fast_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["wait", "--timeout", "1", "--interval", "10", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Timed out after 1s waiting for run '[ULID]'
    ");
}
