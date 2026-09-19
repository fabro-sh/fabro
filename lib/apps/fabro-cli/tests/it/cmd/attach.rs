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
    created_run_id, output_stdout, resolve_run, server_endpoint, wait_for_status,
    write_gated_workflow,
};
use crate::support::run_output_filters;

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
    // The `run.created` record carries the whole run spec, whose graph
    // and settings vary with the fixture's socket path and node order;
    // the test does not check it.
    if event.pointer("/item/record/kind") == Some(&Value::String("run.created".to_string())) {
        if let Some(spec) = event.pointer_mut("/item/record/spec") {
            *spec = Value::String("[RUN_SPEC]".to_string());
        }
    }
    redact_volatile_fields(&mut event);
    event
}

/// Replace, at every depth, the wall-clock epoch milliseconds a stream
/// item carries, the content digests of the graph, which hashes the
/// fixture's temporary path, and the commit shas of the fixture's repository.
fn redact_volatile_fields(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                if key == "recorded_at" {
                    *field = Value::String("[EPOCH_MS]".to_string());
                } else {
                    redact_volatile_fields(field);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_volatile_fields),
        Value::String(text)
            if matches!(text.len(), 40 | 64)
                && text.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            *text = "[DIGEST]".to_string();
        }
        _ => {}
    }
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
    let workflow = context.install_fixture("simple.fabro");

    let run = context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--detach",
            "--environment",
            "local",
            workflow.to_str().unwrap(),
        ])
        .assert()
        .success();
    let run_id = created_run_id(run.get_output());

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
        ✓ Start  [TIME]
        Base: [BASE]
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
                "{base_url}/api/v1/runs/{run_id}/questions/{}/answer",
                question_id.replace('#', "%23")
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
        ✓ start  [DURATION]
        Base: [BASE]
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
        // The gate's question: Petri records it as a parsed step progress.
        if log_events.iter().any(|item| {
            item.pointer("/item/derived/parsed/kind") == Some(&Value::String("question".into()))
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
        log_events.iter().any(|item| {
            item.pointer("/item/derived/parsed/kind") == Some(&Value::String("question".into()))
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
        "run_id": "[ULID]",
        "stream_seq": 1,
        "kind": "platform",
        "id": "[EVENT_ID]",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "seq": 1,
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "kind": "run.created",
            "spec": "[RUN_SPEC]",
            "title": "Wait for approval",
            "web_url": "http://localhost:3000/runs/[ULID]"
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 2,
        "kind": "platform",
        "id": "[EVENT_ID]",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "seq": 2,
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "kind": "run.lifecycle",
            "transition": "submitted",
            "status": {
              "kind": "submitted"
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 3,
        "kind": "platform",
        "id": "[EVENT_ID]",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "seq": 3,
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "kind": "run.lifecycle",
            "transition": "start_requested",
            "source": "start"
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 4,
        "kind": "platform",
        "id": "[EVENT_ID]",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "seq": 4,
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "kind": "run.lifecycle",
            "transition": "runnable",
            "status": {
              "kind": "runnable"
            },
            "source": "start_requested"
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 5,
        "kind": "platform",
        "id": "[EVENT_ID]",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "seq": 5,
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "kind": "run.lifecycle",
            "transition": "starting",
            "status": {
              "kind": "starting"
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 6,
        "kind": "platform",
        "id": "[EVENT_ID]",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "seq": 6,
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "kind": "run.lifecycle",
            "transition": "running",
            "status": {
              "kind": "running"
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 7,
        "kind": "petri",
        "id": "coordinator/0/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "coordinator",
            "seq": 0,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 0,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "run.started",
              "format_version": 7,
              "key": "[ULID]",
              "root": 0,
              "middleware_chain": [
                "circuit-breaker"
              ]
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 8,
        "kind": "petri",
        "id": "coordinator/1/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "coordinator",
            "seq": 1,
            "index": 0
          },
          "origin": "external",
          "context": {},
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 1,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "graph.registered",
              "digest": "[DIGEST]"
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 9,
        "kind": "petri",
        "id": "coordinator/2/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "coordinator",
            "seq": 2,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 2,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "invocation.declared",
              "invocation": 0,
              "call": null,
              "graph": "[DIGEST]",
              "context": {},
              "secret_bindings": "none",
              "sandbox": "isolated"
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 10,
        "kind": "petri",
        "id": "coordinator/3/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "coordinator",
            "seq": 3,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 3,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "execution.declared",
              "execution": 0,
              "invocation": 0,
              "predecessor": null,
              "start": {
                "entry": "graph_entries",
                "context": {},
                "prior_firings": {},
                "execution_index": 0,
                "max_executions": 32
              },
              "middleware_state": {
                "circuit-breaker": [
                  1,
                  {
                    "loop_signatures": {},
                    "restart_signatures": {},
                    "pending": {}
                  }
                ]
              }
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 11,
        "kind": "petri",
        "id": "execution 0/0/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 0,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 0,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "execution.started",
              "entry": "graph_entries",
              "context": {},
              "prior_firings": {},
              "execution_index": 0,
              "max_executions": 32
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 12,
        "kind": "petri",
        "id": "execution 0/1/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 1,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 1,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "admission.decided",
              "decision_id": "execution_start",
              "decision": "admit",
              "trace": []
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 13,
        "kind": "petri",
        "id": "execution 0/1/1",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 1,
            "index": 1
          },
          "origin": "derived",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "derived": {
            "event": "visit.started",
            "inputs": [
              {
                "edge": 5,
                "generation": 0,
                "payload": null,
                "from": 0
              }
            ]
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 14,
        "kind": "petri",
        "id": "execution 0/1/2",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 1,
            "index": 2
          },
          "origin": "derived",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "derived": {
            "event": "wait.state.changed",
            "state": "awaiting_admission"
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 15,
        "kind": "petri",
        "id": "execution 0/2/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 2,
            "index": 0
          },
          "origin": "core",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 2,
            "origin": "core",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "token.emitted",
              "edge": 5,
              "generation": 0,
              "payload": null,
              "from": 0
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 16,
        "kind": "petri",
        "id": "execution 0/3/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 3,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 3,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "admission.decided",
              "decision_id": {
                "attempt_start": {
                  "firing": 1,
                  "attempt": 1
                }
              },
              "decision": "admit",
              "trace": []
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 17,
        "kind": "petri",
        "id": "execution 0/4/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 4,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 4,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "scope.acquired",
              "scope": 0,
              "lease": 0,
              "workspace": "invocation-0-scope-0",
              "provider": "host",
              "instance": "host-g[ID]",
              "working_directory": "[RUN_DIR]/petri/scopes/invocation-0-scope-0/work",
              "duration_ms": "[DURATION_MS]"
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 18,
        "kind": "petri",
        "id": "execution 0/5/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 5,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 5,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "step.started",
              "firing": 1,
              "attempt": 1
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 19,
        "kind": "petri",
        "id": "execution 0/5/1",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 5,
            "index": 1
          },
          "origin": "derived",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "derived": {
            "event": "wait.state.changed",
            "state": "running"
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 20,
        "kind": "petri",
        "id": "execution 0/6/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 6,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 6,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "step.progress.recorded",
              "firing": 1,
              "ev": {
                "log": {
                  "stream": "stderr",
                  "line": "checkout: [TEMP_DIR] is not a Git repository; the workspace starts empty"
                }
              }
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 21,
        "kind": "petri",
        "id": "execution 0/7/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 7,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 7,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "step.progress.recorded",
              "firing": 1,
              "ev": {
                "custom": {
                  "$note": {
                    "kind": "fabro.checkpoint",
                    "payload": {
                      "execution": 0,
                      "firing": 1,
                      "attempt": 1,
                      "workspace": "invocation-0-scope-0",
                      "git_commit_sha": "[DIGEST]",
                      "reused": false
                    }
                  }
                }
              }
            }
          },
          "derived": {
            "parsed": {
              "kind": "note",
              "note": {
                "kind": "fabro.checkpoint",
                "payload": {
                  "execution": 0,
                  "firing": 1,
                  "attempt": 1,
                  "workspace": "invocation-0-scope-0",
                  "git_commit_sha": "[DIGEST]",
                  "reused": false
                }
              }
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 22,
        "kind": "petri",
        "id": "execution 0/8/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 8,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 8,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "step.finished",
              "firing": 1,
              "attempt": 1,
              "outcome": {
                "status": "success",
                "output": {
                  "outcome": "succeeded",
                  "failure_class": ""
                },
                "metrics": {
                  "duration_ms": "[DURATION_MS]"
                },
                "context_updates": {
                  "failure_class": "",
                  "internal.run_id": "petri"
                }
              }
            }
          },
          "derived": {
            "final": true,
            "exhausted": false
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 23,
        "kind": "petri",
        "id": "execution 0/8/1",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 8,
            "index": 1
          },
          "origin": "derived",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "derived": {
            "event": "visit.completed",
            "outcome": {
              "status": "success",
              "output": {
                "outcome": "succeeded",
                "failure_class": ""
              },
              "metrics": {
                "duration_ms": "[DURATION_MS]"
              },
              "context_updates": {
                "failure_class": "",
                "internal.run_id": "petri"
              }
            },
            "executed": true,
            "attempts": 1
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 24,
        "kind": "platform",
        "id": "[EVENT_ID]",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "seq": 7,
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "kind": "run.branch",
            "run_branch": "fabro/run/[ULID]",
            "base_sha": "[DIGEST]",
            "workspace": "invocation-0-scope-0"
          },
          "position": {
            "execution": 0,
            "firing": 1
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 25,
        "kind": "platform",
        "id": "[EVENT_ID]",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "seq": 8,
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "kind": "git.identity",
            "name": "Fabro",
            "email": "noreply@fabro.sh",
            "source": "default"
          },
          "position": {
            "execution": 0,
            "firing": 1
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 26,
        "kind": "platform",
        "id": "[EVENT_ID]",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "seq": 9,
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "kind": "checkpoint",
            "execution": 0,
            "firing": 1,
            "attempt": 1,
            "workspace": "invocation-0-scope-0",
            "git_commit_sha": "[DIGEST]",
            "operation": {
              "execution": 0,
              "decision": {
                "attempt_start": {
                  "firing": 1,
                  "attempt": 1
                }
              },
              "effect": "checkpoint"
            }
          },
          "position": {
            "execution": 0,
            "firing": 1
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 27,
        "kind": "petri",
        "id": "execution 0/9/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 9,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 9,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "routing.resolved",
              "decision_id": {
                "route": {
                  "firing": 1,
                  "attempt": 1
                }
              },
              "groups": [
                {
                  "group": 0,
                  "draw": null,
                  "trace": [],
                  "decision": {
                    "emit": 0
                  }
                }
              ]
            }
          },
          "derived": {
            "groups": [
              {
                "group": 0,
                "target": {
                  "id": 2,
                  "name": "approve",
                  "kind": "attractor/human",
                  "meta": {
                    "label": "Approve?",
                    "shape": "hexagon",
                    "kind": "human",
                    "classes": [],
                    "span": {
                      "line": 5,
                      "column": 3
                    },
                    "edges": {
                      "1": {
                        "to": "ship",
                        "label": "[A] Approve"
                      },
                      "2": {
                        "to": "revise",
                        "label": "[R] Revise"
                      }
                    }
                  }
                }
              }
            ]
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 28,
        "kind": "petri",
        "id": "execution 0/9/1",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 9,
            "index": 1
          },
          "origin": "derived",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 2,
              "name": "approve",
              "kind": "attractor/human",
              "meta": {
                "label": "Approve?",
                "shape": "hexagon",
                "kind": "human",
                "classes": [],
                "span": {
                  "line": 5,
                  "column": 3
                },
                "edges": {
                  "1": {
                    "to": "ship",
                    "label": "[A] Approve"
                  },
                  "2": {
                    "to": "revise",
                    "label": "[R] Revise"
                  }
                }
              }
            },
            "firing": 2,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "derived": {
            "event": "visit.started",
            "inputs": [
              {
                "edge": 0,
                "generation": 0,
                "payload": {
                  "outcome": "succeeded",
                  "failure_class": ""
                },
                "from": 1
              }
            ]
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 29,
        "kind": "petri",
        "id": "execution 0/9/2",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 9,
            "index": 2
          },
          "origin": "derived",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 2,
              "name": "approve",
              "kind": "attractor/human",
              "meta": {
                "label": "Approve?",
                "shape": "hexagon",
                "kind": "human",
                "classes": [],
                "span": {
                  "line": 5,
                  "column": 3
                },
                "edges": {
                  "1": {
                    "to": "ship",
                    "label": "[A] Approve"
                  },
                  "2": {
                    "to": "revise",
                    "label": "[R] Revise"
                  }
                }
              }
            },
            "firing": 2,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "derived": {
            "event": "wait.state.changed",
            "state": "awaiting_admission"
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 30,
        "kind": "petri",
        "id": "execution 0/10/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 10,
            "index": 0
          },
          "origin": "core",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 10,
            "origin": "core",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "route.applied",
              "kind": "edge",
              "firing": 1,
              "group": 0,
              "edge": 0
            }
          },
          "derived": {
            "target": {
              "id": 2,
              "name": "approve",
              "kind": "attractor/human",
              "meta": {
                "label": "Approve?",
                "shape": "hexagon",
                "kind": "human",
                "classes": [],
                "span": {
                  "line": 5,
                  "column": 3
                },
                "edges": {
                  "1": {
                    "to": "ship",
                    "label": "[A] Approve"
                  },
                  "2": {
                    "to": "revise",
                    "label": "[R] Revise"
                  }
                }
              }
            },
            "transition": "Continue",
            "back": false
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 31,
        "kind": "petri",
        "id": "execution 0/11/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 11,
            "index": 0
          },
          "origin": "core",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 0,
              "name": "start",
              "kind": "attractor/stage",
              "meta": {
                "label": "Start",
                "shape": "Mdiamond",
                "kind": "start",
                "classes": [],
                "span": {
                  "line": 3,
                  "column": 3
                },
                "admission_hooks": "step",
                "edges": {
                  "0": {
                    "to": "approve",
                    "label": null
                  }
                }
              }
            },
            "firing": 1,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 11,
            "origin": "core",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "token.emitted",
              "edge": 0,
              "generation": 0,
              "payload": {
                "outcome": "succeeded",
                "failure_class": ""
              },
              "from": 1
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 32,
        "kind": "petri",
        "id": "execution 0/12/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 12,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 2,
              "name": "approve",
              "kind": "attractor/human",
              "meta": {
                "label": "Approve?",
                "shape": "hexagon",
                "kind": "human",
                "classes": [],
                "span": {
                  "line": 5,
                  "column": 3
                },
                "edges": {
                  "1": {
                    "to": "ship",
                    "label": "[A] Approve"
                  },
                  "2": {
                    "to": "revise",
                    "label": "[R] Revise"
                  }
                }
              }
            },
            "firing": 2,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 12,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "admission.decided",
              "decision_id": {
                "attempt_start": {
                  "firing": 2,
                  "attempt": 1
                }
              },
              "decision": "admit",
              "trace": []
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 33,
        "kind": "petri",
        "id": "execution 0/13/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 13,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 2,
              "name": "approve",
              "kind": "attractor/human",
              "meta": {
                "label": "Approve?",
                "shape": "hexagon",
                "kind": "human",
                "classes": [],
                "span": {
                  "line": 5,
                  "column": 3
                },
                "edges": {
                  "1": {
                    "to": "ship",
                    "label": "[A] Approve"
                  },
                  "2": {
                    "to": "revise",
                    "label": "[R] Revise"
                  }
                }
              }
            },
            "firing": 2,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 13,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "step.started",
              "firing": 2,
              "attempt": 1
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 34,
        "kind": "petri",
        "id": "execution 0/13/1",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 13,
            "index": 1
          },
          "origin": "derived",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 2,
              "name": "approve",
              "kind": "attractor/human",
              "meta": {
                "label": "Approve?",
                "shape": "hexagon",
                "kind": "human",
                "classes": [],
                "span": {
                  "line": 5,
                  "column": 3
                },
                "edges": {
                  "1": {
                    "to": "ship",
                    "label": "[A] Approve"
                  },
                  "2": {
                    "to": "revise",
                    "label": "[R] Revise"
                  }
                }
              }
            },
            "firing": 2,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "derived": {
            "event": "wait.state.changed",
            "state": "running"
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 35,
        "kind": "petri",
        "id": "execution 0/14/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 14,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 2,
              "name": "approve",
              "kind": "attractor/human",
              "meta": {
                "label": "Approve?",
                "shape": "hexagon",
                "kind": "human",
                "classes": [],
                "span": {
                  "line": 5,
                  "column": 3
                },
                "edges": {
                  "1": {
                    "to": "ship",
                    "label": "[A] Approve"
                  },
                  "2": {
                    "to": "revise",
                    "label": "[R] Revise"
                  }
                }
              }
            },
            "firing": 2,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 14,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "step.progress.recorded",
              "firing": 2,
              "ev": {
                "custom": {
                  "$question": {
                    "id": "approve#2",
                    "text": "Approve?",
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
                    "default": "A",
                    "freeform": false,
                    "sensitive": false
                  }
                }
              }
            }
          },
          "derived": {
            "parsed": {
              "kind": "question",
              "question": {
                "id": "approve#2",
                "text": "Approve?",
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
                "default": "A",
                "freeform": false,
                "sensitive": false
              }
            }
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 36,
        "kind": "petri",
        "id": "execution 0/14/1",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 14,
            "index": 1
          },
          "origin": "derived",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 2,
              "name": "approve",
              "kind": "attractor/human",
              "meta": {
                "label": "Approve?",
                "shape": "hexagon",
                "kind": "human",
                "classes": [],
                "span": {
                  "line": 5,
                  "column": 3
                },
                "edges": {
                  "1": {
                    "to": "ship",
                    "label": "[A] Approve"
                  },
                  "2": {
                    "to": "revise",
                    "label": "[R] Revise"
                  }
                }
              }
            },
            "firing": 2,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "derived": {
            "event": "wait.state.changed",
            "state": "awaiting_answer"
          }
        }
      },
      {
        "run_id": "[ULID]",
        "stream_seq": 37,
        "kind": "petri",
        "id": "execution 0/15/0",
        "recorded_at": "[EPOCH_MS]",
        "item": {
          "id": {
            "log": "execution",
            "execution": 0,
            "seq": 15,
            "index": 0
          },
          "origin": "external",
          "context": {
            "invocation": 0,
            "execution": 0
          },
          "subject": {
            "node": {
              "id": 2,
              "name": "approve",
              "kind": "attractor/human",
              "meta": {
                "label": "Approve?",
                "shape": "hexagon",
                "kind": "human",
                "classes": [],
                "span": {
                  "line": 5,
                  "column": 3
                },
                "edges": {
                  "1": {
                    "to": "ship",
                    "label": "[A] Approve"
                  },
                  "2": {
                    "to": "revise",
                    "label": "[R] Revise"
                  }
                }
              }
            },
            "firing": 2,
            "visit": 1,
            "attempt": 1,
            "generation": 0,
            "branch": {
              "role": "none"
            }
          },
          "recorded_at": "[EPOCH_MS]",
          "record": {
            "seq": 15,
            "origin": "external",
            "recorded_at": "[EPOCH_MS]",
            "body": {
              "event": "step.progress.recorded",
              "firing": 2,
              "ev": {
                "log": {
                  "stream": "stdout",
                  "line": "waiting for an answer: Approve?"
                }
              }
            }
          }
        }
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
                    "{base_url}/api/v1/runs/{run_id}/questions/{}/answer",
                    question_id.replace('#', "%23")
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
