use std::time::Duration;

use fabro_test::{fabro_snapshot, test_context};

use crate::cmd::support::{read_text, setup_artifact_run, text_tree};

fn artifact_filters(context: &fabro_test::TestContext) -> Vec<(String, String)> {
    let mut filters = context.filters();
    filters.push((
        r"\[STORAGE_DIR\]/runs/\d{8}-\[ULID\]".to_string(),
        "[RUN_DIR]".to_string(),
    ));
    filters
}

#[test]
fn artifact_commands_share_populated_run_fixture() {
    let context = test_context!();
    let setup = setup_artifact_run(&context);
    let filters = artifact_filters(&context);

    let mut list_json = context.command();
    list_json.args(["artifact", "list", &setup.run.run_id, "--json"]);
    fabro_snapshot!(filters.clone(), list_json, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    [
      {
        "node_slug": "create_assets",
        "retry": 1,
        "relative_path": "assets/node_a/summary.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/files/create_assets/retry_1/assets/node_a/summary.txt",
        "size": 5
      },
      {
        "node_slug": "create_assets",
        "retry": 1,
        "relative_path": "assets/shared/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/files/create_assets/retry_1/assets/shared/report.txt",
        "size": 3
      },
      {
        "node_slug": "create_colliding",
        "retry": 1,
        "relative_path": "assets/other/summary.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/files/create_colliding/retry_1/assets/other/summary.txt",
        "size": 4
      },
      {
        "node_slug": "create_colliding",
        "retry": 1,
        "relative_path": "assets/retry/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/files/create_colliding/retry_1/assets/retry/report.txt",
        "size": 6
      },
      {
        "node_slug": "retry_assets",
        "retry": 1,
        "relative_path": "assets/retry/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/files/retry_assets/retry_1/assets/retry/report.txt",
        "size": 5
      },
      {
        "node_slug": "retry_assets",
        "retry": 2,
        "relative_path": "assets/retry/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/files/retry_assets/retry_2/assets/retry/report.txt",
        "size": 6
      }
    ]
    ----- stderr -----
    "#);

    let mut list_filtered = context.command();
    list_filtered.args([
        "artifact",
        "list",
        &setup.run.run_id,
        "--node",
        "retry_assets",
        "--retry",
        "2",
        "--json",
    ]);
    fabro_snapshot!(filters.clone(), list_filtered, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    [
      {
        "node_slug": "retry_assets",
        "retry": 2,
        "relative_path": "assets/retry/report.txt",
        "absolute_path": "[RUN_DIR]/cache/artifacts/files/retry_assets/retry_2/assets/retry/report.txt",
        "size": 6
      }
    ]
    ----- stderr -----
    "#);

    let single_dest = context.temp_dir.join("artifact-one");
    let mut cp_single = context.command();
    cp_single.args([
        "artifact",
        "cp",
        &format!("{}:assets/shared/report.txt", setup.run.run_id),
        single_dest.to_str().unwrap(),
        "--node",
        "create_assets",
    ]);
    fabro_snapshot!(context.filters(), cp_single, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Copied assets/shared/report.txt to [TEMP_DIR]/artifact-one/report.txt
    ----- stderr -----
    ");
    assert_eq!(read_text(&single_dest.join("report.txt")), "one");

    let tree_dest = context.temp_dir.join("artifact-tree");
    let mut cp_tree = context.command();
    cp_tree.args([
        "artifact",
        "cp",
        &setup.run.run_id,
        tree_dest.to_str().unwrap(),
        "--tree",
    ]);
    cp_tree.timeout(Duration::from_secs(30));
    fabro_snapshot!(context.filters(), cp_tree, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Copied 6 artifact(s) to [TEMP_DIR]/artifact-tree
    ----- stderr -----
    ");
    insta::assert_snapshot!(
        text_tree(&tree_dest).join("\n"),
        @r"
        create_assets/retry_1/assets/node_a/summary.txt = alpha
        create_assets/retry_1/assets/shared/report.txt = one
        create_colliding/retry_1/assets/other/summary.txt = beta
        create_colliding/retry_1/assets/retry/report.txt = second
        retry_assets/retry_1/assets/retry/report.txt = first
        retry_assets/retry_2/assets/retry/report.txt = second
        "
    );

    let ambiguous_dest = context.temp_dir.join("artifact-ambiguous");
    let mut cp_ambiguous = context.command();
    cp_ambiguous.args([
        "artifact",
        "cp",
        &format!("{}:assets/retry/report.txt", setup.run.run_id),
        ambiguous_dest.to_str().unwrap(),
    ]);
    fabro_snapshot!(context.filters(), cp_ambiguous, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Path 'assets/retry/report.txt' matches multiple artifacts: create_colliding:retry_1, retry_assets:retry_1, retry_assets:retry_2. Use --node and/or --retry to disambiguate.
    ");

    let flat_dest = context.temp_dir.join("artifact-flat");
    let mut cp_flat = context.command();
    cp_flat.args(["artifact", "cp", &setup.run.run_id, flat_dest.to_str().unwrap()]);
    fabro_snapshot!(context.filters(), cp_flat, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Filename collision: 'summary.txt' exists in both create_assets:retry_1 and create_colliding:retry_1. Use --tree to preserve directory structure, or --node and/or --retry to filter.
    ");
}
