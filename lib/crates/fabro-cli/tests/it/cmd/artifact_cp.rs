use fabro_test::{fabro_snapshot, test_context};

use super::support::{read_text, setup_artifact_run, setup_completed_dry_run, text_tree};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["artifact", "cp", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Copy artifacts from a workflow run

    Usage: fabro artifact cp [OPTIONS] <SOURCE> [DEST]

    Arguments:
      <SOURCE>  Source: RUN_ID (all artifacts) or RUN_ID:path (specific artifact)
      [DEST]    Destination directory (defaults to current directory) [default: .]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --node <NODE>                Filter to artifacts from a specific node
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --retry <RETRY>              Filter to artifacts from a specific retry attempt
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --tree                       Preserve {node_slug}/retry_{N}/ directory structure
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn artifact_cp_empty_run_reports_no_artifacts() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let dest = context.temp_dir.join("artifact-dest");
    let mut cmd = context.command();
    cmd.args(["artifact", "cp", &run.run_id, dest.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: No artifacts found for this run
    ");
}

#[test]
fn artifact_cp_specific_path_copies_single_asset() {
    let context = test_context!();
    let setup = setup_artifact_run(&context);
    let dest = context.temp_dir.join("artifact-one");
    let mut cmd = context.command();
    cmd.args([
        "artifact",
        "cp",
        &format!("{}:assets/shared/report.txt", setup.run.run_id),
        dest.to_str().unwrap(),
        "--node",
        "create_assets",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Copied assets/shared/report.txt to [TEMP_DIR]/artifact-one/report.txt
    ----- stderr -----
    ");
    assert_eq!(read_text(&dest.join("report.txt")), "one");
}

#[test]
fn artifact_cp_ambiguous_path_requires_node_or_retry() {
    let context = test_context!();
    let setup = setup_artifact_run(&context);
    let dest = context.temp_dir.join("artifact-one");
    let mut cmd = context.command();
    cmd.args([
        "artifact",
        "cp",
        &format!("{}:assets/retry/report.txt", setup.run.run_id),
        dest.to_str().unwrap(),
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Path 'assets/retry/report.txt' matches multiple artifacts: create_colliding:retry_1, retry_assets:retry_1, retry_assets:retry_2. Use --node and/or --retry to disambiguate.
    ");
}

#[test]
fn artifact_cp_tree_preserves_structure() {
    let context = test_context!();
    let setup = setup_artifact_run(&context);
    let dest = context.temp_dir.join("artifact-tree");
    let mut cmd = context.command();
    cmd.args([
        "artifact",
        "cp",
        &setup.run.run_id,
        dest.to_str().unwrap(),
        "--tree",
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Copied 6 artifact(s) to [TEMP_DIR]/artifact-tree
    ----- stderr -----
    ");
    insta::assert_snapshot!(
        text_tree(&dest).join("\n"),
        @r"
        create_assets/retry_1/assets/node_a/summary.txt = alpha
        create_assets/retry_1/assets/shared/report.txt = one
        create_colliding/retry_1/assets/other/summary.txt = beta
        create_colliding/retry_1/assets/retry/report.txt = second
        retry_assets/retry_1/assets/retry/report.txt = first
        retry_assets/retry_2/assets/retry/report.txt = second
        "
    );
}

#[test]
fn artifact_cp_flat_mode_rejects_filename_collisions() {
    let context = test_context!();
    let setup = setup_artifact_run(&context);
    let dest = context.temp_dir.join("artifact-flat");
    let mut cmd = context.command();
    cmd.args(["artifact", "cp", &setup.run.run_id, dest.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Filename collision: 'summary.txt' exists in both create_assets:retry_1 and create_colliding:retry_1. Use --tree to preserve directory structure, or --node and/or --retry to filter.
    ");
}
