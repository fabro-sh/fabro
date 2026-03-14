use assert_cmd::Command;
use predicates::prelude::*;

#[allow(deprecated)]
fn arc() -> Command {
    Command::cargo_bin("fabro").unwrap()
}

// == LLM: prompt ==============================================================

#[test]
fn prompt_reads_from_stdin() {
    let result = arc()
        .args(["llm", "prompt", "--no-stream", "-m", "test-model"])
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
fn exec_missing_api_key_exits_with_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    arc()
        .args(["exec", "test prompt"])
        .env_clear()
        .env("HOME", tmp.path().to_str().unwrap())
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("API key not set"));
}

#[test]
#[ignore = "requires API key"]
fn exec_creates_file() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Create a file called hello.txt containing exactly 'Hello'",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
    let path = tmp.path().join("hello.txt");
    assert!(path.exists(), "hello.txt should have been created");
    let content = std::fs::read_to_string(&path).expect("read hello.txt");
    assert!(
        content.contains("Hello"),
        "Expected 'Hello' in hello.txt, got: {content}"
    );
}

#[test]
#[ignore = "requires API key"]
fn exec_shell_command() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Run the shell command `echo arc_test_marker_42` and tell me what it printed",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
}

#[test]
#[ignore = "requires API key"]
fn exec_read_only_blocks_write() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "read-only",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Create a file called forbidden.txt containing 'should not exist'",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
    assert!(
        !tmp.path().join("forbidden.txt").exists(),
        "forbidden.txt should NOT exist under read-only permissions"
    );
}

#[test]
#[ignore = "requires API key"]
fn exec_json_output_format() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "full",
            "--output-format",
            "json",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Create a file called test.txt containing 'test'",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("valid utf8");
    assert!(!stdout.trim().is_empty(), "json output should not be empty");
    // Every non-empty line should be valid JSON
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty(), "should have at least one NDJSON line");
    let first: serde_json::Value =
        serde_json::from_str(lines[0]).expect("first line should be valid JSON");
    assert!(
        first.get("event").is_some() || first.get("type").is_some(),
        "NDJSON line should have an event or type field, got: {first}"
    );
}

#[test]
#[ignore = "requires API key"]
fn exec_read_and_edit() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("data.txt"), "old content").expect("write data.txt");
    arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Read data.txt then replace its entire content with 'new content'",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
    let content = std::fs::read_to_string(tmp.path().join("data.txt")).expect("read data.txt");
    assert!(
        content.contains("new content"),
        "Expected 'new content' in data.txt, got: {content}"
    );
}

// == Arc: serve =========================================================

#[test]
#[cfg(feature = "server")]
fn serve_help() {
    let output = arc().args(["serve", "--help"]).output().expect("runs");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    insta::assert_snapshot!(stdout);
}

// == Doctor ===================================================================

#[test]
fn doctor_no_color_when_no_color_set() {
    arc()
        .args(["doctor", "--dry-run"])
        .env_clear()
        .env("NO_COLOR", "1")
        .assert()
        .stdout(predicate::str::contains("\x1b[").not());
}

// == JSONL logging ============================================================

#[test]
fn dry_run_writes_jsonl_and_live_json() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run");

    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--run-dir",
            run_dir.to_str().unwrap(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success();

    // progress.jsonl must exist and contain valid JSON lines
    let jsonl_path = run_dir.join("progress.jsonl");
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
    let live_path = run_dir.join("live.json");
    assert!(live_path.exists(), "live.json should exist");
    let live_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&live_path).unwrap()).unwrap();
    assert!(live_content.get("ts").is_some());
    assert!(live_content.get("run_id").is_some());
    assert!(live_content.get("event").is_some());
}

// == --run-id passthrough =====================================================

#[test]
fn run_id_passthrough_uses_provided_ulid() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    let my_ulid = "01JTEST1234567890ABCDE";

    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            my_ulid,
            "--run-dir",
            run_dir.to_str().unwrap(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(my_ulid));
}

// == --detach flag =============================================================

#[test]
fn detach_flag_appears_in_help() {
    arc()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--detach"));
}

#[test]
fn detach_prints_ulid_and_exits() {
    let output = arc()
        .args([
            "run",
            "--detach",
            "--dry-run",
            "--auto-approve",
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let ulid = stdout.trim();
    // ULID is 26 uppercase alphanumeric chars
    assert_eq!(ulid.len(), 26, "expected 26-char ULID, got: {ulid:?}");
    assert!(
        ulid.chars().all(|c| c.is_ascii_alphanumeric()),
        "expected alphanumeric ULID, got: {ulid:?}"
    );
}

#[test]
fn detach_creates_run_dir_with_detach_log() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("detached-run");

    let output = arc()
        .args([
            "run",
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--run-dir",
            run_dir.to_str().unwrap(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let ulid = String::from_utf8(output).unwrap();
    let ulid = ulid.trim();
    assert!(!ulid.is_empty(), "should print a ULID");

    // Run dir should have been created with detach.log
    assert!(run_dir.exists(), "run dir should exist");
    assert!(
        run_dir.join("detach.log").exists(),
        "detach.log should exist in run dir"
    );
}

#[test]
fn detach_conflicts_with_resume() {
    arc()
        .args([
            "run",
            "--detach",
            "--resume",
            "/tmp/fake-checkpoint.json",
            "../../../test/simple.fabro",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}
