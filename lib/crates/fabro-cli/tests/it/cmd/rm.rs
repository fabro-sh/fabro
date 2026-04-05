use fabro_test::{fabro_snapshot, test_context};
use serde_json::Value;

use super::support::{setup_completed_fast_dry_run, setup_created_fast_dry_run, setup_local_sandbox_run};
use walkdir::WalkDir;

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
      -f, --force                      Force removal of active runs
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn rm_deletes_completed_run() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
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
    ps.args(["-a", "--json", "--label", &context.test_case_label()]);
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
    let run = setup_created_fast_dry_run(&context);
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
    let run = setup_created_fast_dry_run(&context);
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
    ps.args(["-a", "--json", "--label", &context.test_case_label()]);
    fabro_snapshot!(context.filters(), ps, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    []
    ----- stderr -----
    "###);
}

#[test]
fn rm_force_deletes_run_without_sandbox_json_when_store_has_sandbox() {
    let context = test_context!();
    let setup = setup_local_sandbox_run(&context);
    let _ = std::fs::remove_file(setup.run.run_dir.join("sandbox.json"));

    let mut filters = context.filters();
    filters.push((
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["rm", "--force", &setup.run.run_id]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    [ULID]
    ");
    assert!(
        !setup.run.run_dir.exists(),
        "run directory should be deleted even without sandbox.json"
    );
}

#[test]
fn rm_partial_failure_reports_which_identifiers_failed() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
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

#[test]
fn rm_partial_failure_json_includes_removed_and_errors() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);

    let output = context
        .command()
        .args(["--json", "rm", &run.run_id, "does-not-exist"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("rm JSON should parse");
    assert_eq!(
        value["removed"],
        Value::Array(vec![Value::String(run.run_id.clone())])
    );
    assert_eq!(value["errors"][0]["identifier"], "does-not-exist");
    assert!(
        value["errors"][0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("does-not-exist"))
    );
    assert!(
        !run.run_dir.exists(),
        "existing run should still be removed"
    );
}

#[test]
fn rm_json_removes_run_when_store_locator_is_corrupt() {
    let context = test_context!();
    let run = setup_completed_fast_dry_run(&context);
    let by_id_path = find_store_catalog_entry(&context.storage_dir.join("store"), &run.run_id);
    let original = std::fs::read(&by_id_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", by_id_path.display()));

    // Corrupt the by-id locator. Deletion should still succeed via the by-start
    // fallback path instead of surfacing a false partial failure.
    std::fs::write(&by_id_path, b"{not valid json")
        .unwrap_or_else(|err| panic!("failed to corrupt {}: {err}", by_id_path.display()));
    scopeguard::defer! {
        let _ = std::fs::write(&by_id_path, &original);
    }

    let output = context
        .command()
        .args(["--json", "rm", &run.run_id])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "rm should still succeed when the locator is corrupt"
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("rm JSON should parse");
    assert_eq!(
        value["removed"],
        Value::Array(vec![Value::String(run.run_id.clone())])
    );
    assert_eq!(value["errors"], Value::Array(Vec::new()));
    assert!(
        !run.run_dir.exists(),
        "run directory should still be deleted"
    );
}

fn find_store_catalog_entry(root: &std::path::Path, run_id: &str) -> std::path::PathBuf {
    let expected_name = format!("{run_id}.json");
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy() == expected_name)
                && path
                    .components()
                    .any(|component| component.as_os_str() == "by-id")
        })
        .unwrap_or_else(|| {
            panic!(
                "missing by-id catalog entry for {run_id} under {}",
                root.display()
            )
        })
}
