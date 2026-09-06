#![expect(
    clippy::disallowed_methods,
    reason = "These git integration tests intentionally exercise the real git CLI to validate repository helper behavior."
)]

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use fabro_agent::Sandbox;
use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
use fabro_types::{RunEvent, WorkflowSettings, fixtures};
use fabro_workflow::event::Emitter;
use fabro_workflow::git;
use fabro_workflow::handler::HandlerRegistry;
use fabro_workflow::handler::exit::ExitHandler;
use fabro_workflow::handler::start::StartHandler;
use fabro_workflow::run_options::{GitCheckpointOptions, RunOptions};
use fabro_workflow::test_support::run_graph;
use tokio_util::sync::CancellationToken;

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
        cancel_token:     CancellationToken::new(),
        run_id:           fixtures::RUN_2,
        settings:         WorkflowSettings::default(),
        git:              None,
        pre_run_git:      None,
        fork_source_ref:  None,
        labels:           HashMap::new(),
        github_app:       None,
        base_branch:      None,
        display_base_sha: None,
        workflow_slug:    None,
    }
}

#[test]
fn push_ref_to_bare_remote() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    let remote_dir = dir.path().join("remote.git");

    init_bare_remote(&remote_dir);
    init_repo(&repo_dir);
    add_origin(&repo_dir, &remote_dir);

    rename_branch(&repo_dir, "test-push");
    let url = format!("file://{}", remote_dir.display());
    git::push_ref(&repo_dir, &url, "refs/heads/test-push").unwrap();

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

    git::push_branch(&repo_dir, "origin", "main").unwrap();

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

    git::push_branch(&repo_dir, "origin", "main").unwrap();
    empty_commit(&repo_dir, "second");

    assert!(git::branch_needs_push(&repo_dir, "origin", "main"));
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

    git::push_branch(&repo_dir, "origin", "main").unwrap();

    assert!(!git::branch_needs_push(&repo_dir, "origin", "main"));
}

#[test]
fn remote_branch_sha_ignores_a_locally_rewritten_tracking_ref() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    let remote_dir = dir.path().join("remote.git");

    init_bare_remote(&remote_dir);
    init_repo(&repo_dir);
    add_origin(&repo_dir, &remote_dir);
    rename_branch(&repo_dir, "main");
    git::push_branch(&repo_dir, "origin", "main").unwrap();
    let remote_sha = git::head_sha(&repo_dir).unwrap();

    empty_commit(&repo_dir, "local-only");
    let local_sha = git::head_sha(&repo_dir).unwrap();
    let update_tracking = Command::new("git")
        .args(["update-ref", "refs/remotes/origin/main", "HEAD"])
        .current_dir(&repo_dir)
        .output()
        .expect("git update-ref should run");
    assert_success(&update_tracking, "git update-ref");
    assert!(!git::branch_needs_push(&repo_dir, "origin", "main"));

    assert_eq!(
        git::remote_branch_sha_noninteractive(&repo_dir, "origin", "main").unwrap(),
        Some(remote_sha.clone()),
    );
    assert_ne!(local_sha, remote_sha);
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
        base_sha:   Some(base_sha),
        run_branch: None,
    });

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

/// Sandbox double for remote-style runs: commands and files operate on a real
/// local checkout, but the workflow engine's run directory is reported as
/// inaccessible (as it is for Docker/Daytona) and the sandbox exposes a
/// runtime directory outside the checkout.
struct RemoteRuntimeSandbox {
    inner:             fabro_agent::LocalSandbox,
    hidden_path:       String,
    runtime_directory: String,
}

#[async_trait::async_trait]
impl Sandbox for RemoteRuntimeSandbox {
    async fn read_file_bytes(&self, path: &str) -> fabro_sandbox::Result<Vec<u8>> {
        self.inner.read_file_bytes(path).await
    }

    async fn write_file(&self, path: &str, content: &str) -> fabro_sandbox::Result<()> {
        self.inner.write_file(path, content).await
    }

    async fn delete_file(&self, path: &str) -> fabro_sandbox::Result<()> {
        self.inner.delete_file(path).await
    }

    async fn file_exists(&self, path: &str) -> fabro_sandbox::Result<bool> {
        if path == self.hidden_path {
            return Ok(false);
        }
        self.inner.file_exists(path).await
    }

    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> fabro_sandbox::Result<Vec<fabro_agent::DirEntry>> {
        self.inner.list_directory(path, depth).await
    }

    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> fabro_sandbox::Result<fabro_agent::ExecResult> {
        self.inner
            .exec_command(command, timeout_ms, working_dir, env_vars, cancel_token)
            .await
    }

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &fabro_sandbox::GrepOptions,
    ) -> fabro_sandbox::Result<Vec<String>> {
        self.inner.grep(pattern, path, options).await
    }

    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> fabro_sandbox::Result<()> {
        self.inner
            .download_file_to_local(remote_path, local_path)
            .await
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> fabro_sandbox::Result<()> {
        self.inner
            .upload_file_from_local(local_path, remote_path)
            .await
    }

    async fn initialize(&self) -> fabro_sandbox::Result<()> {
        Ok(())
    }

    async fn cleanup(&self) -> fabro_sandbox::Result<()> {
        Ok(())
    }

    fn working_directory(&self) -> &str {
        self.inner.working_directory()
    }

    fn runtime_directory(&self) -> Option<&str> {
        Some(&self.runtime_directory)
    }

    fn platform(&self) -> &str {
        self.inner.platform()
    }

    fn os_version(&self) -> String {
        self.inner.os_version()
    }
}

fn git_status_porcelain(repo_dir: &Path) -> String {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_dir)
        .output()
        .expect("git status --porcelain should run");
    assert_success(&output, "git status --porcelain");
    String::from_utf8(output.stdout).expect("git status output should be UTF-8")
}

fn git_committed_files(repo_dir: &Path, sha: &str) -> String {
    let output = Command::new("git")
        .args(["show", "--name-only", "--format=", sha])
        .current_dir(repo_dir)
        .output()
        .expect("git show --name-only should run");
    assert_success(&output, "git show --name-only");
    String::from_utf8(output.stdout).expect("git show output should be UTF-8")
}

/// Remote-style prompt demotion must materialize blobs in the sandbox runtime
/// directory, outside the checkout, so a real checkpoint commit can never pick
/// them up, and re-resolution must recreate a deleted materialized file from
/// the durable blob store. Regression test for issue #798.
#[tokio::test]
async fn remote_prompt_demotion_stays_outside_checkout_and_survives_checkpoint() {
    use std::time::Duration;

    use fabro_store::test_support as store_test_support;
    use fabro_types::settings::run::RunCheckpointSettings;
    use fabro_workflow::context::Context;
    use fabro_workflow::git::GitAuthor;
    use fabro_workflow::runtime_store::RunStoreHandle;
    use fabro_workflow::{artifact, sandbox_git};
    use object_store::memory::InMemory;

    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    init_repo(&repo_dir);
    let runtime_dir = dir.path().join("fabro").join("runtime");
    let run_dir = dir.path().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();

    let sandbox = RemoteRuntimeSandbox {
        inner:             fabro_agent::LocalSandbox::new(repo_dir.clone()),
        hidden_path:       run_dir.to_string_lossy().to_string(),
        runtime_directory: runtime_dir.to_string_lossy().to_string(),
    };

    let store = store_test_support::test_database(
        Arc::new(InMemory::new()),
        "runs/",
        Duration::from_millis(1),
        None,
    );
    let run_store: RunStoreHandle = store.create_run(&fixtures::RUN_2).await.unwrap().into();

    let oversized = serde_json::json!("x".repeat(64 * 1024));
    let oversized_bytes = serde_json::to_vec(&oversized).unwrap();
    let mut values = HashMap::from([("dataset".to_string(), oversized.clone())]);
    artifact::demote_large_values_for_prompt(
        &mut values,
        &mut HashMap::new(),
        &run_store,
        &sandbox,
        &run_dir,
    )
    .await;

    let marker = values["dataset"]
        .get("fabroLargeValue")
        .expect("oversized value should demote to a marker");
    let blob_path = marker["path"].as_str().unwrap().to_string();
    assert!(
        blob_path.starts_with(&runtime_dir.to_string_lossy().to_string()),
        "materialized blob {blob_path} should live under the sandbox runtime directory"
    );
    assert!(
        !blob_path.starts_with(&repo_dir.to_string_lossy().to_string()),
        "materialized blob {blob_path} must not live inside the checkout"
    );

    // The agent-facing path is readable through the sandbox.
    let contents = sandbox.read_file_bytes(&blob_path).await.unwrap();
    assert_eq!(contents, oversized_bytes);

    // Materialization leaves the checkout clean, and a real checkpoint commit
    // stages no runtime blob file.
    assert_eq!(git_status_porcelain(&repo_dir), "");
    let sha = sandbox_git::git_checkpoint(
        &sandbox,
        &fixtures::RUN_2.to_string(),
        "work",
        "succeeded",
        1,
        &RunCheckpointSettings::default(),
        &GitAuthor::default(),
    )
    .await
    .expect("checkpoint commit should succeed");
    assert_eq!(git_committed_files(&repo_dir, &sha).trim(), "");
    assert_eq!(git_status_porcelain(&repo_dir), "");

    // Removing the materialized file and resolving the value again recreates
    // it from the durable blob store.
    std::fs::remove_file(&blob_path).unwrap();
    let blob_hash = fabro_types::BlobHash::new(&oversized_bytes);
    let context = Context::new();
    context.set(
        "report",
        serde_json::json!(fabro_types::format_blob_ref(&blob_hash)),
    );
    let resolved = artifact::resolved_context_snapshot(&context, &run_store, &sandbox, &run_dir)
        .await
        .unwrap();
    assert_eq!(
        resolved["report"],
        serde_json::json!(format!("file://{blob_path}"))
    );
    assert_eq!(
        sandbox.read_file_bytes(&blob_path).await.unwrap(),
        oversized_bytes
    );
}
