//! Petri runs on a Docker environment through a real server and its
//! worker: the workspace lives inside the run's container, every stage's
//! checkpoint is committed there and published to the snapshot repository
//! on the host, and a restart brings the container's workspace back to the
//! snapshot its durable state names, in the retained container or in a
//! fresh one when the old one is gone.
//!
//! An Ask Fabro session on a finished Docker run attaches to the container
//! Petri created and reads a file the workflow wrote there, its model the
//! twin.
//!
//! The runs use the built-in Docker provider on this machine's daemon.
//! Tests skip when no daemon answers, unless `FABRO_REQUIRE_SANDBOX_BACKENDS`
//! requires it. The server, detached run and crash come from `petri.rs`.

#![expect(
    clippy::disallowed_methods,
    reason = "these scenarios inspect backend availability and drive the Docker daemon with its CLI"
)]

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use fabro_petri::checkpoint::CheckpointKey;
use fabro_static::EnvVars;
use fabro_test::{
    TwinScenario, TwinScenarios, TwinToolCall, expect_reqwest_json, test_context, twin_openai,
};
use fabro_types::{WorkflowPath, WorkflowVersion};
use serde_json::json;

use super::petri::{
    RunningServer, crash, run_detached_in, wait_for_status, wait_for_success, wait_for_worker,
    write_petri_workflow,
};
use crate::support::TEST_DEV_TOKEN;

/// The server-side environment the runs select.
const ENVIRONMENT: &str = "docker";
/// The twin's model, for the Ask Fabro session.
const MODEL: &str = "gpt-5.4";

/// A server with a Docker environment beside the default local one.
async fn docker_server() -> RunningServer {
    docker_server_with("", &[]).await
}

/// `docker_server`, with `settings` appended to the server's settings and
/// `secrets` in its vault.
async fn docker_server_with(settings: &str, secrets: &[(&str, &str)]) -> RunningServer {
    docker_server_with_env(settings, secrets, &[]).await
}

async fn docker_server_with_env(
    settings: &str,
    secrets: &[(&str, &str)],
    env: &[(&str, &str)],
) -> RunningServer {
    let server = RunningServer::start_with_env(settings, secrets, env).await;
    let body = json!({
        "id": ENVIRONMENT,
        "provider": "docker",
        "image": { "docker": null, "dockerfile": null },
        "resources": { "cpu": null, "memory": null, "disk": null },
        "network": { "mode": "allow_all", "allow": [] },
        "lifecycle": { "preserve": false, "stop_on_terminal": true, "auto_stop": null },
        "labels": {},
        "env": {}
    });
    let response = fabro_test::test_http_client()
        .post(format!("{}/api/v1/environments", server.api_base_url))
        .bearer_auth(TEST_DEV_TOKEN)
        .json(&body)
        .send()
        .await
        .expect("the environment create sends");
    expect_reqwest_json(
        response,
        fabro_http::StatusCode::CREATED,
        "POST /api/v1/environments",
    )
    .await;
    server
}

async fn checkout_version(server: &RunningServer, script: &str, settings: &str) -> String {
    let path = |name| WorkflowPath::new(name).expect("the workflow path is valid");
    let dot = format!(
        "digraph Checkout {{ start [shape=Mdiamond]; verify [shape=parallelogram, goal_gate=true, script={}]; exit [shape=Msquare]; start -> verify -> exit; }}",
        serde_json::to_string(script).expect("the script serializes"),
    );
    let version = WorkflowVersion::new(
        path("workflow.fabro"),
        BTreeMap::from([
            (path("workflow.fabro"), dot),
            (
                path("workflow.toml"),
                format!("_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n{settings}"),
            ),
        ]),
        BTreeMap::new(),
    )
    .expect("the checkout workflow version is valid");
    let response = fabro_test::test_http_client()
        .post(format!("{}/api/v1/workflow-versions", server.api_base_url))
        .bearer_auth(TEST_DEV_TOKEN)
        .json(&version)
        .send()
        .await
        .expect("the checkout request sends");
    expect_reqwest_json(
        response,
        fabro_http::StatusCode::CREATED,
        "register checkout workflow",
    )
    .await["workflow_version_id"]
        .as_str()
        .expect("the response names its workflow version")
        .to_owned()
}

async fn create_checkout_run(
    server: &RunningServer,
    version: &str,
    target: serde_json::Value,
) -> fabro_http::Response {
    fabro_test::test_http_client()
        .post(format!("{}/api/v1/runs", server.api_base_url))
        .bearer_auth(TEST_DEV_TOKEN)
        .json(&json!({"workflow_version_id": version, "environment_id": ENVIRONMENT, "target": target, "args": {"auto_approve": true}}))
        .send().await.expect("the run creation request sends")
}

async fn start_checkout_run(server: &RunningServer, id: &str) {
    let response = fabro_test::test_http_client()
        .post(format!("{}/api/v1/runs/{id}/start", server.api_base_url))
        .bearer_auth(TEST_DEV_TOKEN)
        .json(&json!({}))
        .send()
        .await
        .expect("the checkout request sends");
    fabro_test::expect_reqwest_status(response, fabro_http::StatusCode::OK, "start checkout run")
        .await;
}

/// The public create/start path must deliver the requested Git revision before
/// stage one. Redirect only this server's Git transport to a local fixture;
/// the run still names a GitHub target and exercises production acquisition.
#[tokio::test(flavor = "multi_thread")]
async fn a_git_target_reaches_docker_at_its_pinned_revision_and_checkpoints_edits() {
    if !fabro_test::docker_available() {
        return;
    }
    let context = test_context!();
    let upstream = context.temp_dir.join("upstream");
    std::fs::create_dir(&upstream).unwrap();
    git(&upstream, &["init", "--initial-branch=main"]);
    git(&upstream, &["config", "user.name", "Checkout Test"]);
    git(&upstream, &["config", "user.email", "checkout@example.com"]);
    git(&upstream, &["config", "commit.gpgsign", "false"]);
    std::fs::write(upstream.join("README.md"), "existing repository bytes\n").unwrap();
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-m", "original"]);
    let pin = git(&upstream, &["rev-parse", "HEAD"]);
    let blob = git(&upstream, &["hash-object", "README.md"]);
    std::fs::write(upstream.join("README.md"), "new branch tip\n").unwrap();
    git(&upstream, &["commit", "-am", "move main"]);
    let config = context.temp_dir.join("checkout.gitconfig");
    let remote = "https://github.com/checkout-fixture/source.git";
    std::fs::write(
        &config,
        format!(
            "[url \"file://{}\"]\n\tinsteadOf = {remote}\n",
            upstream.display()
        ),
    )
    .unwrap();
    let mut server = docker_server_with_env(
        "",
        &[(EnvVars::GITHUB_TOKEN, "ghu_checkout_fixture")],
        &[("GIT_CONFIG_GLOBAL", config.to_str().unwrap())],
    )
    .await;
    let version = checkout_version(&server, &format!(
        "set -eu; test $(git hash-object README.md) = {blob}; git merge-base --is-ancestor {pin} HEAD; test $(git remote get-url origin) = {remote}; printf 'edited existing file\\n' >> README.md"
    ), "").await;
    let response = create_checkout_run(
        &server,
        &version,
        json!({"kind": "git", "repo": "checkout-fixture/source", "branch": "main", "sha": pin}),
    )
    .await;
    let created = expect_reqwest_json(
        response,
        fabro_http::StatusCode::CREATED,
        "create Git target run",
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let state = super::petri::run_json(&server, &format!("runs/{id}/state")).await;
    assert_eq!(state["spec"]["target"]["sha"], pin);
    // Neither a moving remote nor a process restart may change the admitted
    // source; starting the run must need only the retained source repository.
    server.kill();
    std::fs::remove_dir_all(upstream).unwrap();
    server.launch().await;
    start_checkout_run(&server, id).await;
    wait_for_success(&server, id).await;
    let repository = snapshot_repository(&server, id);
    let commits = snapshot_commits(&repository);
    let edit = commits
        .iter()
        .find(|(_, subject, _)| subject.ends_with(": verify (success)"))
        .unwrap();
    assert_eq!(
        git(&repository, &["show", &format!("{}:README.md", edit.0)]),
        "existing repository bytes\nedited existing file"
    );
    assert!(commits.iter().any(|(sha, _, _)| sha == &pin));
    cleanup(id);

    let empty = checkout_version(&server, "test -z \"$(git ls-files)\"", "").await;
    let response = create_checkout_run(&server, &empty, json!({"kind": "none"})).await;
    let created = expect_reqwest_json(
        response,
        fabro_http::StatusCode::CREATED,
        "create empty target run",
    )
    .await;
    let id = created["id"].as_str().unwrap();
    start_checkout_run(&server, id).await;
    wait_for_success(&server, id).await;
    cleanup(id);

    let disabled = checkout_version(
        &server,
        "test -z \"$(git ls-files)\"",
        "[run.clone]\nenabled = false\n",
    )
    .await;
    let response = create_checkout_run(
        &server,
        &disabled,
        json!({"kind": "git", "repo": "checkout-fixture/source", "branch": "main"}),
    )
    .await;
    let rejected = expect_reqwest_json(
        response,
        fabro_http::StatusCode::UNPROCESSABLE_ENTITY,
        "reject Git target with checkout disabled",
    )
    .await;
    assert_eq!(
        rejected["errors"][0]["code"],
        "target_environment_unsupported"
    );

    let response = create_checkout_run(
        &server,
        &version,
        json!({"kind": "git", "repo": "checkout-fixture/source", "branch": "main"}),
    )
    .await;
    let rejected = expect_reqwest_json(
        response,
        fabro_http::StatusCode::UNPROCESSABLE_ENTITY,
        "reject unavailable Git target",
    )
    .await;
    assert_eq!(rejected["errors"][0]["code"], "target_checkout_failed");
    assert_eq!(
        rejected["errors"][0]["detail"],
        "could not check out checkout-fixture/source; check repository access and the requested revision"
    );
    server.shutdown();
}

/// The run's container on the daemon, by Petri's run label: the one the
/// run's scope lives in.
fn container_of(run_id: &str) -> Option<String> {
    let output = Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=petri.run={run_id}"),
        ])
        .output()
        .expect("docker ps runs");
    let ids: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    assert!(ids.len() <= 1, "one container per run: {ids:?}");
    ids.into_iter().next()
}

/// `sh -c script` inside the container's workspace.
fn docker_exec(container: &str, script: &str) -> String {
    let output = Command::new("docker")
        .args(["exec", "-w", "/workspace", container, "sh", "-c", script])
        .output()
        .expect("docker exec runs");
    assert!(
        output.status.success(),
        "docker exec failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn docker_rm(container: &str) {
    let status = Command::new("docker")
        .args(["rm", "-f", container])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("docker rm runs");
    assert!(status.success(), "the container is removed");
}

/// Remove whatever the run left on the daemon, so a failed assertion does
/// not leak a container.
fn cleanup(run_id: &str) {
    if let Some(container) = container_of(run_id) {
        docker_rm(&container);
    }
}

/// The snapshot repository of the run's one workspace, on the host.
fn snapshot_repository(server: &RunningServer, run_id: &str) -> PathBuf {
    let snapshots = server.petri_run_dir(run_id).join("snapshots");
    let mut repositories: Vec<PathBuf> = std::fs::read_dir(&snapshots)
        .expect("the snapshots directory lists")
        .map(|entry| entry.expect("an entry reads").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "git"))
        .collect();
    assert_eq!(repositories.len(), 1, "one workspace: {repositories:?}");
    repositories.remove(0)
}

/// `git` in a repository on the host, its stdout.
fn git(repository: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The commits the snapshot repository holds, oldest first, as
/// `(sha, subject, key)`.
fn snapshot_commits(repository: &Path) -> Vec<(String, String, Option<CheckpointKey>)> {
    let log = git(repository, &[
        "log",
        "--topo-order",
        "--reverse",
        "--all",
        "--format=%H%x00%s%x00%B%x1e",
    ]);
    log.split('\u{1e}')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| {
            let mut parts = entry.trim_start().splitn(3, '\0');
            let sha = parts.next().unwrap_or_default().to_string();
            let subject = parts.next().unwrap_or_default().to_string();
            let body = parts.next().unwrap_or_default();
            (sha, subject, CheckpointKey::from_message(body))
        })
        .collect()
}

fn subjects(commits: &[(String, String, Option<CheckpointKey>)]) -> Vec<&str> {
    commits
        .iter()
        .map(|(_, subject, _)| subject.as_str())
        .collect()
}

/// Two command stages: `one` writes a file; `two` checks it is the one
/// `one` wrote, that nothing else is in the workspace beside the
/// repository's own files, and writes another.
fn two_stage_bundle(context: &fabro_test::TestContext) -> PathBuf {
    write_petri_workflow(
        context,
        "digraph Stages {\n  graph [goal=\"Two stages\", default_max_retries=0]\n  start \
         [shape=Mdiamond]\n  exit [shape=Msquare]\n  one [shape=parallelogram, script=\"echo one > \
         one.txt\"]\n  two [shape=parallelogram, script=\"test \\\"$(cat one.txt)\\\" = one && \
         test ! -e stray.txt && echo two > two.txt\"]\n  start -> one -> two -> exit\n}\n",
    )
}

/// The commit subjects one run of the two-stage bundle produces.
fn two_stage_subjects(run_id: &str) -> Vec<String> {
    ["start", "one", "two", "exit"]
        .iter()
        .map(|node| format!("fabro({run_id}): {node} (success)"))
        .collect()
}

/// The checkpoint records and the published snapshots name the same
/// commits, and the two stages' trees hold their files.
fn assert_snapshots_complete(server: &RunningServer, run_id: &str, repository: &Path) {
    let commits = snapshot_commits(repository);
    assert_eq!(subjects(&commits), two_stage_subjects(run_id));
    let (one, _, _) = &commits[1];
    let (two, _, _) = &commits[2];
    assert_eq!(git(repository, &["show", &format!("{one}:one.txt")]), "one");
    assert_eq!(git(repository, &["show", &format!("{two}:two.txt")]), "two");
    let refs = git(repository, &[
        "for-each-ref",
        "--format=%(objectname)",
        "refs/checkpoints/",
    ]);
    let mut published: Vec<&str> = refs.lines().collect();
    published.sort_unstable();
    let checkpoints = futures_lite_block_on(server.checkpoints(run_id));
    let mut recorded: Vec<&str> = checkpoints.iter().map(|(_, sha)| sha.as_str()).collect();
    recorded.sort_unstable();
    assert_eq!(
        published, recorded,
        "every record names a published snapshot"
    );
    assert_eq!(checkpoints.len(), 4, "{checkpoints:?}");
}

/// Wait on a future from a synchronous helper inside a multi-thread test.
fn futures_lite_block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
}

/// What the worker's log says it did to the sandbox workspace at resume.
fn restore_actions(server: &RunningServer, run_id: &str) -> Vec<String> {
    let log = std::fs::read_to_string(server.worker_log(run_id)).unwrap_or_default();
    log.lines()
        .filter(|line| line.contains("workspace brought to its durable snapshot"))
        .filter_map(|line| {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("action=").map(str::to_owned))
        })
        .collect()
}

/// A run on Docker: every stage is committed inside the container, each
/// checkpoint is published to the snapshot repository on the host, and
/// nothing of the workspace is on the host.
#[tokio::test(flavor = "multi_thread")]
async fn a_docker_run_publishes_every_stages_checkpoint_from_the_container() {
    if !fabro_test::docker_available() {
        return;
    }
    let context = test_context!();
    let server = docker_server().await;
    let workspace = two_stage_bundle(&context);
    let run_id = run_detached_in(&context, &server, &workspace, ENVIRONMENT, &[
        "--auto-approve",
    ]);
    wait_for_success(&server, &run_id).await;

    let repository = snapshot_repository(&server, &run_id);
    assert_snapshots_complete(&server, &run_id, &repository);
    assert!(
        !server.petri_run_dir(&run_id).join("scopes").exists(),
        "no workspace is on the host"
    );
    assert!(
        container_of(&run_id).is_some(),
        "the container is retained after the run"
    );
    cleanup(&run_id);
    server.shutdown();
}

/// A worker killed after the first stage's durable finish, with the
/// container's workspace changed behind Petri's back: the restart resumes
/// on the retained container, the workspace is reset to the snapshot, and
/// the second stage sees the first stage's files and nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn a_retained_container_whose_workspace_drifted_is_reset_on_restart() {
    if !fabro_test::docker_available() {
        return;
    }
    let context = test_context!();
    let mut server = docker_server().await;
    let workspace = two_stage_bundle(&context);
    server.hold("record", "one");
    let run_id = run_detached_in(&context, &server, &workspace, ENVIRONMENT, &[
        "--auto-approve",
    ]);

    wait_for_status(&server, &run_id, &["running"]).await;
    let worker = wait_for_worker(&run_id);
    server.wait_until_held(&run_id, "record", "one");
    let container = container_of(&run_id).expect("the run's container exists");
    docker_exec(
        &container,
        "echo junk > one.txt && echo stray > stray.txt && git status --porcelain",
    );
    crash(&mut server, worker, None);

    server.release("record", "one");
    server.launch().await;
    let resumed = wait_for_worker(&run_id);
    assert_ne!(resumed, worker);
    wait_for_success(&server, &run_id).await;

    assert_eq!(
        container_of(&run_id).as_deref(),
        Some(container.as_str()),
        "the run continued in its retained container"
    );
    assert_eq!(restore_actions(&server, &run_id), vec!["Reset".to_string()]);
    let repository = snapshot_repository(&server, &run_id);
    assert_snapshots_complete(&server, &run_id, &repository);
    cleanup(&run_id);
    server.shutdown();
}

/// A worker killed after the first stage's durable finish, with the
/// container removed while the run is down: the restart gets a fresh
/// container, the workspace is restored into it from the snapshot
/// repository, and the second stage sees the first stage's files.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_container_is_replaced_and_its_workspace_restored_from_the_snapshot() {
    if !fabro_test::docker_available() {
        return;
    }
    let context = test_context!();
    let mut server = docker_server().await;
    let workspace = two_stage_bundle(&context);
    server.hold("record", "one");
    let run_id = run_detached_in(&context, &server, &workspace, ENVIRONMENT, &[
        "--auto-approve",
    ]);

    wait_for_status(&server, &run_id, &["running"]).await;
    let worker = wait_for_worker(&run_id);
    server.wait_until_held(&run_id, "record", "one");
    let container = container_of(&run_id).expect("the run's container exists");
    crash(&mut server, worker, None);
    docker_rm(&container);
    assert_eq!(container_of(&run_id), None, "the container is gone");

    server.release("record", "one");
    server.launch().await;
    wait_for_success(&server, &run_id).await;

    let fresh = container_of(&run_id).expect("a fresh container was created");
    assert_ne!(fresh, container);
    assert_eq!(restore_actions(&server, &run_id), vec![
        "Restored".to_string()
    ]);
    let repository = snapshot_repository(&server, &run_id);
    assert_snapshots_complete(&server, &run_id, &repository);
    cleanup(&run_id);
    server.shutdown();
}

/// What the session is asked, and what the twin is told to answer once it
/// has read the file.
const QUESTION: &str = "Read hello.txt in the workspace and tell me what it says.";
const CONTENT: &str = "hello-from-petri";

/// A one-stage bundle whose command writes `hello.txt` into the workspace.
fn hello_file_bundle(context: &fabro_test::TestContext) -> PathBuf {
    write_petri_workflow(
        context,
        &format!(
            "digraph Hello {{\n  graph [goal=\"Write a file\", default_max_retries=0]\n  start \
             [shape=Mdiamond]\n  exit [shape=Msquare]\n  write [shape=parallelogram, script=\"echo \
             {CONTENT} > hello.txt\"]\n  start -> write -> exit\n}}\n"
        ),
    )
}

/// The twin's script for the session's turn.
fn turn_scenario() -> TwinScenario {
    TwinScenario::responses(MODEL).input_contains(QUESTION)
}

/// The events of a session turn's stream, in order.
fn turn_events(stream: &str) -> Vec<serde_json::Value> {
    stream
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str(data).expect("session event data is JSON"))
        .collect()
}

/// The twin's request log for `namespace`: the input text of each request
/// that carried the session's question, in order. The server's other
/// requests to the twin (a run title) are left out.
async fn question_inputs(twin: &fabro_test::TwinOpenAi, namespace: &str) -> Vec<String> {
    let logs = twin.request_logs(namespace).await;
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
        .filter(|input| input.contains(QUESTION))
        .collect()
}

/// Ask Fabro on a finished Docker run, through a real server: the session
/// attaches to the container Petri created (stopped at the run's end, so
/// the attach starts it again) and its tool reads a file the workflow
/// wrote inside it. Ask Fabro's tool policy is read-only: the shell tool
/// is hidden from the model and refused, so the turn reads the file with
/// the model's `read_file` tool, scripted on the twin, and the twin's
/// follow-up request carries the file's content back as the tool's answer.
#[tokio::test(flavor = "multi_thread")]
async fn an_ask_fabro_turn_reads_a_file_inside_the_runs_container() {
    if !fabro_test::docker_available() {
        return;
    }
    let context = test_context!();
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    let server = docker_server_with(
        &format!(
            "\n[llm.providers.openai]\nbase_url = \"{}\"\n",
            twin.base_url
        ),
        &[(EnvVars::OPENAI_API_KEY, namespace.as_str())],
    )
    .await;
    TwinScenarios::new(namespace.clone())
        .scenario(turn_scenario().tool_call(TwinToolCall::new(
            "read_file",
            json!({ "file_path": "/workspace/hello.txt" }),
        )))
        .scenario(turn_scenario().text(format!("hello.txt says: {CONTENT}")))
        .load(twin)
        .await;
    let workspace = hello_file_bundle(&context);
    let run_id = run_detached_in(&context, &server, &workspace, ENVIRONMENT, &[
        "--auto-approve",
    ]);
    wait_for_success(&server, &run_id).await;
    assert!(
        container_of(&run_id).is_some(),
        "the container is retained after the run"
    );

    let client = fabro_test::test_http_client();
    let response = client
        .post(format!(
            "{}/api/v1/runs/{run_id}/sessions",
            server.api_base_url
        ))
        .bearer_auth(TEST_DEV_TOKEN)
        .json(&json!({ "title": "Ask Fabro", "model": MODEL }))
        .send()
        .await
        .expect("the session create sends");
    let session = expect_reqwest_json(
        response,
        fabro_http::StatusCode::CREATED,
        "POST /api/v1/runs/{id}/sessions",
    )
    .await;
    let session_id = session["id"].as_str().expect("the session id");

    let response = client
        .post(format!(
            "{}/api/v1/sessions/{session_id}/turns",
            server.api_base_url
        ))
        .bearer_auth(TEST_DEV_TOKEN)
        .json(&json!({ "input": QUESTION }))
        .send()
        .await
        .expect("the turn sends");
    assert_eq!(
        response.status(),
        fabro_http::StatusCode::OK,
        "POST /api/v1/sessions/{{id}}/turns"
    );
    // The stream ends with the turn.
    let stream = response.text().await.expect("the turn's stream reads");
    let events = turn_events(&stream);

    let outcome = events
        .iter()
        .find(|event| {
            event["event"] == "run.session.turn.succeeded"
                || event["event"] == "run.session.turn.failed"
        })
        .unwrap_or_else(|| panic!("the turn ends: {events:?}"));
    assert_eq!(
        outcome["event"],
        "run.session.turn.succeeded",
        "the turn ended in the container: {outcome}\nserver stderr:\n{}",
        server.stderr_text()
    );
    let read = events
        .iter()
        .find(|event| {
            event["event"] == "run.session.tool_call.completed"
                && event["properties"]["tool_name"] == "read_file"
        })
        .unwrap_or_else(|| panic!("the read_file call completed: {events:?}"));
    assert_eq!(read["properties"]["is_error"], false, "{read}");
    assert!(
        read["properties"]["output"].to_string().contains(CONTENT),
        "the tool read the file inside the container: {read}"
    );
    // The tool-call round's assistant message carries no text; the reply
    // is the last one.
    let reply = events
        .iter()
        .rev()
        .find(|event| event["event"] == "run.session.assistant_message")
        .unwrap_or_else(|| panic!("the model replied: {events:?}"));
    assert!(
        reply["properties"]["text"]
            .as_str()
            .is_some_and(|text| text.contains(CONTENT)),
        "the reply names the file's content: {reply}"
    );

    // The twin's follow-up request carried the tool's answer.
    let inputs = question_inputs(twin, &namespace).await;
    assert_eq!(inputs.len(), 2, "{inputs:?}");
    assert!(
        inputs[1].contains(CONTENT),
        "the model read the file's content from the tool: {}",
        inputs[1]
    );

    cleanup(&run_id);
    server.shutdown();
}
