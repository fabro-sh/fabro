#![expect(
    clippy::disallowed_methods,
    reason = "These worker-auth regressions start a real server subprocess, write isolated auth fixtures, and spawn the compiled fabro binary."
)]
#![expect(
    clippy::disallowed_types,
    reason = "These regressions intentionally own Child processes to exercise the real server-dispatched worker path."
)]
#![expect(
    clippy::unwrap_used,
    reason = "Integration-test setup for real-subprocess auth harness; panic-on-failure is the desired behavior."
)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use fabro_client::{AuthEntry, AuthStore, OAuthEntry, ServerTarget, StoredSubject};
use fabro_config::{Storage, envfile};
use fabro_store::EventEnvelope;
use fabro_test::{apply_test_isolation, expect_reqwest_json, isolated_storage_dir, test_context};
use hkdf::Hkdf;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use sha2::Sha256;

use super::support::{find_run_dir, output_stderr, output_stdout};
use crate::support::{
    TEST_SESSION_SECRET, issue_test_github_jwt, parse_event_envelopes, unique_run_id,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const TEST_GITHUB_CLIENT_SECRET: &str = "github-client-secret";
const WORKER_TOKEN_ISSUER: &str = "fabro-server-worker";
const WORKER_TOKEN_SCOPE: &str = "run:worker";
const WORKER_TOKEN_TTL_SECS: u64 = 72 * 60 * 60;

struct RunningGithubOnlyServer {
    child:         Option<Child>,
    home_root:     tempfile::TempDir,
    worker_home:   PathBuf,
    _storage_root: tempfile::TempDir,
    storage_dir:   PathBuf,
    api_base_url:  String,
}

impl RunningGithubOnlyServer {
    async fn start() -> Self {
        let home_root = tempfile::tempdir_in("/tmp").unwrap();
        let worker_home = home_root.path().join("worker-home");
        std::fs::create_dir_all(&worker_home).unwrap();

        let storage_root = isolated_storage_dir();
        let storage_dir = storage_root.path().join("storage");
        let port = reserve_port();
        let api_base_url = format!("http://127.0.0.1:{port}");
        let config_path = home_root.path().join("settings.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"_version = 1

[server.web]
enabled = true
url = "{api_base_url}"

[server.auth]
methods = ["github"]

[server.auth.github]
allowed_usernames = ["octocat"]

[server.integrations.github]
client_id = "github-client-id"
"#
            ),
        )
        .unwrap();
        envfile::merge_env_file(
            &Storage::new(&storage_dir).runtime_directory().env_path(),
            [
                ("SESSION_SECRET", TEST_SESSION_SECRET),
                ("GITHUB_APP_CLIENT_SECRET", TEST_GITHUB_CLIENT_SECRET),
            ],
        )
        .unwrap();

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
        apply_test_isolation(&mut cmd, home_root.path());
        cmd.env("FABRO_HOME", &worker_home);
        cmd.args(["server", "start", "--foreground"])
            .arg("--storage-dir")
            .arg(&storage_dir)
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--config")
            .arg(&config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().expect("github-only server should spawn");
        wait_for_http_ready(&api_base_url, &mut child).await;

        Self {
            child: Some(child),
            home_root,
            worker_home,
            _storage_root: storage_root,
            storage_dir,
            api_base_url,
        }
    }

    fn target(&self) -> String {
        format!("{}/api/v1", self.api_base_url)
    }

    fn shutdown(mut self) {
        let mut stop = Command::new(env!("CARGO_BIN_EXE_fabro"));
        apply_test_isolation(&mut stop, self.home_root.path());
        stop.env("FABRO_HOME", &self.worker_home);
        stop.args(["server", "stop"])
            .arg("--storage-dir")
            .arg(&self.storage_dir);
        let output = stop.output().expect("server stop should run");
        assert!(
            output.status.success(),
            "server stop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let output = self
            .child
            .take()
            .expect("server child should still be present")
            .wait_with_output()
            .expect("server output should be readable");
        assert!(
            output.status.success(),
            "github-only server exited unsuccessfully\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for RunningGithubOnlyServer {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

#[derive(serde::Serialize)]
struct WorkerTokenClaims {
    iss:    String,
    iat:    u64,
    exp:    u64,
    run_id: String,
    scope:  String,
    jti:    String,
}

fn reserve_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_submitter_auth(home_dir: &Path, target: &str, access_token: &str) {
    let auth_store = AuthStore::new(home_dir.join(".fabro").join("auth.json"));
    let target = ServerTarget::http_url(target).unwrap();
    let now = Utc::now();
    auth_store
        .put(
            &target,
            AuthEntry::OAuth(OAuthEntry {
                access_token:             access_token.to_string(),
                access_token_expires_at:  now + ChronoDuration::minutes(10),
                refresh_token:            "refresh-unused".to_string(),
                refresh_token_expires_at: now + ChronoDuration::days(30),
                subject:                  StoredSubject {
                    idp_issuer:  "https://github.com".to_string(),
                    idp_subject: "12345".to_string(),
                    login:       "octocat".to_string(),
                    name:        "The Octocat".to_string(),
                    email:       "octocat@example.com".to_string(),
                },
                logged_in_at:             now,
            }),
        )
        .unwrap();
}

fn write_probe_workflow(path: &Path) {
    std::fs::write(
        path,
        r#"digraph WorkerAuthProbe {
  graph [goal="Verify github-only worker auth", default_max_retries=0]
  start [shape=Mdiamond]
  exit [shape=Msquare]
  probe [shape=parallelogram, script="printf worker-auth-ok"]
  start -> probe -> exit
}
"#,
    )
    .unwrap();
}

fn issue_worker_token_for_run(storage_dir: &Path, run_id: &str) -> String {
    let runtime_directory = Storage::new(storage_dir).runtime_directory();
    let session_secret = envfile::read_env_file(&runtime_directory.env_path())
        .expect("server env should load")
        .get("SESSION_SECRET")
        .cloned()
        .expect("server env should include SESSION_SECRET");
    let hkdf = Hkdf::<Sha256>::new(None, session_secret.as_bytes());
    let mut key = [0_u8; 32];
    hkdf.expand(b"fabro-worker-jwt-v1", &mut key)
        .expect("worker jwt hkdf output should fit");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let claims = WorkerTokenClaims {
        iss:    WORKER_TOKEN_ISSUER.to_string(),
        iat:    now,
        exp:    now + WORKER_TOKEN_TTL_SECS,
        run_id: run_id.to_string(),
        scope:  WORKER_TOKEN_SCOPE.to_string(),
        jti:    format!("{:032x}", rand::random::<u128>()),
    };

    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&key),
    )
    .expect("worker token should encode")
}

fn wait_for_run_dir(storage_dir: &Path, run_id: &str) -> PathBuf {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        if let Some(run_dir) = find_run_dir(storage_dir, run_id) {
            return run_dir;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for run dir for {run_id}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

async fn wait_for_http_ready(base_url: &str, child: &mut Child) {
    let client = fabro_test::test_http_client();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match client.get(format!("{base_url}/health")).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(_) | Err(_) if Instant::now() < deadline => {
                if let Some(status) = child.try_wait().expect("server process should poll") {
                    let mut stderr = Vec::new();
                    if let Some(stderr_pipe) = child.stderr.as_mut() {
                        stderr_pipe
                            .read_to_end(&mut stderr)
                            .expect("server stderr should be readable");
                    }
                    panic!(
                        "github-only server exited before becoming ready with status {status}\nstderr:\n{}",
                        String::from_utf8_lossy(&stderr)
                    );
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(response) => panic!("server at {base_url} was not ready: {}", response.status()),
            Err(err) => panic!("server at {base_url} was not ready: {err}"),
        }
    }
}

async fn run_events(api_base_url: &str, run_id: &str, access_token: &str) -> Vec<EventEnvelope> {
    let response = fabro_test::test_http_client()
        .get(format!("{api_base_url}/api/v1/runs/{run_id}/events"))
        .bearer_auth(access_token)
        .send()
        .await
        .expect("event request should succeed");
    let body: serde_json::Value = expect_reqwest_json(
        response,
        fabro_http::StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/events"),
    )
    .await;
    parse_event_envelopes(&body)
}

async fn wait_for_completed_events(
    api_base_url: &str,
    run_id: &str,
    access_token: &str,
) -> Vec<EventEnvelope> {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        let events = run_events(api_base_url, run_id, access_token).await;
        if events
            .iter()
            .any(|event| event.event.event_name() == "run.completed")
        {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for run.completed for {run_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn github_only_server_dispatched_worker_succeeds_without_worker_auth_store() {
    let context = test_context!();
    let server = RunningGithubOnlyServer::start().await;
    let target = server.target();
    let access_token = issue_test_github_jwt(&server.api_base_url);
    write_submitter_auth(&context.home_dir, &target, &access_token);
    assert!(!server.worker_home.join("auth.json").exists());
    assert!(!server.worker_home.join("auth.lock").exists());

    let workflow = context.temp_dir.join("worker-auth.fabro");
    write_probe_workflow(&workflow);
    let run_id = unique_run_id();
    let output = context
        .run_cmd()
        .args([
            "--server",
            &target,
            "--run-id",
            &run_id,
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            "local",
            workflow.to_str().unwrap(),
        ])
        .output()
        .expect("detached run should execute");

    assert!(
        output.status.success(),
        "github-only detached run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output_stdout(&output).trim(), run_id);

    let _run_dir = wait_for_run_dir(&server.storage_dir, &run_id);
    let events = wait_for_completed_events(&server.api_base_url, &run_id, &access_token).await;

    assert!(events.iter().any(|event| {
        matches!(
            event.event.actor.as_ref(),
            Some(fabro_api::types::Principal::Worker { run_id: actor_run_id })
                if actor_run_id.to_string() == run_id
        )
    }));
    assert!(!server.worker_home.join("auth.json").exists());
    assert!(!server.worker_home.join("auth.lock").exists());

    server.shutdown();
}

#[test]
fn runner_rejects_bogus_worker_token_against_github_only_server() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let context = test_context!();
        let server = RunningGithubOnlyServer::start().await;
        let target = server.target();
        let access_token = issue_test_github_jwt(&server.api_base_url);
        write_submitter_auth(&context.home_dir, &target, &access_token);

        let workflow = context.temp_dir.join("worker-auth-negative.fabro");
        write_probe_workflow(&workflow);
        let run_id = unique_run_id();
        let create_output = context
            .create_cmd()
            .args([
                "--server",
                &target,
                "--run-id",
                &run_id,
                "--dry-run",
                "--auto-approve",
                "--no-retro",
                "--sandbox",
                "local",
                workflow.to_str().unwrap(),
            ])
            .output()
            .expect("remote create should execute");

        assert!(
            create_output.status.success(),
            "github-only create failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&create_output.stdout),
            String::from_utf8_lossy(&create_output.stderr)
        );
        assert_eq!(output_stdout(&create_output).trim(), run_id);

        let run_dir = wait_for_run_dir(&server.storage_dir, &run_id);
        let worker_root = tempfile::tempdir_in("/tmp").unwrap();
        let worker_home = worker_root.path().join("fabro-home");
        std::fs::create_dir_all(&worker_home).unwrap();
        let auth_file = worker_root.path().join("missing").join("auth.json");
        let bogus_token = issue_worker_token_for_run(&server.storage_dir, &unique_run_id());

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
        apply_test_isolation(&mut cmd, worker_root.path());
        cmd.env("FABRO_HOME", &worker_home);
        cmd.env("FABRO_AUTH_FILE", &auth_file);
        cmd.env("FABRO_WORKER_TOKEN", bogus_token);
        cmd.args([
            "__run-worker",
            "--server",
            &target,
            "--run-dir",
            run_dir.to_str().unwrap(),
            "--run-id",
            &run_id,
            "--mode",
            "start",
        ]);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let output = cmd.output().expect("worker should execute");

        assert!(
            !output.status.success(),
            "worker should fail with a bogus token\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = output_stderr(&output);
        assert!(
            stderr.contains("403")
                || stderr.contains("Forbidden")
                || stderr.contains("Authentication required")
                || stderr.contains("Access denied"),
            "{stderr}"
        );
        assert!(!auth_file.exists());
        assert!(!auth_file.with_extension("lock").exists());

        server.shutdown();
    });
}
