use std::process::Command;

#[test]
fn no_args_prints_usage() {
    let output = Command::new(env!("CARGO_BIN_EXE_agent"))
        .env_clear()
        .output()
        .expect("failed to execute");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Usage:"),
        "expected stderr to contain 'Usage:', got: {stderr}"
    );
}

#[test]
fn help_flag_prints_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_agent"))
        .env_clear()
        .arg("--help")
        .output()
        .expect("failed to execute");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Task prompt"),
        "expected stdout to contain 'Task prompt', got: {stdout}"
    );
}

#[test]
fn missing_api_key_exits_with_error() {
    let tmp = std::env::temp_dir();
    let output = Command::new(env!("CARGO_BIN_EXE_agent"))
        .env_clear()
        .current_dir(&tmp)
        .arg("test prompt")
        .output()
        .expect("failed to execute");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("API key not set"),
        "expected stderr to contain 'API key not set', got: {stderr}"
    );
}

#[test]
fn invalid_permissions_value() {
    let output = Command::new(env!("CARGO_BIN_EXE_agent"))
        .env_clear()
        .args(["--permissions", "bogus", "test prompt"])
        .output()
        .expect("failed to execute");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid value"),
        "expected stderr to contain 'invalid value', got: {stderr}"
    );
}
