#![expect(
    clippy::disallowed_methods,
    reason = "These git integration tests intentionally exercise the real git CLI to validate repository helper behavior."
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use fabro_agent::Sandbox;
use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
use fabro_types::{RunEvent, WorkflowSettings, fixtures};
use fabro_workflow::event::Emitter;
use fabro_workflow::git::{
    MetadataStore, add_worktree, branch_needs_push, create_branch, push_branch, push_ref,
    remove_worktree, replace_worktree,
};
use fabro_workflow::handler::HandlerRegistry;
use fabro_workflow::handler::exit::ExitHandler;
use fabro_workflow::handler::start::StartHandler;
use fabro_workflow::run_options::{GitCheckpointOptions, RunOptions};
use fabro_workflow::test_support::run_graph;

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("failed to create repo dir");
    let init = Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init should run");
    assert_success(&init, "git init");
    let commit = Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@test",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(dir)
        .output()
        .expect("git commit --allow-empty should run");
    assert_success(&commit, "git commit --allow-empty");
}

fn init_bare_remote(dir: &Path) {
    std::fs::create_dir_all(
        dir.parent()
            .expect("bare remote path should have a parent directory"),
    )
    .expect("failed to create bare remote parent dir");
    let init = Command::new("git")
        .args(["init", "--bare"])
        .arg(dir)
        .output()
        .expect("git init --bare should run");
    assert_success(&init, "git init --bare");
}

fn add_origin(repo_dir: &Path, remote_dir: &Path) {
    let output = Command::new("git")
        .args(["remote", "add", "origin"])
        .arg(remote_dir)
        .current_dir(repo_dir)
        .output()
        .expect("git remote add origin should run");
    assert_success(&output, "git remote add origin");
}

fn rename_branch(repo_dir: &Path, branch: &str) {
    let output = Command::new("git")
        .args(["branch", "-M", branch])
        .current_dir(repo_dir)
        .output()
        .expect("git branch -M should run");
    assert_success(&output, "git branch -M");
}

fn empty_commit(repo_dir: &Path, message: &str) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@test",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ])
        .current_dir(repo_dir)
        .output()
        .expect("git commit --allow-empty should run");
    assert_success(&output, "git commit --allow-empty");
}

fn list_branch(repo_dir: &Path, branch: &str) -> String {
    let output = Command::new("git")
        .args(["branch", "--list", branch])
        .current_dir(repo_dir)
        .output()
        .expect("git branch --list should run");
    assert_success(&output, "git branch --list");
    String::from_utf8(output.stdout).expect("git branch --list output should be UTF-8")
}

fn local_env(repo: &Path) -> Arc<dyn Sandbox> {
    Arc::new(fabro_agent::LocalSandbox::new(repo.to_path_buf()))
}

fn simple_graph() -> Graph {
    let mut g = Graph::new("git_checkpoint");
    g.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Create git checkpoints".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    g.nodes.insert("exit".to_string(), exit);

    g
}

fn make_registry() -> HandlerRegistry {
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry
}

fn test_run_options(run_dir: &Path) -> RunOptions {
    RunOptions {
        run_dir:          run_dir.to_path_buf(),
        cancel_token:     None,
        run_id:           fixtures::RUN_2,
        settings:         WorkflowSettings::default(),
        git:              None,
        host_repo_path:   None,
        labels:           HashMap::new(),
        github_app:       None,
        base_branch:      None,
        display_base_sha: None,
        workflow_slug:    None,
    }
}

#[test]
fn replace_worktree_replaces_stale() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    create_branch(dir.path(), "stale-branch").unwrap();

    let wt_path = dir.path().join("stale-wt");
    add_worktree(dir.path(), &wt_path, "stale-branch").unwrap();
    assert!(wt_path.join(".git").exists());

    replace_worktree(dir.path(), &wt_path, "stale-branch").unwrap();
    assert!(wt_path.join(".git").exists());

    remove_worktree(dir.path(), &wt_path).unwrap();
}

#[test]
fn push_ref_to_bare_remote() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    let remote_dir = dir.path().join("remote.git");

    init_bare_remote(&remote_dir);
    init_repo(&repo_dir);
    add_origin(&repo_dir, &remote_dir);

    create_branch(&repo_dir, "test-push").unwrap();
    let url = format!("file://{}", remote_dir.display());
    push_ref(&repo_dir, &url, "refs/heads/test-push").unwrap();

    assert!(list_branch(&remote_dir, "test-push").contains("test-push"));
}

#[test]
fn push_branch_to_remote() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    let remote_dir = dir.path().join("remote.git");

    init_bare_remote(&remote_dir);
    init_repo(&repo_dir);
    add_origin(&repo_dir, &remote_dir);
    rename_branch(&repo_dir, "main");

    push_branch(&repo_dir, "origin", "main").unwrap();

    assert!(list_branch(&remote_dir, "main").contains("main"));
}

#[test]
fn branch_needs_push_when_ahead() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    let remote_dir = dir.path().join("remote.git");

    init_bare_remote(&remote_dir);
    init_repo(&repo_dir);
    add_origin(&repo_dir, &remote_dir);
    rename_branch(&repo_dir, "main");

    push_branch(&repo_dir, "origin", "main").unwrap();
    empty_commit(&repo_dir, "second");

    assert!(branch_needs_push(&repo_dir, "origin", "main"));
}

#[test]
fn branch_needs_push_when_in_sync() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    let remote_dir = dir.path().join("remote.git");

    init_bare_remote(&remote_dir);
    init_repo(&repo_dir);
    add_origin(&repo_dir, &remote_dir);
    rename_branch(&repo_dir, "main");

    push_branch(&repo_dir, "origin", "main").unwrap();

    assert!(!branch_needs_push(&repo_dir, "origin", "main"));
}

#[tokio::test]
async fn git_checkpoint_skips_start_node() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path();
    init_repo(repo);

    let base_sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let run_tmp = tempfile::tempdir().unwrap();
    let mut g = simple_graph();
    g.nodes.insert("work".to_string(), Node::new("work"));
    g.edges.clear();
    g.edges.push(Edge::new("start", "work"));
    g.edges.push(Edge::new("work", "exit"));

    let events = Arc::new(std::sync::Mutex::new(Vec::<RunEvent>::new()));
    let events_clone = Arc::clone(&events);
    let emitter = Emitter::new(fixtures::RUN_2);
    emitter.on_event(move |event| {
        events_clone.lock().unwrap().push(event.clone());
    });

    let mut run_options = test_run_options(run_tmp.path());
    run_options.git = Some(GitCheckpointOptions {
        base_sha:    Some(base_sha),
        run_branch:  None,
        meta_branch: Some(MetadataStore::branch_name(&fixtures::RUN_2.to_string())),
    });
    run_options.host_repo_path = Some(PathBuf::from(repo));

    Box::pin(run_graph(
        make_registry(),
        Arc::new(emitter),
        local_env(repo),
        &g,
        &run_options,
    ))
    .await
    .unwrap();

    let collected = events.lock().unwrap();
    let checkpoint_node_ids: Vec<&str> = collected
        .iter()
        .filter(|event| {
            event.event_name() == "checkpoint.completed"
                && event.properties().is_ok_and(|properties| {
                    properties
                        .get("git_commit_sha")
                        .and_then(|value| value.as_str())
                        .is_some()
                })
        })
        .filter_map(|event| event.node_id.as_deref())
        .collect();
    assert!(!checkpoint_node_ids.contains(&"start"));
    assert!(checkpoint_node_ids.contains(&"work"));
}
