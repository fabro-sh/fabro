//! Fork, rewind, retry and the timeline over Petri runs, through a real
//! server and its worker subprocess (the integration plan's F5.1).
//!
//! The harness is `petri.rs`'s: a foreground server on disk storage, a run
//! created and started with `fabro run --detach`, executed by the worker
//! the server launches over the HTTP run store. Every checkpoint of a run
//! is a commit on its run branch and a `checkpoint` platform record at its
//! Petri position; the timeline lists them, and a fork seeds a new run from
//! the source's records up to one of them, whose worker restores the
//! checkpoint's files into a fresh workspace and continues from there.

#![expect(
    clippy::disallowed_methods,
    reason = "these scenarios start a real server subprocess and read its workspaces on disk"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fabro_test::test_context;

use super::petri::{
    RunningServer, host_plugin, run_detached, run_json, wait_for_status, wait_for_success,
    write_petri_workflow,
};

/// Three command stages that build on each other's files: `one` writes a
/// file, `two` another, `three` checks both and writes its own.
fn three_stage_dot() -> String {
    "digraph Stages {\n  graph [goal=\"Three stages\", default_max_retries=0]\n  start \
     [shape=Mdiamond]\n  exit [shape=Msquare]\n  one [shape=parallelogram, script=\"echo one > \
     one.txt\"]\n  two [shape=parallelogram, script=\"echo two > two.txt\"]\n  three \
     [shape=parallelogram, script=\"test \\\"$(cat one.txt)\\\" = one && test \\\"$(cat \
     two.txt)\\\" = two && echo three > three.txt\"]\n  start -> one -> two -> three -> exit\n}\n"
        .to_string()
}

/// Three stages whose middle one fails the first time it runs and passes
/// the next: the marker outside the workspace remembers the first run. Its
/// failure ends the run (`on_failure=exit`) rather than routing on.
fn flaky_dot(marker: &Path) -> String {
    format!(
        "digraph Flaky {{\n  graph [goal=\"A transient failure\", default_max_retries=0]\n  \
         start [shape=Mdiamond]\n  exit [shape=Msquare]\n  one [shape=parallelogram, \
         script=\"echo one > one.txt\"]\n  flaky [shape=parallelogram, max_retries=0, on_failure=exit, \
         script=\"if [ ! -f {marker} ]; then touch {marker}; exit 1; fi; echo flaky > \
         flaky.txt\"]\n  three [shape=parallelogram, script=\"test \\\"$(cat one.txt)\\\" = one \
         && test \\\"$(cat flaky.txt)\\\" = flaky && echo three > three.txt\"]\n  start -> one \
         -> flaky -> three -> exit\n}}\n",
        marker = marker.display()
    )
}

/// A parallel node with two command branches and a join.
fn parallel_dot() -> String {
    "digraph Branches {\n  graph [goal=\"Two branches\", default_max_retries=0]\n  start \
     [shape=Mdiamond]\n  exit [shape=Msquare]\n  fan [shape=component]\n  a \
     [shape=parallelogram, script=\"echo a > a.txt\"]\n  b [shape=parallelogram, script=\"echo \
     b > b.txt\"]\n  join [shape=tripleoctagon]\n  done [shape=parallelogram, script=\"echo done \
     > done.txt\"]\n  start -> fan\n  fan -> a\n  fan -> b\n  a -> join\n  b -> join\n  join -> \
     done -> exit\n}\n"
        .to_string()
}

/// Run a CLI command against the server; the caller judges the exit.
fn cli(context: &fabro_test::TestContext, server: &RunningServer, args: &[&str]) -> Output {
    let target = server.target();
    context
        .command()
        .args(args)
        .args(["--server", &target])
        .output()
        .expect("the CLI command executes")
}

/// Run a CLI command that must succeed, and parse its stdout as JSON.
fn cli_json(
    context: &fabro_test::TestContext,
    server: &RunningServer,
    args: &[&str],
) -> serde_json::Value {
    let output = cli(context, server, args);
    assert!(
        output.status.success(),
        "`fabro {}` failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "`fabro {}` printed no JSON: {err}\nstdout:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

async fn timeline(server: &RunningServer, run_id: &str) -> serde_json::Value {
    run_json(server, &format!("runs/{run_id}/timeline")).await
}

/// The timeline's entries as `(node, execution, firing, attempt, sha)`.
fn entries(timeline: &serde_json::Value) -> Vec<(String, u64, u64, u64, String)> {
    timeline["entries"]
        .as_array()
        .expect("the timeline has entries")
        .iter()
        .map(|entry| {
            (
                entry["node_name"].as_str().unwrap_or_default().to_string(),
                entry["execution"].as_u64().expect("an execution"),
                entry["firing"].as_u64().expect("a firing"),
                entry["attempt"].as_u64().expect("an attempt"),
                entry["run_commit_sha"]
                    .as_str()
                    .expect("a checkpoint commit")
                    .to_string(),
            )
        })
        .collect()
}

fn nodes(timeline: &serde_json::Value) -> Vec<String> {
    entries(timeline)
        .into_iter()
        .map(|(node, ..)| node)
        .collect()
}

/// The one host workspace of the run.
fn workspace(server: &RunningServer, run_id: &str) -> PathBuf {
    let scopes = server.petri_run_dir(run_id).join("scopes");
    let mut workspaces: Vec<PathBuf> = std::fs::read_dir(&scopes)
        .expect("the scopes directory lists")
        .map(|entry| entry.expect("an entry reads").path().join("work"))
        .collect();
    assert_eq!(workspaces.len(), 1, "one workspace: {workspaces:?}");
    workspaces.remove(0)
}

/// The subjects of the commits on the workspace's branch, oldest first.
fn commit_subjects(workspace: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["log", "--reverse", "--format=%s"])
        .current_dir(workspace)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git log failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn current_branch(workspace: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(workspace)
        .output()
        .expect("git runs");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn read(workspace: &Path, name: &str) -> String {
    std::fs::read_to_string(workspace.join(name))
        .unwrap_or_else(|err| panic!("{name} in {}: {err}", workspace.display()))
}

/// A three-stage run forked at its first stage's checkpoint continues with
/// the other two in a new run: the new run keeps the source's records up to
/// `one`, its workspace holds `one`'s file restored from the checkpoint,
/// and `two` and `three` run on it and commit on the new run branch. The
/// new run says where it came from.
#[tokio::test(flavor = "multi_thread")]
async fn a_fork_at_the_first_stage_continues_with_the_rest_on_its_files() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let bundle = write_petri_workflow(&context, &three_stage_dot());
    let source = run_detached(&context, &server, &bundle);
    wait_for_success(&server, &source).await;
    let source_timeline = timeline(&server, &source).await;
    assert_eq!(nodes(&source_timeline), [
        "start", "one", "two", "three", "exit"
    ]);
    let source_entries = entries(&source_timeline);
    let (_, one_execution, one_firing, _, one_sha) = source_entries[1].clone();

    let forked = cli_json(&context, &server, &["fork", &source, "one", "--json"]);
    assert_eq!(forked["source_run_id"], source);
    assert_eq!(forked["target"], "@2");
    assert_eq!(forked["execution"].as_u64(), Some(one_execution));
    assert_eq!(forked["firing"].as_u64(), Some(one_firing));
    assert_eq!(forked["checkpoint_sha"], one_sha);
    assert_eq!(forked["rerun_last"], false);
    let fork = forked["new_run_id"]
        .as_str()
        .expect("the new run id")
        .to_string();
    assert_ne!(fork, source);
    wait_for_success(&server, &fork).await;

    // The fork's timeline: the source's checkpoints up to `one`, then its own.
    let fork_timeline = timeline(&server, &fork).await;
    assert_eq!(nodes(&fork_timeline), [
        "start", "one", "two", "three", "exit"
    ]);
    let fork_entries = entries(&fork_timeline);
    assert_eq!(fork_entries[..2], source_entries[..2]);
    assert_ne!(fork_entries[2].4, source_entries[2].4);
    assert_eq!(fork_timeline["forked_from"]["source_run_id"], source);
    assert_eq!(
        fork_timeline["forked_from"]["execution"].as_u64(),
        Some(one_execution)
    );
    assert_eq!(
        fork_timeline["forked_from"]["firing"].as_u64(),
        Some(one_firing)
    );
    assert_eq!(fork_timeline["forked_from"]["rerun_last"], false);
    assert!(source_timeline["forked_from"].is_null());

    // The fork's workspace: `one.txt` restored from the checkpoint, the
    // rest made by the fork's own stages, on the fork's run branch after the
    // source's commits.
    let fork_workspace = workspace(&server, &fork);
    assert_eq!(read(&fork_workspace, "one.txt"), "one\n");
    assert_eq!(read(&fork_workspace, "two.txt"), "two\n");
    assert_eq!(read(&fork_workspace, "three.txt"), "three\n");
    assert_eq!(current_branch(&fork_workspace), format!("fabro/run/{fork}"));
    assert_eq!(commit_subjects(&fork_workspace), [
        format!("fabro({source}): start (success)"),
        format!("fabro({source}): one (success)"),
        format!("fabro({fork}): two (success)"),
        format!("fabro({fork}): three (success)"),
        format!("fabro({fork}): exit (success)"),
    ]);

    // The projection names the origin on both sides.
    let state = run_json(&server, &format!("runs/{fork}/state")).await;
    assert_eq!(state["forked_from"]["source_run_id"], source);
    assert_eq!(state["spec"]["fork_source_ref"]["source_run_id"], source);
    assert_eq!(state["spec"]["fork_source_ref"]["checkpoint_sha"], one_sha);
    assert!(state["retried_from"].is_null());
    let summary = run_json(&server, &format!("runs/{source}")).await;
    assert!(summary["superseded_by"].is_null());
    assert_eq!(summary["lifecycle"]["archived"], false);
    server.shutdown();
}

/// A retry of a run whose last stage failed reruns that stage: the failure
/// was transient, so the retry passes it on the files of the stage before
/// and finishes the run.
#[tokio::test(flavor = "multi_thread")]
async fn a_retry_reruns_the_failed_stage_and_succeeds_when_the_failure_was_transient() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let marker = context.temp_dir.join("flaky.marker");
    let bundle = write_petri_workflow(&context, &flaky_dot(&marker));
    let source = run_detached(&context, &server, &bundle);
    let status = wait_for_status(&server, &source, &["succeeded", "failed"]).await;
    assert_eq!(status, "failed", "the first run fails on `flaky`");
    assert!(marker.exists(), "the first run left its marker");
    assert_eq!(nodes(&timeline(&server, &source).await), [
        "start", "one", "flaky"
    ]);

    let retried = cli_json(&context, &server, &["retry", &source, "--json"]);
    assert_eq!(retried["source_run_id"], source);
    let retry = retried["run_id"]
        .as_str()
        .expect("the new run id")
        .to_string();
    wait_for_success(&server, &retry).await;

    let retry_timeline = timeline(&server, &retry).await;
    assert_eq!(nodes(&retry_timeline), [
        "start", "one", "flaky", "three", "exit"
    ]);
    assert_eq!(retry_timeline["forked_from"]["source_run_id"], source);
    assert_eq!(retry_timeline["forked_from"]["rerun_last"], true);
    let retry_workspace = workspace(&server, &retry);
    assert_eq!(read(&retry_workspace, "one.txt"), "one\n");
    assert_eq!(read(&retry_workspace, "flaky.txt"), "flaky\n");
    assert_eq!(read(&retry_workspace, "three.txt"), "three\n");
    assert_eq!(commit_subjects(&retry_workspace), [
        format!("fabro({source}): start (success)"),
        format!("fabro({source}): one (success)"),
        format!("fabro({retry}): flaky (success)"),
        format!("fabro({retry}): three (success)"),
        format!("fabro({retry}): exit (success)"),
    ]);
    let state = run_json(&server, &format!("runs/{retry}/state")).await;
    assert_eq!(state["retried_from"], source);
    assert_eq!(state["spec"]["fork_source_ref"]["source_run_id"], source);

    // The source is untouched, and a second retry is refused for the
    // running or archived cases alone: it is terminal, so it may retry again.
    let summary = run_json(&server, &format!("runs/{source}")).await;
    assert_eq!(summary["lifecycle"]["status"]["kind"], "failed");
    assert!(summary["superseded_by"].is_null());
    server.shutdown();
}

/// A rewind forks the run and supersedes it: the source is archived and
/// names the run that replaced it.
#[tokio::test(flavor = "multi_thread")]
async fn a_rewind_supersedes_its_source() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let bundle = write_petri_workflow(&context, &three_stage_dot());
    let source = run_detached(&context, &server, &bundle);
    wait_for_success(&server, &source).await;

    // Without a target the command lists the timeline instead.
    let listed = cli_json(&context, &server, &["rewind", &source, "--json"]);
    assert_eq!(
        listed["entries"]
            .as_array()
            .map(Vec::len)
            .expect("a timeline"),
        5
    );

    let rewound = cli_json(&context, &server, &["rewind", &source, "@3", "--json"]);
    assert_eq!(rewound["source_run_id"], source);
    assert_eq!(rewound["target"], "@3");
    assert_eq!(rewound["archived"], true);
    assert!(rewound["archive_error"].is_null());
    assert_eq!(rewound["status"], 200);
    let replacement = rewound["new_run_id"]
        .as_str()
        .expect("the new run id")
        .to_string();

    let summary = run_json(&server, &format!("runs/{source}")).await;
    assert_eq!(summary["superseded_by"], replacement);
    assert_eq!(summary["lifecycle"]["archived"], true);
    let state = run_json(&server, &format!("runs/{source}/state")).await;
    assert_eq!(state["superseded_by"], replacement);

    wait_for_success(&server, &replacement).await;
    assert_eq!(nodes(&timeline(&server, &replacement).await), [
        "start", "one", "two", "three", "exit"
    ]);
    let replacement_workspace = workspace(&server, &replacement);
    assert_eq!(read(&replacement_workspace, "two.txt"), "two\n");
    assert_eq!(commit_subjects(&replacement_workspace), [
        format!("fabro({source}): start (success)"),
        format!("fabro({source}): one (success)"),
        format!("fabro({source}): two (success)"),
        format!("fabro({replacement}): three (success)"),
        format!("fabro({replacement}): exit (success)"),
    ]);

    // An archived run is neither forked nor rewound again.
    let refused = cli(&context, &server, &["fork", &source, "@2"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("archived"),
        "stderr:\n{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    server.shutdown();
}

/// The timeline lists every checkpoint the run recorded, in order, with
/// its position and commit, as the CLI prints it and as the API serves it.
#[tokio::test(flavor = "multi_thread")]
async fn the_timeline_lists_every_checkpoint_with_its_commit() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let bundle = write_petri_workflow(&context, &three_stage_dot());
    let run_id = run_detached(&context, &server, &bundle);
    wait_for_success(&server, &run_id).await;

    let recorded = server.checkpoints(&run_id).await;
    let listed = cli_json(&context, &server, &["timeline", &run_id, "--json"]);
    let listed_entries: Vec<(u64, u64, u64, String)> = listed["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| {
            (
                entry["execution"].as_u64().expect("execution"),
                entry["firing"].as_u64().expect("firing"),
                entry["attempt"].as_u64().expect("attempt"),
                entry["run_commit_sha"].as_str().expect("sha").to_string(),
            )
        })
        .collect();
    let recorded_entries: Vec<(u64, u64, u64, String)> = recorded
        .iter()
        .map(|(key, sha)| {
            (
                key.execution,
                key.firing,
                u64::from(key.attempt),
                sha.clone(),
            )
        })
        .collect();
    assert_eq!(listed_entries, recorded_entries);
    assert_eq!(listed_entries.len(), 5);
    let ordinals: Vec<u64> = listed["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["ordinal"].as_u64().expect("ordinal"))
        .collect();
    assert_eq!(ordinals, [1, 2, 3, 4, 5]);
    let stages: Vec<&str> = listed["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["stage"].as_str().unwrap_or("-"))
        .collect();
    assert_eq!(stages, ["start@1", "one@1", "two@1", "three@1", "exit@1"]);
    assert!(listed["forked_from"].is_null());

    let table = cli(&context, &server, &["timeline", &run_id]);
    assert!(table.status.success());
    let stderr = String::from_utf8_lossy(&table.stderr);
    for (ordinal, node) in ["start", "one", "two", "three", "exit"].iter().enumerate() {
        assert!(
            stderr.contains(&format!("@{}", ordinal + 1)) && stderr.contains(node),
            "the table names @{} {node}:\n{stderr}",
            ordinal + 1
        );
    }
    let api = timeline(&server, &run_id).await;
    assert_eq!(nodes(&api), ["start", "one", "two", "three", "exit"]);
    server.shutdown();
}

/// A checkpoint inside a parallel branch is not a fork position: Petri
/// refuses it, and the refusal says why. The join, in the root, is.
#[tokio::test(flavor = "multi_thread")]
async fn a_fork_inside_a_parallel_branch_is_refused() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let bundle = write_petri_workflow(&context, &parallel_dot());
    let source = run_detached(&context, &server, &bundle);
    wait_for_success(&server, &source).await;

    let listed = timeline(&server, &source).await;
    let all = entries(&listed);
    let branch = all
        .iter()
        .zip(1_u64..)
        .find(|((node, execution, ..), _)| *execution != 0 && (node == "a" || node == "b"))
        .map(|(_, ordinal)| ordinal)
        .expect("a branch stage has a checkpoint in a child execution");
    let refused = cli(&context, &server, &["fork", &source, &format!("@{branch}")]);
    assert!(
        !refused.status.success(),
        "a fork inside a branch is refused"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("inside a child invocation cannot be forked"),
        "the refusal names the branch:\n{stderr}"
    );
    // Nothing was started for it.
    let runs = run_json(&server, "runs").await;
    let ids: Vec<&str> = runs["data"]
        .as_array()
        .or_else(|| runs.as_array())
        .expect("a run list")
        .iter()
        .filter_map(|run| run["id"].as_str())
        .collect();
    assert!(ids.iter().all(|id| *id == source), "runs: {ids:?}");

    // A fork at the stage after the join continues from the root.
    let done = all
        .iter()
        .zip(1_u64..)
        .find(|((node, ..), _)| node == "done")
        .map(|(_, ordinal)| ordinal)
        .expect("`done` has a checkpoint");
    let forked = cli_json(&context, &server, &[
        "fork",
        &source,
        &format!("@{done}"),
        "--json",
    ]);
    let fork = forked["new_run_id"]
        .as_str()
        .expect("the new run id")
        .to_string();
    wait_for_success(&server, &fork).await;
    let fork_workspace = workspace(&server, &fork);
    assert_eq!(read(&fork_workspace, "done.txt"), "done\n");
    server.shutdown();
}
