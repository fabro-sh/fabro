use fabro_test::{fabro_snapshot, test_context};
use predicates::prelude::*;

#[allow(deprecated)]
fn arc() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("fabro").unwrap();
    cmd.arg("--no-upgrade-check");
    cmd
}

#[test]
fn invalid_permissions() {
    let context = test_context!();
    let mut cmd = context.exec_cmd();
    cmd.args(["--permissions", "bogus", "test prompt"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: invalid value 'bogus' for '--permissions <PERMISSIONS>'
      [possible values: read-only, read-write, full]

    For more information, try '--help'.
    ");
}

#[test]
fn no_prompt() {
    let context = test_context!();
    fabro_snapshot!(context.filters(), context.exec_cmd(), @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <PROMPT>

    Usage: fabro exec --no-upgrade-check --storage-dir <STORAGE_DIR> <PROMPT>

    For more information, try '--help'.
    ");
}

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
