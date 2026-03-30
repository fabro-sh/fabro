use fabro_test::test_context;

use super::{
    completed_nodes, find_run_dir, fixture, has_event, read_conclusion, read_json, scenario_tests,
    timeout_for,
};

// 1. command_pipeline — two command nodes in sequence, no LLM
scenario_tests!(command_pipeline);

fn scenario_command_pipeline(sandbox: &str) {
    dotenvy::dotenv().ok();
    let context = test_context!();

    context
        .validate()
        .arg(fixture("command_pipeline.fabro"))
        .assert()
        .success();

    context
        .run_cmd()
        .args(["--auto-approve", "--no-retro", "--sandbox", sandbox])
        .arg(fixture("command_pipeline.fabro"))
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&context.storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(
        conclusion["status"].as_str(),
        Some("success"),
        "conclusion status should be success"
    );

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"step1".to_string()),
        "step1 should be completed"
    );
    assert!(
        nodes.contains(&"step2".to_string()),
        "step2 should be completed"
    );

    // Verify step1 stdout
    let stdout1 = std::fs::read_to_string(run_dir.join("nodes/step1/stdout.log"))
        .expect("step1 stdout.log should exist");
    assert!(
        stdout1.contains("hello-from-step1"),
        "step1 stdout should contain hello-from-step1, got: {stdout1}"
    );
}

// 2. conditional_branching — command + diamond gate, success path taken
scenario_tests!(conditional_branching);

fn scenario_conditional_branching(sandbox: &str) {
    dotenvy::dotenv().ok();
    let context = test_context!();

    context
        .run_cmd()
        .args(["--auto-approve", "--no-retro", "--sandbox", sandbox])
        .arg(fixture("conditional_branching.fabro"))
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&context.storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("success"));

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"passed".to_string()),
        "passed node should be in completed_nodes: {nodes:?}"
    );
    assert!(
        !nodes.contains(&"failed".to_string()),
        "failed node should NOT be in completed_nodes: {nodes:?}"
    );
}

// 3. agent_linear — single agent node with LLM
scenario_tests!(agent_linear);

fn scenario_agent_linear(sandbox: &str) {
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

    // Agent node should produce prompt.md and response.md
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

// 4. human_gate — human gate with --auto-approve selects first edge
scenario_tests!(human_gate);

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

// 5. command_agent_mixed — command writes file, agent reads it, command verifies
scenario_tests!(command_agent_mixed);

fn scenario_command_agent_mixed(sandbox: &str) {
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
        .arg(fixture("command_agent_mixed.fabro"))
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&context.storage_dir);
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

    // Verify command node saw the flag
    let stdout = std::fs::read_to_string(run_dir.join("nodes/verify/stdout.log"))
        .expect("verify stdout.log should exist");
    assert!(
        stdout.contains("SCENARIO_FLAG_42"),
        "verify stdout should contain SCENARIO_FLAG_42, got: {stdout}"
    );
}

// 6. full_stack — command + agent + human gate + goal_gate, kitchen sink
scenario_tests!(full_stack);

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
        has_event(&run_dir, "WorkflowRunStarted"),
        "progress should contain WorkflowRunStarted"
    );
    assert!(
        has_event(&run_dir, "WorkflowRunCompleted"),
        "progress should contain WorkflowRunCompleted"
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
