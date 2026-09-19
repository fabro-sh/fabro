//! Petri runs on a Docker environment through a real server and its
//! worker: the workspace lives inside the run's container, every stage's
//! checkpoint is committed there and published to the snapshot repository
//! on the host, and a restart brings the container's workspace back to the
//! snapshot its durable state names, in the retained container or in a
//! fresh one when the old one is gone.
//!
//! The runs take their scope through the sandbox-driver Docker plugin on
//! this machine's daemon, so the tests skip, and say why, when the
//! executable is not found or no daemon answers, unless
//! `FABRO_REQUIRE_SANDBOX_PLUGINS` is set and the plugin is missing. The
//! server, the detached run and the crash come from `petri.rs`.

#![expect(
    clippy::disallowed_methods,
    reason = "these scenarios locate the plugin through the process environment and drive the Docker daemon with its CLI"
)]
#![expect(clippy::print_stderr, reason = "a skipped test says why on its stderr")]

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use fabro_petri::checkpoint::CheckpointKey;
use fabro_static::EnvVars;
use fabro_test::{expect_reqwest_json, test_context};
use serde_json::json;

use super::petri::{
    REQUIRE_ENV, RunningServer, crash, run_detached_in, wait_for_status, wait_for_success,
    wait_for_worker, write_petri_workflow,
};
use crate::support::TEST_DEV_TOKEN;

const DOCKER_PLUGIN: &str = "sandbox-driver-docker";
/// The server-side environment the runs select.
const ENVIRONMENT: &str = "docker";

/// The Docker plugin as Petri's lookup finds it, with a daemon that
/// answers. `None`, after saying so, when the test should skip; a panic
/// when the environment forbids a skip and the plugin is missing.
fn docker_plugin() -> Option<PathBuf> {
    let found = env::var_os(EnvVars::PETRI_SANDBOX_DOCKER_PLUGIN)
        .map(PathBuf::from)
        .or_else(|| {
            env::split_paths(&env::var_os(EnvVars::PATH)?)
                .map(|dir| dir.join(DOCKER_PLUGIN))
                .find(|candidate| candidate.is_file())
        });
    let Some(found) = found else {
        assert!(
            env::var_os(REQUIRE_ENV).is_none(),
            "{REQUIRE_ENV} is set, but {DOCKER_PLUGIN} is not on PATH and {} is unset",
            EnvVars::PETRI_SANDBOX_DOCKER_PLUGIN
        );
        eprintln!(
            "skipping: {DOCKER_PLUGIN} is not on PATH and {} is unset",
            EnvVars::PETRI_SANDBOX_DOCKER_PLUGIN
        );
        return None;
    };
    let daemon = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !daemon {
        eprintln!("skipping: no Docker daemon answers");
        return None;
    }
    Some(found)
}

/// A server with a Docker environment beside the default local one.
async fn docker_server() -> RunningServer {
    let server = RunningServer::start().await;
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
        .filter(|line| line.contains("sandbox workspace brought to its durable snapshot"))
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
    if docker_plugin().is_none() {
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
    if docker_plugin().is_none() {
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
    if docker_plugin().is_none() {
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
