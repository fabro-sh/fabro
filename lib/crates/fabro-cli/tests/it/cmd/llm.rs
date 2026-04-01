use std::process::Output;

use fabro_test::{TwinScenario, TwinScenarios, fabro_snapshot, test_context, twin_openai};
use predicates::prelude::*;

async fn run_success_output(mut cmd: assert_cmd::Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.assert().success().get_output().clone())
        .await
        .expect("blocking command task should complete")
}

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

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn prompt_no_stream_generates_response() {
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

#[fabro_macros::e2e_test(twin)]
async fn twin_prompt_no_stream() {
    let context = test_context!();
    let (base_url, api_key) = fabro_test::e2e_openai!();
    let mut cmd = context.llm();
    cmd.env("OPENAI_BASE_URL", base_url);
    cmd.env("OPENAI_API_KEY", api_key);
    cmd.args(["prompt", "--no-stream", "-m", "gpt-5.4-mini", "Say hello"]);
    cmd.write_stdin("");
    let output = run_success_output(cmd).await;
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "deterministic: Say hello"
    );
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn prompt_stream_generates_response() {
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

#[fabro_macros::e2e_test(twin)]
async fn twin_prompt_stream() {
    let context = test_context!();
    let (base_url, api_key) = fabro_test::e2e_openai!();
    let mut cmd = context.llm();
    cmd.env("OPENAI_BASE_URL", base_url);
    cmd.env("OPENAI_API_KEY", api_key);
    cmd.args(["prompt", "-m", "gpt-5.4-mini", "Say hello"]);
    cmd.write_stdin("");
    let output = run_success_output(cmd).await;
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "deterministic: Say hello"
    );
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn prompt_usage_shows_tokens() {
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

#[fabro_macros::e2e_test(twin)]
async fn twin_prompt_usage() {
    let context = test_context!();
    let (base_url, api_key) = fabro_test::e2e_openai!();
    let mut cmd = context.llm();
    cmd.env("OPENAI_BASE_URL", base_url);
    cmd.env("OPENAI_API_KEY", api_key);
    cmd.args([
        "prompt",
        "--no-stream",
        "-u",
        "-m",
        "gpt-5.4-mini",
        "Say hello",
    ]);
    cmd.write_stdin("");
    let output = run_success_output(cmd).await;
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "deterministic: Say hello"
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Tokens:"),
        "stderr should include token usage"
    );
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn prompt_schema_no_stream_generates_json() {
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

#[fabro_macros::e2e_test(twin)]
async fn twin_prompt_schema_no_stream() {
    let context = test_context!();
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    TwinScenarios::new(namespace.clone())
        .scenario(
            TwinScenario::responses("gpt-5.4-mini")
                .stream(false)
                .input_contains("Return JSON")
                .text(r#"{"greeting":"hello"}"#),
        )
        .load(twin)
        .await;

    let mut cmd = context.llm();
    twin.configure_command(&mut cmd, &namespace);
    cmd.args([
        "prompt",
        "--no-stream",
        "-m",
        "gpt-5.4-mini",
        "--schema",
        r#"{"type":"object","properties":{"greeting":{"type":"string"}},"required":["greeting"]}"#,
        "Return JSON",
    ]);
    cmd.write_stdin("");
    let output = run_success_output(cmd).await;

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
    assert_eq!(parsed["greeting"], "hello");
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn prompt_schema_stream_generates_json() {
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

#[fabro_macros::e2e_test(twin)]
async fn twin_prompt_schema_stream() {
    let context = test_context!();
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    TwinScenarios::new(namespace.clone())
        .scenario(
            TwinScenario::responses("gpt-5.4-mini")
                .stream(true)
                .input_contains("Return JSON")
                .text(r#"{"greeting":"hello"}"#),
        )
        .load(twin)
        .await;

    let mut cmd = context.llm();
    twin.configure_command(&mut cmd, &namespace);
    cmd.args([
        "prompt",
        "-m",
        "gpt-5.4-mini",
        "--schema",
        r#"{"type":"object","properties":{"greeting":{"type":"string"}},"required":["greeting"]}"#,
        "Return JSON",
    ]);
    cmd.write_stdin("");
    let output = run_success_output(cmd).await;

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
    assert_eq!(parsed["greeting"], "hello");
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn chat_multi_turn_with_system_prompt() {
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

#[fabro_macros::e2e_test(twin)]
async fn twin_chat_multi_turn() {
    let context = test_context!();
    let (base_url, api_key) = fabro_test::e2e_openai!();
    let mut cmd = context.command();
    cmd.env("OPENAI_BASE_URL", base_url);
    cmd.env("OPENAI_API_KEY", api_key);
    cmd.args([
        "llm",
        "chat",
        "-m",
        "gpt-5.4-mini",
        "-s",
        "You are a pilot. End every response with 'Roger that.'",
    ]);
    cmd.write_stdin("What is your profession?\nWhat did I just ask you?\n");
    let output = run_success_output(cmd).await;

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stdout.trim().is_empty(), "stdout should not be empty");
    assert!(
        stderr.contains("Using model:"),
        "stderr should show model info"
    );
}
