//! The run controls on a Petri run through a real server and its worker:
//! a pause holds the next stage until the unpause and the API says
//! `paused` in between; `SIGUSR1` and `SIGUSR2` on the worker do the same
//! without the API; a steer reaches the agent stage on the twin, which
//! sees it in its next request, and the stream carries the control record;
//! two live agent stages are steered apart by their stage labels, and an
//! unnamed steer between them is refused; an interrupt during a long tool
//! call ends the agent's turn and its text is the next input, while an
//! interrupt of a gate stage is refused with `no_live_turn`; a run paused
//! when its server and worker die resumes paused and goes on once unpaused.
//!
//! The worker answers each steer and interrupt over its control stream,
//! and the endpoint's response is that answer: `202` with `delivered` and
//! the stage, `409` with the refusal's code, or `202` with `pending` when
//! the worker never answers (a test hook mutes the worker's answers).
//!
//! The harness is `petri.rs`'s: a foreground server on disk storage, the
//! run started with `fabro run --detach`, and the host scope through the
//! sandbox-driver host plugin, so the tests skip, and say why, when the
//! plugin is not found.

#![expect(
    clippy::disallowed_methods,
    reason = "these scenarios stage workspaces with sync std::fs, start a real server subprocess and poll processes"
)]
#![expect(
    clippy::print_stderr,
    reason = "a scenario says where it is, and why it skipped"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fabro_petri::engine::{self, RunStatus};
use fabro_static::EnvVars;
use fabro_test::{TwinScenario, TwinScenarios, TwinToolCall, test_context, twin_openai};
use serde_json::{Value, json};

use super::petri::{
    RunningServer, answer, count_of, host_plugin, run_detached, run_detached_with, run_json,
    run_status, run_stream, settled_stream, stream_names, wait_for_questions, wait_for_status,
    wait_for_worker, wait_until_gate_is_polled, write_petri_workflow,
};
use crate::support::TEST_DEV_TOKEN;

const POLL: Duration = Duration::from_millis(50);
const RUN_TIMEOUT: Duration = Duration::from_mins(1);
/// How long a stage that must not start is watched for.
const HOLD: Duration = Duration::from_secs(1);

const MODEL: &str = "gpt-5.4";
const PROMPT: &str = "Wait for the gate, then report.";
const STEER: &str = "Steer: mention the word lighthouse in your report.";
/// Two agent stages side by side: each waits on its own gate, each is
/// steered apart.
const PROMPT_A: &str = "Alpha: wait for the gate, then report.";
const PROMPT_B: &str = "Bravo: wait for the gate, then report.";
const STEER_A: &str = "Steer alpha: mention the word lighthouse.";
const STEER_B: &str = "Steer bravo: mention the word windmill.";
/// The text an interrupt carries: the agent's next input once its turn
/// is stopped.
const INTERRUPT_STEER: &str = "Stop waiting and summarize what you have.";

/// Two command stages: `a` waits on `gate`, `b` leaves `marker`.
fn two_stage_workspace(context: &fabro_test::TestContext, gate: &Path, marker: &Path) -> PathBuf {
    write_petri_workflow(
        context,
        &format!(
            "digraph Two {{\n  graph [goal=\"Run two commands\", default_max_retries=0]\n  start \
             [shape=Mdiamond]\n  exit [shape=Msquare]\n  a [shape=parallelogram, script=\"while [ \
             ! -f {gate} ]; do sleep 0.05; done\", max_retries=0]\n  b [shape=parallelogram, \
             script=\"touch {marker}\", max_retries=0]\n  start -> a -> b -> exit\n}}\n",
            gate = gate.display(),
            marker = marker.display()
        ),
    )
}

/// `POST /runs/{id}/<action>` as a user; the response status and body.
async fn control(
    server: &RunningServer,
    run_id: &str,
    action: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = fabro_test::test_http_client()
        .post(format!(
            "{}/api/v1/runs/{run_id}/{action}",
            server.api_base_url
        ))
        .bearer_auth(TEST_DEV_TOKEN);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("the control sends");
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, body)
}

async fn pause(server: &RunningServer, run_id: &str) {
    let (status, body) = control(server, run_id, "pause", None).await;
    assert_eq!(status, 200, "pause: {body}");
}

async fn unpause(server: &RunningServer, run_id: &str) {
    let (status, body) = control(server, run_id, "unpause", None).await;
    assert_eq!(status, 200, "unpause: {body}");
}

async fn steer(server: &RunningServer, run_id: &str, text: &str) {
    steer_stage(server, run_id, text, None).await;
}

/// `POST /runs/{id}/steer` naming `stage`, or no stage: the worker
/// answers `delivered`, to the stage it steered.
async fn steer_stage(server: &RunningServer, run_id: &str, text: &str, stage: Option<&str>) {
    let (status, body) = steer_request(server, run_id, text, stage).await;
    assert_eq!(status, 202, "steer: {body}");
    assert_eq!(body["outcome"], "delivered", "steer: {body}");
    let delivered = body["stage"].as_str().unwrap_or_default();
    match stage {
        Some(stage) => assert_eq!(delivered, stage, "steer: {body}"),
        None => assert!(!delivered.is_empty(), "steer: {body}"),
    }
}

/// `POST /runs/{id}/steer` naming `stage`, or no stage; the status and
/// body, for a steer the worker may refuse.
async fn steer_request(
    server: &RunningServer,
    run_id: &str,
    text: &str,
    stage: Option<&str>,
) -> (u16, Value) {
    let mut body = json!({ "text": text, "interrupt": false });
    if let Some(stage) = stage {
        body["stage"] = json!(stage);
    }
    control(server, run_id, "steer", Some(body)).await
}

/// A refused control: 409, with the refusal's code and message in the
/// error entry.
fn assert_refused(status: u16, body: &Value, code: &str, message: &str) {
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["errors"][0]["code"], code, "{body}");
    assert_eq!(body["errors"][0]["detail"], message, "{body}");
}

/// `fabro steer <run> --stage <stage> <text>` against the server.
fn steer_by_cli(
    context: &fabro_test::TestContext,
    server: &RunningServer,
    run_id: &str,
    stage: &str,
    text: &str,
) {
    let output = context
        .command()
        .args(["steer", "--server", &server.target(), run_id])
        .args(["--stage", stage, text])
        .output()
        .expect("the steer command executes");
    assert!(
        output.status.success(),
        "fabro steer failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("Steer delivered to stage {stage}.")),
        "fabro steer reports the worker's answer\nstderr:\n{stderr}"
    );
}

/// `fabro steer --interrupt <run> <text>` against the server: the stage's
/// current turn is stopped and `text` is its next input.
fn interrupt_by_cli(
    context: &fabro_test::TestContext,
    server: &RunningServer,
    run_id: &str,
    text: &str,
) {
    let output = context
        .command()
        .args(["steer", "--server", &server.target(), run_id])
        .args(["--interrupt", text])
        .output()
        .expect("the steer command executes");
    assert!(
        output.status.success(),
        "fabro steer --interrupt failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Interrupt delivered to stage work@1."),
        "fabro steer --interrupt reports the worker's answer\nstderr:\n{stderr}"
    );
}

/// `fabro steer --interrupt <run> --stage <stage> <text>` for a stage the
/// worker refuses: the command fails and prints the refusal.
fn interrupt_refused_by_cli(
    context: &fabro_test::TestContext,
    server: &RunningServer,
    run_id: &str,
    stage: &str,
    refusal: &str,
) {
    let output = context
        .command()
        .args(["steer", "--server", &server.target(), run_id])
        .args(["--interrupt", "--stage", stage, "Stop."])
        .output()
        .expect("the steer command executes");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "fabro steer --interrupt of `{stage}` succeeded\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains(refusal),
        "fabro steer --interrupt prints the refusal `{refusal}`\nstderr:\n{stderr}"
    );
}

/// The stream's `run.notice` records, as `(code, message)`, in order.
fn notices(items: &[Value]) -> Vec<(String, String)> {
    items
        .iter()
        .filter(|item| item["kind"] == "platform")
        .map(|item| &item["item"]["record"])
        .filter(|record| record["kind"] == "run.notice")
        .map(|record| {
            (
                record["code"].as_str().unwrap_or_default().to_string(),
                record["message"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The stream's `step.progress.recorded` custom payloads of `kind`.
fn progress_of_kind<'a>(items: &'a [Value], kind: &str) -> Vec<&'a Value> {
    items
        .iter()
        .map(|item| &item["item"]["record"]["body"])
        .filter(|body| body["event"] == "step.progress.recorded")
        .map(|body| &body["ev"]["custom"])
        .filter(|custom| custom["kind"] == kind)
        .collect()
}

/// The twin's request inputs that carry `prompt`, in order.
fn inputs_with(logs: &Value, prompt: &str) -> Vec<String> {
    logs["requests"]
        .as_array()
        .expect("the twin request log is an array")
        .iter()
        .map(|request| {
            request["input_text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .filter(|input| input.contains(prompt))
        .collect()
}

/// The run's pending control, as the API shows it.
async fn pending_control(server: &RunningServer, run_id: &str) -> Value {
    run_json(server, &format!("runs/{run_id}")).await["lifecycle"]["pending_control"].clone()
}

/// Wait until the stream names `event` at least `times` times.
async fn wait_for_stream_count(
    server: &RunningServer,
    run_id: &str,
    event: &str,
    times: usize,
) -> Vec<String> {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        let names = stream_names(&run_stream(server, run_id).await);
        if count_of(&names, event) >= times {
            return names;
        }
        assert!(
            Instant::now() < deadline,
            "the stream of {run_id} never carried {event} {times} times: {names:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until the run's pending control is cleared: the record the control
/// asked for has landed.
async fn wait_for_no_pending_control(server: &RunningServer, run_id: &str) {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        let pending = pending_control(server, run_id).await;
        if pending.is_null() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the pending control of {run_id} never cleared: {pending}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The run's Petri outcome: succeeded and whole, or the test says why not.
async fn assert_petri_succeeded(server: &RunningServer, run_id: &str) {
    let store = server.petri_store().await;
    let outcome = engine::outcome_of(&store, run_id)
        .await
        .expect("the run's Petri record inspects");
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
}

/// A pause while `a` runs holds `b` at admission: the API says `paused`
/// with no pending control, `a` finishes on its own, `b` does not start,
/// and the unpause lets it through. Petri's records and Fabro's lifecycle
/// both carry the pause and the unpause.
#[tokio::test(flavor = "multi_thread")]
async fn a_pause_holds_the_next_stage_until_the_unpause() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let gate = context.temp_dir.join("a.gate");
    let marker = context.temp_dir.join("b.marker");
    let workspace = two_stage_workspace(&context, &gate, &marker);
    let run_id = run_detached(&context, &server, &workspace);

    wait_for_status(&server, &run_id, &["running"]).await;
    wait_until_gate_is_polled(&gate);
    eprintln!("run {run_id}: a is waiting on the gate");

    pause(&server, &run_id).await;
    wait_for_status(&server, &run_id, &["paused"]).await;
    wait_for_no_pending_control(&server, &run_id).await;
    eprintln!("run {run_id} is paused");

    std::fs::write(&gate, "go").expect("the gate opens");
    wait_for_stream_count(&server, &run_id, "step.finished", 2).await;
    tokio::time::sleep(HOLD).await;
    assert!(!marker.exists(), "b started while the run was paused");
    assert_eq!(run_status(&server, &run_id).await, "paused");

    unpause(&server, &run_id).await;
    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let items = settled_stream(&server, &run_id).await;
    let names = stream_names(&items);
    assert_eq!(
        status,
        "succeeded",
        "stream: {names:?}\nserver stderr:\n{}",
        server.stderr_text()
    );
    assert!(marker.exists(), "b ran after the unpause");
    assert_petri_succeeded(&server, &run_id).await;

    for event in [
        "run.paused",
        "run.unpaused",
        "lifecycle:pause_requested",
        "lifecycle:paused",
        "lifecycle:unpause_requested",
        "lifecycle:unpaused",
    ] {
        assert_eq!(count_of(&names, event), 1, "{event}: {names:?}");
    }
    let unpaused = names
        .iter()
        .position(|name| name == "run.unpaused")
        .expect("the unpause is recorded");
    // The stages start in order: `start`, `a`, then `b` after the unpause.
    let b_started = names
        .iter()
        .enumerate()
        .filter(|(_, name)| *name == "step.started")
        .nth(2)
        .map(|(index, _)| index)
        .expect("b started");
    assert!(
        unpaused < b_started,
        "b started before the unpause: {names:?}"
    );
    assert!(pending_control(&server, &run_id).await.is_null());
    server.shutdown();
}

/// `SIGUSR1` on the worker pauses the run the way the API's pause does,
/// and `SIGUSR2` unpauses it: `b` is held at admission in between, Petri's
/// records and Fabro's lifecycle both carry the pause and the unpause, and
/// no control request is recorded, since none went through the API.
#[tokio::test(flavor = "multi_thread")]
async fn the_user_signals_pause_and_unpause_the_worker() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let gate = context.temp_dir.join("a.gate");
    let marker = context.temp_dir.join("b.marker");
    let workspace = two_stage_workspace(&context, &gate, &marker);
    let run_id = run_detached(&context, &server, &workspace);

    wait_for_status(&server, &run_id, &["running"]).await;
    let worker = wait_for_worker(&run_id);
    wait_until_gate_is_polled(&gate);
    eprintln!("run {run_id}: a is waiting on the gate; sending SIGUSR1 to worker {worker}");

    fabro_proc::sigusr1(worker);
    wait_for_status(&server, &run_id, &["paused"]).await;
    eprintln!("run {run_id} is paused");

    std::fs::write(&gate, "go").expect("the gate opens");
    wait_for_stream_count(&server, &run_id, "step.finished", 2).await;
    tokio::time::sleep(HOLD).await;
    assert!(!marker.exists(), "b started while the run was paused");
    assert_eq!(run_status(&server, &run_id).await, "paused");
    assert!(pending_control(&server, &run_id).await.is_null());

    fabro_proc::sigusr2(worker);
    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let items = settled_stream(&server, &run_id).await;
    let names = stream_names(&items);
    assert_eq!(
        status,
        "succeeded",
        "stream: {names:?}\nserver stderr:\n{}",
        server.stderr_text()
    );
    assert!(marker.exists(), "b ran after the unpause");
    assert_petri_succeeded(&server, &run_id).await;

    for event in [
        "run.paused",
        "run.unpaused",
        "lifecycle:paused",
        "lifecycle:unpaused",
    ] {
        assert_eq!(count_of(&names, event), 1, "{event}: {names:?}");
    }
    for event in ["lifecycle:pause_requested", "lifecycle:unpause_requested"] {
        assert_eq!(
            count_of(&names, event),
            0,
            "a signal is not an API request: {event}: {names:?}"
        );
    }
    let unpaused = names
        .iter()
        .position(|name| name == "run.unpaused")
        .expect("the unpause is recorded");
    let b_started = names
        .iter()
        .enumerate()
        .filter(|(_, name)| *name == "step.started")
        .nth(2)
        .map(|(index, _)| index)
        .expect("b started");
    assert!(
        unpaused < b_started,
        "b started before the unpause: {names:?}"
    );
    server.shutdown();
}

/// A steer sent while the agent stage waits on a tool reaches its
/// session: the twin sees the steer text in the follow-up request, the
/// stream carries the `control.requested` record, and the run succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn a_steer_reaches_the_agent_stage_on_the_twin() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    let server = RunningServer::start_with(
        &format!(
            "\n[llm.providers.openai]\nbase_url = \"{}\"\n",
            twin.base_url
        ),
        &[(EnvVars::OPENAI_API_KEY, &namespace)],
    )
    .await;
    let gate = context.temp_dir.join("steer.gate");
    let scenario = || TwinScenario::responses(MODEL).input_contains(PROMPT);
    TwinScenarios::new(namespace.clone())
        .scenario(scenario().tool_call(TwinToolCall::new(
            "shell",
            json!({ "command": format!("while [ ! -f {} ]; do sleep 0.05; done", gate.display()) }),
        )))
        .scenario(scenario().text("The gate opened."))
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains(STEER)
                .text("Lighthouse noted."),
        )
        .load(twin)
        .await;
    let workspace = write_petri_workflow(
        &context,
        &format!(
            "digraph Steer {{\n  graph [goal=\"Wait then report\", default_max_retries=0]\n  \
             start [shape=Mdiamond]\n  exit [shape=Msquare]\n  work [shape=box, \
             prompt=\"{PROMPT}\", max_retries=0]\n  start -> work -> exit\n}}\n"
        ),
    );
    let run_id = run_detached_with(&context, &server, &workspace, &[
        "--auto-approve",
        "--provider",
        "openai",
        "--model",
        MODEL,
    ]);

    wait_for_status(&server, &run_id, &["running"]).await;
    wait_until_gate_is_polled(&gate);
    eprintln!("run {run_id}: the agent's tool is waiting on the gate");
    // A stage that is not running: refused in the response, and on the
    // stream as a notice under the same code.
    let (status, body) = steer_request(&server, &run_id, "Steer nobody.", Some("nope")).await;
    assert_refused(
        status,
        &body,
        "no_such_stage",
        "Steer of stage `nope` refused: no stage named `nope` is running",
    );
    steer(&server, &run_id, STEER).await;
    wait_for_stream_count(&server, &run_id, "control.requested", 1).await;
    eprintln!("run {run_id}: the steer is recorded");
    std::fs::write(&gate, "go").expect("the gate opens");

    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let items = settled_stream(&server, &run_id).await;
    let names = stream_names(&items);
    assert_eq!(
        status,
        "succeeded",
        "stream: {names:?}\nserver stderr:\n{}",
        server.stderr_text()
    );
    assert_petri_succeeded(&server, &run_id).await;
    assert_eq!(
        notices(&items),
        [(
            "no_such_stage".to_string(),
            "Steer of stage `nope` refused: no stage named `nope` is running".to_string()
        )],
        "the refused steer is the one notice"
    );

    let delivery = items
        .iter()
        .find(|item| item["item"]["record"]["body"]["event"] == "control.requested")
        .expect("the steer is in the stream");
    let text = serde_json::to_string(delivery).expect("the item serializes");
    assert!(
        text.contains(STEER),
        "the control record carries the steer: {text}"
    );
    assert!(
        text.contains("\"deliverable\":true"),
        "the steer was delivered to a live firing: {text}"
    );

    let logs = twin.request_logs(&namespace).await;
    let inputs: Vec<String> = logs["requests"]
        .as_array()
        .expect("the twin request log is an array")
        .iter()
        .map(|request| {
            request["input_text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .filter(|input| input.contains(PROMPT))
        .collect();
    assert_eq!(
        inputs.len(),
        3,
        "the tool call, its answer, the steer: {inputs:?}"
    );
    assert!(
        !inputs[1].contains(STEER),
        "the answer's request came before the steer's turn: {}",
        inputs[1]
    );
    assert!(
        inputs[2].contains(STEER),
        "the follow-up request carries the steer: {}",
        inputs[2]
    );
    server.shutdown();
}

/// Two agent stages live at once, as the branches of a parallel node: a
/// steer that names no stage is refused with a notice naming both, and a
/// steer to each label (`a@1` over the API, `b@1` through the CLI flag)
/// reaches that stage's session and no other.
#[tokio::test(flavor = "multi_thread")]
async fn two_live_agent_stages_are_steered_apart_by_their_labels() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    let server = RunningServer::start_with(
        &format!(
            "\n[llm.providers.openai]\nbase_url = \"{}\"\n",
            twin.base_url
        ),
        &[(EnvVars::OPENAI_API_KEY, &namespace)],
    )
    .await;
    let gate_a = context.temp_dir.join("a.gate");
    let gate_b = context.temp_dir.join("b.gate");
    let wait_on = |gate: &Path| {
        TwinToolCall::new(
            "shell",
            json!({ "command": format!("while [ ! -f {} ]; do sleep 0.05; done", gate.display()) }),
        )
    };
    TwinScenarios::new(namespace.clone())
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains(PROMPT_A)
                .tool_call(wait_on(&gate_a)),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains(PROMPT_A)
                .text("Alpha's gate opened."),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains(STEER_A)
                .text("Lighthouse noted."),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains(PROMPT_B)
                .tool_call(wait_on(&gate_b)),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains(PROMPT_B)
                .text("Bravo's gate opened."),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains(STEER_B)
                .text("Windmill noted."),
        )
        .load(twin)
        .await;
    let workspace = write_petri_workflow(
        &context,
        &format!(
            "digraph Pair {{\n  graph [goal=\"Two agents wait then report\", \
             default_max_retries=0]\n  start [shape=Mdiamond]\n  exit [shape=Msquare]\n  fan \
             [shape=component]\n  a [shape=box, prompt=\"{PROMPT_A}\", max_retries=0]\n  b \
             [shape=box, prompt=\"{PROMPT_B}\", max_retries=0]\n  join \
             [shape=tripleoctagon]\n  start -> fan\n  fan -> a\n  fan -> b\n  a -> join\n  b -> \
             join\n  join -> exit\n}}\n"
        ),
    );
    let run_id = run_detached_with(&context, &server, &workspace, &[
        "--auto-approve",
        "--provider",
        "openai",
        "--model",
        MODEL,
    ]);

    wait_for_status(&server, &run_id, &["running"]).await;
    wait_until_gate_is_polled(&gate_a);
    wait_until_gate_is_polled(&gate_b);
    eprintln!("run {run_id}: both agents' tools are waiting on their gates");

    // Unnamed, the steer has two candidates and is refused with both
    // named: in the response, and on the stream.
    let (status, body) = steer_request(&server, &run_id, "Steer nobody.", None).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["errors"][0]["code"], "steer_refused", "{body}");
    let detail = body["errors"][0]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("a@1") && detail.contains("b@1"),
        "the refusal names both live stages: {body}"
    );
    let names = wait_for_stream_count(&server, &run_id, "run.notice", 1).await;
    assert_eq!(count_of(&names, "control.requested"), 0, "{names:?}");
    let notice = run_stream(&server, &run_id)
        .await
        .into_iter()
        .find(|item| item["item"]["record"]["kind"] == "run.notice")
        .expect("the refusal is recorded");
    let message = notice["item"]["record"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        notice["item"]["record"]["code"], "steer_refused",
        "{notice}"
    );
    assert!(
        message.contains("a@1") && message.contains("b@1"),
        "the notice names both live stages: {message}"
    );

    steer_stage(&server, &run_id, STEER_A, Some("a@1")).await;
    steer_by_cli(&context, &server, &run_id, "b@1", STEER_B);
    wait_for_stream_count(&server, &run_id, "control.requested", 2).await;
    eprintln!("run {run_id}: both steers are recorded");
    std::fs::write(&gate_a, "go").expect("gate a opens");
    std::fs::write(&gate_b, "go").expect("gate b opens");

    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let items = settled_stream(&server, &run_id).await;
    let names = stream_names(&items);
    assert_eq!(
        status,
        "succeeded",
        "stream: {names:?}\nserver stderr:\n{}",
        server.stderr_text()
    );
    assert_petri_succeeded(&server, &run_id).await;

    let logs = twin.request_logs(&namespace).await;
    for (prompt, steer, other) in [(PROMPT_A, STEER_A, STEER_B), (PROMPT_B, STEER_B, STEER_A)] {
        let inputs = inputs_with(&logs, prompt);
        assert_eq!(
            inputs.len(),
            3,
            "{prompt}: the tool call, its answer, the steer: {inputs:?}"
        );
        assert!(
            !inputs[1].contains(steer),
            "{prompt}: the answer's request came before the steer's turn: {}",
            inputs[1]
        );
        assert!(
            inputs[2].contains(steer),
            "{prompt}: the follow-up request carries its own steer: {}",
            inputs[2]
        );
        assert!(
            !inputs[2].contains(other),
            "{prompt}: the other stage's steer stayed away: {}",
            inputs[2]
        );
    }
    server.shutdown();
}

/// An interrupt while the agent's tool call waits on a gate that never
/// opens: `fabro steer --interrupt` stops the turn (the tool call is
/// cancelled, no answer is reached), the session is kept, and the text is
/// the agent's next input, which the twin answers. The stream carries the
/// `control.requested` record with the `$interrupt` value and the stage's
/// `attractor.turn.interrupted` report, and the run succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_ends_the_turn_and_its_text_is_the_next_input() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    let server = RunningServer::start_with(
        &format!(
            "\n[llm.providers.openai]\nbase_url = \"{}\"\n",
            twin.base_url
        ),
        &[(EnvVars::OPENAI_API_KEY, &namespace)],
    )
    .await;
    // The gate is never opened: only the interrupt ends the tool call.
    let gate = context.temp_dir.join("interrupt.gate");
    TwinScenarios::new(namespace.clone())
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains(PROMPT)
                .tool_call(TwinToolCall::new(
                    "shell",
                    json!({ "command": format!("while [ ! -f {} ]; do sleep 0.05; done", gate.display()) }),
                )),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains(INTERRUPT_STEER)
                .text("Summary: I was waiting on the gate."),
        )
        .load(twin)
        .await;
    let workspace = write_petri_workflow(
        &context,
        &format!(
            "digraph Interrupt {{\n  graph [goal=\"Wait then report\", default_max_retries=0]\n  \
             start [shape=Mdiamond]\n  exit [shape=Msquare]\n  work [shape=box, \
             prompt=\"{PROMPT}\", max_retries=0]\n  start -> work -> exit\n}}\n"
        ),
    );
    let run_id = run_detached_with(&context, &server, &workspace, &[
        "--auto-approve",
        "--provider",
        "openai",
        "--model",
        MODEL,
    ]);

    wait_for_status(&server, &run_id, &["running"]).await;
    wait_until_gate_is_polled(&gate);
    eprintln!("run {run_id}: the agent's tool is waiting on the gate; interrupting");
    interrupt_by_cli(&context, &server, &run_id, INTERRUPT_STEER);
    wait_for_stream_count(&server, &run_id, "control.requested", 1).await;
    eprintln!("run {run_id}: the interrupt is recorded");

    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let items = settled_stream(&server, &run_id).await;
    let names = stream_names(&items);
    assert_eq!(
        status,
        "succeeded",
        "stream: {names:?}\nserver stderr:\n{}",
        server.stderr_text()
    );
    assert!(!gate.exists(), "nothing opened the gate");
    assert_petri_succeeded(&server, &run_id).await;
    assert_eq!(notices(&items), Vec::new(), "nothing was refused");

    let delivery = items
        .iter()
        .find(|item| item["item"]["record"]["body"]["event"] == "control.requested")
        .expect("the interrupt is in the stream");
    let record = &delivery["item"]["record"]["body"];
    assert_eq!(
        record["ctl"]["deliver"]["$interrupt"]["steer"], INTERRUPT_STEER,
        "the control record carries the interrupt and its text: {record}"
    );
    assert_eq!(
        delivery["item"]["derived"]["deliverable"], true,
        "the interrupt was delivered to a live firing: {delivery}"
    );
    let interrupted = progress_of_kind(&items, "attractor.turn.interrupted");
    assert_eq!(interrupted.len(), 1, "{names:?}");
    assert_eq!(interrupted[0]["node"], "work", "{}", interrupted[0]);
    assert_eq!(interrupted[0]["backend"], "api", "{}", interrupted[0]);

    let logs = twin.request_logs(&namespace).await;
    let inputs = inputs_with(&logs, PROMPT);
    assert_eq!(
        inputs.len(),
        2,
        "the interrupted turn, then the steered one: {inputs:?}"
    );
    assert!(
        !inputs[0].contains(INTERRUPT_STEER),
        "the first request came before the interrupt: {}",
        inputs[0]
    );
    assert!(
        inputs[1].contains(INTERRUPT_STEER),
        "the next request carries the interrupt's text as its input: {}",
        inputs[1]
    );
    server.shutdown();
}

/// A workflow of one human gate: `yes` leaves `marker`.
fn gate_workspace(context: &fabro_test::TestContext, marker: &Path) -> PathBuf {
    write_petri_workflow(
        context,
        &format!(
            "digraph Gate {{\n  graph [goal=\"Ask before running\"]\n  start [shape=Mdiamond]\n  \
             exit [shape=Msquare]\n  gate [shape=hexagon, label=\"Go?\", \
             question_type=\"yes_no\"]\n  yes [shape=parallelogram, script=\"touch {marker}\"]\n  \
             start -> gate\n  gate -> yes [label=\"[Y] Yes\"]\n  gate -> exit [label=\"[N] \
             No\"]\n  yes -> exit\n}}\n",
            marker = marker.display()
        ),
    )
}

const GATE_INTERRUPT_REFUSAL: &str =
    "Interrupt of stage `gate` refused: the stage has no model turn to interrupt";
const UNNAMED_INTERRUPT_REFUSAL: &str =
    "Interrupt refused: Run has no active steerable agent session.";

/// An interrupt of a stage with no model turn to stop: the gate the run is
/// blocked on, named by its node, is refused by Petri with `no_live_turn`;
/// unnamed, with no agent stage live, the worker refuses it with
/// `interrupt_refused`. Each refusal is the endpoint's own answer, a 409
/// with the code and the reason, and `fabro steer` prints it; each is also
/// a `run.notice` record on the stream naming the stage and Petri's
/// reason. Nothing is delivered, and the gate's question is untouched: its
/// answer routes the run to its end.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_of_a_gate_stage_is_refused_with_no_live_turn() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let marker = context.temp_dir.join("yes.marker");
    let workspace = gate_workspace(&context, &marker);
    let run_id = run_detached_with(&context, &server, &workspace, &[]);

    let pending = wait_for_questions(&server, &run_id, 1).await;
    assert_eq!(pending[0]["stage"], "gate@1", "{}", pending[0]);
    let question_id = pending[0]["id"].as_str().expect("an id").to_string();
    eprintln!("run {run_id}: the gate is asking; interrupting it");

    let (status, body) = control(
        &server,
        &run_id,
        "interrupt",
        Some(json!({ "stage": "gate" })),
    )
    .await;
    assert_refused(status, &body, "no_live_turn", GATE_INTERRUPT_REFUSAL);
    let (status, body) = control(&server, &run_id, "interrupt", None).await;
    assert_refused(
        status,
        &body,
        "interrupt_refused",
        UNNAMED_INTERRUPT_REFUSAL,
    );
    interrupt_refused_by_cli(&context, &server, &run_id, "gate", GATE_INTERRUPT_REFUSAL);
    let names = wait_for_stream_count(&server, &run_id, "run.notice", 3).await;
    assert_eq!(count_of(&names, "control.requested"), 0, "{names:?}");
    let refused = notices(&run_stream(&server, &run_id).await);
    // The notice names the stage and carries Petri's reason as it spells
    // it, so the web and the CLI can show both.
    assert_eq!(refused, [
        (
            "no_live_turn".to_string(),
            GATE_INTERRUPT_REFUSAL.to_string()
        ),
        (
            "interrupt_refused".to_string(),
            UNNAMED_INTERRUPT_REFUSAL.to_string()
        ),
        (
            "no_live_turn".to_string(),
            GATE_INTERRUPT_REFUSAL.to_string()
        ),
    ]);
    assert_eq!(run_status(&server, &run_id).await, "blocked");

    answer(&server, &run_id, &question_id, json!({ "kind": "yes" })).await;
    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let items = settled_stream(&server, &run_id).await;
    let names = stream_names(&items);
    assert_eq!(
        status,
        "succeeded",
        "stream: {names:?}\nserver stderr:\n{}",
        server.stderr_text()
    );
    assert!(marker.exists(), "the answer routed the gate");
    assert_petri_succeeded(&server, &run_id).await;
    assert_eq!(
        count_of(&names, "control.requested"),
        1,
        "only the answer was delivered: {names:?}"
    );
    assert!(
        progress_of_kind(&items, "attractor.turn.interrupted").is_empty(),
        "no turn was stopped: {names:?}"
    );
    server.shutdown();
}

/// A worker that never answers a control: the endpoint waits its bound
/// (5 s) and answers `202` with `pending`; the control was still applied,
/// so its refusal is on the stream as a notice. The worker's answers are
/// muted through the server's test hook, forwarded to the worker by name.
#[tokio::test(flavor = "multi_thread")]
async fn a_control_the_worker_never_answers_is_pending() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server =
        RunningServer::start_with_env("", &[], &[(EnvVars::FABRO_TEST_CONTROL_ACKS_MUTED, "1")])
            .await;
    let marker = context.temp_dir.join("yes.marker");
    let workspace = gate_workspace(&context, &marker);
    let run_id = run_detached_with(&context, &server, &workspace, &[]);

    let pending = wait_for_questions(&server, &run_id, 1).await;
    let question_id = pending[0]["id"].as_str().expect("an id").to_string();
    eprintln!("run {run_id}: the gate is asking; interrupting it with the answers muted");

    let asked = Instant::now();
    let (status, body) = control(
        &server,
        &run_id,
        "interrupt",
        Some(json!({ "stage": "gate" })),
    )
    .await;
    assert_eq!(status, 202, "interrupt: {body}");
    assert_eq!(body, json!({ "outcome": "pending" }));
    assert!(
        asked.elapsed() >= Duration::from_secs(4),
        "the endpoint waited its bound for the answer: {:?}",
        asked.elapsed()
    );
    let refused = notices(&run_stream(&server, &run_id).await);
    assert_eq!(refused, [(
        "no_live_turn".to_string(),
        GATE_INTERRUPT_REFUSAL.to_string()
    )]);

    answer(&server, &run_id, &question_id, json!({ "kind": "yes" })).await;
    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "server stderr:\n{}",
        server.stderr_text()
    );
    assert!(marker.exists(), "the answer routed the gate");
    server.shutdown();
}

/// A run paused with its next stage held at admission, whose server and
/// worker then die, resumes paused: the resumed worker reports the pause
/// again, admits nothing until the unpause, then finishes the run. (A
/// stage that was mid-flight at the crash is re-dispatched on resume
/// without a new admission: a pause holds admission, never running work.)
#[tokio::test(flavor = "multi_thread")]
async fn a_run_paused_before_a_crash_resumes_paused() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let mut server = RunningServer::start().await;
    let gate = context.temp_dir.join("a.gate");
    let marker = context.temp_dir.join("b.marker");
    let workspace = two_stage_workspace(&context, &gate, &marker);
    let run_id = run_detached(&context, &server, &workspace);

    wait_for_status(&server, &run_id, &["running"]).await;
    let worker = wait_for_worker(&run_id);
    wait_until_gate_is_polled(&gate);
    pause(&server, &run_id).await;
    wait_for_status(&server, &run_id, &["paused"]).await;
    wait_for_no_pending_control(&server, &run_id).await;
    // `a` finishes under the pause; `b` reaches admission and is held.
    std::fs::write(&gate, "go").expect("the gate opens");
    wait_for_stream_count(&server, &run_id, "step.finished", 2).await;
    tokio::time::sleep(HOLD).await;
    assert!(!marker.exists(), "b started while the run was paused");
    eprintln!("run {run_id} is paused with b held at admission; crashing");

    server.kill();
    fabro_proc::sigkill_process_group(worker);
    let deadline = Instant::now() + Duration::from_secs(10);
    while fabro_proc::process_running(worker) {
        assert!(Instant::now() < deadline, "the worker did not die");
        std::thread::sleep(POLL);
    }

    server.launch().await;
    eprintln!("server restarted");
    let resumed = wait_for_worker(&run_id);
    assert_ne!(resumed, worker, "a new worker was launched");
    // The resumed worker reports the pause it came back under.
    let names = wait_for_stream_count(&server, &run_id, "lifecycle:paused", 2).await;
    assert_eq!(count_of(&names, "run.paused"), 1, "{names:?}");
    tokio::time::sleep(HOLD).await;
    assert!(
        !marker.exists(),
        "b was admitted while the resumed run was paused"
    );
    assert_eq!(run_status(&server, &run_id).await, "paused");
    assert!(pending_control(&server, &run_id).await.is_null());

    unpause(&server, &run_id).await;
    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let items = settled_stream(&server, &run_id).await;
    let names = stream_names(&items);
    assert_eq!(
        status,
        "succeeded",
        "stream: {names:?}\nserver stderr:\n{}",
        server.stderr_text()
    );
    assert!(marker.exists(), "b ran after the unpause");
    assert_petri_succeeded(&server, &run_id).await;
    assert_eq!(count_of(&names, "run.paused"), 1, "{names:?}");
    assert_eq!(count_of(&names, "run.unpaused"), 1, "{names:?}");
    assert_eq!(count_of(&names, "lifecycle:paused"), 2, "{names:?}");
    assert_eq!(count_of(&names, "lifecycle:unpaused"), 1, "{names:?}");
    assert_eq!(count_of(&names, "lifecycle:running"), 2, "{names:?}");
    let unpaused = names
        .iter()
        .position(|name| name == "run.unpaused")
        .expect("the unpause is recorded");
    let b_started = names
        .iter()
        .enumerate()
        .filter(|(_, name)| *name == "step.started")
        .nth(2)
        .map(|(index, _)| index)
        .expect("b started");
    assert!(
        unpaused < b_started,
        "b started before the unpause: {names:?}"
    );
    server.shutdown();
}
