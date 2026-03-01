use assert_cmd::Command;
use predicates::prelude::*;

#[allow(deprecated)]
fn arc() -> Command {
    Command::cargo_bin("arc").unwrap()
}

// == LLM: models ==============================================================

#[test]
fn models_list_prints_all_models() {
    arc()
        .args(["llm", "models", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-opus-4-6"))
        .stdout(predicate::str::contains("claude-sonnet-4-5"))
        .stdout(predicate::str::contains("gpt-5.2"))
        .stdout(predicate::str::contains("gemini-3.1-pro-preview"))
        .stdout(predicate::str::contains("anthropic"))
        .stdout(predicate::str::contains("openai"))
        .stdout(predicate::str::contains("gemini"));
}

#[test]
fn models_list_filters_by_provider() {
    let assert = arc()
        .args(["llm", "models", "list", "--provider", "anthropic"])
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
fn models_list_filters_by_query() {
    arc()
        .args(["llm", "models", "list", "--query", "opus"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-opus-4-6"))
        .stdout(predicate::str::contains("claude-sonnet-4-5").not());
}

#[test]
fn models_list_query_is_case_insensitive() {
    arc()
        .args(["llm", "models", "list", "--query", "OPUS"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-opus-4-6"));
}

#[test]
fn models_list_query_matches_aliases() {
    arc()
        .args(["llm", "models", "list", "--query", "codex"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gpt-5.2-codex"));
}

#[test]
fn models_bare_defaults_to_list() {
    arc()
        .args(["llm", "models"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-opus-4-6"))
        .stdout(predicate::str::contains("gpt-5.2"))
        .stdout(predicate::str::contains("gemini-3.1-pro-preview"));
}

#[test]
fn models_sync_downloads_and_saves() {
    let server = httpmock::MockServer::start();
    let mock_response = serde_json::json!({
        "data": [{"id": "test-model", "name": "Test Model"}]
    });
    server.mock(|when, then| {
        when.method("GET").path("/api/v1/models");
        then.status(200)
            .header("content-type", "application/json")
            .body(serde_json::to_string(&mock_response).unwrap());
    });

    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("models.json");

    arc()
        .args([
            "llm",
            "models",
            "sync",
            "--url",
            &server.url("/api/v1/models"),
            "--output",
            output_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Saved models to"));

    let contents = std::fs::read_to_string(&output_path).unwrap();
    let expected = serde_json::to_string_pretty(&mock_response).unwrap();
    assert_eq!(contents, expected);
}

#[test]
fn models_sync_reports_http_errors() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("GET").path("/api/v1/models");
        then.status(500);
    });

    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("models.json");

    arc()
        .args([
            "llm",
            "models",
            "sync",
            "--url",
            &server.url("/api/v1/models"),
            "--output",
            output_path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error").or(predicate::str::contains("Error")));
}

#[test]
#[ignore = "requires network"]
fn models_sync_integration_smoke_test() {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("models.json");

    arc()
        .args([
            "llm",
            "models",
            "sync",
            "--output",
            output_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(contents.contains("\"data\""));
}

#[test]
fn models_sync_help_mentions_openrouter() {
    arc()
        .args(["llm", "models", "sync", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("openrouter").or(predicate::str::contains("OpenRouter")));
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

// == Agent ====================================================================

#[test]
fn agent_no_prompt_prints_usage() {
    arc()
        .args(["agent"])
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage:"));
}

#[test]
fn agent_help_flag_prints_help() {
    arc()
        .args(["agent", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task prompt"));
}

#[test]
fn agent_missing_api_key_exits_with_error() {
    let tmp = std::env::temp_dir();
    arc()
        .args(["--no-dotenv", "agent", "test prompt"])
        .env_clear()
        .current_dir(&tmp)
        .assert()
        .failure()
        .stderr(predicate::str::contains("API key not set"));
}

#[test]
fn agent_invalid_permissions_value() {
    arc()
        .args(["agent", "--permissions", "bogus", "test prompt"])
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

// == NDJSON logging ===========================================================

#[test]
fn dry_run_writes_ndjson_and_live_json() {
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

    // progress.ndjson must exist and contain valid JSON lines
    let ndjson_path = logs_dir.join("progress.ndjson");
    assert!(ndjson_path.exists(), "progress.ndjson should exist");
    let ndjson_content = std::fs::read_to_string(&ndjson_path).unwrap();
    let lines: Vec<&str> = ndjson_content.lines().collect();
    assert!(
        !lines.is_empty(),
        "progress.ndjson should have at least one line"
    );

    // Every line must be valid JSON with timestamp, run_id, and event keys
    let first_line: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(
        first_line.get("timestamp").is_some(),
        "line should have timestamp"
    );
    assert!(
        first_line.get("run_id").is_some(),
        "line should have run_id"
    );
    assert!(first_line.get("event").is_some(), "line should have event");

    // Events should contain PipelineStarted (may not be first due to exec env events)
    let has_pipeline_started = lines.iter().any(|line| {
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        parsed["event"].get("PipelineStarted").is_some()
    });
    assert!(
        has_pipeline_started,
        "events should contain PipelineStarted"
    );

    // run_id should be non-empty after PipelineStarted
    let last_line: serde_json::Value = serde_json::from_str(lines[lines.len() - 1]).unwrap();
    let run_id = last_line["run_id"].as_str().unwrap();
    assert!(!run_id.is_empty(), "run_id should be non-empty");

    // live.json must exist and contain valid JSON matching the last NDJSON line
    let live_path = logs_dir.join("live.json");
    assert!(live_path.exists(), "live.json should exist");
    let live_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&live_path).unwrap()).unwrap();
    assert!(live_content.get("timestamp").is_some());
    assert!(live_content.get("run_id").is_some());
    assert!(live_content.get("event").is_some());
}
