#![expect(
    clippy::disallowed_methods,
    reason = "These blocking CLI scenario tests spawn the real fabro binary and stream child pipes."
)]
#![expect(
    clippy::disallowed_types,
    reason = "These blocking CLI scenario tests intentionally use std::io readers around real child-process pipes."
)]

use std::io::{BufRead as _, BufReader, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fabro_test::{GitHubAppState, apply_test_isolation, test_context};
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use serde_json::{Value, json};

use crate::support::{
    RealAuthHarness, TEST_DEV_TOKEN, complete_login_via_browser, expire_saved_access_token,
    fatal_error_line, no_redirect_browser_client, run_detached, saved_auth_entry,
};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn auth_login_refresh_logout_flow() {
    let context = test_context!();
    let server = MockServer::start();
    let target = server_target(&server);

    let token_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/auth/cli/token")
            .header("content-type", "application/json");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(token_response("access-1", "refresh-1", 600, 3_600));
    });

    let login_output = complete_login(&context, &target);
    assert!(
        login_output.status.success(),
        "auth login failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&login_output.stdout),
        String::from_utf8_lossy(&login_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&login_output.stderr).contains("Logged in to"),
        "login output should confirm success:\n{}",
        String::from_utf8_lossy(&login_output.stderr)
    );
    token_mock.assert();

    let status = auth_status(&context, &target);
    assert_eq!(status["servers"].as_array().map(Vec::len), Some(1));
    assert_eq!(status["servers"][0]["oauth_state"], "active");
    assert_eq!(status["servers"][0]["login"], "octocat");

    let expired_access_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/system/info")
            .header("authorization", "Bearer access-1");
        then.status(401)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "errors": [{
                    "status": "401",
                    "title": "Unauthorized",
                    "detail": "Access token expired.",
                    "code": "access_token_expired"
                }]
            }));
    });
    let refresh_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/auth/cli/refresh")
            .header("authorization", "Bearer refresh-1");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(token_response("access-2", "refresh-2", 600, 3_600));
    });
    let refreshed_info_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/system/info")
            .header("authorization", "Bearer access-2");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "version": "1.2.3",
                "git_sha": "abcdef0",
                "build_date": "2026-04-20",
                "profile": "release",
                "os": "darwin",
                "arch": "arm64",
                "storage_dir": "/tmp/fabro-auth-flow",
                "storage_engine": "slatedb",
                "runs": { "total": 0, "active": 0 },
                "uptime_secs": 42
            }));
    });

    let system_info = context
        .command()
        .args(["--json", "system", "info", "--server", &target])
        .output()
        .expect("system info should run");
    assert!(
        system_info.status.success(),
        "system info failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_info.stdout),
        String::from_utf8_lossy(&system_info.stderr)
    );
    let system_info_json: Value =
        serde_json::from_slice(&system_info.stdout).expect("system info JSON should parse");
    assert_eq!(system_info_json["version"], "1.2.3");
    expired_access_mock.assert();
    refresh_mock.assert();
    refreshed_info_mock.assert();

    let logout_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/auth/cli/logout")
            .header("authorization", "Bearer refresh-2");
        then.status(204);
    });

    let logout = context
        .command()
        .args(["auth", "logout", "--server", &target])
        .output()
        .expect("auth logout should run");
    assert!(
        logout.status.success(),
        "auth logout failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&logout.stdout),
        String::from_utf8_lossy(&logout.stderr)
    );
    logout_mock.assert();

    let logged_out_status = auth_status(&context, &target);
    assert_eq!(
        logged_out_status["servers"].as_array().map(Vec::len),
        Some(0)
    );
}

#[test]
fn auth_refresh_failure_clears_local_session() {
    let context = test_context!();
    let server = MockServer::start();
    let target = server_target(&server);
    server.mock(|when, then| {
        when.method(POST)
            .path("/auth/cli/token")
            .header("content-type", "application/json");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(token_response(
                "access-revoked",
                "refresh-revoked",
                600,
                3_600,
            ));
    });

    let login_output = complete_login(&context, &target);
    assert!(
        login_output.status.success(),
        "auth login failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&login_output.stdout),
        String::from_utf8_lossy(&login_output.stderr)
    );

    let expired_access_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/system/info")
            .header("authorization", "Bearer access-revoked");
        then.status(401)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "errors": [{
                    "status": "401",
                    "title": "Unauthorized",
                    "detail": "Access token expired.",
                    "code": "access_token_expired"
                }]
            }));
    });
    let refresh_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/auth/cli/refresh")
            .header("authorization", "Bearer refresh-revoked");
        then.status(401)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "error": "refresh_token_revoked",
                "error_description": "CLI session has expired. Run `fabro auth login` again."
            }));
    });

    let system_info = context
        .command()
        .args(["--json", "system", "info", "--server", &target])
        .output()
        .expect("system info should run");
    assert!(
        !system_info.status.success(),
        "system info should fail when refresh is revoked"
    );
    assert_eq!(system_info.status.code(), Some(4));
    assert_eq!(
        fatal_error_line(&system_info.stderr),
        "CLI session has expired. Run `fabro auth login` again."
    );
    expired_access_mock.assert();
    refresh_mock.assert();

    let status = auth_status(&context, &target);
    assert_eq!(status["servers"].as_array().map(Vec::len), Some(0));

    let local_token_mock = server.mock(|when, then| {
        when.method(GET).path("/api/v1/system/info").header(
            "authorization",
            "Bearer fabro_dev_abababababababababababababababababababababababababababababababab",
        );
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "version": "dev-token-should-not-be-used"
            }));
    });
    let auth_required_mock = server.mock(|when, then| {
        when.method(GET).path("/api/v1/system/info");
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

    let second_system_info = context
        .command()
        .args(["--json", "system", "info", "--server", &target])
        .output()
        .expect("follow-up system info should run");
    assert!(
        !second_system_info.status.success(),
        "explicit remote target should not downgrade to a local dev token after refresh failure"
    );
    assert!(
        String::from_utf8_lossy(&second_system_info.stderr).contains("Authentication required."),
        "follow-up request should fail with auth required, got:\n{}",
        String::from_utf8_lossy(&second_system_info.stderr)
    );
    local_token_mock.assert_calls(0);
    auth_required_mock.assert();
}

#[test]
fn auth_required_error_suggests_auth_login() {
    let context = test_context!();
    let server = MockServer::start();
    let target = server_target(&server);

    let auth_required_mock = server.mock(|when, then| {
        when.method(GET).path("/api/v1/system/info");
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

    let output = context
        .command()
        .args(["system", "info", "--server", &target])
        .output()
        .expect("system info should run");

    assert_eq!(output.status.code(), Some(4));
    auth_required_mock.assert();

    let stderr = console::strip_ansi_codes(&String::from_utf8_lossy(&output.stderr)).into_owned();
    assert!(
        stderr.contains("Authentication required."),
        "stderr should surface the auth error, got:\n{stderr}"
    );
    assert!(
        stderr.contains("Run `fabro auth login` to authenticate."),
        "stderr should suggest `fabro auth login`, got:\n{stderr}"
    );
}

#[test]
fn auth_required_hint_is_suppressed_in_json_mode() {
    let context = test_context!();
    let server = MockServer::start();
    let target = server_target(&server);

    server.mock(|when, then| {
        when.method(GET).path("/api/v1/system/info");
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

    let output = context
        .command()
        .args(["--json", "system", "info", "--server", &target])
        .output()
        .expect("system info should run");

    assert_eq!(output.status.code(), Some(4));
    let stderr = console::strip_ansi_codes(&String::from_utf8_lossy(&output.stderr)).into_owned();
    assert!(
        stderr.contains("Authentication required."),
        "stderr should still surface the auth error in --json mode, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("fabro auth login"),
        "stderr should not include the hint in --json mode, got:\n{stderr}"
    );
}

#[test]
fn auth_login_rejects_unix_socket_target() {
    let context = test_context!();
    let socket_path = context.temp_dir.join("fabro.sock");

    let output = context
        .command()
        .args([
            "auth",
            "login",
            "--no-browser",
            "--server",
            socket_path.to_str().unwrap(),
        ])
        .output()
        .expect("auth login should execute");

    assert!(
        !output.status.success(),
        "auth login should fail for unix-socket targets\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("fabro auth login requires an HTTP(S) server target"),
        "unix-socket rejection should explain the transport requirement:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_login_refresh_logout_flow_against_real_server_and_twin_github() {
    let context = test_context!();
    let harness = RealAuthHarness::start(GitHubAppState::new()).await;
    let target = harness.api_target();

    let (login_output, browser_url) = complete_login_via_browser(&context, &target).await;
    assert!(
        login_output.status.success(),
        "auth login failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&login_output.stdout),
        String::from_utf8_lossy(&login_output.stderr)
    );
    assert!(
        browser_url.starts_with(&format!("{}/auth/cli/start", harness.api_base_url)),
        "browser flow should open the server target origin, got: {browser_url}"
    );

    let status = auth_status(&context, &target);
    assert_eq!(status["servers"].as_array().map(Vec::len), Some(1));
    assert_eq!(status["servers"][0]["oauth_state"], "active");
    assert_eq!(status["servers"][0]["login"], "octocat");
    assert_eq!(status["servers"][0]["server"], harness.api_base_url);
    assert!(harness.api_requests.contains("POST /auth/cli/token"));
    assert!(harness.api_requests.contains("GET /auth/cli/start"));
    assert!(harness.api_requests.contains("GET /auth/cli/resume"));
    assert!(harness.api_requests.contains("POST /auth/cli/resume"));

    harness.api_requests.clear();
    let exec_output = context
        .exec_cmd()
        .args([
            "--server",
            &target,
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "say hello",
        ])
        .output()
        .expect("exec should run");
    assert!(
        !exec_output.status.success(),
        "real-server exec should fail on missing provider credentials, not auth\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&exec_output.stdout),
        String::from_utf8_lossy(&exec_output.stderr)
    );
    assert!(harness.api_requests.contains("POST /api/v1/completions"));
    assert!(
        !String::from_utf8_lossy(&exec_output.stderr).contains("API key not set"),
        "exec --server should use server auth instead of local provider auth:\n{}",
        String::from_utf8_lossy(&exec_output.stderr)
    );

    harness.api_requests.clear();
    let workflow = context.install_fixture("simple.fabro");
    let first_run_id = run_detached(&context, &target, &workflow);
    assert!(harness.api_requests.contains("POST /api/v1/runs"));
    assert!(
        harness
            .api_requests
            .contains(&format!("POST /api/v1/runs/{first_run_id}/start"))
    );

    harness.api_requests.clear();
    expire_saved_access_token(&context, &harness.api_base_url);
    let expired_exec_output = context
        .exec_cmd()
        .args([
            "--server",
            &target,
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "say hello again",
        ])
        .output()
        .expect("expired exec should run");
    assert!(
        !expired_exec_output.status.success(),
        "expired exec should still reach the server after refresh\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&expired_exec_output.stdout),
        String::from_utf8_lossy(&expired_exec_output.stderr)
    );
    assert!(harness.api_requests.contains("POST /auth/cli/refresh"));
    assert!(harness.api_requests.contains("POST /api/v1/completions"));

    harness.api_requests.clear();
    let second_run_id = run_detached(&context, &target, &workflow);
    assert!(harness.api_requests.contains("POST /api/v1/runs"));
    assert!(
        harness
            .api_requests
            .contains(&format!("POST /api/v1/runs/{second_run_id}/start"))
    );
    assert!(
        !harness.api_requests.contains("POST /auth/cli/refresh"),
        "subsequent commands should use the freshly persisted access token without another refresh"
    );

    let refreshed_entry = saved_auth_entry(&context);
    let refreshed_refresh_token = refreshed_entry["refresh_token"]
        .as_str()
        .expect("saved auth should include refresh token")
        .to_string();

    let logout = context
        .command()
        .args(["auth", "logout", "--server", &target])
        .output()
        .expect("auth logout should run");
    assert!(
        logout.status.success(),
        "auth logout failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&logout.stdout),
        String::from_utf8_lossy(&logout.stderr)
    );

    let logged_out_status = auth_status(&context, &target);
    assert_eq!(
        logged_out_status["servers"].as_array().map(Vec::len),
        Some(0)
    );

    let logged_out_run = context
        .run_cmd()
        .args([
            "--server",
            &target,
            "--detach",
            "--dry-run",
            "--auto-approve",
            workflow.to_str().unwrap(),
        ])
        .output()
        .expect("logged-out detached run should execute");
    assert!(
        !logged_out_run.status.success(),
        "detached run should fail after logout\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&logged_out_run.stdout),
        String::from_utf8_lossy(&logged_out_run.stderr)
    );

    let refresh_response = fabro_test::test_http_client()
        .post(format!("{}/auth/cli/refresh", harness.api_base_url))
        .bearer_auth(&refreshed_refresh_token)
        .send()
        .await
        .expect("refresh request should succeed");
    assert_eq!(refresh_response.status(), 401);
    let refresh_body: Value = refresh_response
        .json()
        .await
        .expect("refresh error body should parse");
    assert_eq!(refresh_body["error"], "refresh_token_expired");
    assert!(harness.api_requests.contains("POST /auth/cli/logout"));

    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_login_surfaces_access_denied_from_real_browser_flow() {
    let context = test_context!();
    let mut github_state = GitHubAppState::new();
    github_state.allow_authorize = false;
    let harness = RealAuthHarness::start(github_state).await;
    let target = harness.api_target();

    let (login_output, browser_url) = complete_login_via_browser(&context, &target).await;
    assert!(
        browser_url.starts_with(&format!("{}/auth/cli/start", harness.api_base_url)),
        "browser flow should open the server target origin, got: {browser_url}"
    );
    assert!(
        !login_output.status.success(),
        "auth login should fail when GitHub denies access\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&login_output.stdout),
        String::from_utf8_lossy(&login_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&login_output.stderr).contains("Authorization denied."),
        "denial should surface in CLI stderr:\n{}",
        String::from_utf8_lossy(&login_output.stderr)
    );

    let status = auth_status(&context, &target);
    assert_eq!(status["servers"].as_array().map(Vec::len), Some(0));

    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_cli_start_ignores_dev_token_session_and_redirects_to_github_login() {
    let harness = RealAuthHarness::start_with_dev_token(GitHubAppState::new()).await;
    let client = no_redirect_browser_client();

    let login_response = client
        .post(format!("{}/auth/login/dev-token", harness.api_base_url))
        .json(&json!({ "token": TEST_DEV_TOKEN }))
        .send()
        .await
        .expect("dev-token login request should succeed");
    assert_eq!(login_response.status(), reqwest::StatusCode::OK);

    let response = client
        .get(format!(
            "{}/auth/cli/start?redirect_uri=http://127.0.0.1:4444/callback&state=abcdefghijklmnop&code_challenge=challenge&code_challenge_method=S256",
            harness.api_base_url
        ))
        .send()
        .await
        .expect("cli start request should succeed");

    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/auth/login/github?return_to=/auth/cli/resume")
    );

    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_relogin_overwrites_local_entry_and_keeps_previous_refresh_chain_alive() {
    let context = test_context!();
    let harness = RealAuthHarness::start(GitHubAppState::new()).await;
    let target = harness.api_target();

    let (first_login_output, _) = complete_login_via_browser(&context, &target).await;
    assert!(
        first_login_output.status.success(),
        "first auth login failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&first_login_output.stdout),
        String::from_utf8_lossy(&first_login_output.stderr)
    );
    let first_entry = saved_auth_entry(&context);
    let first_refresh_token = first_entry["refresh_token"]
        .as_str()
        .expect("first auth entry should include refresh token")
        .to_string();
    let first_access_token = first_entry["access_token"]
        .as_str()
        .expect("first auth entry should include access token")
        .to_string();

    let (second_login_output, _) = complete_login_via_browser(&context, &target).await;
    assert!(
        second_login_output.status.success(),
        "second auth login failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&second_login_output.stdout),
        String::from_utf8_lossy(&second_login_output.stderr)
    );
    let second_entry = saved_auth_entry(&context);
    let second_refresh_token = second_entry["refresh_token"]
        .as_str()
        .expect("second auth entry should include refresh token")
        .to_string();
    let second_access_token = second_entry["access_token"]
        .as_str()
        .expect("second auth entry should include access token")
        .to_string();

    assert_ne!(second_refresh_token, first_refresh_token);
    assert_ne!(second_access_token, first_access_token);

    let status = auth_status(&context, &target);
    assert_eq!(status["servers"].as_array().map(Vec::len), Some(1));
    assert_eq!(status["servers"][0]["server"], harness.api_base_url);

    let old_refresh_response = fabro_test::test_http_client()
        .post(format!("{}/auth/cli/refresh", harness.api_base_url))
        .bearer_auth(&first_refresh_token)
        .send()
        .await
        .expect("old refresh token should reach the server");
    assert_eq!(old_refresh_response.status(), reqwest::StatusCode::OK);
    let old_refresh_body: Value = old_refresh_response
        .json()
        .await
        .expect("old refresh body should parse");
    assert_ne!(
        old_refresh_body["refresh_token"].as_str(),
        Some(first_refresh_token.as_str())
    );

    let saved_after_old_refresh = saved_auth_entry(&context);
    assert_eq!(
        saved_after_old_refresh["refresh_token"].as_str(),
        Some(second_refresh_token.as_str())
    );

    harness.shutdown().await;
}

fn server_target(server: &MockServer) -> String {
    format!("{}/api/v1", server.base_url())
}

fn token_response(
    access_token: &str,
    refresh_token: &str,
    access_lifetime_secs: i64,
    refresh_lifetime_secs: i64,
) -> Value {
    let now = chrono::Utc::now();
    json!({
        "access_token": access_token,
        "access_token_expires_at": (now + chrono::Duration::seconds(access_lifetime_secs)).to_rfc3339(),
        "refresh_token": refresh_token,
        "refresh_token_expires_at": (now + chrono::Duration::seconds(refresh_lifetime_secs)).to_rfc3339(),
        "subject": {
            "idp_issuer": "https://github.com",
            "idp_subject": "12345",
            "login": "octocat",
            "name": "The Octocat",
            "email": "octocat@example.com"
        }
    })
}

fn auth_status(context: &fabro_test::TestContext, target: &str) -> Value {
    let output = context
        .command()
        .args(["--json", "auth", "status", "--server", target])
        .output()
        .expect("auth status should run");
    assert!(
        output.status.success(),
        "auth status failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("auth status JSON should parse")
}

fn complete_login(context: &fabro_test::TestContext, target: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
    apply_test_isolation(&mut cmd, &context.home_dir);
    cmd.current_dir(&context.temp_dir);
    cmd.args(["auth", "login", "--no-browser", "--server", target]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("auth login should spawn");
    let mut stdout = child
        .stdout
        .take()
        .expect("auth login stdout should be piped");
    let stderr = child
        .stderr
        .take()
        .expect("auth login stderr should be piped");
    let (url_tx, url_rx) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || read_stderr_and_capture_url(stderr, &url_tx));

    let browser_url = wait_for_login_url(&mut child, &mut stdout, &url_rx);
    deliver_callback(&browser_url);

    let status = child.wait().expect("auth login should exit");
    let mut stdout_bytes = Vec::new();
    stdout
        .read_to_end(&mut stdout_bytes)
        .expect("auth login stdout should be readable");
    let stderr_bytes = stderr_reader.join().expect("stderr reader should join");

    Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    }
}

fn read_stderr_and_capture_url(
    stderr: impl std::io::Read,
    url_tx: &mpsc::Sender<String>,
) -> Vec<u8> {
    let mut reader = BufReader::new(stderr);
    let mut stderr_bytes = Vec::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .expect("auth login stderr should be readable");
        if read == 0 {
            break;
        }
        let trimmed = String::from_utf8_lossy(&line).trim().to_string();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            let _ = url_tx.send(trimmed);
        }
        stderr_bytes.extend_from_slice(&line);
    }

    stderr_bytes
}

fn wait_for_login_url(
    child: &mut std::process::Child,
    stdout: &mut impl Read,
    url_rx: &mpsc::Receiver<String>,
) -> String {
    let deadline = Instant::now() + LOGIN_TIMEOUT;

    loop {
        match url_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(url) => return url,
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }

        if let Some(status) = child.try_wait().expect("auth login should stay alive") {
            let mut stdout_bytes = Vec::new();
            stdout
                .read_to_end(&mut stdout_bytes)
                .expect("auth login stdout should be readable");
            panic!(
                "auth login exited before printing the browser URL: {status}\nstdout:\n{}",
                String::from_utf8_lossy(&stdout_bytes),
            );
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().expect("auth login should exit after kill");
            let mut stdout_bytes = Vec::new();
            stdout
                .read_to_end(&mut stdout_bytes)
                .expect("auth login stdout should be readable");
            panic!(
                "timed out waiting for auth login browser URL\nstatus: {status}\nstdout:\n{}",
                String::from_utf8_lossy(&stdout_bytes),
            );
        }
    }
}

fn deliver_callback(browser_url: &str) {
    let parsed = fabro_http::Url::parse(browser_url).expect("browser URL should parse");
    let mut redirect_uri = None;
    let mut state = None;

    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "redirect_uri" => redirect_uri = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }

    let redirect_uri = redirect_uri.expect("browser URL should include redirect_uri");
    let state = state.expect("browser URL should include state");
    super::block_on(async move {
        let response = fabro_test::test_http_client()
            .get(format!("{redirect_uri}?code=cli-code&state={state}"))
            .send()
            .await
            .expect("callback request should succeed");
        assert!(
            response.status().is_success(),
            "callback request failed: {}",
            response.status()
        );
    });
}
