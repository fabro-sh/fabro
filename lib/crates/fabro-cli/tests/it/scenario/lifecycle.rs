#![allow(
    clippy::absolute_paths,
    reason = "This test module prefers explicit type paths over extra imports."
)]
#![expect(
    clippy::disallowed_methods,
    reason = "Integration tests stage fixtures with sync std::fs calls."
)]

use fabro_test::test_context;
use serde_json::Value;

use super::{fixture, run_state, timeout_for};
use crate::support::{fabro_json_snapshot, unique_run_id};

#[fabro_macros::e2e_test()]
fn local_run_lifecycle() {
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
    let label = context.test_case_label();
    let ps_out = cmd(&["ps", "-a", "--json", "--label", &label]).success();
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

    // 3. inspect <run_id> — JSON array with run_spec and conclusion
    let inspect_out = cmd(&["inspect", &run_id]).success();
    let inspect_stdout = String::from_utf8(inspect_out.get_output().stdout.clone()).unwrap();
    let items: Vec<Value> =
        serde_json::from_str(&inspect_stdout).expect("inspect should produce a JSON array");
    assert!(!items.is_empty(), "inspect should return at least one item");
    assert!(
        items[0]["run_spec"].is_object(),
        "inspect should include run_spec"
    );
    assert!(
        items[0]["conclusion"].is_object(),
        "inspect should include conclusion"
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

    // 5. artifact list — no assets yet, should succeed with empty message
    let artifact_list_out = cmd(&["artifact", "list", &run_id]).success();
    let artifact_list_stdout =
        String::from_utf8(artifact_list_out.get_output().stdout.clone()).unwrap();
    assert!(
        artifact_list_stdout.contains("No artifacts found"),
        "artifact list should report no artifacts: {artifact_list_stdout}"
    );

    // 6. system df — mentions "Runs"
    let df_out = cmd(&["system", "df"]).success();
    let df_stdout = String::from_utf8(df_out.get_output().stdout.clone()).unwrap();
    assert!(
        df_stdout.contains("Runs"),
        "system df should mention Runs: {df_stdout}"
    );

    // 7. rm <run_id> — remove the run
    cmd(&["rm", &run_id]).success();

    // 8. ps -a --json — should be empty
    let ps_out2 = cmd(&["ps", "-a", "--json", "--label", &label]).success();
    let ps_stdout2 = String::from_utf8(ps_out2.get_output().stdout.clone()).unwrap();
    let runs2: Vec<Value> =
        serde_json::from_str(&ps_stdout2).expect("ps --json should produce a JSON array");
    assert!(
        runs2.is_empty(),
        "runs should be empty after rm: {ps_stdout2}"
    );
}

#[test]
fn dry_run_create_start_attach_works_with_default_run_lookup() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow = context.install_fixture("simple.fabro");

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow.to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["start", &run_id])
        .assert()
        .success();
    context
        .command()
        .args(["attach", &run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    let state = run_state(&run_dir);
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "status": state.status.map(|status| status.status),
            "has_conclusion": state.conclusion.is_some(),
        }),
        @r#"
        {
          "status": "succeeded",
          "has_conclusion": true
        }
        "#
    );
}

#[test]
fn dry_run_detach_attach_works_with_default_run_lookup() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow = context.install_fixture("simple.fabro");

    context
        .command()
        .args([
            "run",
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow.to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["attach", &run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    let state = run_state(&run_dir);
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "run_dir": run_dir,
            "has_conclusion": state.conclusion.is_some(),
        }),
        @r#"
    {
      "run_dir": "[RUN_DIR]",
      "has_conclusion": true
    }
    "#
    );
}

#[test]
fn completed_run_can_be_attached_by_workflow_slug() {
    let context = test_context!();
    let project = tempfile::tempdir().unwrap();
    let workflow_dir = project.path().join("workflows").join("sluggy");
    let workflow_path = workflow_dir.join("workflow.fabro");
    let run_id = unique_run_id();

    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::write(
        &workflow_path,
        "\
digraph BarBaz {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    )
    .unwrap();

    context
        .command()
        .current_dir(project.path())
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    context
        .command()
        .current_dir(project.path())
        .args(["start", "sluggy"])
        .assert()
        .success();
    context
        .command()
        .current_dir(project.path())
        .args(["attach", &run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();
    context
        .command()
        .current_dir(project.path())
        .args(["attach", "sluggy"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    let state = run_state(&run_dir);
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "run_dir_exists": run_dir.exists(),
            "has_conclusion": state.conclusion.is_some(),
        }),
        @r#"
        {
          "run_dir_exists": true,
          "has_conclusion": true
        }
        "#
    );
}

#[test]
fn completed_run_can_be_attached_by_file_stem() {
    let context = test_context!();
    let workflow_dir = tempfile::tempdir().unwrap();
    let workflow_path = workflow_dir.path().join("alpha.fabro");
    let run_id = unique_run_id();

    std::fs::write(
        &workflow_path,
        "\
digraph FooWorkflow {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    )
    .unwrap();

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    context
        .command()
        .args(["start", "alpha"])
        .assert()
        .success();
    context
        .command()
        .args(["attach", "alpha"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_spec = run_state(&context.find_run_dir(&run_id))
        .spec
        .expect("run spec should exist");
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "graph_name": run_spec.graph.name,
            "workflow_slug": run_spec.workflow_slug,
        }),
        @r#"
        {
          "graph_name": "FooWorkflow",
          "workflow_slug": "alpha"
        }
        "#
    );
}
