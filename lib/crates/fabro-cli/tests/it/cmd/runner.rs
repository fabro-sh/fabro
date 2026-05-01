#![expect(
    clippy::disallowed_methods,
    reason = "These CLI integration tests spawn real fabro worker subprocesses and observe their lifecycle."
)]
#![expect(
    clippy::disallowed_types,
    reason = "integration tests read the spawned child's stdout via std::io::Read"
)]

use std::io::Read;
use std::path::Path;
use std::process::{Child, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use fabro_client::ServerTarget;
use fabro_config::{Storage, envfile};
use fabro_store::EventEnvelope;
use fabro_test::{
    assert_reqwest_status, expect_reqwest_json, fabro_json_snapshot, fabro_snapshot, test_context,
};
use fabro_types::{CommandOutputStream, EventBody, FailureReason, RunEvent, StageId};
use hkdf::Hkdf;
use httpmock::MockServer;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use sha2::Sha256;

use super::support::{
    command_log_text, find_run_dir, local_dev_token, output_stderr, run_events, run_state,
    server_endpoint, server_target, wait_for_event_names, wait_for_status, write_gated_workflow,
};
use crate::support::{seed_dev_token_auth, unique_run_id};

const SHARED_DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const LEAKED_WORKER_PARENT_TOKEN: &str = "leak-worker-parent-token";
const LEAKED_NEW_RELIC_LICENSE: &str = "leak-new-relic-license";
const WORKER_TOKEN_ISSUER: &str = "fabro-server-worker";
const WORKER_TOKEN_SCOPE: &str = "run:worker";
const WORKER_TOKEN_TTL_SECS: u64 = 72 * 60 * 60;

fn auth_context() -> fabro_test::TestContext {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    context
}

fn stored_worker_events(run_dir: &std::path::Path) -> Vec<RunEvent> {
    run_events(run_dir).iter().map(run_event).collect()
}

fn run_event(event: &EventEnvelope) -> RunEvent {
    event.event.clone()
}

fn assert_worker_succeeded(run_dir: &std::path::Path, stdout: &[u8]) {
    assert!(
        stdout.is_empty(),
        "worker should not emit event transport on stdout"
    );
    let events = stored_worker_events(run_dir);
    assert!(events.iter().any(|event| matches!(
        &event.body,
        EventBody::RunCompleted(props) if props.status == "succeeded"
    )));
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

fn worker_token_for_run(storage_dir: &Path, run_id: &str) -> String {
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

fn spawn_worker_process(
    context: &fabro_test::TestContext,
    server: &str,
    run_dir: &std::path::Path,
    run_id: &str,
    mode: &str,
) -> Child {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    fabro_test::apply_test_isolation(&mut cmd, &context.home_dir);
    cmd.current_dir(&context.temp_dir);
    cmd.env(
        "FABRO_WORKER_TOKEN",
        worker_token_for_run(&context.storage_dir, run_id),
    );
    cmd.args([
        "__run-worker",
        "--server",
        server,
        "--run-dir",
        run_dir
            .to_str()
            .expect("run directory path should be valid UTF-8"),
        "--run-id",
        run_id,
        "--mode",
        mode,
    ]);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.spawn().expect("worker should spawn")
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration helper polls child exit without requiring a Tokio runtime."
)]
fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("worker wait should succeed") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for worker to exit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn child_output(mut child: Child, status: ExitStatus) -> Output {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_end(&mut stdout)
            .expect("worker stdout should be readable");
    }
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_end(&mut stderr)
            .expect("worker stderr should be readable");
    }
    Output {
        status,
        stdout,
        stderr,
    }
}

fn worker_command(context: &fabro_test::TestContext, run_id: &str) -> assert_cmd::Command {
    let mut cmd = context.command();
    cmd.env(
        "FABRO_WORKER_TOKEN",
        worker_token_for_run(&context.storage_dir, run_id),
    );
    cmd
}

fn assert_no_worker_env_leak(scope: &str, content: &str) {
    for needle in [
        "MY_API_TOKEN=",
        "NEW_RELIC_LICENSE_KEY=",
        "FABRO_WORKER_TOKEN=",
        LEAKED_WORKER_PARENT_TOKEN,
        LEAKED_NEW_RELIC_LICENSE,
    ] {
        assert!(
            !content.contains(needle),
            "{scope} leaked {needle:?}:\n{content}"
        );
    }
}

async fn wait_for_server_question(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run_id: &str,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + SHARED_DAEMON_TIMEOUT;
    loop {
        let response = client
            .get(format!("{base_url}/api/v1/runs/{run_id}/questions"))
            .query(&[("page[limit]", "100"), ("page[offset]", "0")])
            .send()
            .await
            .expect("question request should succeed");
        let body: serde_json::Value = expect_reqwest_json(
            response,
            fabro_http::StatusCode::OK,
            format!("GET /api/v1/runs/{run_id}/questions?page[limit]=100&page[offset]=0"),
        )
        .await;
        if let Some(question) = body["data"].as_array().and_then(|items| items.first()) {
            return question.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for a pending question"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["__run-worker", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Internal: execute a single workflow run locally

    Usage: fabro __run-worker [OPTIONS] --server <SERVER> --run-dir <RUN_DIR> --run-id <RUN_ID> --mode <MODE>

    Options:
          --json               Output as JSON [env: FABRO_JSON=]
          --server <SERVER>    Fabro server target: http(s) URL or absolute Unix socket path
          --debug              Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check   Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --run-dir <RUN_DIR>  Run scratch directory
          --quiet              Suppress non-essential output [env: FABRO_QUIET=]
          --run-id <RUN_ID>    Run ID
          --mode <MODE>        Worker mode [possible values: start, resume]
          --verbose            Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help               Print help
    ----- stderr -----
    ");
}

#[test]
fn worker_requires_fabro_worker_token_env() {
    let context = auth_context();
    let run_dir = tempfile::tempdir().unwrap();
    let run_id = unique_run_id();
    let output = context
        .command()
        .args([
            "__run-worker",
            "--server",
            "http://127.0.0.1:32276",
            "--run-dir",
            run_dir.path().to_str().unwrap(),
            "--run-id",
            &run_id,
            "--mode",
            "start",
        ])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .output()
        .expect("worker should execute");

    assert!(!output.status.success());
    assert!(
        output_stderr(&output).contains("FABRO_WORKER_TOKEN"),
        "{}",
        output_stderr(&output)
    );
}

#[test]
fn runner_uses_cached_graph_after_source_deleted() {
    let context = auth_context();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_temp(
        "workflow.fabro",
        "\
digraph CachedGraph {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    let server = server_target(&context.storage_dir);
    std::fs::remove_file(&workflow_path).unwrap();

    let output = worker_command(&context, run_id.as_str())
        .args([
            "__run-worker",
            "--server",
            server.as_str(),
            "--run-dir",
            run_dir.to_str().unwrap(),
            "--run-id",
            run_id.as_str(),
            "--mode",
            "start",
        ])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_worker_succeeded(&run_dir, &output);
}

#[test]
fn runner_local_dry_runs_ignore_github_app_configuration() {
    let context = auth_context();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_home(
        ".fabro/settings.toml",
        "\
_version = 1

[server.auth]
methods = [\"dev-token\"]

[server.integrations.github]
app_id = \"fixture-app-id\"
",
    );
    context.write_temp(
        "workflow.fabro",
        "\
digraph GitHubApp {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    context.write_home(".fabro/settings.toml", "_version = 1\n");

    let server = server_target(&context.storage_dir);
    let mut cmd = worker_command(&context, run_id.as_str());
    cmd.env("GITHUB_APP_PRIVATE_KEY", "%%%not-base64%%%");
    cmd.args([
        "__run-worker",
        "--server",
        server.as_str(),
        "--run-dir",
        run_dir.to_str().unwrap(),
        "--run-id",
        run_id.as_str(),
        "--mode",
        "start",
    ]);
    cmd.timeout(SHARED_DAEMON_TIMEOUT);
    let assert = cmd.assert().success();
    assert_worker_succeeded(&run_dir, &assert.get_output().stdout);
}

#[test]
fn runner_runs_without_run_json_when_run_id_is_explicit() {
    let context = auth_context();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_temp(
        "workflow.fabro",
        "\
digraph DetachedStoreOnly {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    let server = server_target(&context.storage_dir);
    let output = worker_command(&context, run_id.as_str())
        .args([
            "__run-worker",
            "--server",
            server.as_str(),
            "--run-dir",
            run_dir.to_str().unwrap(),
            "--run-id",
            run_id.as_str(),
            "--mode",
            "start",
        ])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_worker_succeeded(&run_dir, &output);
}

#[test]
fn server_dispatched_worker_does_not_inherit_parent_secret_env() {
    let mut context = test_context!();
    let server_root = tempfile::tempdir_in("/tmp").unwrap();
    let storage_dir = server_root.path().join("storage");
    let socket_path = server_root.path().join("fabro.sock");
    let config_path = server_root.path().join("settings.toml");
    context.manage_storage_dir(&storage_dir);
    std::fs::write(
        &config_path,
        format!(
            r#"_version = 1

[server.storage]
root = "{}"

[server.auth]
methods = ["dev-token"]
"#,
            storage_dir.display()
        ),
    )
    .expect("writing leak-probe server settings");

    let start_output = context
        .command()
        .env("MY_API_TOKEN", LEAKED_WORKER_PARENT_TOKEN)
        .env("NEW_RELIC_LICENSE_KEY", LEAKED_NEW_RELIC_LICENSE)
        .args(["server", "start"])
        .arg("--storage-dir")
        .arg(&storage_dir)
        .arg("--bind")
        .arg(&socket_path)
        .arg("--config")
        .arg(&config_path)
        .output()
        .expect("server start should execute");
    assert!(
        start_output.status.success(),
        "server start failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&start_output.stdout),
        String::from_utf8_lossy(&start_output.stderr)
    );

    let workflow_path = context.temp_dir.join("worker-leak-probe.fabro");
    std::fs::write(
        &workflow_path,
        r#"digraph WorkerLeakProbe {
  graph [goal="Verify worker subprocess env isolation", default_max_retries=0]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare, label="Exit"]
  probe [shape=parallelogram, label="Probe", script="echo probe-ran; for key in $(printf 'MY%s NEW%s FABRO%s' '_API_TOKEN' '_RELIC_LICENSE_KEY' '_WORKER_TOKEN'); do value=$(printenv \"$key\" || true); if [ -n \"$value\" ]; then echo \"$key=$value\"; fi; done"]
  start -> probe -> exit
}
"#,
    )
    .expect("writing leak-probe workflow");

    let run_id = unique_run_id();
    let dev_token = local_dev_token(&storage_dir).expect("managed server should have a dev token");
    let target = ServerTarget::unix_socket_path(&socket_path).expect("socket path should parse");
    seed_dev_token_auth(&context.home_dir, &target, &dev_token);
    let run_output = context
        .run_cmd()
        .args([
            "--server",
            socket_path.to_str().expect("socket path should be UTF-8"),
            "--run-id",
            run_id.as_str(),
            "--detach",
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            "local",
            workflow_path
                .to_str()
                .expect("workflow path should be UTF-8"),
        ])
        .output()
        .expect("detached leak-probe run should execute");
    assert!(
        run_output.status.success(),
        "detached run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );

    let run_dir = find_run_dir(&storage_dir, &run_id).expect("leak-probe run dir should exist");
    wait_for_status(&run_dir, &["succeeded"]);

    let state = run_state(&run_dir);
    let probe_stage_id = StageId::new("probe", 1);
    let _probe = state
        .stage(&probe_stage_id)
        .expect("probe node state should exist");
    let stdout = command_log_text(&run_dir, &probe_stage_id, CommandOutputStream::Stdout);
    assert!(
        stdout.contains("probe-ran"),
        "probe stage should have executed, got stdout:\n{stdout}"
    );
    assert_no_worker_env_leak("probe stdout", &stdout);
    assert_no_worker_env_leak(
        "run state",
        &serde_json::to_string(&state).expect("run state should serialize"),
    );

    let server_log =
        std::fs::read_to_string(storage_dir.join("logs/server.log")).unwrap_or_default();
    assert_no_worker_env_leak("server log", &server_log);
    assert!(
        server_log.contains("Workflow run started"),
        "main server log should include worker tracing, got:\n{server_log}"
    );
    assert!(
        server_log.contains(&run_id),
        "main server log should include the run id, got:\n{server_log}"
    );

    let run_log_path = run_dir.join("runtime/server.log");
    assert!(
        run_log_path.is_file(),
        "run log should be written at {}",
        run_log_path.display()
    );
    let run_log = std::fs::read_to_string(&run_log_path).expect("run log should be readable");
    assert!(
        run_log.contains("Workflow run started"),
        "per-run log should include worker tracing, got:\n{run_log}"
    );
    assert!(
        run_log.contains(&run_id),
        "per-run log should include the run id, got:\n{run_log}"
    );
    assert_no_worker_env_leak("per-run log", &run_log);
}

#[test]
fn runner_resume_rejects_completed_run_without_mutating_it() {
    let context = auth_context();
    context.write_temp(
        "workflow.fabro",
        "\
digraph Test {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    let run = context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--detach",
            context.temp_dir.join("workflow.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();
    let run_id = String::from_utf8(run.get_output().stdout.clone())
        .unwrap()
        .trim()
        .to_string();
    let run_dir = context.find_run_dir(&run_id);
    let server = server_target(&context.storage_dir);

    context
        .command()
        .args(["wait", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();

    let inspect_before = context
        .command()
        .args(["inspect", &run_id])
        .assert()
        .success();
    let before: serde_json::Value =
        serde_json::from_slice(&inspect_before.get_output().stdout).unwrap();
    let before_summary = serde_json::json!({
        "run_dir": before[0]["run_dir"],
        "start_time": before[0]["start_record"]["start_time"],
        "conclusion_timestamp": before[0]["conclusion"]["timestamp"],
        "conclusion_status": before[0]["conclusion"]["status"],
    });
    fabro_json_snapshot!(context, &before_summary, @r#"
    {
      "run_dir": null,
      "start_time": "[TIMESTAMP]",
      "conclusion_timestamp": "[TIMESTAMP]",
      "conclusion_status": "succeeded"
    }
    "#);

    let mut cmd = worker_command(&context, &run_id);
    cmd.args([
        "__run-worker",
        "--server",
        &server,
        "--run-dir",
        run_dir.to_str().unwrap(),
        "--run-id",
        &run_id,
        "--mode",
        "resume",
    ]);
    cmd.timeout(SHARED_DAEMON_TIMEOUT);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × Precondition failed: run already finished successfully — nothing to resume
    ");

    let inspect_after = context
        .command()
        .args(["inspect", &run_id])
        .assert()
        .success();
    let after: serde_json::Value =
        serde_json::from_slice(&inspect_after.get_output().stdout).unwrap();
    let after_summary = serde_json::json!({
        "run_dir": after[0]["run_dir"],
        "start_time": after[0]["start_record"]["start_time"],
        "conclusion_timestamp": after[0]["conclusion"]["timestamp"],
        "conclusion_status": after[0]["conclusion"]["status"],
    });

    assert_eq!(after_summary, before_summary);
}

#[test]
fn runner_reports_missing_run_spec_without_prefetching_events() {
    let context = auth_context();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let run_dir = tempfile::tempdir().expect("temp run dir should exist");

    let state_mock = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/state"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({
                    "spec": null,
                    "graph_source": null,
                    "start": null,
                    "status": null,
                    "checkpoint": null,
                    "checkpoints": [],
                    "conclusion": null,
                    "retro": null,
                    "retro_prompt": null,
                    "retro_response": null,
                    "sandbox": null,
                    "final_patch": null,
                    "pull_request": null,
                    "stages": {}
                })
                .to_string(),
            );
    });
    let events_mock = server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/runs/{run_id}/events"));
        then.status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"data":[],"meta":{"has_more":false}}"#);
    });

    let output = worker_command(&context, &run_id)
        .args([
            "__run-worker",
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--run-dir",
            run_dir.path().to_str().expect("run dir should be UTF-8"),
            "--run-id",
            &run_id,
            "--mode",
            "start",
        ])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .output()
        .expect("worker should execute");

    assert!(
        !output.status.success(),
        "worker should fail when run spec is missing:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    state_mock.assert();
    events_mock.assert_calls(0);
    assert!(
        output_stderr(&output).contains("has no run spec in store"),
        "{}",
        output_stderr(&output)
    );
}

#[test]
fn detached_run_answers_pending_question_without_interview_scratch_files() {
    let context = auth_context();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("human-gate.fabro");

    context.write_temp(
        "human-gate.fabro",
        r#"digraph HumanGate {
  graph [goal="Approve the release"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare, label="Exit"]
  work  [shape=parallelogram, script="echo ready"]
  approve [shape=hexagon, label="Approve?"]
  ship   [shape=parallelogram, script="echo shipped"]
  revise [shape=parallelogram, script="echo revised"]
  start -> work -> approve
  approve -> ship   [label="[A] Approve"]
  approve -> revise [label="[R] Revise"]
  ship -> exit
  revise -> exit
}
"#,
    );

    let output = context
        .command()
        .args([
            "run",
            "--detach",
            "--run-id",
            run_id.as_str(),
            "--no-retro",
            "--sandbox",
            "local",
            workflow_path.to_str().unwrap(),
        ])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .output()
        .expect("detached run should execute");
    assert!(
        output.status.success(),
        "detached run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let run_dir = context.find_run_dir(&run_id);
    let runtime = tokio::runtime::Runtime::new().expect("test runtime should build");
    let question_id = runtime.block_on(async {
        let (client, base_url) =
            server_endpoint(&context.storage_dir).expect("server endpoint should exist");
        let question = wait_for_server_question(&client, &base_url, &run_id).await;
        let question_id = question["id"]
            .as_str()
            .expect("question id should be present")
            .to_string();

        assert_eq!(question["stage"], "approve");

        let response = client
            .post(format!(
                "{base_url}/api/v1/runs/{run_id}/questions/{question_id}/answer"
            ))
            .json(&serde_json::json!({ "selected_option_key": "A" }))
            .send()
            .await
            .expect("answer submission should succeed");
        assert_reqwest_status(
            response,
            fabro_http::StatusCode::NO_CONTENT,
            format!("POST /api/v1/runs/{run_id}/questions/{question_id}/answer"),
        )
        .await;

        question_id
    });

    context
        .command()
        .args(["wait", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();

    let events = stored_worker_events(&run_dir);
    assert!(events.iter().any(|event| matches!(
        &event.body,
        EventBody::InterviewCompleted(props)
            if props.question_id == question_id && props.answer == "A"
    )));
}

#[test]
fn worker_exits_with_retro_enabled_even_when_stdin_stays_open() {
    let context = auth_context();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("retro-success.fabro");

    context.write_temp(
        ".fabro/project.toml",
        r"_version = 1

[run.execution]
retros = true
",
    );
    context.write_temp(
        "retro-success.fabro",
        r#"digraph RetroSuccess {
  graph [goal="Finish successfully with retro enabled"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare, label="Exit"]
  work  [shape=parallelogram, label="Work", script="true"]
  start -> work -> exit
}
"#,
    );

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    let server = server_target(&context.storage_dir);
    let mut child = spawn_worker_process(&context, &server, &run_dir, &run_id, "start");
    let stdin = child.stdin.take().expect("worker stdin should be piped");

    wait_for_event_names(&run_dir, &["run.completed", "retro.completed"]);
    let status = wait_for_child_exit(&mut child, SHARED_DAEMON_TIMEOUT);
    drop(stdin);
    let output = child_output(child, status);

    assert!(
        output.status.success(),
        "worker should exit successfully after retro even with stdin open:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let events = stored_worker_events(&run_dir);
    let run_completed_index = events
        .iter()
        .position(|event| matches!(&event.body, EventBody::RunCompleted(_)))
        .expect("run.completed should be present");
    let retro_completed_index = events
        .iter()
        .position(|event| matches!(&event.body, EventBody::RetroCompleted(_)))
        .expect("retro.completed should be present");
    assert!(
        retro_completed_index < run_completed_index,
        "retro.completed must precede run.completed"
    );
}

#[cfg(unix)]
#[test]
fn worker_exits_after_sigterm_cancel_even_when_stdin_stays_open() {
    let context = auth_context();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("cancel-gated.fabro");
    let _gate = write_gated_workflow(&workflow_path, "cancel_gated", "Wait for cancellation");

    context
        .command()
        .args([
            "create",
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            "local",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(&run_id);
    let server = server_target(&context.storage_dir);
    let mut child = spawn_worker_process(&context, &server, &run_dir, &run_id, "start");
    let stdin = child.stdin.take().expect("worker stdin should be piped");

    wait_for_event_names(&run_dir, &["run.running"]);
    let worker_pid = child.id();
    assert!(worker_pid > 0, "worker pid should be present");
    fabro_proc::sigterm(worker_pid);

    wait_for_status(&run_dir, &["failed"]);
    let status = wait_for_child_exit(&mut child, SHARED_DAEMON_TIMEOUT);
    drop(stdin);
    let output = child_output(child, status);

    assert!(
        output.status.success(),
        "worker should exit cleanly after SIGTERM cancellation:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let status_record = run_state(&run_dir)
        .status
        .expect("cancelled run should have a status record");
    assert_eq!(status_record, fabro_types::RunStatus::Failed {
        reason: FailureReason::Cancelled,
    });
}
