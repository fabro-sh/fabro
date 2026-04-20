#![expect(
    clippy::disallowed_methods,
    reason = "These blocking CLI scenario tests spawn the real fabro binary and stream child pipes."
)]

use std::io::{BufRead as _, BufReader, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fabro_test::{apply_test_isolation, test_context};
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use serde_json::{Value, json};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn auth_login_refresh_logout_flow() {
    let context = test_context!();
    let server = MockServer::start();
    let target = server_target(&server);

    let config_mock = server.mock(|when, then| {
        when.method(GET).path("/api/v1/auth/cli/config");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "enabled": true,
                "web_url": server.base_url(),
                "methods": ["github"]
            }));
    });
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
    config_mock.assert();
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
        when.method(GET).path("/api/v1/auth/cli/config");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(json!({
                "enabled": true,
                "web_url": server.base_url(),
                "methods": ["github"]
            }));
    });
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
    assert!(
        String::from_utf8_lossy(&system_info.stderr).contains("fabro auth login"),
        "refresh failure should direct the user to log in again:\n{}",
        String::from_utf8_lossy(&system_info.stderr)
    );
    expired_access_mock.assert();
    refresh_mock.assert();

    let status = auth_status(&context, &target);
    assert_eq!(status["servers"].as_array().map(Vec::len), Some(0));
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
    let stderr_reader = std::thread::spawn(move || read_stderr_and_capture_url(stderr, url_tx));

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
    url_tx: mpsc::Sender<String>,
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
