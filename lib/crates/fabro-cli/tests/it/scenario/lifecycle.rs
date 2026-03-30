use std::path::PathBuf;

use fabro_store::RuntimeState;
use fabro_test::test_context;
use serde_json::Value;

use super::{fixture, read_json, timeout_for};

#[test]
#[ignore = "scenario: requires local sandbox"]
fn local_run_lifecycle() {
    dotenvy::dotenv().ok();
    let context = test_context!();

    let cmd = |args: &[&str]| -> assert_cmd::assert::Assert {
        context
            .command()
            .args(args)
            .timeout(timeout_for("local"))
            .assert()
    };

    // 1. Run a workflow
    cmd(&[
        "run",
        "--auto-approve",
        "--no-retro",
        "--sandbox",
        "local",
        fixture("command_pipeline.fabro").to_str().unwrap(),
    ])
    .success();

    // 2. ps -a --json — should list exactly one run
    let ps_out = cmd(&["ps", "-a", "--json"]).success();
    let ps_stdout = String::from_utf8(ps_out.get_output().stdout.clone()).unwrap();
    let runs: Vec<Value> =
        serde_json::from_str(&ps_stdout).expect("ps --json should produce a JSON array");
    assert_eq!(runs.len(), 1, "should have exactly one run: {ps_stdout}");
    let run_id = runs[0]["run_id"]
        .as_str()
        .expect("run should have run_id")
        .to_string();
    assert_eq!(
        runs[0]["workflow_name"].as_str(),
        Some("CommandPipeline"),
        "workflow_name should be CommandPipeline"
    );

    // 3. inspect <run_id> — JSON array with run_record and conclusion
    let inspect_out = cmd(&["inspect", &run_id]).success();
    let inspect_stdout = String::from_utf8(inspect_out.get_output().stdout.clone()).unwrap();
    let items: Vec<Value> =
        serde_json::from_str(&inspect_stdout).expect("inspect should produce a JSON array");
    assert!(!items.is_empty(), "inspect should return at least one item");
    assert!(
        items[0]["run_record"].is_object(),
        "inspect should include run_record"
    );
    assert!(
        items[0]["conclusion"].is_object(),
        "inspect should include conclusion"
    );
    let run_dir = PathBuf::from(
        items[0]["run_dir"]
            .as_str()
            .expect("inspect should include run_dir"),
    );

    // 4. logs <run_id> — non-empty, first line is valid JSONL with event field
    let logs_out = cmd(&["logs", &run_id]).success();
    let logs_stdout = String::from_utf8(logs_out.get_output().stdout.clone()).unwrap();
    assert!(!logs_stdout.is_empty(), "logs should not be empty");
    let first_line = logs_stdout.lines().next().unwrap();
    let log_entry: Value =
        serde_json::from_str(first_line).expect("first log line should be valid JSON");
    assert!(
        log_entry["event"].is_string(),
        "first log line should have an event field"
    );

    // 5. asset list — no assets yet, should succeed with empty message
    let asset_list_out = cmd(&["asset", "list", &run_id]).success();
    let asset_list_stdout = String::from_utf8(asset_list_out.get_output().stdout.clone()).unwrap();
    assert!(
        asset_list_stdout.contains("No assets found"),
        "asset list should report no assets: {asset_list_stdout}"
    );

    // 6. Seed a synthetic asset so asset list/cp have something to work with.
    let asset_dir = RuntimeState::new(&run_dir).asset_stage_dir("step1", 1);
    std::fs::create_dir_all(&asset_dir).unwrap();
    std::fs::write(asset_dir.join("output.txt"), "asset-content-42").unwrap();
    std::fs::write(
        asset_dir.join("manifest.json"),
        r#"{"files_copied":1,"total_bytes":16,"files_skipped":0,"download_errors":0,"hash_errors":0,"captured_assets":[{"path":"output.txt","mime":"text/plain","content_md5":"f02439728c0a94b7bfc465acb1201a1f","content_sha256":"0af9dea3e1c2dec968531c18c9331659b8268e8c9cf24b01cda7b8ce51d2ff00","bytes":16}]}"#,
    )
    .unwrap();
    let retry_two_dir = RuntimeState::new(&run_dir).asset_stage_dir("step1", 2);
    std::fs::create_dir_all(&retry_two_dir).unwrap();
    std::fs::write(retry_two_dir.join("output.txt"), "asset-content-84").unwrap();
    std::fs::write(
        retry_two_dir.join("manifest.json"),
        r#"{"files_copied":1,"total_bytes":16,"files_skipped":0,"download_errors":0,"hash_errors":0,"captured_assets":[{"path":"output.txt","mime":"text/plain","content_md5":"5b4e23e40a1630f9caa15a4cb6cfb79b","content_sha256":"1f71e0df61fc3b4e1ee3aba7ceac9ae391af22595b5b5630d97d34cf33d4d540","bytes":16}]}"#,
    )
    .unwrap();

    // 7. asset list — now shows the seeded assets
    let asset_list_out2 = cmd(&["asset", "list", &run_id, "--json"]).success();
    let asset_list_stdout2 =
        String::from_utf8(asset_list_out2.get_output().stdout.clone()).unwrap();
    let assets: Vec<Value> = serde_json::from_str(&asset_list_stdout2)
        .expect("asset list --json should produce a JSON array");
    assert_eq!(
        assets.len(),
        2,
        "should have two assets: {asset_list_stdout2}"
    );
    assert_eq!(assets[0]["relative_path"].as_str(), Some("output.txt"));
    assert_eq!(assets[0]["node_slug"].as_str(), Some("step1"));
    let retry_filtered_out = cmd(&["asset", "list", &run_id, "--retry", "1", "--json"]).success();
    let retry_filtered_stdout =
        String::from_utf8(retry_filtered_out.get_output().stdout.clone()).unwrap();
    let retry_filtered_assets: Vec<Value> = serde_json::from_str(&retry_filtered_stdout)
        .expect("asset list --json should produce a JSON array");
    assert_eq!(retry_filtered_assets.len(), 1);
    assert_eq!(retry_filtered_assets[0]["retry"].as_u64(), Some(1));

    // 8. asset cp — ambiguous without --retry when multiple retries captured the same path
    let asset_dest = context.temp_dir.join("asset_copy");
    cmd(&[
        "asset",
        "cp",
        &format!("{run_id}:output.txt"),
        asset_dest.to_str().unwrap(),
    ])
    .failure();
    cmd(&[
        "asset",
        "cp",
        &format!("{run_id}:output.txt"),
        asset_dest.to_str().unwrap(),
        "--retry",
        "1",
    ])
    .success();
    let copied = std::fs::read_to_string(asset_dest.join("output.txt")).unwrap();
    assert_eq!(
        copied, "asset-content-42",
        "asset cp should copy file content"
    );

    // 9. cp — download a file from the local sandbox workdir
    let sandbox_json: Value = read_json(&run_dir.join("sandbox.json"));
    let workdir = sandbox_json["working_directory"]
        .as_str()
        .expect("sandbox.json should have working_directory");
    // Plant a file in the sandbox workdir so we can download it
    std::fs::write(
        PathBuf::from(workdir).join("cp_test.txt"),
        "downloaded-via-cp",
    )
    .unwrap();
    let cp_dest = context.temp_dir.join("cp_download.txt");
    cmd(&[
        "cp",
        &format!("{run_id}:cp_test.txt"),
        cp_dest.to_str().unwrap(),
    ])
    .success();
    let cp_content = std::fs::read_to_string(&cp_dest).unwrap();
    assert_eq!(
        cp_content, "downloaded-via-cp",
        "cp should download file from sandbox"
    );

    // 10. system df — mentions "Runs"
    let df_out = cmd(&["system", "df"]).success();
    let df_stdout = String::from_utf8(df_out.get_output().stdout.clone()).unwrap();
    assert!(
        df_stdout.contains("Runs"),
        "system df should mention Runs: {df_stdout}"
    );

    // 11. rm <run_id> — remove the run
    cmd(&["rm", &run_id]).success();

    // 12. ps -a --json — should be empty
    let ps_out2 = cmd(&["ps", "-a", "--json"]).success();
    let ps_stdout2 = String::from_utf8(ps_out2.get_output().stdout.clone()).unwrap();
    let runs2: Vec<Value> =
        serde_json::from_str(&ps_stdout2).expect("ps --json should produce a JSON array");
    assert!(
        runs2.is_empty(),
        "runs should be empty after rm: {ps_stdout2}"
    );
}
