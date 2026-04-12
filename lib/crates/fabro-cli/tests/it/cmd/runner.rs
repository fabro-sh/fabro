#![expect(
    clippy::disallowed_methods,
    reason = "These CLI integration tests spawn real fabro worker subprocesses and observe their lifecycle."
)]

use std::io::Read;
use std::process::{Child, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use fabro_store::EventEnvelope;
use fabro_test::{fabro_snapshot, test_context};
use fabro_types::{EventBody, RunEvent, StatusReason};
use httpmock::MockServer;

use super::support::{
    output_stderr, run_events, run_state, server_target, wait_for_event_names, wait_for_status,
    write_gated_workflow,
};
use crate::support::{fabro_json_snapshot, unique_run_id};

const SHARED_DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn stored_worker_events(run_dir: &std::path::Path) -> Vec<RunEvent> {
    run_events(run_dir).iter().map(run_event).collect()
}

fn run_event(event: &EventEnvelope) -> RunEvent {
    RunEvent::try_from(&event.payload).expect("stored event should parse")
}

fn assert_worker_succeeded(run_dir: &std::path::Path, stdout: &[u8]) {
    assert!(
        stdout.is_empty(),
        "worker should not emit event transport on stdout"
    );
    let events = stored_worker_events(run_dir);
    assert!(events.iter().any(|event| matches!(
        &event.body,
        EventBody::RunCompleted(props) if props.status == "success"
    )));
}

fn spawn_worker_process(
    context: &fabro_test::TestContext,
    server: &str,
    run_dir: &std::path::Path,
    run_id: &str,
    mode: &str,
) -> Child {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    cmd.current_dir(&context.temp_dir);
    cmd.env("NO_COLOR", "1");
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true");
    cmd.env("FABRO_SERVER_MAX_CONCURRENT_RUNS", "64");
    cmd.env("FABRO_TEST_IN_MEMORY_STORE", "1");
    cmd.args([
        "__run-worker",
        "--server",
        server,
        "--run-dir",
        run_dir.to_str().unwrap(),
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

fn server_endpoint(storage_dir: &std::path::Path) -> (reqwest::Client, String) {
    let target = server_target(storage_dir);
    if target.starts_with('/') {
        (
            reqwest::ClientBuilder::new()
                .unix_socket(target)
                .no_proxy()
                .build()
                .expect("test Unix-socket HTTP client should build"),
            "http://fabro".to_string(),
        )
    } else {
        (
            reqwest::ClientBuilder::new()
                .no_proxy()
                .build()
                .expect("test TCP HTTP client should build"),
            target,
        )
    }
}

async fn wait_for_server_question(
    client: &reqwest::Client,
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
        assert!(
            response.status().is_success(),
            "question request failed: {}",
            response.status()
        );
        let body: serde_json::Value = response
            .json()
            .await
            .expect("question response should parse");
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
fn runner_uses_cached_graph_after_source_deleted() {
    let context = test_context!();
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

    let output = context
        .command()
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
fn runner_uses_snapshotted_app_id_for_github_credentials() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_home(
        ".fabro/settings.toml",
        "\
_version = 1

[server.integrations.github]
app_id = \"snapshotted-app-id\"
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
    let state = run_state(&run_dir);
    let run = state.run.as_ref().expect("run record should exist");
    let resolved_server = fabro_config::resolve_server_from_file(&run.settings).unwrap();
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "app_id": resolved_server.integrations.github.app_id.map(|value| value.as_source()),
        }),
        @r#"
        {
          "app_id": "snapshotted-app-id"
        }
        "#
    );

    context.write_home(".fabro/settings.toml", "_version = 1\n");

    let server = server_target(&context.storage_dir);
    let mut cmd = context.command();
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
    let context = test_context!();
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
    let output = context
        .command()
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
fn runner_resume_rejects_completed_run_without_mutating_it() {
    let context = test_context!();
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
      "conclusion_status": "success"
    }
    "#);

    let mut cmd = context.command();
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
    error: Precondition failed: run already finished successfully — nothing to resume
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
fn runner_reports_missing_run_record_without_prefetching_events() {
    let context = test_context!();
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
                    "run": null,
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
                    "nodes": {}
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

    let output = context
        .command()
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
        "worker should fail when run record is missing:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    state_mock.assert();
    events_mock.assert_calls(0);
    assert!(
        output_stderr(&output).contains("has no run record in store"),
        "{}",
        output_stderr(&output)
    );
}

#[test]
fn detached_run_answers_pending_question_without_interview_scratch_files() {
    let context = test_context!();
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
        let (client, base_url) = server_endpoint(&context.storage_dir);
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
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

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
    let context = test_context!();
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
        run_completed_index < retro_completed_index,
        "retro should still run after run.completed"
    );
}

#[cfg(unix)]
#[test]
fn worker_exits_after_sigterm_cancel_even_when_stdin_stays_open() {
    let context = test_context!();
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
    assert_eq!(status_record.status.to_string(), "failed");
    assert_eq!(status_record.reason, Some(StatusReason::Cancelled));
}
