use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::Value;

use super::support::{setup_completed_fast_dry_run, setup_created_fast_dry_run};
use crate::support::unique_run_id;

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
          --json                 Output as JSON [env: FABRO_JSON=]
          --server <SERVER>      Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --before <BEFORE>      Only include runs started before this date (YYYY-MM-DD prefix match)
          --debug                Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check     Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --workflow <WORKFLOW>  Filter by workflow name (substring match)
          --label <KEY=VALUE>    Filter by label (KEY=VALUE, repeatable, AND semantics)
          --orphans              Include orphan directories (no matching durable run)
          --verbose              Enable verbose output [env: FABRO_VERBOSE=]
      -a, --all                  Show all runs, not just running (like docker ps -a)
      -q, --quiet                Only display run IDs
      -h, --help                 Print help
    ----- stderr -----
    ");
}

#[test]
fn ps_accepts_local_tcp_server_target() {
    let context = test_context!();
    let storage_root = tempfile::tempdir_in("/tmp").unwrap();
    let storage_dir = storage_root.path().join("storage");
    std::fs::create_dir_all(&storage_dir).unwrap();

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "start", "--bind", "127.0.0.1"])
        .assert()
        .success();

    let status_output = context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status_json: Value = serde_json::from_slice(&status_output).unwrap();
    let bind = status_json["bind"]
        .as_str()
        .expect("bind should be present");

    let output = context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["ps", "-a", "--json", "--server", &format!("http://{bind}")])
        .output()
        .expect("ps should run");

    context
        .command()
        .env("FABRO_STORAGE_DIR", &storage_dir)
        .args(["server", "stop"])
        .assert()
        .success();

    assert!(
        output.status.success(),
        "ps against local TCP target failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let runs: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(runs.is_empty(), "new local TCP server should have no runs");
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
fn setup_completed_fast_dry_run_preserves_handle_when_another_run_exists() {
    let context = test_context!();

    let created = setup_created_fast_dry_run(&context);
    let completed = setup_completed_fast_dry_run(&context);

    assert_ne!(created.run_id, completed.run_id);
    assert_ne!(created.run_dir, completed.run_dir);
    assert!(created.run_dir.exists(), "created run dir should exist");
    assert!(completed.run_dir.exists(), "completed run dir should exist");
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
    let simple = context.temp_dir.join("simple.fabro");
    let branching = context.temp_dir.join("branching.fabro");
    context.write_temp(
        "simple.fabro",
        r#"digraph Simple {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  run [shape=parallelogram, script="true"]
  start -> run -> exit
}
"#,
    );
    context.write_temp(
        "branching.fabro",
        r#"digraph Branching {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  run [shape=parallelogram, script="true"]
  start -> run -> exit
}
"#,
    );

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
        .create_cmd()
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

#[test]
fn ps_uses_configured_server_target_without_server_flag() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let mock = server.mock(|when, then| {
        when.method("GET").path("/api/v1/runs");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "data": [{
                        "run_id": run_id,
                        "workflow_name": "Remote Workflow",
                        "workflow_slug": "remote-workflow",
                        "goal": "Remote goal",
                        "title": "Remote goal",
                        "labels": {
                            "suite": "remote"
                        },
                        "host_repo_path": "/srv/repo",
                        "repository": { "name": "repo" },
                        "start_time": "2026-04-05T12:00:00Z",
                        "created_at": "2026-04-05T12:00:00Z",
                        "status": "succeeded",
                        "status_reason": null,
                        "duration_ms": 123,
                        "total_usd_micros": null
                    }],
                    "meta": { "has_more": false }
                })
                .to_string(),
            );
    });
    context.write_home(
        ".fabro/settings.toml",
        format!(
            "_version = 1\n\n[cli.target]\ntype = \"http\"\nurl = \"{}/api/v1\"\n",
            server.base_url()
        ),
    );

    let output = context
        .ps()
        .args(["-a", "--json"])
        .output()
        .expect("ps should execute");

    assert!(output.status.success(), "ps should succeed");
    let runs: Vec<Value> = serde_json::from_slice(&output.stdout).expect("ps JSON should parse");
    mock.assert();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["workflow_name"], "Remote Workflow");
    assert_eq!(runs[0]["host_repo_path"], "/srv/repo");
}
