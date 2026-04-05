use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

use super::support::{fixture, setup_completed_fast_dry_run, setup_created_fast_dry_run};

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
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --before <BEFORE>            Only include runs started before this date (YYYY-MM-DD prefix match)
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --workflow <WORKFLOW>        Filter by workflow name (substring match)
          --label <KEY=VALUE>          Filter by label (KEY=VALUE, repeatable, AND semantics)
          --orphans                    Include orphan directories (no run.json)
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -a, --all                        Show all runs, not just running (like docker ps -a)
      -q, --quiet                      Only display run IDs
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn ps_default_excludes_non_running_runs() {
    let context = test_context!();
    setup_completed_fast_dry_run(&context);
    let mut cmd = context.ps();
    cmd.args(["--label", &context.test_case_label()]);

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
    setup_completed_fast_dry_run(&context);
    setup_created_fast_dry_run(&context);
    let output = context
        .ps()
        .args(["-a", "--json", "--label", &context.test_case_label()])
        .output()
        .expect("ps should run");

    assert!(output.status.success(), "ps should succeed");
    let runs: Vec<Value> = serde_json::from_slice(&output.stdout).expect("ps JSON should parse");
    assert_eq!(runs.len(), 2, "expected submitted + completed runs");
    assert!(
        runs.iter().all(|run| run["workflow_name"] == "Simple"),
        "all runs should belong to the Simple workflow: {runs:#?}"
    );
    assert!(
        runs.iter()
            .all(|run| run["labels"]["fabro_test_case"] == context.test_case_id()),
        "all runs should be scoped to the current test case: {runs:#?}"
    );
    assert!(
        runs.iter()
            .all(|run| run["labels"]["fabro_test_run"] == context.test_run_id()),
        "all runs should be scoped to the current test session: {runs:#?}"
    );
    assert!(
        runs.iter().any(|run| run["status"] == "submitted"),
        "ps should include the created run: {runs:#?}"
    );
    assert!(
        runs.iter().any(|run| run["status"] == "succeeded"),
        "ps should include the completed run: {runs:#?}"
    );
}

#[test]
fn ps_quiet_outputs_run_ids_only() {
    let context = test_context!();
    setup_completed_fast_dry_run(&context);
    setup_created_fast_dry_run(&context);
    let mut cmd = context.ps();
    cmd.args(["-a", "--quiet", "--label", &context.test_case_label()]);

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

    context
        .run_cmd()
        .args([
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--label",
            "suite=alpha",
        ])
        .arg(&simple)
        .assert()
        .success();
    context
        .run_cmd()
        .args([
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--label",
            "suite=beta",
        ])
        .arg(&branching)
        .assert()
        .success();

    let output = context
        .ps()
        .args([
            "-a",
            "--json",
            "--workflow",
            "Simple",
            "--label",
            "suite=alpha",
            "--label",
            &context.test_case_label(),
        ])
        .output()
        .expect("ps should run");

    assert!(output.status.success(), "ps should succeed");
    let runs: Vec<Value> = serde_json::from_slice(&output.stdout).expect("ps JSON should parse");
    assert_eq!(
        runs.len(),
        1,
        "workflow+label filter should isolate one run"
    );
    let run = &runs[0];
    assert_eq!(run["workflow_name"], "Simple");
    assert_eq!(run["status"], "succeeded");
    assert_eq!(run["labels"]["suite"], "alpha");
    assert_eq!(run["labels"]["fabro_test_case"], context.test_case_id());
    assert_eq!(run["labels"]["fabro_test_run"], context.test_run_id());
}
