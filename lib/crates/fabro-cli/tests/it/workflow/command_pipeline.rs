#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use fabro_test::test_context;

use super::{
    completed_nodes, dump_export, find_run_dir, fixture, read_conclusion, run_id_for,
    sandbox_tests, stage_dump_dir, timeout_for,
};

sandbox_tests!(command_pipeline);

fn scenario_command_pipeline(sandbox: &str) {
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

    let run_dir = find_run_dir(&context);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(
        conclusion["status"].as_str(),
        Some("succeeded"),
        "conclusion status should be succeeded"
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

    let export_dir = dump_export(&context, &run_id_for(&run_dir));
    let stdout1 =
        std::fs::read_to_string(stage_dump_dir(&export_dir, "step1@1").join("stdout.log"))
            .expect("step1 stdout.log should exist");
    assert!(
        stdout1.contains("hello-from-step1"),
        "step1 stdout should contain hello-from-step1, got: {stdout1}"
    );
}
