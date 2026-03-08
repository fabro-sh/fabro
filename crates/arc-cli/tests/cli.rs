use assert_cmd::Command;
use predicates::prelude::*;

#[allow(deprecated)]
fn arc() -> Command {
    Command::cargo_bin("arc").unwrap()
}

// == Models ===================================================================

#[test]
fn model_list_prints_all_models() {
    arc()
        .args(["model", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-opus-4-6"))
        .stdout(predicate::str::contains("claude-sonnet-4-5"))
        .stdout(predicate::str::contains("gpt-5.2"))
        .stdout(predicate::str::contains("gemini-3.1-pro-preview"))
        .stdout(predicate::str::contains("anthropic"))
        .stdout(predicate::str::contains("openai"))
        .stdout(predicate::str::contains("gemini"))
        .stdout(predicate::str::contains("gpt-5.4"));
}

#[test]
fn model_list_filters_by_provider() {
    let assert = arc()
        .args(["model", "list", "--provider", "anthropic"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-opus-4-6"))
        .stdout(predicate::str::contains("claude-sonnet-4-5"));

    // Should NOT contain other providers
    assert
        .stdout(predicate::str::contains("gpt-5.2").not())
        .stdout(predicate::str::contains("gemini-3.1-pro-preview").not());
}

#[test]
fn model_list_filters_by_query() {
    arc()
        .args(["model", "list", "--query", "opus"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-opus-4-6"))
        .stdout(predicate::str::contains("claude-sonnet-4-5").not());
}

#[test]
fn model_list_query_is_case_insensitive() {
    arc()
        .args(["model", "list", "--query", "OPUS"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-opus-4-6"));
}

#[test]
fn model_list_query_matches_aliases() {
    arc()
        .args(["model", "list", "--query", "codex"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gpt-5.2-codex"));
}

#[test]
fn model_bare_defaults_to_list() {
    arc()
        .args(["model"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-opus-4-6"))
        .stdout(predicate::str::contains("gpt-5.2"))
        .stdout(predicate::str::contains("gemini-3.1-pro-preview"));
}

// == LLM: prompt ==============================================================

#[test]
fn prompt_errors_without_prompt_text() {
    arc()
        .args(["llm", "prompt"])
        .write_stdin("")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no prompt provided"));
}

#[test]
fn prompt_reads_from_stdin() {
    let result = arc()
        .args([
            "--no-dotenv",
            "llm",
            "prompt",
            "--no-stream",
            "-m",
            "test-model",
        ])
        .write_stdin("hello from stdin")
        .assert()
        .failure();

    // Should NOT complain about missing prompt
    result.stderr(predicate::str::contains("no prompt provided").not());
}

#[test]
fn prompt_concatenates_stdin_and_arg() {
    let result = arc()
        .args([
            "--no-dotenv",
            "llm",
            "prompt",
            "--no-stream",
            "-m",
            "test-model",
            "summarize this",
        ])
        .write_stdin("some input text")
        .assert()
        .failure();

    result.stderr(predicate::str::contains("no prompt provided").not());
}

#[test]
fn prompt_rejects_bad_option_format() {
    arc()
        .args(["llm", "prompt", "-o", "bad_option", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected key=value"));
}

#[test]
#[ignore = "requires API key"]
fn prompt_no_stream_generates_response() {
    arc()
        .args([
            "llm",
            "prompt",
            "--no-stream",
            "-m",
            "claude-sonnet-4-5",
            "Say just the word 'hello'",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
#[ignore = "requires API key"]
fn prompt_stream_generates_response() {
    arc()
        .args([
            "llm",
            "prompt",
            "-m",
            "claude-sonnet-4-5",
            "Say just the word 'hello'",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
#[ignore = "requires API key"]
fn prompt_usage_shows_tokens() {
    arc()
        .args([
            "llm",
            "prompt",
            "--no-stream",
            "-u",
            "-m",
            "claude-sonnet-4-5",
            "Say just the word 'hello'",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Tokens:"));
}

#[test]
fn prompt_schema_rejects_invalid_json() {
    arc()
        .args([
            "--no-dotenv",
            "llm",
            "prompt",
            "--no-stream",
            "-m",
            "test-model",
            "--schema",
            "not json",
            "hello",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--schema must be valid JSON"));
}

#[test]
#[ignore = "requires API key"]
fn prompt_schema_no_stream_generates_json() {
    let assert = arc()
        .args([
            "llm", "prompt", "--no-stream", "-m", "claude-sonnet-4-5",
            "--schema", r#"{"type":"object","properties":{"greeting":{"type":"string"}},"required":["greeting"]}"#,
            "Return a JSON object with a greeting field set to hello",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
    assert!(
        parsed.get("greeting").is_some(),
        "expected 'greeting' key in output"
    );
}

#[test]
#[ignore = "requires API key"]
fn prompt_schema_stream_generates_json() {
    let assert = arc()
        .args([
            "llm", "prompt", "-m", "claude-sonnet-4-5",
            "--schema", r#"{"type":"object","properties":{"greeting":{"type":"string"}},"required":["greeting"]}"#,
            "Return a JSON object with a greeting field set to hello",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
    assert!(
        parsed.get("greeting").is_some(),
        "expected 'greeting' key in output"
    );
}

// == LLM: chat ================================================================

#[test]
#[ignore = "requires API key"]
fn chat_multi_turn_with_system_prompt() {
    let assert = arc()
        .args([
            "llm",
            "chat",
            "-m",
            "claude-haiku-4-5",
            "-s",
            "You are a pilot. End every response with 'Roger that.'",
        ])
        .write_stdin("What is your profession?\nWhat did I just ask you?\n")
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    // Verify model info printed to stderr
    assert!(
        stderr.contains("Using model:"),
        "stderr should show model info"
    );

    // Verify the system prompt influenced the output
    assert!(
        stdout.to_lowercase().contains("roger that"),
        "response should follow pilot system prompt, got: {stdout}"
    );

    // Verify multi-turn: the second response should reference the first question
    assert!(
        stdout.to_lowercase().contains("profession")
            || stdout.to_lowercase().contains("asked")
            || stdout.to_lowercase().contains("pilot"),
        "second response should show multi-turn context, got: {stdout}"
    );
}

// == Exec =====================================================================

#[test]
fn exec_no_prompt_prints_usage() {
    arc()
        .args(["exec"])
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage:"));
}

#[test]
fn exec_help_flag_prints_help() {
    arc()
        .args(["exec", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task prompt"));
}

#[test]
fn exec_missing_api_key_exits_with_error() {
    let tmp = std::env::temp_dir();
    arc()
        .args(["--no-dotenv", "exec", "test prompt"])
        .env_clear()
        .current_dir(&tmp)
        .assert()
        .failure()
        .stderr(predicate::str::contains("API key not set"));
}

#[test]
fn exec_invalid_permissions_value() {
    arc()
        .args(["exec", "--permissions", "bogus", "test prompt"])
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

// == Arc: validate ======================================================

#[test]
fn validate_simple() {
    arc()
        .args(["validate", "../../test/simple.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_branching() {
    arc()
        .args(["validate", "../../test/branching.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_conditions() {
    arc()
        .args(["validate", "../../test/conditions.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_parallel() {
    arc()
        .args(["validate", "../../test/parallel.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_styled() {
    arc()
        .args(["validate", "../../test/styled.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_legacy_tool() {
    arc()
        .args(["validate", "../../test/legacy_tool.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_invalid() {
    arc()
        .args(["validate", "../../test/invalid.dot"])
        .assert()
        .failure();
}

// == Arc: serve =========================================================

#[test]
fn serve_help() {
    arc()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--port"))
        .stdout(predicate::str::contains("--host"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--provider"));
}

// == Arc: run --dry-run =================================================

#[test]
fn dry_run_simple() {
    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "../../test/simple.dot",
        ])
        .assert()
        .success();
}

#[test]
fn dry_run_branching() {
    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "../../test/branching.dot",
        ])
        .assert()
        .success();
}

#[test]
fn dry_run_conditions() {
    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "../../test/conditions.dot",
        ])
        .assert()
        .success();
}

#[test]
fn dry_run_parallel() {
    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "../../test/parallel.dot",
        ])
        .assert()
        .success();
}

#[test]
fn dry_run_styled() {
    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "../../test/styled.dot",
        ])
        .assert()
        .success();
}

#[test]
fn dry_run_legacy_tool() {
    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "../../test/legacy_tool.dot",
        ])
        .assert()
        .success();
}

// == Doctor ===================================================================

#[test]
fn doctor_runs_and_prints_header() {
    arc()
        .args(["--no-dotenv", "doctor"])
        .env_clear()
        .assert()
        .stdout(predicate::str::contains("Arc Doctor"));
}

#[test]
fn doctor_verbose_runs_and_prints_header() {
    arc()
        .args(["--no-dotenv", "doctor", "-v"])
        .env_clear()
        .assert()
        .stdout(predicate::str::contains("Arc Doctor"));
}

#[test]
fn doctor_no_color_when_no_color_set() {
    arc()
        .args(["--no-dotenv", "doctor"])
        .env_clear()
        .env("NO_COLOR", "1")
        .assert()
        .stdout(predicate::str::contains("\x1b[").not());
}

#[test]
fn doctor_live_flag_accepted() {
    arc()
        .args(["--no-dotenv", "doctor", "--live"])
        .env_clear()
        .assert()
        .stdout(predicate::str::contains("Arc Doctor"));
}

// == JSONL logging ============================================================

#[test]
fn dry_run_writes_jsonl_and_live_json() {
    let tmp = tempfile::tempdir().unwrap();
    let logs_dir = tmp.path().join("logs");

    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--logs-dir",
            logs_dir.to_str().unwrap(),
            "../../test/simple.dot",
        ])
        .assert()
        .success();

    // progress.jsonl must exist and contain valid JSON lines
    let jsonl_path = logs_dir.join("progress.jsonl");
    assert!(jsonl_path.exists(), "progress.jsonl should exist");
    let jsonl_content = std::fs::read_to_string(&jsonl_path).unwrap();
    let lines: Vec<&str> = jsonl_content.lines().collect();
    assert!(
        !lines.is_empty(),
        "progress.jsonl should have at least one line"
    );

    // Every line must be valid JSON with ts, run_id, and event keys
    let first_line: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(first_line.get("ts").is_some(), "line should have ts");
    assert!(
        first_line.get("run_id").is_some(),
        "line should have run_id"
    );
    assert!(first_line.get("event").is_some(), "line should have event");

    // Events should contain WorkflowRunStarted (may not be first due to exec env events)
    let has_run_started = lines.iter().any(|line| {
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        parsed["event"].as_str() == Some("WorkflowRunStarted")
    });
    assert!(has_run_started, "events should contain WorkflowRunStarted");

    // run_id should be non-empty after WorkflowRunStarted
    let last_line: serde_json::Value = serde_json::from_str(lines[lines.len() - 1]).unwrap();
    let run_id = last_line["run_id"].as_str().unwrap();
    assert!(!run_id.is_empty(), "run_id should be non-empty");

    // live.json must exist and contain valid JSON matching the last JSONL line
    let live_path = logs_dir.join("live.json");
    assert!(live_path.exists(), "live.json should exist");
    let live_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&live_path).unwrap()).unwrap();
    assert!(live_content.get("ts").is_some());
    assert!(live_content.get("run_id").is_some());
    assert!(live_content.get("event").is_some());
}
