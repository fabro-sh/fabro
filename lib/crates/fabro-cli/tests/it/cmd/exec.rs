use std::process::Output;

use fabro_test::{fabro_snapshot, test_context};

async fn run_success_output(mut cmd: assert_cmd::Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.assert().success().get_output().clone())
        .await
        .expect("blocking command task should complete")
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["exec", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Run an agentic coding session

    Usage: fabro exec [OPTIONS] <PROMPT>

    Arguments:
      <PROMPT>  Task prompt

    Options:
          --json                           Output as JSON [env: FABRO_JSON=]
          --provider <PROVIDER>            LLM provider (anthropic, openai, gemini, kimi, zai, minimax, inception)
          --model <MODEL>                  Model name (defaults per provider)
          --no-upgrade-check               Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --permissions <PERMISSIONS>      Permission level for tool execution [possible values: read-only, read-write, full]
          --auto-approve                   Skip interactive prompts; deny tools outside permission level
          --quiet                          Suppress non-essential output [env: FABRO_QUIET=]
          --debug                          Print LLM request/response debug info to stderr
          --storage-dir <STORAGE_DIR>      Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --verbose                        Print full LLM request/response JSON to stderr
          --server-url <SERVER_URL>        Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
          --skills-dir <SKILLS_DIR>        Directory containing skill files (overrides default discovery)
          --output-format <OUTPUT_FORMAT>  Output format (text for human-readable, json for NDJSON event stream) [possible values: text, json]
      -h, --help                           Print help
    ----- stderr -----
    ");
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
    let context = test_context!();
    let mut cmd = context.exec_cmd();
    cmd.arg("test prompt");
    cmd.env_clear();
    cmd.env("HOME", &context.home_dir);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: API key not set for provider 'anthropic'
    ");
}

#[test]
fn exec_uses_user_config_defaults() {
    let context = test_context!();
    context.write_home(
        ".fabro/user.toml",
        "[exec]\nprovider = \"openai\"\nmodel = \"gpt-4.1-mini\"\npermissions = \"read-only\"\noutput_format = \"json\"\n",
    );

    let mut cmd = context.exec_cmd();
    cmd.arg("test prompt");
    cmd.env_clear();
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_STORAGE_DIR", &context.storage_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true");

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: API key not set for provider 'openai'
    ");
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn exec_creates_file() {
    let context = test_context!();
    context
        .exec_cmd()
        .args([
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Create a file called hello.txt containing exactly 'Hello'",
        ])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
    let path = context.temp_dir.join("hello.txt");
    assert!(path.exists(), "hello.txt should have been created");
    let content = std::fs::read_to_string(&path).expect("read hello.txt");
    assert!(
        content.contains("Hello"),
        "Expected 'Hello' in hello.txt, got: {content}"
    );
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn exec_shell_command() {
    let context = test_context!();
    context
        .exec_cmd()
        .args([
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Run the shell command `echo arc_test_marker_42` and tell me what it printed",
        ])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn exec_read_only_blocks_write() {
    let context = test_context!();
    context
        .exec_cmd()
        .args([
            "--auto-approve",
            "--permissions",
            "read-only",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Create a file called forbidden.txt containing 'should not exist'",
        ])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
    assert!(
        !context.temp_dir.join("forbidden.txt").exists(),
        "forbidden.txt should NOT exist under read-only permissions"
    );
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn exec_json_output_format() {
    let context = test_context!();
    let output = context
        .exec_cmd()
        .args([
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

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn exec_read_and_edit() {
    let context = test_context!();
    context.write_temp("data.txt", "old content");
    context
        .exec_cmd()
        .args([
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Read data.txt then replace its entire content with 'new content'",
        ])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
    let content =
        std::fs::read_to_string(context.temp_dir.join("data.txt")).expect("read data.txt");
    assert!(
        content.contains("new content"),
        "Expected 'new content' in data.txt, got: {content}"
    );
}

#[fabro_macros::e2e_test(twin)]
async fn twin_exec_creates_file() {
    let context = test_context!();
    let twin = fabro_test::twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    fabro_test::TwinScenarios::new(namespace.clone())
        .scenario(
            fabro_test::TwinScenario::responses("gpt-5.4-mini")
                .input_contains("Create a file called hello.txt containing exactly 'Hello'")
                .tool_call(fabro_test::TwinToolCall::write_file("hello.txt", "Hello"))
                .text("Done."),
        )
        .load(twin)
        .await;

    let mut cmd = context.exec_cmd();
    twin.configure_command(&mut cmd, &namespace);
    cmd.args([
        "--auto-approve",
        "--permissions",
        "full",
        "--provider",
        "openai",
        "--model",
        "gpt-5.4-mini",
        "Create a file called hello.txt containing exactly 'Hello'",
    ]);
    let _output = run_success_output(cmd).await;

    let content =
        std::fs::read_to_string(context.temp_dir.join("hello.txt")).expect("read hello.txt");
    assert_eq!(content, "Hello");
}

#[fabro_macros::e2e_test(twin)]
async fn twin_exec_shell_command() {
    let context = test_context!();
    let twin = fabro_test::twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    fabro_test::TwinScenarios::new(namespace.clone())
        .scenario(
            fabro_test::TwinScenario::responses("gpt-5.4-mini")
                .input_contains(
                    "Run the shell command `echo hello_from_shell` and tell me what it printed",
                )
                .tool_call(fabro_test::TwinToolCall::shell("echo hello_from_shell"))
                .text("It printed hello_from_shell."),
        )
        .load(twin)
        .await;

    let mut cmd = context.exec_cmd();
    twin.configure_command(&mut cmd, &namespace);
    cmd.args([
        "--auto-approve",
        "--permissions",
        "full",
        "--provider",
        "openai",
        "--model",
        "gpt-5.4-mini",
        "Run the shell command `echo hello_from_shell` and tell me what it printed",
    ]);
    let output = run_success_output(cmd).await;
    let stdout = String::from_utf8(output.stdout).expect("valid utf8");
    assert!(
        stdout.contains("hello_from_shell"),
        "expected shell marker in output, got: {stdout}"
    );
}

#[fabro_macros::e2e_test(twin)]
async fn twin_exec_json_output() {
    let context = test_context!();
    let twin = fabro_test::twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    fabro_test::TwinScenarios::new(namespace.clone())
        .scenario(
            fabro_test::TwinScenario::responses("gpt-5.4-mini")
                .input_contains("Create a file called test.txt containing 'test'")
                .tool_call(fabro_test::TwinToolCall::write_file("test.txt", "test"))
                .text("Done."),
        )
        .load(twin)
        .await;

    let mut cmd = context.exec_cmd();
    twin.configure_command(&mut cmd, &namespace);
    cmd.args([
        "--auto-approve",
        "--permissions",
        "full",
        "--output-format",
        "json",
        "--provider",
        "openai",
        "--model",
        "gpt-5.4-mini",
        "Create a file called test.txt containing 'test'",
    ]);
    let output = run_success_output(cmd).await;
    let stdout = String::from_utf8(output.stdout).expect("valid utf8");
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(!lines.is_empty(), "json output should not be empty");
    let parsed: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("each line should be valid JSON"))
        .collect();
    let first = &parsed[0];
    assert!(
        first.get("event").is_some() || first.get("type").is_some(),
        "NDJSON line should have an event or type field, got: {first}"
    );
}

#[fabro_macros::e2e_test(twin)]
async fn twin_exec_read_and_edit() {
    let context = test_context!();
    context.write_temp("data.txt", "old content");
    let twin = fabro_test::twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    fabro_test::TwinScenarios::new(namespace.clone())
        .scenario(
            fabro_test::TwinScenario::responses("gpt-5.4-mini")
                .input_contains("Read data.txt then replace its entire content with 'new content'")
                .tool_call(fabro_test::TwinToolCall::read_file("data.txt")),
        )
        .scenario(
            fabro_test::TwinScenario::responses("gpt-5.4-mini")
                .tool_call(fabro_test::TwinToolCall::write_file(
                    "data.txt",
                    "new content",
                ))
                .text("Done."),
        )
        .load(twin)
        .await;

    let mut cmd = context.exec_cmd();
    twin.configure_command(&mut cmd, &namespace);
    cmd.args([
        "--auto-approve",
        "--permissions",
        "full",
        "--provider",
        "openai",
        "--model",
        "gpt-5.4-mini",
        "Read data.txt then replace its entire content with 'new content'",
    ]);
    let _output = run_success_output(cmd).await;

    let content =
        std::fs::read_to_string(context.temp_dir.join("data.txt")).expect("read data.txt");
    assert_eq!(content, "new content");
}
