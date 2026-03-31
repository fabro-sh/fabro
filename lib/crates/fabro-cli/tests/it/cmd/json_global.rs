use std::process::Command;

use fabro_test::test_context;
use serde_json::Value;

use super::support::{fixture, output_stderr, output_stdout, setup_completed_dry_run};

fn dot_is_available() -> bool {
    Command::new("dot")
        .arg("-V")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[test]
fn completion_rejects_json() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "completion", "zsh"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json is not supported for this command"));
}

#[test]
fn settings_json_outputs_parseable_json() {
    let context = test_context!();
    let output = context
        .settings()
        .arg("--json")
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("settings --json should parse");
    assert!(value.is_object());
}

#[test]
fn ps_supports_global_flag_and_env_var() {
    let context = test_context!();
    setup_completed_dry_run(&context);

    let global_output = context
        .command()
        .args(["--json", "ps", "-a"])
        .output()
        .expect("command should run");
    assert!(global_output.status.success());
    let global_runs: Value =
        serde_json::from_slice(&global_output.stdout).expect("global --json should parse");
    assert!(global_runs.as_array().is_some_and(|runs| !runs.is_empty()));

    let env_output = context
        .command()
        .env("FABRO_JSON", "1")
        .args(["ps", "-a"])
        .output()
        .expect("command should run");
    assert!(env_output.status.success());
    let env_runs: Value =
        serde_json::from_slice(&env_output.stdout).expect("FABRO_JSON output should parse");
    assert_eq!(global_runs, env_runs);
}

#[test]
fn logs_json_wins_over_pretty() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);

    let output = context
        .command()
        .args(["--json", "logs", "--pretty", &run.run_id])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let first_line = stdout.lines().find(|line| !line.is_empty()).unwrap();
    let value: Value = serde_json::from_str(first_line).expect("logs output should remain JSONL");
    assert!(value.get("event").is_some());
}

#[test]
fn graph_json_without_output_is_rejected() {
    let context = test_context!();
    let workflow = fixture("simple.fabro");

    let output = context
        .command()
        .args(["--json", "graph", workflow.to_str().unwrap()])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = output_stderr(&output);
    assert!(stderr.contains("--json is not supported for this command"));
}

#[test]
fn graph_json_with_output_reports_file() {
    if !dot_is_available() {
        return;
    }

    let context = test_context!();
    let output_path = context.temp_dir.join("graph.svg");
    let workflow = fixture("simple.fabro");

    let output = context
        .command()
        .args([
            "--json",
            "graph",
            workflow.to_str().unwrap(),
            "--output",
            output_path.to_str().unwrap(),
        ])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output_stdout(&output),
        output_stderr(&output)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("graph JSON should parse");
    assert_eq!(value["format"], "svg");
    assert_eq!(value["path"], output_path.to_string_lossy().to_string());
    assert!(output_path.exists());
}

#[test]
fn secret_list_json_missing_env_is_empty_array() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "secret", "list"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("secret list should parse");
    assert_eq!(value, Value::Array(vec![]));
}
