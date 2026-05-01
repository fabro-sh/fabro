use fabro_test::test_context;
use serde_json::Value;

use super::support::{fixture, output_stderr, output_stdout, setup_seeded_completed_dry_run};

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
fn settings_uses_json_output_format_from_home_config() {
    let context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "_version = 1\n\n[cli.output]\nformat = \"json\"\n",
    );

    let output = context.settings().output().expect("command should run");

    assert!(output.status.success());
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("settings config JSON should parse");
    assert!(value.is_object());
}

#[test]
fn auth_status_ignores_json_output_format_from_home_config() {
    let context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "_version = 1\n\n[cli.output]\nformat = \"json\"\n",
    );

    let output = context
        .command()
        .args(["auth", "status"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout should stay empty in text mode"
    );
    let stderr = output_stderr(&output);
    assert!(stderr.contains("Not logged in to any servers."));
    assert!(!stderr.contains("FABRO_DEV_TOKEN:"));
}

#[test]
fn secret_list_uses_json_output_format_from_home_config() {
    let context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "_version = 1\n\n[cli.output]\nformat = \"json\"\n",
    );

    let output = context
        .command()
        .args(["secret", "list"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("secret list config JSON should parse");
    assert!(value.is_array(), "secret list JSON should be an array");
}

#[test]
fn completion_succeeds_with_json_output_format_from_home_config() {
    let context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "_version = 1\n\n[cli.output]\nformat = \"json\"\n",
    );

    let output = context
        .command()
        .args(["completion", "zsh"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let stdout = output_stdout(&output);
    assert!(stdout.contains("#compdef"));
    assert!(serde_json::from_slice::<Value>(&output.stdout).is_err());
}

#[test]
fn ps_supports_global_flag_and_env_var() {
    let context = test_context!();
    setup_seeded_completed_dry_run(&context);
    let test_case_label = context.test_case_label();

    let global_output = context
        .command()
        .args(["--json", "ps", "-a", "--label", &test_case_label])
        .output()
        .expect("command should run");
    assert!(global_output.status.success());
    let global_runs: Value =
        serde_json::from_slice(&global_output.stdout).expect("global --json should parse");
    assert!(global_runs.as_array().is_some_and(|runs| !runs.is_empty()));

    let env_output = context
        .command()
        .env("FABRO_JSON", "1")
        .args(["ps", "-a", "--label", &test_case_label])
        .output()
        .expect("command should run");
    assert!(env_output.status.success());
    let env_runs: Value =
        serde_json::from_slice(&env_output.stdout).expect("FABRO_JSON output should parse");

    let normalize = |runs: &Value| {
        let mut rows = runs
            .as_array()
            .expect("ps output should be an array")
            .iter()
            .map(|run| {
                (
                    run["run_id"]
                        .as_str()
                        .expect("run_id should be present")
                        .to_string(),
                    run["workflow_name"]
                        .as_str()
                        .expect("workflow_name should be present")
                        .to_string(),
                    run["workflow_slug"]
                        .as_str()
                        .expect("workflow_slug should be present")
                        .to_string(),
                    run["goal"]
                        .as_str()
                        .expect("goal should be present")
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        rows.sort_unstable();
        rows
    };

    assert_eq!(normalize(&global_runs), normalize(&env_runs));
}

#[test]
fn logs_json_wins_over_pretty() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);

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
fn secret_list_json_missing_env_outputs_json_array() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "secret", "list"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("secret list should parse");
    assert!(value.is_array(), "secret list JSON should be an array");
}
