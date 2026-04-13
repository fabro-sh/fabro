use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.install();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Set up the Fabro environment (LLMs, certs, GitHub)

    Usage: fabro install [OPTIONS]

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --web-url <WEB_URL>          Base URL for the web UI (used for OAuth callback URLs) [default: http://localhost:3000]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --non-interactive            Run install without prompts; use hidden scripted flags for inputs
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn install_json_requires_non_interactive() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "install"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json is only supported for install with --non-interactive"));
}

#[test]
fn install_json_non_interactive_is_not_rejected_as_unsupported() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "install", "--non-interactive"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Non-interactive install requires additional flags"));
    assert!(!stderr.contains("--json is not supported for this command"));
}

#[test]
fn install_json_non_interactive_allows_github_app_strategy() {
    let context = test_context!();
    let output = context
        .command()
        .env_remove("MISSING_ANTHROPIC_API_KEY")
        .args([
            "--json",
            "install",
            "--non-interactive",
            "--llm-provider",
            "anthropic",
            "--llm-api-key-env",
            "MISSING_ANTHROPIC_API_KEY",
            "--github-strategy",
            "app",
            "--github-owner",
            "personal",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("GitHub App setup is not supported with --non-interactive"));
    assert!(!stderr.contains("requires --github-username"));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("install JSON error should parse");
    assert_eq!(value["event"], "install_error");
    assert_eq!(value["status"], "error");
}

#[test]
fn non_interactive_without_inputs_prints_scripted_usage_and_fails() {
    let context = test_context!();
    let output = context
        .command()
        .args(["install", "--non-interactive"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Non-interactive install requires additional flags"));
    assert!(stderr.contains("--llm-provider"));
    assert!(stderr.contains("--github-strategy"));
}

#[test]
fn hidden_non_interactive_args_require_non_interactive() {
    let context = test_context!();
    let output = context
        .command()
        .args(["install", "--llm-provider", "anthropic"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("requires --non-interactive"));
}
