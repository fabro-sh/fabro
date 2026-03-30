use fabro_test::{fabro_snapshot, test_context};
use predicates::prelude::*;

#[test]
fn prompt_bad_option() {
    let context = test_context!();
    let mut cmd = context.llm();
    cmd.args(["prompt", "-o", "bad_option", "hello"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: invalid value 'bad_option' for '--option <OPTION>': expected key=value, got bad_option

    For more information, try '--help'.
    ");
}

#[test]
fn prompt_no_text() {
    let context = test_context!();
    let mut cmd = context.llm();
    cmd.arg("prompt");
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Error: no prompt provided. Pass a prompt as an argument or pipe text via stdin.
    ");
}

#[test]
fn prompt_schema_invalid() {
    let context = test_context!();
    let mut cmd = context.llm();
    cmd.args([
        "prompt",
        "--no-stream",
        "-m",
        "test-model",
        "--schema",
        "not json",
        "hello",
    ]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Using model: test-model
    error: --schema must be valid JSON
      > expected ident at line 1 column 2
    ");
}

#[test]
fn prompt_reads_from_stdin() {
    let context = test_context!();
    let result = context
        .llm()
        .args(["prompt", "--no-stream", "-m", "test-model"])
        .write_stdin("hello from stdin")
        .assert()
        .failure();

    // Should NOT complain about missing prompt
    result.stderr(predicate::str::contains("no prompt provided").not());
}

#[test]
fn prompt_concatenates_stdin_and_arg() {
    let context = test_context!();
    let result = context
        .llm()
        .args([
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
#[ignore = "requires API key"]
fn prompt_no_stream_generates_response() {
    dotenvy::dotenv().ok();
    let context = test_context!();
    context
        .llm()
        .args([
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
    dotenvy::dotenv().ok();
    let context = test_context!();
    context
        .llm()
        .args([
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
    dotenvy::dotenv().ok();
    let context = test_context!();
    context
        .llm()
        .args([
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
#[ignore = "requires API key"]
fn prompt_schema_no_stream_generates_json() {
    dotenvy::dotenv().ok();
    let context = test_context!();
    let assert = context
        .llm()
        .args([
            "prompt", "--no-stream", "-m", "claude-sonnet-4-5",
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
    dotenvy::dotenv().ok();
    let context = test_context!();
    let assert = context
        .llm()
        .args([
            "prompt", "-m", "claude-sonnet-4-5",
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
fn chat_multi_turn_with_system_prompt() {
    dotenvy::dotenv().ok();
    let context = test_context!();
    let assert = context
        .command()
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
