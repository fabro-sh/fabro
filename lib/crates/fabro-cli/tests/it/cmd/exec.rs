#![allow(
    clippy::absolute_paths,
    reason = "This test module prefers explicit type paths over extra imports."
)]
#![expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "Integration tests stage fixtures with sync std::fs calls and a blocking TCP server."
)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::Output;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use fabro_test::{fabro_snapshot, preserve_coverage_env, test_context};
use httpmock::MockServer;
use serde_json::json;

use crate::support::fatal_error_line;

async fn run_success_output(mut cmd: assert_cmd::Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.assert().success().get_output().clone())
        .await
        .expect("blocking command task should complete")
}

struct MidStreamDecodeErrorAnthropicServer {
    addr:          SocketAddr,
    request_count: Arc<AtomicUsize>,
    shutdown:      Arc<AtomicBool>,
    join_handle:   Option<thread::JoinHandle<()>>,
}

impl MidStreamDecodeErrorAnthropicServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test LLM server should bind");
        let addr = listener
            .local_addr()
            .expect("test LLM server should expose local addr");
        let request_count = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_request_count = Arc::clone(&request_count);
        let thread_shutdown = Arc::clone(&shutdown);
        let join_handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if thread_shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                handle_anthropic_test_connection(&mut stream, &thread_request_count);
            }
        });

        Self {
            addr,
            request_count,
            shutdown,
            join_handle: Some(join_handle),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }
}

impl Drop for MidStreamDecodeErrorAnthropicServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

fn handle_anthropic_test_connection(stream: &mut TcpStream, request_count: &Arc<AtomicUsize>) {
    let Some(request) = read_http_request(stream) else {
        return;
    };
    if !request.starts_with("POST /v1/messages ") {
        write_http_response(stream, "404 Not Found", "text/plain", "not found");
        return;
    }

    let attempt = request_count.fetch_add(1, Ordering::SeqCst);
    if attempt == 0 {
        write_chunk_decode_error_response(stream, &partial_anthropic_stream("partial"));
    } else {
        write_http_response(
            stream,
            "200 OK",
            "text/event-stream",
            &complete_anthropic_stream("Recovered"),
        );
    }
}

fn read_http_request(stream: &mut TcpStream) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    let mut body_start_and_len = None;

    loop {
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);

        if body_start_and_len.is_none() {
            if let Some(header_end) = find_header_end(&request) {
                body_start_and_len =
                    Some((header_end + 4, parse_content_length(&request[..header_end])));
            }
        }

        if let Some((body_start, body_len)) = body_start_and_len {
            if request.len() >= body_start + body_len {
                break;
            }
        }
    }

    Some(String::from_utf8_lossy(&request).into_owned())
}

fn find_header_end(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0)
}

fn write_chunk_decode_error_response(stream: &mut TcpStream, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n{:x}\r\n{body}\r\nnot-hex\r\n",
        body.len()
    );
    let _ = stream.flush();
}

fn write_http_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn partial_anthropic_stream(text: &str) -> String {
    anthropic_event(
        "message_start",
        &serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_stream",
                "type": "message",
                "role": "assistant",
                "model": "claude-haiku-4-5",
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 1, "output_tokens": 0 }
            }
        }),
    ) + &anthropic_event(
        "content_block_start",
        &serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" }
        }),
    ) + &anthropic_event(
        "content_block_delta",
        &serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": text }
        }),
    )
}

fn complete_anthropic_stream(text: &str) -> String {
    partial_anthropic_stream(text)
        + &anthropic_event(
            "content_block_stop",
            &serde_json::json!({
                "type": "content_block_stop",
                "index": 0
            }),
        )
        + &anthropic_event(
            "message_delta",
            &serde_json::json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": "end_turn",
                    "stop_sequence": null
                },
                "usage": { "output_tokens": 1 }
            }),
        )
        + &anthropic_event(
            "message_stop",
            &serde_json::json!({
                "type": "message_stop"
            }),
        )
}

fn anthropic_event(event: &str, data: &serde_json::Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
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
          --server <SERVER>                Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --provider <PROVIDER>            LLM provider (built-in or configured provider ID)
          --model <MODEL>                  Model name (defaults per provider)
          --no-upgrade-check               Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --permissions <PERMISSIONS>      Permission level for tool execution [possible values: read-only, read-write, full]
          --quiet                          Suppress non-essential output [env: FABRO_QUIET=]
          --auto-approve                   Skip interactive prompts; deny tools outside permission level
          --debug                          Print LLM request/response debug info to stderr
          --verbose                        Print full LLM request/response JSON to stderr
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

    Usage: fabro exec --no-upgrade-check <PROMPT>

    For more information, try '--help'.
    ");
}

#[test]
fn exec_uses_user_config_defaults() {
    let context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "_version = 1\n\n[cli.exec.model]\nprovider = \"openai\"\nname = \"gpt-4.1-mini\"\n\n[cli.exec.agent]\npermissions = \"read-only\"\n\n[cli.output]\nformat = \"json\"\n",
    );

    let mut cmd = context.exec_cmd();
    cmd.arg("test prompt");
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_STORAGE_DIR", &context.storage_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled");

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × LLM credentials not configured for provider 'openai'
    ");
}

#[test]
fn exec_accepts_configured_custom_provider_from_settings() {
    let context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "_version = 1\n\n[llm.providers.bedrock]\nadapter = \"openai_compatible\"\nagent_profile = \"openai\"\nbase_url = \"https://bedrock.example.invalid/v1\"\n\n[llm.providers.bedrock.auth]\ncredentials = [\"env:BEDROCK_API_KEY\"]\n\n[cli.exec.model]\nprovider = \"bedrock\"\nname = \"bedrock-claude-sonnet-4-6\"\n",
    );

    let mut cmd = context.exec_cmd();
    cmd.arg("test prompt");
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_STORAGE_DIR", &context.storage_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled");

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × LLM credentials not configured for provider 'bedrock'
    ");
}

#[test]
fn exec_server_target_uses_remote_transport_instead_of_local_api_key_resolution() {
    let context = test_context!();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/api/v1/completions");
        // Use a non-retriable error so this test covers transport routing
        // without paying the retry backoff cost of a 5xx response.
        then.status(400).body("server-routed-marker");
    });

    let mut cmd = context.exec_cmd();
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled");
    cmd.args([
        "--server",
        &format!("{}/api/v1", server.base_url()),
        "--provider",
        "openai",
        "--model",
        "gpt-5.4-mini",
        "test prompt",
    ]);

    let output = cmd.assert().failure().get_output().clone();
    let stderr = String::from_utf8(output.stderr).expect("valid utf8");
    assert!(
        stderr.contains("server-routed-marker"),
        "expected remote server failure marker, got: {stderr}"
    );
    assert!(
        !stderr.contains("API key not set"),
        "exec should not fail local API key validation when --server is set: {stderr}"
    );
}

#[test]
fn exec_server_target_accepts_configured_custom_provider_from_settings() {
    let context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "_version = 1\n\n[llm.providers.bedrock]\nadapter = \"openai_compatible\"\nagent_profile = \"openai\"\nbase_url = \"https://bedrock.example.invalid/v1\"\n\n[llm.providers.bedrock.auth]\ncredentials = [\"env:BEDROCK_API_KEY\"]\n\n[cli.exec.model]\nprovider = \"bedrock\"\nname = \"bedrock-claude-sonnet-4-6\"\n",
    );
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/api/v1/completions");
        then.status(400).body("custom-server-routed-marker");
    });

    let mut cmd = context.exec_cmd();
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled");
    cmd.args([
        "--server",
        &format!("{}/api/v1", server.base_url()),
        "test prompt",
    ]);

    let output = cmd.assert().failure().get_output().clone();
    let stderr = String::from_utf8(output.stderr).expect("valid utf8");
    assert!(
        stderr.contains("custom-server-routed-marker"),
        "expected remote server failure marker, got: {stderr}"
    );
    assert!(
        !stderr.contains("unknown provider: bedrock"),
        "exec should resolve custom providers from settings for remote transport: {stderr}"
    );
}

#[test]
fn exec_configured_server_target_alone_does_not_reroute_exec() {
    let context = test_context!();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/api/v1/completions");
        then.status(500).body("config-should-not-be-used");
    });
    context.set_http_target(&server.base_url());

    let mut cmd = context.exec_cmd();
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled");
    cmd.args([
        "--provider",
        "openai",
        "--model",
        "gpt-5.4-mini",
        "test prompt",
    ]);

    let output = cmd.assert().failure().get_output().clone();
    let stderr = String::from_utf8(output.stderr).expect("valid utf8");
    assert!(
        stderr.contains("LLM credentials not configured for provider 'openai'"),
        "expected local credential resolution failure, got: {stderr}"
    );
    assert!(
        !stderr.contains("config-should-not-be-used"),
        "exec should ignore configured server.target without --server: {stderr}"
    );
}

#[test]
fn exec_cli_server_target_overrides_configured_server_target() {
    let context = test_context!();
    let config_server = MockServer::start();
    config_server.mock(|when, then| {
        when.method("POST").path("/api/v1/completions");
        then.status(500).body("config-should-not-be-used");
    });
    let cli_server = MockServer::start();
    cli_server.mock(|when, then| {
        when.method("POST").path("/api/v1/completions");
        // Use a non-retriable error so this test covers target precedence
        // without paying the retry backoff cost of a 5xx response.
        then.status(400).body("cli-override-marker");
    });
    context.set_http_target(&config_server.base_url());

    let mut cmd = context.exec_cmd();
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled");
    cmd.args([
        "--server",
        &format!("{}/api/v1", cli_server.base_url()),
        "--provider",
        "openai",
        "--model",
        "gpt-5.4-mini",
        "test prompt",
    ]);

    let output = cmd.assert().failure().get_output().clone();
    let stderr = String::from_utf8(output.stderr).expect("valid utf8");
    assert!(
        stderr.contains("cli-override-marker"),
        "expected CLI server target to win, got: {stderr}"
    );
    assert!(
        !stderr.contains("config-should-not-be-used"),
        "configured server.target should not be used when --server is passed: {stderr}"
    );
}

#[test]
fn exec_server_target_uses_saved_cli_auth_without_local_api_key_resolution() {
    let context = test_context!();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/completions")
            .header("authorization", "Bearer access-oauth");
        then.status(400).body("oauth-routed-marker");
    });
    write_auth_entry(
        &context,
        &format!("{}/api/v1", server.base_url()),
        "access-oauth",
        "refresh-oauth",
    );

    let mut cmd = context.exec_cmd();
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled");
    cmd.args([
        "--server",
        &format!("{}/api/v1", server.base_url()),
        "--provider",
        "openai",
        "--model",
        "gpt-5.4-mini",
        "test prompt",
    ]);

    let output = cmd.assert().failure().get_output().clone();
    let stderr = String::from_utf8(output.stderr).expect("valid utf8");
    assert!(
        stderr.contains("oauth-routed-marker"),
        "expected exec to use stored CLI auth for remote transport, got: {stderr}"
    );
    assert!(
        !stderr.contains("API key not set"),
        "exec should not fall back to local provider auth when stored CLI auth exists: {stderr}"
    );
}

#[test]
fn exec_server_target_auth_failure_exits_with_4() {
    let context = test_context!();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/api/v1/completions");
        then.status(401)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "errors": [{
                    "status": "401",
                    "title": "Unauthorized",
                    "detail": "Authentication required.",
                    "code": "authentication_required"
                }]
            }));
    });

    let mut cmd = context.exec_cmd();
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled");
    cmd.args([
        "--server",
        &format!("{}/api/v1", server.base_url()),
        "--provider",
        "openai",
        "--model",
        "gpt-5.4-mini",
        "test prompt",
    ]);

    let output = cmd.assert().failure().get_output().clone();
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(
        fatal_error_line(&output.stderr),
        "LLM error: Authentication error for openai: Authentication required."
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = console::strip_ansi_codes(&stderr);
    assert!(
        stderr.contains("Run `fabro auth login` to authenticate."),
        "auth failures should retain the login help footer:\n{stderr}"
    );
}

#[test]
fn exec_direct_provider_auth_failure_stays_exit_1() {
    let context = test_context!();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method("POST")
            .path("/v1/messages")
            .header("x-api-key", "test-key");
        then.status(401)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "error": {
                    "type": "authentication_error",
                    "message": "bad key"
                }
            }));
    });

    // Override anthropic base_url via settings to point to the mock server
    context.write_home(
        ".fabro/settings.toml",
        format!(
            "_version = 1\n\n[llm.providers.anthropic]\nbase_url = \"{}/v1\"\n",
            server.base_url()
        ),
    );

    let mut cmd = context.exec_cmd();
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled")
        .env("ANTHROPIC_API_KEY", "test-key");
    cmd.args([
        "--provider",
        "anthropic",
        "--model",
        "claude-haiku-4-5",
        "test prompt",
    ]);

    let output = cmd.assert().failure().get_output().clone();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        fatal_error_line(&output.stderr),
        "LLM error: Authentication error for anthropic: bad key"
    );
}

#[test]
fn exec_retries_retryable_mid_stream_body_decode_error() {
    let context = test_context!();
    let llm_server = MidStreamDecodeErrorAnthropicServer::start();
    context.write_home(
        ".fabro/settings.toml",
        format!(
            "_version = 1\n\n[llm.providers.anthropic]\nbase_url = \"{}/v1\"\n",
            llm_server.base_url()
        ),
    );

    let mut cmd = context.exec_cmd();
    cmd.env_clear();
    preserve_coverage_env!(cmd);
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_STORAGE_DIR", &context.storage_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled")
        .env("ANTHROPIC_API_KEY", "test-key");
    cmd.args([
        "--provider",
        "anthropic",
        "--model",
        "claude-haiku-4-5",
        "--permissions",
        "read-only",
        "Say exactly: Recovered",
    ]);

    let output = cmd.output().expect("command should execute");
    assert!(
        output.status.success(),
        "exec should retry the failed LLM stream and succeed after the second response; requests: {}\nstdout:\n{}\nstderr:\n{}",
        llm_server.request_count(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        llm_server.request_count(),
        2,
        "exec should retry the retryable mid-stream body decode error"
    );
}

fn write_auth_entry(
    context: &fabro_test::TestContext,
    target: &str,
    access_token: &str,
    refresh_token: &str,
) {
    let now = Utc::now();
    let canonical = target
        .trim_end_matches('/')
        .trim_end_matches("/api/v1")
        .to_ascii_lowercase();
    context.write_home(
        ".fabro/auth.json",
        serde_json::to_string_pretty(&json!({
            "servers": {
                canonical: {
                    "kind": "oauth",
                    "access_token": access_token,
                    "access_token_expires_at": (now + ChronoDuration::minutes(10)).to_rfc3339(),
                    "refresh_token": refresh_token,
                    "refresh_token_expires_at": (now + ChronoDuration::days(30)).to_rfc3339(),
                    "subject": {
                        "idp_issuer": "https://github.com",
                        "idp_subject": "12345",
                        "login": "octocat",
                        "name": "The Octocat",
                        "email": "octocat@example.com"
                    },
                    "logged_in_at": now.to_rfc3339(),
                }
            }
        }))
        .expect("auth file should serialize"),
    );
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
        .timeout(std::time::Duration::from_mins(2))
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
        .timeout(std::time::Duration::from_mins(2))
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
        .timeout(std::time::Duration::from_mins(2))
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
        .timeout(std::time::Duration::from_mins(2))
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
        .timeout(std::time::Duration::from_mins(2))
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
