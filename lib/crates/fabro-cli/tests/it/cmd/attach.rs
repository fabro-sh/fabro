#![expect(
    clippy::disallowed_types,
    reason = "integration tests: read child-process stdout line-by-line via std::io::BufReader"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fabro_test::{
    apply_filters, assert_reqwest_status, expect_reqwest_json, fabro_json_snapshot, fabro_snapshot,
    test_context,
};
use serde_json::Value;

use super::support::{
    output_stdout, resolve_run, server_endpoint, wait_for_status, write_gated_workflow,
};
use crate::support::{run_output_filters, unique_run_id};

const SHARED_DAEMON_TIMEOUT: Duration = Duration::from_secs(30);

async fn wait_for_server_question(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run_id: &str,
) -> Value {
    let deadline = std::time::Instant::now() + SHARED_DAEMON_TIMEOUT;
    loop {
        let response = client
            .get(format!("{base_url}/api/v1/runs/{run_id}/questions"))
            .query(&[("page[limit]", "100"), ("page[offset]", "0")])
            .send()
            .await
            .expect("question request should succeed");
        let body: Value = expect_reqwest_json(
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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn format_output_snapshot(output: &Output, filters: &[(String, String)]) -> String {
    let stdout = apply_filters(&String::from_utf8_lossy(&output.stdout), filters);
    let stderr = apply_filters(&String::from_utf8_lossy(&output.stderr), filters);

    format!(
        "success: {success}\nexit_code: {code}\n----- stdout -----\n{stdout}----- stderr -----\n{stderr}",
        success = output.status.success(),
        code = output.status.code().unwrap_or(-1),
        stdout = stdout,
        stderr = stderr,
    )
}

fn normalize_attach_json_progress_event(mut event: Value) -> Value {
    if let Some(properties) = event.get_mut("properties").and_then(Value::as_object_mut) {
        if properties.contains_key("manifest_blob") {
            properties.insert(
                "manifest_blob".to_string(),
                Value::String("[BLOB_ID]".to_string()),
            );
        }
        if properties.contains_key("definition_blob") {
            properties.insert(
                "definition_blob".to_string(),
                Value::String("[BLOB_ID]".to_string()),
            );
        }
    }
    // Strip v2-shape server/version fields that the bridge emits,
    // since the test fixture's socket path is randomised per run.
    if let Some(settings) = event
        .pointer_mut("/properties/settings")
        .and_then(Value::as_object_mut)
    {
        settings.remove("_version");
        settings.remove("server");
        settings.remove("version");
    }
    if let Some(target) = event
        .pointer_mut("/properties/settings/cli/target")
        .and_then(Value::as_object_mut)
    {
        if target.contains_key("path") {
            target.insert(
                "path".to_string(),
                Value::String("[CLI_SOCKET]".to_string()),
            );
        }
    }
    event
}

fn wait_for_output_signal(
    child: &mut std::process::Child,
    stdout: &mut impl Read,
    stderr_reader: std::thread::JoinHandle<Vec<u8>>,
    signal_rx: &mpsc::Receiver<()>,
    needle: &str,
) -> std::thread::JoinHandle<Vec<u8>> {
    let deadline = Instant::now() + SHARED_DAEMON_TIMEOUT;
    let mut stderr_reader = Some(stderr_reader);

    loop {
        match signal_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(()) => {
                return stderr_reader
                    .take()
                    .expect("stderr reader should still be available");
            }
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }

        if let Some(status) = child.try_wait().expect("attach should stay alive") {
            let mut stdout_bytes = Vec::new();
            stdout
                .read_to_end(&mut stdout_bytes)
                .expect("attach stdout should be readable");
            let stderr_bytes = stderr_reader
                .take()
                .expect("stderr reader should still be available")
                .join()
                .expect("stderr reader should join");
            panic!(
                "attach exited before emitting {needle:?}\nstatus: {status}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes)
            );
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().expect("attach should exit after kill");
            let mut stdout_bytes = Vec::new();
            stdout
                .read_to_end(&mut stdout_bytes)
                .expect("attach stdout should be readable");
            let stderr_bytes = stderr_reader
                .take()
                .expect("stderr reader should still be available")
                .join()
                .expect("stderr reader should join");
            panic!(
                "timed out waiting for attach output {needle:?}\nstatus: {status}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes)
            );
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration helper polls a child process without a Tokio runtime."
)]
fn wait_for_child_exit(child: &mut std::process::Child, label: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|err| panic!("{label} status should be readable: {err}"))
        {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child
                .wait()
                .unwrap_or_else(|err| panic!("{label} should exit after kill: {err}"));
            panic!("{label} did not exit before timeout; killed with status {status}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn start_detached_human_run(
    context: &fabro_test::TestContext,
    filename: &str,
    source: &str,
) -> String {
    context.ensure_home_server_auth_methods();
    let workflow = context.temp_dir.join(filename);
    context.write_temp(filename, source);

    let output = context
        .command()
        .env("OPENAI_API_KEY", "test")
        .args([
            "run",
            "--detach",
            "--environment",
            "local",
            "--provider",
            "openai",
            workflow.to_str().expect("workflow path should be UTF-8"),
        ])
        .output()
        .expect("detached run should execute");
    assert!(
        output.status.success(),
        "detached run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output_stdout(&output).trim().to_string()
}

fn wait_for_pending_question(context: &fabro_test::TestContext, run_id: &str) {
    tokio::runtime::Runtime::new()
        .expect("test runtime should build")
        .block_on(async {
            let (client, base_url) =
                server_endpoint(&context.storage_dir).expect("server endpoint should exist");
            wait_for_server_question(&client, &base_url, run_id).await;
        });
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration helper writes scripted answers to an attach child process."
)]
fn attach_with_stdin(context: &fabro_test::TestContext, run_id: &str, input: &[u8]) -> Output {
    let mut attach_cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    fabro_test::apply_test_isolation(&mut attach_cmd, &context.home_dir);
    attach_cmd.current_dir(&context.temp_dir);
    attach_cmd.args(["attach", run_id]);
    attach_cmd.stdin(Stdio::piped());
    attach_cmd.stdout(Stdio::piped());
    attach_cmd.stderr(Stdio::piped());

    let mut child = attach_cmd.spawn().expect("attach should spawn");
    let mut stdout = child.stdout.take().expect("attach stdout should be piped");
    let mut stderr = child.stderr.take().expect("attach stderr should be piped");
    {
        let mut stdin = child.stdin.take().expect("attach stdin should be piped");
        stdin
            .write_all(input)
            .expect("scripted attach input should be writable");
    }

    let status = wait_for_child_exit(&mut child, "attach");
    let mut stdout_bytes = Vec::new();
    stdout
        .read_to_end(&mut stdout_bytes)
        .expect("attach stdout should be readable");
    let mut stderr_bytes = Vec::new();
    stderr
        .read_to_end(&mut stderr_bytes)
        .expect("attach stderr should be readable");
    Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    }
}

#[test]
fn attach_reprompts_invalid_yes_no_then_accepts_valid_answer() {
    let context = test_context!();
    let run_id = start_detached_human_run(
        &context,
        "yes-no-gate.fabro",
        r#"digraph HumanGate {
  graph [goal="Wait for yes/no"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare, label="Exit"]
  approve [shape=hexagon, label="Continue?", question_type="yes_no"]
  ship   [shape=parallelogram, script="echo shipped"]
  start -> approve
  approve -> ship [label="[Y] Yes"]
  ship -> exit
}
"#,
    );
    let cleanup_run_id = run_id.clone();
    scopeguard::defer! {
        let _ = context.command().args(["rm", "--force", &cleanup_run_id]).output();
    }
    wait_for_pending_question(&context, &run_id);

    let output = attach_with_stdin(&context, &run_id, b"dasf\ny\n");

    assert!(
        output.status.success(),
        "attach should succeed after corrected yes/no input:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("Please enter y or n."),
        "attach should explain invalid yes/no input:\n{stderr}"
    );
    assert!(
        !stderr.contains("Interview ended without an answer"),
        "invalid input should not detach the interview:\n{stderr}"
    );
}

#[test]
fn attach_reprompts_invalid_choice_then_accepts_valid_answer() {
    let context = test_context!();
    let run_id = start_detached_human_run(
        &context,
        "choice-gate.fabro",
        r#"digraph HumanGate {
  graph [goal="Wait for choice"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare, label="Exit"]
  approve [shape=hexagon, label="Approve?"]
  ship   [shape=parallelogram, script="echo shipped"]
  revise [shape=parallelogram, script="echo revised"]
  start -> approve
  approve -> ship   [label="[A] Approve"]
  approve -> revise [label="[R] Revise"]
  ship -> exit
  revise -> exit
}
"#,
    );
    let cleanup_run_id = run_id.clone();
    scopeguard::defer! {
        let _ = context.command().args(["rm", "--force", &cleanup_run_id]).output();
    }
    wait_for_pending_question(&context, &run_id);

    let output = attach_with_stdin(&context, &run_id, b"bogus\nA\n");

    assert!(
        output.status.success(),
        "attach should succeed after corrected choice input:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("Please enter one of: A, R."),
        "attach should explain invalid choice input:\n{stderr}"
    );
    assert!(
        !stderr.contains("Interview ended without an answer"),
        "invalid input should not detach the interview:\n{stderr}"
    );
}

#[test]
fn attach_replays_completed_detached_run() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let run_id = unique_run_id();
    let workflow = context.install_fixture("simple.fabro");

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--detach",
            "--run-id",
            run_id.as_str(),
            workflow.to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["wait", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();

    let mut cmd = context.command();
    cmd.args(["attach", &run_id]);
    cmd.timeout(SHARED_DAEMON_TIMEOUT);
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
        Web UI: http://localhost:3000/runs/[ULID]
        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Run Tests  [TIME]
        ✓ Report  [TIME]
        ✓ Exit  [TIME]
    ");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration test keeps a child stdin pipe open to reproduce attach waiting on input while the API answers the same question."
)]
fn attach_advances_when_pending_question_is_answered_elsewhere() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let workflow = context.temp_dir.join("human-gate.fabro");
    context.write_temp(
        "human-gate.fabro",
        r#"digraph HumanGate {
  graph [goal="Wait for approval"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare, label="Exit"]
  approve [shape=hexagon, label="Approve?"]
  ship   [shape=parallelogram, script="echo shipped"]
  start -> approve
  approve -> ship [label="[A] Approve"]
  ship -> exit
}
"#,
    );

    let run_output = context
        .command()
        .env("OPENAI_API_KEY", "test")
        .args([
            "run",
            "--detach",
            "--environment",
            "local",
            "--provider",
            "openai",
            workflow.to_str().unwrap(),
        ])
        .output()
        .expect("detached run should execute");
    assert!(
        run_output.status.success(),
        "detached run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let run_id = output_stdout(&run_output).trim().to_string();
    let cleanup_run_id = run_id.clone();
    scopeguard::defer! {
        let _ = context.command().args(["rm", "--force", &cleanup_run_id]).output();
    }

    let runtime = tokio::runtime::Runtime::new().expect("test runtime should build");
    let (client, base_url) =
        server_endpoint(&context.storage_dir).expect("server endpoint should exist");
    let question = runtime.block_on(wait_for_server_question(&client, &base_url, &run_id));
    let question_id = question["id"]
        .as_str()
        .expect("question id should be present")
        .to_string();

    let mut attach_cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    fabro_test::apply_test_isolation(&mut attach_cmd, &context.home_dir);
    attach_cmd.current_dir(&context.temp_dir);
    attach_cmd.args(["attach", &run_id]);
    attach_cmd.stdin(Stdio::piped());
    attach_cmd.stdout(Stdio::piped());
    attach_cmd.stderr(Stdio::piped());
    let mut child = attach_cmd.spawn().expect("attach should spawn");
    let _stdin = child.stdin.take().expect("attach stdin should be piped");
    let mut stdout = child.stdout.take().expect("attach stdout should be piped");
    let stderr = child.stderr.take().expect("attach stderr should be piped");
    let (signal_tx, signal_rx) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut stderr_bytes = Vec::new();
        let mut line = Vec::new();

        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .expect("attach stderr should be readable");
            if read == 0 {
                break;
            }
            if line
                .windows("Approve?".len())
                .any(|window| window == "Approve?".as_bytes())
            {
                let _ = signal_tx.send(());
            }
            stderr_bytes.extend_from_slice(&line);
        }

        stderr_bytes
    });
    let stderr_reader = wait_for_output_signal(
        &mut child,
        &mut stdout,
        stderr_reader,
        &signal_rx,
        "Approve?",
    );

    runtime.block_on(async {
        let response = client
            .post(format!(
                "{base_url}/api/v1/runs/{run_id}/questions/{question_id}/answer"
            ))
            .json(&serde_json::json!({ "kind": "selected", "option_key": "A" }))
            .send()
            .await
            .expect("answer submission should succeed");
        assert_reqwest_status(
            response,
            fabro_http::StatusCode::NO_CONTENT,
            format!("POST /api/v1/runs/{run_id}/questions/{question_id}/answer"),
        )
        .await;
    });

    let status = wait_for_child_exit(&mut child, "attach");
    let mut stdout_bytes = Vec::new();
    stdout
        .read_to_end(&mut stdout_bytes)
        .expect("attach stdout should be readable");
    let output = Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_reader.join().expect("stderr reader should join"),
    };
    assert!(
        status.success(),
        "attach failed after external answer:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration test uses a dedicated stderr reader thread so the child process can stream output concurrently."
)]
fn attach_before_completion_streams_to_finished_state() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let gate = write_gated_workflow(&context.temp_dir.join("slow.fabro"), "slow", "Run slowly");

    let mut run_cmd = context.command();
    run_cmd.env("OPENAI_API_KEY", "test");
    run_cmd.args([
        "run",
        "--detach",
        "--provider",
        "openai",
        "--environment",
        "local",
        "slow.fabro",
    ]);
    let run_output = run_cmd.output().expect("command should execute");
    assert!(
        run_output.status.success(),
        "run --detach failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let run_id = output_stdout(&run_output).trim().to_string();
    let run = resolve_run(&context, &run_id);
    wait_for_status(&run.run_dir, &["running"]);

    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut attach_cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fabro"));
    fabro_test::apply_test_isolation(&mut attach_cmd, &context.home_dir);
    attach_cmd.current_dir(&context.temp_dir);
    attach_cmd.args(["attach", &run_id]);
    attach_cmd.stdout(Stdio::piped());
    attach_cmd.stderr(Stdio::piped());
    let mut child = attach_cmd.spawn().expect("attach should spawn");
    let mut stdout = child.stdout.take().expect("attach stdout should be piped");
    let stderr = child.stderr.take().expect("attach stderr should be piped");
    let (signal_tx, signal_rx) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut stderr_bytes = Vec::new();
        let mut line = Vec::new();

        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .expect("attach stderr should be readable");
            if read == 0 {
                break;
            }
            if line
                .windows("✓ start".len())
                .any(|window| window == "✓ start".as_bytes())
            {
                let _ = signal_tx.send(());
            }
            stderr_bytes.extend_from_slice(&line);
        }

        stderr_bytes
    });
    let stderr_reader = wait_for_output_signal(
        &mut child,
        &mut stdout,
        stderr_reader,
        &signal_rx,
        "✓ start",
    );
    gate.release();
    let status = child.wait().expect("attach should exit");
    let mut stdout_bytes = Vec::new();
    stdout
        .read_to_end(&mut stdout_bytes)
        .expect("attach stdout should be readable");
    let output = Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_reader.join().expect("stderr reader should join"),
    };
    let snapshot = format_output_snapshot(&output, &filters);
    wait_for_status(&run.run_dir, &["succeeded"]);

    insta::assert_snapshot!(snapshot, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
        Web UI: http://localhost:3000/runs/[ULID]
        Sandbox: local (ready in [TIME])
        ✓ start  [DURATION]
        ✓ wait  [DURATION]
        ✓ exit  [DURATION]
    ");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration test polls events for a human gate without creating a Tokio runtime."
)]
fn attach_json_errors_without_prompting_for_human_input() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let workflow = context.temp_dir.join("human-gate.fabro");
    context.write_temp(
        "human-gate.fabro",
        r#"digraph HumanGate {
  graph [goal="Wait for approval"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare, label="Exit"]
  approve [shape=hexagon, label="Approve?"]
  ship   [shape=parallelogram, script="echo shipped"]
  revise [shape=parallelogram, script="echo revised"]
  start -> approve
  approve -> ship   [label="[A] Approve"]
  approve -> revise [label="[R] Revise"]
  ship -> exit
  revise -> exit
}
"#,
    );

    let run_output = context
        .command()
        .env("OPENAI_API_KEY", "test")
        .args([
            "run",
            "--detach",
            "--environment",
            "local",
            "--provider",
            "openai",
            workflow.to_str().unwrap(),
        ])
        .output()
        .expect("detached run should execute");
    assert!(
        run_output.status.success(),
        "detached run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let run_id = output_stdout(&run_output).trim().to_string();
    let cleanup_run_id = run_id.clone();
    scopeguard::defer! {
        let _ = context.command().args(["rm", "--force", &cleanup_run_id]).output();
    }
    let deadline = std::time::Instant::now() + SHARED_DAEMON_TIMEOUT;
    loop {
        let events_output = context
            .command()
            .args(["events", &run_id, "--json"])
            .output()
            .expect("events should execute");
        assert!(events_output.status.success(), "events should succeed");
        let log_events: Vec<Value> = String::from_utf8(events_output.stdout)
            .expect("stdout should be UTF-8")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("log line should be valid JSON"))
            .collect();
        if log_events.iter().any(|event| {
            event["event"] == "stage.started"
                && event["node_id"] == "approve"
                && event["properties"]["handler_type"] == "human"
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for human gate to start for {run_id}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let output = context
        .command()
        .args(["--json", "attach", &run_id])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .output()
        .expect("attach should execute");

    assert!(!output.status.success(), "attach --json should fail fast");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("--json is non-interactive"));
    assert!(
        !stderr.contains("Approve?"),
        "attach should not prompt on stderr"
    );
    let events_output = context
        .command()
        .args(["events", &run_id, "--json"])
        .output()
        .expect("events should execute");
    assert!(events_output.status.success(), "events should succeed");
    let log_events: Vec<Value> = String::from_utf8(events_output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("log line should be valid JSON"))
        .collect();
    assert!(
        log_events.iter().any(|event| {
            event["event"] == "stage.started"
                && event["node_id"] == "approve"
                && event["properties"]["handler_type"] == "human"
        }),
        "the run should still be waiting on the human gate"
    );
    assert!(
        !log_events.iter().any(|event| {
            event["node_id"] == "approve"
                && matches!(
                    event["event"].as_str(),
                    Some("stage.completed" | "stage.failed" | "interview.completed")
                )
        }),
        "attach --json should not answer the interview"
    );

    let progress: Vec<Value> = String::from_utf8(output.stdout)
        .expect("stdout should be UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("attach JSON output should be JSONL"))
        .map(normalize_attach_json_progress_event)
        .collect();
    fabro_json_snapshot!(context, &progress, @r#"
    [
      {
        "actor": {
          "auth_method": "dev_token",
          "identity": {
            "issuer": "fabro:dev",
            "subject": "dev"
          },
          "kind": "user",
          "login": "dev"
        },
        "event": "run.created",
        "id": "[EVENT_ID]",
        "properties": {
          "graph": {
            "attrs": {
              "goal": {
                "String": "Wait for approval"
              }
            },
            "edges": [
              {
                "attrs": {},
                "from": "start",
                "to": "approve"
              },
              {
                "attrs": {
                  "label": {
                    "String": "[A] Approve"
                  }
                },
                "from": "approve",
                "to": "ship"
              },
              {
                "attrs": {
                  "label": {
                    "String": "[R] Revise"
                  }
                },
                "from": "approve",
                "to": "revise"
              },
              {
                "attrs": {},
                "from": "ship",
                "to": "exit"
              },
              {
                "attrs": {},
                "from": "revise",
                "to": "exit"
              }
            ],
            "name": "HumanGate",
            "nodes": {
              "approve": {
                "attrs": {
                  "label": {
                    "String": "Approve?"
                  },
                  "shape": {
                    "String": "hexagon"
                  }
                },
                "id": "approve"
              },
              "exit": {
                "attrs": {
                  "label": {
                    "String": "Exit"
                  },
                  "shape": {
                    "String": "Msquare"
                  }
                },
                "id": "exit"
              },
              "revise": {
                "attrs": {
                  "script": {
                    "String": "echo revised"
                  },
                  "shape": {
                    "String": "parallelogram"
                  }
                },
                "id": "revise"
              },
              "ship": {
                "attrs": {
                  "script": {
                    "String": "echo shipped"
                  },
                  "shape": {
                    "String": "parallelogram"
                  }
                },
                "id": "ship"
              },
              "start": {
                "attrs": {
                  "label": {
                    "String": "Start"
                  },
                  "shape": {
                    "String": "Mdiamond"
                  }
                },
                "id": "start"
              }
            }
          },
          "manifest_blob": "[BLOB_ID]",
          "provenance": {
            "client": {
              "name": "fabro-cli",
              "user_agent": "fabro-cli/[VERSION]",
              "version": "[VERSION]"
            },
            "server": {
              "version": "[VERSION]"
            },
            "subject": {
              "auth_method": "dev_token",
              "identity": {
                "issuer": "fabro:dev",
                "subject": "dev"
              },
              "kind": "user",
              "login": "dev"
            }
          },
          "run_dir": "[RUN_DIR]",
          "settings": {
            "project": {
              "description": null,
              "metadata": {},
              "name": null
            },
            "run": {
              "agent": {
                "fabro_tools": false,
                "mcps": {},
                "permissions": null
              },
              "artifacts": {
                "include": []
              },
              "checkpoint": {
                "exclude_globs": [],
                "skip_git_hooks": false
              },
              "clone": {
                "enabled": true
              },
              "environment": {
                "env": {},
                "id": "local",
                "image": {
                  "docker": null,
                  "dockerfile": null
                },
                "labels": {},
                "lifecycle": {
                  "auto_stop": null,
                  "preserve": false,
                  "stop_on_terminal": true
                },
                "network": {
                  "allow": [],
                  "mode": "allow_all"
                },
                "provider": "local",
                "resources": {
                  "cpu": null,
                  "disk": null,
                  "memory": null
                },
                "volumes": []
              },
              "execution": {
                "approval": "prompt",
                "mode": "normal"
              },
              "git": {
                "author": null
              },
              "goal": {
                "type": "inline",
                "value": "Wait for approval"
              },
              "hooks": [],
              "inputs": {},
              "integrations": {
                "github": {
                  "permissions": {}
                }
              },
              "interviews": {
                "provider": null,
                "slack": null
              },
              "meta_branch": {
                "enabled": true,
                "push": true
              },
              "metadata": {},
              "model": {
                "controls": {
                  "reasoning_effort": null,
                  "speed": null
                },
                "fallbacks": [],
                "name": "gpt-5.5",
                "provider": "openai"
              },
              "notifications": {},
              "prepare": {
                "commands": [],
                "timeout_ms": 300000
              },
              "pull_request": null,
              "run_branch": {
                "enabled": true,
                "push": true
              },
              "scm": {
                "github": null,
                "owner": null,
                "provider": null,
                "repository": null
              },
              "working_dir": null
            },
            "workflow": {
              "description": null,
              "graph": "workflow.fabro",
              "metadata": {},
              "name": null
            }
          },
          "source_directory": "[TEMP_DIR]",
          "title": "Wait for approval",
          "web_url": "http://localhost:3000/runs/[ULID]",
          "workflow_slug": "human-gate",
          "workflow_source": "digraph HumanGate {/n  graph [goal=\"Wait for approval\"]/n  start [shape=Mdiamond, label=\"Start\"]/n  exit  [shape=Msquare, label=\"Exit\"]/n  approve [shape=hexagon, label=\"Approve?\"]/n  ship   [shape=parallelogram, script=\"echo shipped\"]/n  revise [shape=parallelogram, script=\"echo revised\"]/n  start -> approve/n  approve -> ship   [label=\"[A] Approve\"]/n  approve -> revise [label=\"[R] Revise\"]/n  ship -> exit/n  revise -> exit/n}/n"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "event": "run.submitted",
        "id": "[EVENT_ID]",
        "properties": {
          "definition_blob": "[BLOB_ID]"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "auth_method": "dev_token",
          "identity": {
            "issuer": "fabro:dev",
            "subject": "dev"
          },
          "kind": "user",
          "login": "dev"
        },
        "event": "run.start_requested",
        "id": "[EVENT_ID]",
        "properties": {
          "resume": false
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "auth_method": "dev_token",
          "identity": {
            "issuer": "fabro:dev",
            "subject": "dev"
          },
          "kind": "user",
          "login": "dev"
        },
        "event": "run.runnable",
        "id": "[EVENT_ID]",
        "properties": {
          "source": "start_requested"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "run.starting",
        "id": "[EVENT_ID]",
        "properties": {},
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "sandbox.initializing",
        "id": "[EVENT_ID]",
        "properties": {
          "provider": "local"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "sandbox.ready",
        "id": "[EVENT_ID]",
        "properties": {
          "duration_ms": "[DURATION_MS]",
          "provider": "local"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "sandbox.initialized",
        "id": "[EVENT_ID]",
        "properties": {
          "id": "local:[ULID]",
          "provider": "local",
          "working_directory": "[TEMP_DIR]"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "run.started",
        "id": "[EVENT_ID]",
        "properties": {
          "goal": "Wait for approval",
          "name": "HumanGate"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "run.running",
        "id": "[EVENT_ID]",
        "properties": {},
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "stage.started",
        "id": "[EVENT_ID]",
        "node_id": "start",
        "node_label": "Start",
        "properties": {
          "attempt": 1,
          "handler_type": "start",
          "index": 0,
          "max_attempts": 1
        },
        "run_id": "[ULID]",
        "stage_id": "start@1",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "stage.completed",
        "id": "[EVENT_ID]",
        "node_id": "start",
        "node_label": "Start",
        "properties": {
          "attempt": 1,
          "context_values": {
            "current.preamble": "Goal: Wait for approval/n",
            "current_node": "start",
            "graph.goal": "Wait for approval",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.run_id": "[ULID]",
            "internal.thread_id": null
          },
          "index": 0,
          "max_attempts": 1,
          "node_visits": {
            "start": 1
          },
          "status": "succeeded",
          "timing": {
            "active_time_ms": "[ACTIVE_TIME_MS]",
            "inference_time_ms": "[INFERENCE_TIME_MS]",
            "tool_time_ms": "[TOOL_TIME_MS]",
            "wall_time_ms": "[WALL_TIME_MS]"
          }
        },
        "run_id": "[ULID]",
        "stage_id": "start@1",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "edge.selected",
        "id": "[EVENT_ID]",
        "properties": {
          "from_node": "start",
          "is_jump": false,
          "reason": "unconditional",
          "stage_status": "succeeded",
          "to_node": "approve"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "checkpoint.completed",
        "id": "[EVENT_ID]",
        "node_id": "start",
        "node_label": "start",
        "properties": {
          "completed_nodes": [
            "start"
          ],
          "context_values": {
            "current_node": "start",
            "failure_class": "",
            "failure_signature": "",
            "graph.goal": "Wait for approval",
            "internal.fidelity": "compact",
            "internal.node_visit_count": 1,
            "internal.retry_count.start": 0,
            "internal.run_id": "[ULID]",
            "internal.thread_id": null,
            "outcome": "succeeded"
          },
          "current_node": "start",
          "next_node_id": "approve",
          "node_outcomes": {
            "start": {
              "status": "succeeded",
              "usage": null
            }
          },
          "node_visits": {
            "start": 1
          },
          "status": "succeeded"
        },
        "run_id": "[ULID]",
        "stage_id": "start@1",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "stage.started",
        "id": "[EVENT_ID]",
        "node_id": "approve",
        "node_label": "Approve?",
        "properties": {
          "attempt": 1,
          "handler_type": "human",
          "index": 1,
          "max_attempts": 1
        },
        "run_id": "[ULID]",
        "stage_id": "approve@1",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "interview.started",
        "id": "[EVENT_ID]",
        "node_id": "approve",
        "node_label": "approve",
        "properties": {
          "allow_freeform": false,
          "options": [
            {
              "key": "A",
              "label": "[A] Approve"
            },
            {
              "key": "R",
              "label": "[R] Revise"
            }
          ],
          "question": "Approve?",
          "question_id": "[ULID]",
          "question_type": "multiple_choice",
          "stage": "approve"
        },
        "run_id": "[ULID]",
        "stage_id": "approve@1",
        "ts": "[TIMESTAMP]"
      },
      {
        "actor": {
          "kind": "worker",
          "run_id": "[ULID]"
        },
        "event": "run.blocked",
        "id": "[EVENT_ID]",
        "properties": {
          "blocked_reason": "human_input_required"
        },
        "run_id": "[ULID]",
        "ts": "[TIMESTAMP]"
      }
    ]
    "#);

    let run = resolve_run(&context, &run_id);
    tokio::runtime::Runtime::new()
        .expect("test runtime should build")
        .block_on(async {
            let (client, base_url) =
                server_endpoint(&context.storage_dir).expect("server endpoint should exist");
            let question = wait_for_server_question(&client, &base_url, &run_id).await;
            let question_id = question["id"]
                .as_str()
                .expect("question id should be present");

            let response = client
                .post(format!(
                    "{base_url}/api/v1/runs/{run_id}/questions/{question_id}/answer"
                ))
                .json(&serde_json::json!({ "kind": "selected", "option_key": "A" }))
                .send()
                .await
                .expect("answer submission should succeed");
            assert_reqwest_status(
                response,
                fabro_http::StatusCode::NO_CONTENT,
                format!("POST /api/v1/runs/{run_id}/questions/{question_id}/answer"),
            )
            .await;
        });
    wait_for_status(&run.run_dir, &["succeeded"]);
}
