use fabro_test::test_context;

use super::{completed_nodes, find_run_dir, fixture, read_conclusion, sandbox_tests, timeout_for};

sandbox_tests!(human_gate);

fn scenario_human_gate(sandbox: &str) {
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
        .arg(fixture("human_gate.fabro"))
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&context.storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("success"));

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"ship".to_string()),
        "ship should be in completed_nodes (auto-approve picks first edge): {nodes:?}"
    );
    assert!(
        !nodes.contains(&"revise".to_string()),
        "revise should NOT be in completed_nodes: {nodes:?}"
    );
}
