use fabro_test::{fabro_snapshot, test_context};

use super::support::{setup_asset_run, setup_completed_dry_run};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["asset", "list", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    List assets for a workflow run

    Usage: fabro asset list [OPTIONS] <RUN_ID>

    Arguments:
      <RUN_ID>  Run ID (or prefix)

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --node <NODE>                Filter to assets from a specific node
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --retry <RETRY>              Filter to assets from a specific retry attempt
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn asset_list_empty_run_reports_no_assets() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["asset", "list", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    No assets found for this run.
    ----- stderr -----
    ");
}

#[test]
fn asset_list_json_outputs_entries() {
    let context = test_context!();
    let setup = setup_asset_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\[STORAGE_DIR\]/runs/\d{8}-\[ULID\]".to_string(),
        "[RUN_DIR]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["asset", "list", &setup.run.run_id, "--json"]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    [
      {
        "node_slug": "create_assets",
        "retry": 1,
        "relative_path": "assets/node_a/summary.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/assets/create_assets/retry_1/assets/node_a/summary.txt",
        "size": 5
      },
      {
        "node_slug": "create_assets",
        "retry": 1,
        "relative_path": "assets/shared/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/assets/create_assets/retry_1/assets/shared/report.txt",
        "size": 3
      },
      {
        "node_slug": "create_colliding",
        "retry": 1,
        "relative_path": "assets/other/summary.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/assets/create_colliding/retry_1/assets/other/summary.txt",
        "size": 4
      },
      {
        "node_slug": "create_colliding",
        "retry": 1,
        "relative_path": "assets/retry/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/assets/create_colliding/retry_1/assets/retry/report.txt",
        "size": 6
      },
      {
        "node_slug": "retry_assets",
        "retry": 1,
        "relative_path": "assets/retry/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/assets/retry_assets/retry_1/assets/retry/report.txt",
        "size": 5
      },
      {
        "node_slug": "retry_assets",
        "retry": 2,
        "relative_path": "assets/retry/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/assets/retry_assets/retry_2/assets/retry/report.txt",
        "size": 6
      }
    ]
    ----- stderr -----
    "#);
}

#[test]
fn asset_list_filters_by_node_and_retry() {
    let context = test_context!();
    let setup = setup_asset_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\[STORAGE_DIR\]/runs/\d{8}-\[ULID\]".to_string(),
        "[RUN_DIR]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args([
        "asset",
        "list",
        &setup.run.run_id,
        "--node",
        "retry_assets",
        "--retry",
        "2",
        "--json",
    ]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    [
      {
        "node_slug": "retry_assets",
        "retry": 2,
        "relative_path": "assets/retry/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/assets/retry_assets/retry_2/assets/retry/report.txt",
        "size": 6
      }
    ]
    ----- stderr -----
    "#);
}
