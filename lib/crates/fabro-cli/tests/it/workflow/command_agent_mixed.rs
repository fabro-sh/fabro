use fabro_test::test_context;

use super::{
    completed_nodes, find_run_dir, fixture, read_conclusion, sandbox_tests, store_dump_export,
    timeout_for,
};

sandbox_tests!(command_agent_mixed, keys = ["ANTHROPIC_API_KEY"]);

fn scenario_command_agent_mixed(sandbox: &str) {
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
        .arg(fixture("command_agent_mixed.fabro"))
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&context);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("success"));

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"setup".to_string()),
        "setup should be completed"
    );
    assert!(
        nodes.contains(&"work".to_string()),
        "work should be completed"
    );
    assert!(
        nodes.contains(&"verify".to_string()),
        "verify should be completed"
    );

    let export_dir = store_dump_export(&context, &run_dir.file_name().unwrap().to_string_lossy());
    let stdout = std::fs::read_to_string(export_dir.join("nodes/verify/visit-1/stdout.log"))
        .expect("verify stdout.log should exist");
    assert!(
        stdout.contains("SCENARIO_FLAG_42"),
        "verify stdout should contain SCENARIO_FLAG_42, got: {stdout}"
    );
}
