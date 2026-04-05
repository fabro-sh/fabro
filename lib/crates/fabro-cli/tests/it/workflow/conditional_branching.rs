use fabro_test::test_context;

use super::{completed_nodes, find_run_dir, fixture, read_conclusion, sandbox_tests, timeout_for};

sandbox_tests!(conditional_branching);

fn scenario_conditional_branching(sandbox: &str) {
    let context = test_context!();

    context
        .run_cmd()
        .args(["--auto-approve", "--no-retro", "--sandbox", sandbox])
        .arg(fixture("conditional_branching.fabro"))
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&context);
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
