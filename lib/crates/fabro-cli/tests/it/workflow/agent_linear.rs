use fabro_test::test_context;

use super::{completed_nodes, find_run_dir, fixture, read_conclusion, sandbox_tests, timeout_for};

sandbox_tests!(agent_linear, keys = ["ANTHROPIC_API_KEY"]);

fn scenario_agent_linear(sandbox: &str) {
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
        .arg(fixture("agent_linear.fabro"))
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&context.storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("success"));

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"work".to_string()),
        "work should be completed"
    );

    let prompt_path = run_dir.join("nodes/work/prompt.md");
    assert!(prompt_path.exists(), "nodes/work/prompt.md should exist");

    let response_path = run_dir.join("nodes/work/response.md");
    assert!(
        response_path.exists(),
        "nodes/work/response.md should exist"
    );
    let response = std::fs::read_to_string(&response_path).unwrap();
    assert!(!response.is_empty(), "response.md should not be empty");
}
