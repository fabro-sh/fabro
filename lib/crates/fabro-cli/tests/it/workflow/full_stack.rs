use fabro_test::test_context;

use super::{
    completed_nodes, find_run_dir, fixture, has_event, read_conclusion, read_json, sandbox_tests,
    timeout_for,
};

sandbox_tests!(full_stack);

fn scenario_full_stack(sandbox: &str) {
    dotenvy::dotenv().ok();
    let context = test_context!();

    context
        .run_cmd()
        .args([
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            sandbox,
            "--model",
            "claude-haiku-4-5",
        ])
        .arg(fixture("full_stack.fabro"))
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&context.storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(
        conclusion["status"].as_str(),
        Some("success"),
        "conclusion: {conclusion}"
    );
    assert!(
        conclusion["duration_ms"].as_u64().unwrap_or(0) > 0,
        "duration_ms should be > 0"
    );

    // RunRecord should have key fields
    let run_record = read_json(&run_dir.join("run.json"));
    assert!(
        run_record["run_id"].as_str().is_some(),
        "run record should have run_id"
    );
    assert!(
        run_record["graph"]["name"].as_str().is_some(),
        "run record should have graph.name"
    );

    // Progress events
    assert!(
        has_event(&run_dir, "run.started"),
        "progress should contain run.started"
    );
    assert!(
        has_event(&run_dir, "run.completed"),
        "progress should contain run.completed"
    );

    // All expected nodes completed
    let nodes = completed_nodes(&run_dir);
    for expected in &["setup", "plan", "approve", "impl", "verify"] {
        assert!(
            nodes.contains(&expected.to_string()),
            "{expected} should be in completed_nodes: {nodes:?}"
        );
    }

    // Verify node stdout should contain PASS
    let stdout = std::fs::read_to_string(run_dir.join("nodes/verify/stdout.log"))
        .expect("verify stdout.log should exist");
    assert!(
        stdout.contains("PASS"),
        "verify stdout should contain PASS, got: {stdout}"
    );
}
