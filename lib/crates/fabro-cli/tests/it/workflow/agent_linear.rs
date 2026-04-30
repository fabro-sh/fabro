use fabro_test::test_context;

use super::{
    completed_nodes, find_run_dir, fixture, has_event, read_conclusion, sandbox_tests, timeout_for,
};

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

    let run_dir = find_run_dir(&context);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("succeeded"));

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"work".to_string()),
        "work should be completed"
    );

    assert!(
        has_event(&run_dir, "stage.prompt"),
        "progress should contain stage.prompt"
    );
    assert!(
        has_event(&run_dir, "stage.completed"),
        "progress should contain stage.completed"
    );
}
