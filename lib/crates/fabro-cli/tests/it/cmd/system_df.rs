use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

use super::support::setup_completed_fast_dry_run;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["system", "df", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Show disk usage

    Usage: fabro system df [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
      -v, --verbose                    Show per-run breakdown
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn system_df_summarizes_runs_and_logs() {
    let context = test_context!();
    setup_completed_fast_dry_run(&context);
    std::fs::create_dir_all(context.storage_dir.join("logs")).unwrap();
    std::fs::write(context.storage_dir.join("logs/cli.log"), b"log line\n").unwrap();

    let output = context
        .command()
        .args(["system", "df"])
        .output()
        .expect("command should run");

    assert!(output.status.success(), "system df failed");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be valid UTF-8");
    assert!(
        stdout.contains("Runs"),
        "system df should summarize runs: {stdout}"
    );
    assert!(
        stdout.contains("Logs"),
        "system df should summarize logs: {stdout}"
    );
    assert!(
        stdout.contains("Data directory:"),
        "system df should print the storage directory: {stdout}"
    );
}

#[test]
fn system_df_verbose_lists_runs_with_reclaimable_marker() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);

    let output = context
        .command()
        .args(["system", "df", "-v"])
        .output()
        .expect("command should run");

    assert!(output.status.success(), "system df -v failed");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be valid UTF-8");
    assert!(
        stdout.contains("RUN ID"),
        "verbose system df should print the run table: {stdout}"
    );
    assert!(
        stdout.contains(&run.run_id[..12]),
        "verbose system df should include the current test run: {stdout}"
    );
    assert!(
        stdout.contains("Simple"),
        "verbose system df should include the workflow name: {stdout}"
    );
    assert!(
        stdout.contains("completed"),
        "verbose system df should include the run status: {stdout}"
    );
    assert!(
        stdout.contains("* = reclaimable"),
        "verbose system df should include the reclaimable marker legend: {stdout}"
    );
}

#[test]
fn system_df_json_verbose_includes_runs() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);

    let output = context
        .command()
        .args(["--json", "system", "df", "--verbose"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("system df JSON should parse");
    assert!(value["summary"].is_array());
    assert!(
        value["runs"]
            .as_array()
            .is_some_and(|runs| runs.iter().any(|entry| entry["run_id"] == run.run_id))
    );
}
