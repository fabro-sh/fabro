//! Integration tests for `DaytonaSandbox`.
//!
//! These tests require a `DAYTONA_API_KEY` environment variable and network access.
//! Run with: `cargo test --package arc-workflows -- --ignored daytona`

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use fabro_agent::Sandbox;
use fabro_config::FabroConfig;
use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
use fabro_llm::provider::Provider;
use fabro_sandbox::daytona::{DaytonaConfig, DaytonaSandbox, DaytonaSnapshotConfig};
use fabro_workflows::artifact::sync_artifacts_to_env;
use fabro_workflows::context::Context;
use fabro_workflows::error::FabroError;
use fabro_workflows::event::EventEmitter;
use fabro_workflows::handler::exit::ExitHandler;
use fabro_workflows::handler::start::StartHandler;
use fabro_workflows::handler::{Handler, HandlerRegistry};
use fabro_workflows::outcome::{Outcome, OutcomeExt, StageStatus};
use fabro_workflows::records::Checkpoint;
use fabro_workflows::run_settings::{GitCheckpointSettings, RunSettings};
use fabro_workflows::test_support::WorkflowRunner;

async fn create_env() -> DaytonaSandbox {
    let creds = load_github_app_credentials();
    create_env_with_github_app(Some(creds)).await
}

async fn create_env_with_github_app(
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> DaytonaSandbox {
    dotenvy::dotenv().ok();
    if let Some(home) = dirs::home_dir() {
        dotenvy::from_path(home.join(".fabro/.env")).ok();
    }
    DaytonaSandbox::new(DaytonaConfig::default(), github_app, None, None)
        .await
        .expect("Failed to create Daytona client — is DAYTONA_API_KEY set?")
}

fn load_github_app_credentials() -> fabro_github::GitHubAppCredentials {
    dotenvy::dotenv().ok();

    // Read app_id from ~/.fabro/server.toml
    let home = dirs::home_dir().expect("No home directory");
    let config_path = home.join(".fabro/server.toml");
    let config_str = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", config_path.display()));

    #[derive(serde::Deserialize)]
    struct Config {
        #[serde(default)]
        git: GitSection,
    }
    #[derive(serde::Deserialize, Default)]
    struct GitSection {
        app_id: Option<String>,
    }

    let config: Config = toml::from_str(&config_str).expect("Failed to parse server.toml");
    let app_id = config
        .git
        .app_id
        .expect("app_id not set in server.toml [git] section");

    let raw = std::env::var("GITHUB_APP_PRIVATE_KEY").expect("GITHUB_APP_PRIVATE_KEY not set");
    let private_key_pem = if raw.starts_with("-----") {
        raw
    } else {
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &raw)
            .expect("GITHUB_APP_PRIVATE_KEY is not valid base64");
        String::from_utf8(bytes).expect("GITHUB_APP_PRIVATE_KEY decoded to invalid UTF-8")
    };
    fabro_github::GitHubAppCredentials {
        app_id,
        private_key_pem,
    }
}

#[tokio::test]
#[ignore]
async fn daytona_exec_command() {
    let creds = load_github_app_credentials();
    let env = create_env_with_github_app(Some(creds)).await;
    env.initialize().await.unwrap();

    let result = env
        .exec_command("echo hello", 30_000, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("hello"));

    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_exec_command_with_pipe() {
    let creds = load_github_app_credentials();
    let env = create_env_with_github_app(Some(creds)).await;
    env.initialize().await.unwrap();

    let result = env
        .exec_command("echo hello world | wc -w", 30_000, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.trim().contains('2'));

    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_exec_command_cancelled() {
    let creds = load_github_app_credentials();
    let env = create_env_with_github_app(Some(creds)).await;
    env.initialize().await.unwrap();

    let token = tokio_util::sync::CancellationToken::new();
    let token_clone = token.clone();

    // Cancel the token shortly after starting
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        token_clone.cancel();
    });

    // Execute a command that would normally take a while
    let result = env
        .exec_command("sleep 10", 30_000, None, None, Some(token))
        .await
        .unwrap();

    assert_eq!(result.exit_code, -1);
    assert!(result.timed_out);
    assert_eq!(result.stderr, "Command cancelled");

    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_exec_command_local_timeout() {
    let creds = load_github_app_credentials();
    let env = create_env_with_github_app(Some(creds)).await;
    env.initialize().await.unwrap();

    // Use a tiny timeout_ms of 100ms, our local timeout is 100 + 2000 = 2100ms.
    // If the server doesn't enforce the timeout properly or drops the connection,
    // our local timeout should catch it. To simulate this without making a bad server,
    // we can't easily force the local timeout to hit before the server timeout
    // without mocking. But if we run `sleep 10` and Daytona does NOT respect the
    // short timeout parameter, the local 2.1s timeout will definitely fire.
    // Let's at least test that a 100ms timeout works and doesn't run for 10s.
    let start = std::time::Instant::now();
    let result = env
        .exec_command("sleep 10", 100, None, None, None)
        .await
        .unwrap();

    let duration = start.elapsed();

    // It should either fail with Daytona's timeout (duration < 2000ms) or our
    // local timeout (duration ~2100ms). Both are valid success conditions for
    // the system as a whole avoiding a stall.
    assert!(
        duration < std::time::Duration::from_millis(3000),
        "Command stalled for longer than the local timeout mechanism"
    );
    assert!(result.exit_code != 0);

    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_file_round_trip() {
    let env = create_env().await;
    env.initialize().await.unwrap();

    let test_path = "test_round_trip.txt";
    let content = "Hello from Daytona integration test!";

    // Write
    env.write_file(test_path, content).await.unwrap();

    // Exists
    assert!(env.file_exists(test_path).await.unwrap());

    // Read
    let read_back = env.read_file(test_path, None, None).await.unwrap();
    assert!(read_back.contains(content));

    // Delete
    env.delete_file(test_path).await.unwrap();
    assert!(!env.file_exists(test_path).await.unwrap());

    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_full_lifecycle() {
    let env = create_env().await;

    // Initialize (creates sandbox + clones repo)
    env.initialize().await.unwrap();

    // Verify platform
    assert_eq!(env.platform(), "linux");

    // Verify working directory is accessible
    let result = env
        .exec_command("pwd", 10_000, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);

    // List directory
    let entries = env.list_directory(".", None).await.unwrap();
    assert!(!entries.is_empty());

    // Cleanup (deletes sandbox)
    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_snapshot_sandbox() {
    use fabro_sandbox::daytona::DaytonaSnapshotConfig;

    dotenvy::dotenv().ok();

    let config = DaytonaConfig {
        auto_stop_interval: Some(60),
        snapshot: Some(DaytonaSnapshotConfig {
            name: "fabro-test-snapshot".to_string(),
            cpu: Some(2),
            memory: Some(4),
            disk: Some(10),
            dockerfile: Some(fabro_sandbox::daytona::DockerfileSource::Inline(
                "FROM ubuntu:22.04\nRUN apt-get update && apt-get install -y ripgrep".to_string(),
            )),
        }),
        ..DaytonaConfig::default()
    };

    let creds = load_github_app_credentials();
    let env = DaytonaSandbox::new(config, Some(creds), None, None)
        .await
        .expect("Failed to create Daytona client — is DAYTONA_API_KEY set?");
    env.initialize().await.unwrap();

    // Verify rg is available (installed by snapshot)
    let result = env
        .exec_command("rg --version", 10_000, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("ripgrep"));

    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_artifact_sync_uploads_and_rewrites_pointer() {
    let env = create_env().await;
    env.initialize().await.unwrap();

    // Create a local artifact file (simulating what offload_large_values produces)
    let dir = tempfile::tempdir().unwrap();
    let artifact_content = "x".repeat(150 * 1024); // 150KB
    let artifact_json = serde_json::json!(artifact_content);
    let artifact_file = dir.path().join("response.plan.json");
    std::fs::write(
        &artifact_file,
        serde_json::to_string(&artifact_json).unwrap(),
    )
    .unwrap();

    // Build updates with a file:// pointer (as offload_large_values would)
    let pointer = format!("file://{}", artifact_file.display());
    let mut updates = HashMap::new();
    updates.insert("response.plan".to_string(), serde_json::json!(pointer));

    // Sync — the local file doesn't exist in the Daytona sandbox, so it should upload
    sync_artifacts_to_env(&mut updates, &env).await.unwrap();

    // Pointer should be rewritten to the Daytona working directory
    let new_pointer = updates["response.plan"].as_str().unwrap();
    let expected_prefix = format!("file://{}/.fabro/artifacts/", env.working_directory());
    assert!(
        new_pointer.starts_with(&expected_prefix),
        "pointer should reference Daytona path, got: {new_pointer}"
    );

    // Verify the file actually exists in the sandbox by reading it back
    let remote_path = new_pointer.strip_prefix("file://").unwrap();
    assert!(
        env.file_exists(remote_path).await.unwrap(),
        "artifact file should exist in Daytona sandbox at {remote_path}"
    );

    let remote_content = env.read_file(remote_path, None, None).await.unwrap();
    assert!(
        remote_content.len() > 100 * 1024,
        "remote artifact should be >100KB, got {} bytes",
        remote_content.len()
    );

    env.cleanup().await.unwrap();
}

// ---------------------------------------------------------------------------
// Full pipeline E2E on Daytona
// ---------------------------------------------------------------------------

/// Handler that produces a >100KB context_update to trigger artifact offloading.
struct LargeOutputHandler;

#[async_trait::async_trait]
impl Handler for LargeOutputHandler {
    async fn execute(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let mut outcome = Outcome::success();
        let large_value = "x".repeat(150 * 1024);
        outcome.context_updates.insert(
            format!("response.{}", node.id),
            serde_json::json!(large_value),
        );
        Ok(outcome)
    }
}

#[tokio::test]
#[ignore]
async fn daytona_pipeline_artifact_offload_and_sync() {
    let env = create_env().await;
    env.initialize().await.unwrap();
    let env: Arc<dyn Sandbox> = Arc::new(env);

    // Pipeline: start -> big_output -> exit
    let mut graph = Graph::new("DaytonaArtifactPipeline");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test artifact offload+sync on Daytona".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    let mut big_output = Node::new("big_output");
    big_output.attrs.insert(
        "label".to_string(),
        AttrValue::String("Big Output".to_string()),
    );
    graph.nodes.insert("big_output".to_string(), big_output);

    graph.edges.push(Edge::new("start", "big_output"));
    graph.edges.push(Edge::new("big_output", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(LargeOutputHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), env.clone());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Checkpoint should have a pointer rewritten for Daytona
    let checkpoint =
        Checkpoint::load(&dir.path().join("checkpoint.json")).expect("checkpoint should load");
    let pointer_value = checkpoint
        .context_values
        .get("response.big_output")
        .expect("context should have response.big_output");
    let pointer_str = pointer_value.as_str().expect("pointer should be a string");
    let expected_prefix = format!("file://{}/.fabro/artifacts/", env.working_directory());
    assert!(
        pointer_str.starts_with(&expected_prefix),
        "pointer should reference Daytona path, got: {pointer_str}"
    );

    // Verify the artifact file is readable in the sandbox
    let remote_path = pointer_str.strip_prefix("file://").unwrap();
    assert!(
        env.file_exists(remote_path).await.unwrap(),
        "artifact should exist in Daytona sandbox at {remote_path}"
    );

    let remote_content = env.read_file(remote_path, None, None).await.unwrap();
    assert!(
        remote_content.len() > 100 * 1024,
        "remote artifact should be >100KB, got {} bytes",
        remote_content.len()
    );

    env.cleanup().await.unwrap();
}

// ---------------------------------------------------------------------------
// CLI Backend on Daytona — real CLI tools via exec_command
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Git checkpoint E2E on Daytona
// ---------------------------------------------------------------------------

/// Handler that writes a file via exec_command so git has something to commit.
struct FileWriterHandler;

#[async_trait::async_trait]
impl Handler for FileWriterHandler {
    async fn execute(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let content = format!("output from {}", node.id);
        let cmd = format!("echo '{content}' > {}.txt", node.id);
        let _ = services
            .sandbox
            .exec_command(&cmd, 10_000, None, None, None)
            .await;
        Ok(Outcome::success())
    }
}

/// Set up git inside a Daytona sandbox for checkpoint commits.
/// Returns (run_id, base_sha, branch_name) on success.
async fn setup_daytona_git(sandbox: &dyn Sandbox) -> (String, String, String) {
    // Get current HEAD as base SHA
    let sha_result = sandbox
        .exec_command("git rev-parse HEAD", 10_000, None, None, None)
        .await
        .expect("git rev-parse HEAD should succeed");
    assert_eq!(
        sha_result.exit_code, 0,
        "git rev-parse HEAD failed: {}",
        sha_result.stderr
    );
    let base_sha = sha_result.stdout.trim().to_string();

    let run_id = ulid::Ulid::new().to_string();
    let branch_name = format!("fabro/run/{run_id}");

    let checkout_cmd = format!("git checkout -b {branch_name}");
    let checkout_result = sandbox
        .exec_command(&checkout_cmd, 10_000, None, None, None)
        .await
        .expect("git checkout should succeed");
    assert_eq!(
        checkout_result.exit_code, 0,
        "git checkout -b failed (exit {}): stdout={} stderr={}",
        checkout_result.exit_code, checkout_result.stdout, checkout_result.stderr
    );

    (run_id, base_sha, branch_name)
}

#[tokio::test]
#[ignore]
async fn daytona_git_checkpoint_remote_emits_events() {
    let env = create_env().await;
    env.initialize().await.unwrap();
    let env: Arc<dyn Sandbox> = Arc::new(env);

    // Install git if not available (the default ubuntu:22.04 image may not have it)
    let git_check = env
        .exec_command("git --version", 10_000, None, None, None)
        .await;
    if git_check.as_ref().map_or(true, |r| r.exit_code != 0) {
        let install = env
            .exec_command(
                "apt-get update -qq && apt-get install -y -qq git >/dev/null 2>&1",
                120_000,
                None,
                None,
                None,
            )
            .await
            .expect("apt-get install git should not error");
        assert_eq!(
            install.exit_code, 0,
            "git install failed: {}",
            install.stderr
        );
    }

    // Set up git in the sandbox
    let (_run_id, base_sha, branch_name) = setup_daytona_git(&*env).await;

    // Pipeline: start -> work -> exit
    let mut graph = Graph::new("DaytonaGitCheckpoint");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test Remote git checkpoint".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    let mut work = Node::new("work");
    work.attrs
        .insert("label".to_string(), AttrValue::String("Work".to_string()));
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    // Set up event collection
    let dir = tempfile::tempdir().unwrap();
    let emitter = EventEmitter::new();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });
    }

    let mut registry = HandlerRegistry::new(Box::new(FileWriterHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(emitter), env.clone());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "git-cp-test".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: Some(dir.path().to_path_buf()),
        git: Some(GitCheckpointSettings {
            base_sha: Some(base_sha),
            run_branch: Some(branch_name),
            meta_branch: None,
        }),
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Assert CheckpointCompleted events with git SHAs were emitted
    {
        let events = events.lock().unwrap();
        let git_events: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let fabro_workflows::event::WorkflowRunEvent::CheckpointCompleted {
                    node_id,
                    git_commit_sha: Some(sha),
                    ..
                } = e
                {
                    Some((node_id.clone(), sha.clone()))
                } else {
                    None
                }
            })
            .collect();
        // Only the "work" node gets a checkpoint — start is skipped and exit breaks
        // before the checkpoint code runs.
        assert_eq!(
            git_events.len(),
            1,
            "expected 1 CheckpointCompleted event with SHA (work node only), got {}",
            git_events.len()
        );
        assert!(
            git_events
                .iter()
                .all(|(_, sha)| sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())),
            "all SHAs should be 40-char hex, got: {git_events:?}"
        );
    }

    // Assert diff.patch was written for the work node
    let work_diff = dir.path().join("nodes").join("work").join("diff.patch");
    assert!(work_diff.exists(), "diff.patch should exist for work node");

    // Verify checkpoint.json has git_commit_sha
    let checkpoint =
        Checkpoint::load(&dir.path().join("checkpoint.json")).expect("checkpoint should load");
    assert!(
        checkpoint.git_commit_sha.is_some(),
        "checkpoint should have git_commit_sha"
    );

    // Assert final.patch exists and contains changes from the run
    let final_patch = dir.path().join("final.patch");
    assert!(final_patch.exists(), "final.patch should exist in run_dir");
    let patch_content = std::fs::read_to_string(&final_patch).unwrap();
    assert!(!patch_content.is_empty(), "final.patch should not be empty");

    env.cleanup().await.unwrap();
}

// ---------------------------------------------------------------------------
// Parallel git branching on Daytona
// ---------------------------------------------------------------------------

use fabro_workflows::handler::fan_in::FanInHandler;
use fabro_workflows::handler::parallel::ParallelHandler;

/// End-to-end: parallel branches get isolated worktrees in Daytona sandbox,
/// fan-in fast-forwards to winner.
#[tokio::test]
#[ignore]
async fn daytona_parallel_git_branching_e2e() {
    let env = create_env().await;
    env.initialize().await.unwrap();
    let env: Arc<dyn Sandbox> = Arc::new(env);

    // Install git if not available
    let git_check = env
        .exec_command("git --version", 10_000, None, None, None)
        .await;
    if git_check.as_ref().map_or(true, |r| r.exit_code != 0) {
        let install = env
            .exec_command(
                "apt-get update -qq && apt-get install -y -qq git >/dev/null 2>&1",
                120_000,
                None,
                None,
                None,
            )
            .await
            .expect("apt-get install git should not error");
        assert_eq!(
            install.exit_code, 0,
            "git install failed: {}",
            install.stderr
        );
    }

    // Set up git in the sandbox (uses existing repo from Daytona project clone)
    let (run_id, base_sha, branch_name) = setup_daytona_git(&*env).await;

    // Pipeline: start -> fan_out -> {branch_a, branch_b} -> fan_in -> exit
    let mut graph = Graph::new("DaytonaParallelGitBranching");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test parallel git branching on Daytona".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut fan_out = Node::new("fan_out");
    fan_out.attrs.insert(
        "shape".to_string(),
        AttrValue::String("component".to_string()),
    );
    graph.nodes.insert("fan_out".to_string(), fan_out);

    let branch_a = Node::new("branch_a");
    graph.nodes.insert("branch_a".to_string(), branch_a);

    let branch_b = Node::new("branch_b");
    graph.nodes.insert("branch_b".to_string(), branch_b);

    let mut fan_in = Node::new("fan_in");
    fan_in.attrs.insert(
        "shape".to_string(),
        AttrValue::String("tripleoctagon".to_string()),
    );
    graph.nodes.insert("fan_in".to_string(), fan_in);

    let mut exit_node = Node::new("exit");
    exit_node.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit_node);

    graph.edges.push(Edge::new("start", "fan_out"));
    graph.edges.push(Edge::new("fan_out", "branch_a"));
    graph.edges.push(Edge::new("fan_out", "branch_b"));
    graph.edges.push(Edge::new("branch_a", "fan_in"));
    graph.edges.push(Edge::new("branch_b", "fan_in"));
    graph.edges.push(Edge::new("fan_in", "exit"));

    let run_tmp = tempfile::tempdir().unwrap();
    let emitter = EventEmitter::new();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });
    }

    let mut registry = HandlerRegistry::new(Box::new(FileWriterHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("parallel", Box::new(ParallelHandler));
    registry.register("parallel.fan_in", Box::new(FanInHandler::new(None)));

    let engine = WorkflowRunner::new(registry, Arc::new(emitter), Arc::clone(&env));

    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: run_tmp.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: run_id.clone(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: Some(run_tmp.path().to_path_buf()),
        git: Some(GitCheckpointSettings {
            base_sha: Some(base_sha),
            run_branch: Some(branch_name),
            meta_branch: None,
        }),
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("daytona parallel pipeline should succeed");
    assert_eq!(
        outcome.status,
        StageStatus::Success,
        "pipeline failed: {:?}",
        outcome.failure_reason()
    );

    // Verify parallel.results has head_sha for each branch
    let checkpoint =
        Checkpoint::load(&run_tmp.path().join("checkpoint.json")).expect("checkpoint should load");
    let parallel_results = checkpoint
        .context_values
        .get("parallel.results")
        .expect("parallel.results should be in context");
    let results_arr = parallel_results.as_array().expect("should be an array");
    assert_eq!(results_arr.len(), 2, "should have 2 branch results");

    // Both branches should have head_sha (40-char hex)
    let has_sha = results_arr.iter().all(|v| {
        v.get("head_sha")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
    });
    assert!(has_sha, "all branches should have 40-char hex head_sha");

    // Branch SHAs should differ (each branch made unique changes)
    let sha_a = results_arr
        .iter()
        .find(|v| v.get("id").and_then(|v| v.as_str()) == Some("branch_a"))
        .and_then(|v| v.get("head_sha").and_then(|v| v.as_str()))
        .unwrap();
    let sha_b = results_arr
        .iter()
        .find(|v| v.get("id").and_then(|v| v.as_str()) == Some("branch_b"))
        .and_then(|v| v.get("head_sha").and_then(|v| v.as_str()))
        .unwrap();
    assert_ne!(sha_a, sha_b, "branch SHAs should differ");

    // Verify fan_in selected a winner and set best_head_sha
    let best_id = checkpoint
        .context_values
        .get("parallel.fan_in.best_id")
        .and_then(|v| v.as_str().map(String::from))
        .expect("fan_in should have selected a best_id");
    assert_eq!(
        best_id, "branch_a",
        "heuristic should pick branch_a (lexical)"
    );

    let best_head_sha = checkpoint
        .context_values
        .get("parallel.fan_in.best_head_sha")
        .and_then(|v| v.as_str().map(String::from));
    assert!(
        best_head_sha.is_some(),
        "fan_in should have set best_head_sha"
    );

    // Verify winner's file exists in sandbox
    let winner_check = env
        .exec_command("cat branch_a.txt", 10_000, None, None, None)
        .await
        .expect("cat should succeed");
    assert_eq!(winner_check.exit_code, 0, "winner's file should exist");
    assert!(
        winner_check.stdout.contains("branch_a"),
        "winner's file should have correct content, got: {}",
        winner_check.stdout
    );

    // Verify events
    {
        let events = events.lock().unwrap();
        let parallel_started: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    fabro_workflows::event::WorkflowRunEvent::ParallelStarted { .. }
                )
            })
            .collect();
        assert_eq!(
            parallel_started.len(),
            1,
            "should have exactly one ParallelStarted event"
        );
        let parallel_completed: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    fabro_workflows::event::WorkflowRunEvent::ParallelCompleted { .. }
                )
            })
            .collect();
        assert_eq!(
            parallel_completed.len(),
            1,
            "should have exactly one ParallelCompleted event"
        );
    }

    env.cleanup().await.expect("Daytona cleanup should succeed");
}

// ---------------------------------------------------------------------------
// CLI Backend on Daytona — real CLI tools via exec_command
// ---------------------------------------------------------------------------

use fabro_workflows::handler::agent::{CodergenBackend, CodergenResult};
use fabro_workflows::handler::llm::AgentCliBackend;

/// Helper: run a real CLI backend test on Daytona.
///
/// Installs the CLI tool in the sandbox, then runs the AgentCliBackend against it.
async fn run_daytona_cli_test(provider: Provider, model: &str, install_command: &str) {
    let creds = load_github_app_credentials();
    dotenvy::dotenv().ok();
    let config = DaytonaConfig {
        snapshot: Some(DaytonaSnapshotConfig {
            name: "daytona-medium".into(),
            cpu: None,
            memory: None,
            disk: None,
            dockerfile: None,
        }),
        ..DaytonaConfig::default()
    };
    let env = DaytonaSandbox::new(config, Some(creds), None, None)
        .await
        .expect("Failed to create Daytona client — is DAYTONA_API_KEY set?");
    env.initialize().await.unwrap();
    let env: Arc<dyn Sandbox> = Arc::new(env);

    // Install prerequisites (bash, curl, Node 20 via nodesource) if not available
    let prereq_check = env
        .exec_command(
            "bash --version && curl --version && node --version && npm --version",
            10_000,
            None,
            None,
            None,
        )
        .await;
    if prereq_check.as_ref().map_or(true, |r| r.exit_code != 0) {
        let prereq = env
            .exec_command(
                "apt-get update -qq && apt-get install -y -qq bash curl ca-certificates gnupg >/dev/null 2>&1 \
                 && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - >/dev/null 2>&1 \
                 && apt-get install -y -qq nodejs >/dev/null 2>&1",
                180_000,
                None,
                None,
                None,
            )
            .await
            .expect("prerequisite install should not error");
        assert_eq!(
            prereq.exit_code, 0,
            "prerequisite install failed: {}",
            prereq.stderr
        );
    }

    // Install the CLI tool inside the Daytona sandbox
    let install_result = env
        .exec_command(install_command, 120_000, None, None, None)
        .await
        .expect("install command should not error");
    assert_eq!(
        install_result.exit_code, 0,
        "install command failed (exit {}): {}",
        install_result.exit_code, install_result.stdout
    );

    let backend = AgentCliBackend::new(model.to_string(), provider);
    let node = Node::new("daytona_cli_test");
    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let result = backend
        .run(
            &node,
            "What is 2+2? Reply with just the number.",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await;

    match result {
        Ok(CodergenResult::Text { text, usage, .. }) => {
            assert!(
                text.contains('4'),
                "{provider}/{model} on Daytona: expected '4', got: {text}"
            );
            if let Some(u) = usage {
                assert!(
                    u.input_tokens > 0,
                    "{provider}/{model}: input_tokens should be > 0"
                );
            }
        }
        Ok(CodergenResult::Full(_)) => panic!("expected Text result"),
        Err(e) => panic!("{provider}/{model} on Daytona failed: {e}"),
    }

    // Verify log files
    let provider_path = dir.path().join("provider_used.json");
    assert!(
        provider_path.exists(),
        "{provider}/{model}: provider_used.json should exist"
    );
    let provider_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&provider_path).unwrap()).unwrap();
    assert_eq!(provider_json["mode"], "cli");

    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore] // requires DAYTONA_API_KEY + Claude CLI auth
async fn daytona_cli_claude() {
    run_daytona_cli_test(
        Provider::Anthropic,
        "haiku",
        "curl -fsSL https://claude.ai/install.sh | bash",
    )
    .await;
}

#[tokio::test]
#[ignore] // requires DAYTONA_API_KEY + OpenAI/Codex auth
async fn daytona_cli_codex() {
    run_daytona_cli_test(Provider::OpenAi, "o4-mini", "npm install -g @openai/codex").await;
}

#[tokio::test]
#[ignore] // requires DAYTONA_API_KEY + Gemini auth
async fn daytona_cli_gemini() {
    run_daytona_cli_test(
        Provider::Gemini,
        "gemini-2.5-flash",
        "npm install -g @google/gemini-cli",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Daytona shadow commit E2E with MetadataStore
// ---------------------------------------------------------------------------

use fabro_workflows::git::MetadataStore;

/// End-to-end test: pipeline with git checkpointing enabled + `meta_branch`
/// writes shadow branch on the host repo and includes `Fabro-Checkpoint` trailer in sandbox commits.
#[tokio::test]
#[ignore]
async fn daytona_git_checkpoint_with_shadow_branch() {
    let env = create_env().await;
    env.initialize().await.unwrap();
    let env: Arc<dyn Sandbox> = Arc::new(env);

    // Install git if not available
    let git_check = env
        .exec_command("git --version", 10_000, None, None, None)
        .await;
    if git_check.as_ref().map_or(true, |r| r.exit_code != 0) {
        let install = env
            .exec_command(
                "apt-get update -qq && apt-get install -y -qq git >/dev/null 2>&1",
                120_000,
                None,
                None,
                None,
            )
            .await
            .expect("apt-get install git should not error");
        assert_eq!(
            install.exit_code, 0,
            "git install failed: {}",
            install.stderr
        );
    }

    // Set up git in the sandbox
    let (run_id, base_sha, branch_name) = setup_daytona_git(&*env).await;

    // Create a temp git repo on the host for MetadataStore
    let host_repo = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(host_repo.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
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
        .current_dir(host_repo.path())
        .output()
        .unwrap();

    // Pipeline: start -> work -> exit
    let mut graph = Graph::new("DaytonaShadowBranch");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test Daytona shadow branch".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    let mut work = Node::new("work");
    work.attrs
        .insert("label".to_string(), AttrValue::String("Work".to_string()));
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let dir = tempfile::tempdir().unwrap();
    // Write graph.fabro so init_run can read it
    std::fs::write(dir.path().join("graph.fabro"), "digraph {}").unwrap();

    let mut registry = HandlerRegistry::new(Box::new(FileWriterHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let meta_branch = MetadataStore::branch_name(&run_id);
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), env.clone());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: run_id.clone(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: Some(host_repo.path().to_path_buf()),
        git: Some(GitCheckpointSettings {
            base_sha: Some(base_sha),
            run_branch: Some(branch_name),
            meta_branch: Some(meta_branch),
        }),
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Assert shadow branch on host has checkpoint data
    let checkpoint = MetadataStore::read_checkpoint(host_repo.path(), &run_id)
        .expect("read_checkpoint should not error")
        .expect("shadow branch should contain checkpoint data");
    assert!(
        !checkpoint.completed_nodes.is_empty(),
        "checkpoint should have completed nodes"
    );
    assert!(
        checkpoint.completed_nodes.contains(&"work".to_string()),
        "checkpoint should contain the 'work' node"
    );

    // Assert sandbox commit has Fabro-Checkpoint trailer
    let log_result = env
        .exec_command("git log --format=%B -1", 10_000, None, None, None)
        .await
        .expect("git log should succeed");
    assert_eq!(log_result.exit_code, 0);
    let commit_msg = log_result.stdout.trim().to_string();
    assert!(
        commit_msg.contains("Fabro-Checkpoint:"),
        "sandbox commit should have Fabro-Checkpoint trailer, got:\n{commit_msg}"
    );
    assert!(
        commit_msg.contains("Fabro-Run:"),
        "sandbox commit should have Fabro-Run trailer, got:\n{commit_msg}"
    );

    // Assert final.patch exists
    let final_patch = dir.path().join("final.patch");
    assert!(final_patch.exists(), "final.patch should exist in run_dir");

    env.cleanup().await.unwrap();
}

// ---------------------------------------------------------------------------
// Asset collection e2e — Daytona sandbox
// ---------------------------------------------------------------------------

/// Handler that creates asset files via exec_command on the sandbox.
struct AssetCreatorHandler;

#[async_trait::async_trait]
impl Handler for AssetCreatorHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let script = concat!(
            "mkdir -p test-results && ",
            "echo '<testsuites><testsuite name=\"example\"/></testsuites>' > test-results/report.xml && ",
            "echo 'test output' > test-results/output.txt"
        );
        services
            .sandbox
            .exec_command(script, 30_000, None, None, None)
            .await
            .map_err(|e| FabroError::handler(format!("exec failed: {e}")))?;
        Ok(Outcome::success())
    }
}

/// Daytona sandbox: asset collection discovers files on the remote sandbox and
/// downloads them to the local logs directory.
#[tokio::test]
#[ignore]
async fn daytona_asset_collection() {
    let env = create_env().await;
    env.initialize().await.unwrap();
    let env: Arc<dyn Sandbox> = Arc::new(env);

    let dir = tempfile::tempdir().unwrap();

    let mut registry = HandlerRegistry::new(Box::new(AssetCreatorHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), env.clone());

    let mut graph = Graph::new("DaytonaAssetTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test asset collection on Daytona".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut create_assets = Node::new("create_assets");
    create_assets.attrs.insert(
        "label".to_string(),
        AttrValue::String("Create Assets".to_string()),
    );
    graph
        .nodes
        .insert("create_assets".to_string(), create_assets);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    graph.edges.push(Edge::new("start", "create_assets"));
    graph.edges.push(Edge::new("create_assets", "exit"));

    let config = RunSettings {
        config: FabroConfig {
            assets: Some(fabro_config::run::AssetsConfig {
                include: vec!["test-results/**".to_string()],
            }),
            ..FabroConfig::default()
        },
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "asset-test-daytona".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let assets_dir = dir
        .path()
        .join("artifacts")
        .join("assets")
        .join("create_assets")
        .join("retry_1");

    let report_path = assets_dir.join("test-results/report.xml");
    assert!(
        report_path.exists(),
        "report.xml should be collected from Daytona sandbox at {}",
        report_path.display()
    );
    let content = std::fs::read_to_string(&report_path).unwrap();
    assert!(content.contains("testsuites"));

    let manifest_path = assets_dir.join("manifest.json");
    assert!(manifest_path.exists(), "manifest.json should exist");

    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_ssh_access() {
    let env = create_env().await;
    env.initialize().await.unwrap();

    let ssh_command = env.create_ssh_access(Some(60.0)).await.unwrap();
    assert!(!ssh_command.is_empty(), "ssh_command should not be empty");
    assert!(
        ssh_command.contains("ssh"),
        "ssh_command should contain 'ssh': {ssh_command}",
    );

    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_ssh_access_before_init_fails() {
    let env = create_env().await;

    let result = env.create_ssh_access(Some(60.0)).await;
    assert!(result.is_err(), "should fail before initialize()");
    assert!(
        result.unwrap_err().contains("not initialized"),
        "error should mention not initialized"
    );
}

// ---------------------------------------------------------------------------
// GitHub App Installation Access Token (IAT) clone tests
// ---------------------------------------------------------------------------

/// E2E: Clone the current (private) repo using GitHub App IAT credentials.
/// Verifies the full flow: JWT signing, installation lookup, token creation, clone.
#[tokio::test]
#[ignore]
async fn daytona_clone_private_repo_with_github_app_iat() {
    let creds = load_github_app_credentials();
    let env = create_env_with_github_app(Some(creds)).await;

    // initialize() clones the current repo — with IAT credentials this should succeed
    env.initialize().await.unwrap();

    // Verify the clone worked: CLAUDE.md should exist in the workspace
    let result = env
        .exec_command("test -f CLAUDE.md && echo EXISTS", 10_000, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0, "CLAUDE.md should exist after clone");
    assert!(
        result.stdout.contains("EXISTS"),
        "clone should have populated the workspace"
    );

    // Install git if not available (the default ubuntu:22.04 image may not have it)
    let git_check = env
        .exec_command("git --version", 10_000, None, None, None)
        .await;
    if git_check.as_ref().map_or(true, |r| r.exit_code != 0) {
        let install = env
            .exec_command(
                "apt-get update -qq && apt-get install -y -qq git >/dev/null 2>&1",
                120_000,
                None,
                None,
                None,
            )
            .await
            .expect("apt-get install git should not error");
        assert_eq!(
            install.exit_code, 0,
            "git install failed: {}",
            install.stderr
        );
    }

    // Verify this is actually the fabro repo
    let result = env
        .exec_command("git remote get-url origin", 10_000, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stdout.contains("fabro-sh/fabro"),
        "origin should point to fabro-sh/fabro, got: {}",
        result.stdout.trim()
    );

    env.cleanup().await.unwrap();
}

/// E2E: Verify that repos in an installed org get credentials (needed for pushing).
#[tokio::test]
#[ignore]
async fn daytona_clone_public_repo_gets_credentials() {
    let creds = load_github_app_credentials();

    // Directly test resolve_clone_credentials against a repo in an org where the app is installed
    let (username, password) = fabro_github::resolve_clone_credentials(&creds, "fabro-sh", "fabro")
        .await
        .unwrap();

    assert_eq!(
        username.as_deref(),
        Some("x-access-token"),
        "installed org repo should get credentials for pushing"
    );
    assert!(
        password.is_some(),
        "installed org repo should get a token for pushing"
    );
}

/// E2E: Verify that requesting an IAT for a repo the app isn't installed on
/// gives a clear error message.
#[tokio::test]
#[ignore]
async fn daytona_iat_not_installed_gives_clear_error() {
    let creds = load_github_app_credentials();

    let result = fabro_github::resolve_clone_credentials(&creds, "torvalds", "linux").await;

    assert!(
        result.is_err(),
        "should fail for repo the app isn't installed on"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("not installed"),
        "error should mention 'not installed', got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Push run branch to origin after each checkpoint (GitHub App)
// ---------------------------------------------------------------------------

/// E2E: After each remote checkpoint, the run branch is pushed to origin.
/// Verifies the branch appears on the remote via `git ls-remote`.
#[tokio::test]
#[ignore]
async fn daytona_git_push_run_branch_to_origin() {
    let creds = load_github_app_credentials();
    let env = create_env_with_github_app(Some(creds)).await;
    env.initialize().await.unwrap();
    let env: Arc<dyn Sandbox> = Arc::new(env);

    // Install git if not available
    let git_check = env
        .exec_command("git --version", 10_000, None, None, None)
        .await;
    if git_check.as_ref().map_or(true, |r| r.exit_code != 0) {
        let install = env
            .exec_command(
                "apt-get update -qq && apt-get install -y -qq git >/dev/null 2>&1",
                120_000,
                None,
                None,
                None,
            )
            .await
            .expect("apt-get install git should not error");
        assert_eq!(
            install.exit_code, 0,
            "git install failed: {}",
            install.stderr
        );
    }

    // Set up git in the sandbox
    let (run_id, base_sha, branch_name) = setup_daytona_git(&*env).await;

    // Pipeline: start -> work -> exit
    let mut graph = Graph::new("DaytonaGitPush");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test push run branch to origin".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    let mut work = Node::new("work");
    work.attrs
        .insert("label".to_string(), AttrValue::String("Work".to_string()));
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let dir = tempfile::tempdir().unwrap();

    let mut registry = HandlerRegistry::new(Box::new(FileWriterHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), env.clone());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: run_id.clone(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: Some(dir.path().to_path_buf()),
        git: Some(GitCheckpointSettings {
            base_sha: Some(base_sha),
            run_branch: Some(branch_name.clone()),
            meta_branch: None,
        }),
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Verify the run branch was pushed to origin
    let ls_remote_cmd = format!("git ls-remote --heads origin {branch_name}");
    let ls_result = env
        .exec_command(&ls_remote_cmd, 30_000, None, None, None)
        .await
        .expect("git ls-remote should succeed");
    assert_eq!(
        ls_result.exit_code, 0,
        "git ls-remote failed: {}",
        ls_result.stdout
    );
    assert!(
        ls_result.stdout.contains(&branch_name),
        "run branch should exist on origin after push, got: {}",
        ls_result.stdout.trim()
    );

    // Clean up the remote branch
    let delete_cmd = format!("git push origin --delete {branch_name}");
    let delete_result = env
        .exec_command(&delete_cmd, 30_000, None, None, None)
        .await;
    if let Ok(r) = &delete_result {
        if r.exit_code != 0 {
            eprintln!(
                "Warning: failed to delete remote branch {branch_name}: {}",
                r.stdout
            );
        }
    }

    env.cleanup().await.unwrap();
}

/// Diagnose toolbox proxy staleness after idle time.
///
/// Creates a sandbox, runs a command, sleeps for increasing durations, then
/// retries. If a call fails, makes raw HTTP requests to capture the actual
/// underlying error that the SDK normally swallows.
///
/// Run: cargo test -p arc-workflows -- --ignored daytona_toolbox_idle_diagnostic --nocapture
#[tokio::test]
#[ignore]
async fn daytona_toolbox_idle_diagnostic() {
    let creds = load_github_app_credentials();
    let env = create_env_with_github_app(Some(creds)).await;
    env.initialize().await.unwrap();

    // 1. Verify toolbox works immediately after init
    let result = env
        .exec_command("echo alive", 30_000, None, None, None)
        .await;
    eprintln!(
        "[t=0s] exec_command after init: {:?}",
        result.as_ref().map(|r| r.exit_code)
    );
    assert!(
        result.is_ok(),
        "exec_command should work immediately after init"
    );

    let sandbox_name = env.sandbox_info();
    eprintln!("[t=0s] sandbox: {sandbox_name}");

    // 2. Sleep for increasing durations and test
    for sleep_secs in [1, 2, 3] {
        eprintln!("\n--- sleeping {sleep_secs}s ---");
        tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;

        let result = env
            .exec_command("echo alive", 30_000, None, None, None)
            .await;

        match &result {
            Ok(r) => {
                eprintln!(
                    "[t=+{sleep_secs}s] OK exit_code={} stdout={}",
                    r.exit_code,
                    r.stdout.trim()
                );
            }
            Err(e) => {
                eprintln!("[t=+{sleep_secs}s] FAILED: {e}");

                // Diagnose with raw HTTP calls
                let api_key = std::env::var("DAYTONA_API_KEY").unwrap_or_default();
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(15))
                    .build()
                    .unwrap();
                let api_url = std::env::var("DAYTONA_API_URL")
                    .or_else(|_| std::env::var("DAYTONA_SERVER_URL"))
                    .unwrap_or_else(|_| "https://app.daytona.io/api".to_string());

                // Check sandbox state
                let state_resp = client
                    .get(format!("{api_url}/sandbox/{sandbox_name}"))
                    .bearer_auth(&api_key)
                    .send()
                    .await;
                match state_resp {
                    Ok(resp) => {
                        let body = resp.text().await.unwrap_or_default();
                        let state = serde_json::from_str::<serde_json::Value>(&body)
                            .ok()
                            .and_then(|v| v.get("state").cloned());
                        eprintln!("[diag] sandbox state: {state:?}");
                    }
                    Err(e) => {
                        eprintln!("[diag] sandbox API failed: {e}");
                    }
                }

                // Get toolbox proxy URL and try a direct call
                let proxy_resp = client
                    .get(format!(
                        "{api_url}/sandbox/{sandbox_name}/toolbox-proxy-url"
                    ))
                    .bearer_auth(&api_key)
                    .send()
                    .await;
                if let Ok(resp) = proxy_resp {
                    let body = resp.text().await.unwrap_or_default();
                    eprintln!(
                        "[diag] proxy URL response: {}",
                        &body[..body.len().min(200)]
                    );
                    if let Some(url) = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(String::from))
                    {
                        let toolbox_url = format!("{url}/{sandbox_name}/process/execute");
                        eprintln!("[diag] trying direct POST to {toolbox_url}");
                        let direct = client
                            .post(&toolbox_url)
                            .bearer_auth(&api_key)
                            .json(&serde_json::json!({"command": "echo diag", "timeout": 10}))
                            .send()
                            .await;
                        match direct {
                            Ok(resp) => {
                                let status = resp.status();
                                let body = resp.text().await.unwrap_or_default();
                                eprintln!(
                                    "[diag] direct call: {status} body={}",
                                    &body[..body.len().min(300)]
                                );
                            }
                            Err(e) => {
                                // Walk the FULL error source chain
                                let mut msg = format!("[diag] direct call FAILED: {e}");
                                let mut source: Option<&dyn std::error::Error> =
                                    std::error::Error::source(&e);
                                while let Some(cause) = source {
                                    msg.push_str(&format!("\n  caused by: {cause}"));
                                    source = cause.source();
                                }
                                eprintln!("{msg}");
                            }
                        }
                    }
                }

                panic!("exec_command failed after {sleep_secs}s idle: {e}");
            }
        }
    }

    eprintln!("\n=== PASS: all idle durations survived ===");
    env.cleanup().await.unwrap();
}

/// E2E test for `fabro cp` against a live Daytona sandbox.
///
/// Creates a sandbox, saves a SandboxRecord, reconnects via `cp::reconnect`,
/// uploads a file, downloads it back, and verifies the round-trip.
#[tokio::test]
#[ignore]
async fn daytona_cp_upload_download_round_trip() {
    use fabro_sandbox::reconnect::reconnect;
    use fabro_sandbox::SandboxRecord;

    // 1. Create and initialize a real Daytona sandbox
    let env = create_env().await;
    env.initialize().await.unwrap();

    let sandbox_name = env.sandbox_info();
    assert!(
        !sandbox_name.is_empty(),
        "sandbox_info() should return the Daytona sandbox name"
    );

    // 2. Build a SandboxRecord (same as `fabro run` would persist)
    let record = SandboxRecord {
        provider: "daytona".to_string(),
        working_directory: env.working_directory().to_string(),
        identifier: Some(sandbox_name.clone()),
        host_working_directory: None,
        container_mount_point: None,
        data_host: None,
    };

    // 3. Save to temp dir and reload (verify serialization round-trip)
    let tmp = tempfile::tempdir().unwrap();
    let record_path = tmp.path().join("sandbox.json");
    record.save(&record_path).unwrap();
    let loaded = SandboxRecord::load(&record_path).unwrap();
    assert_eq!(loaded.provider, "daytona");
    assert_eq!(loaded.identifier.as_deref(), Some(sandbox_name.as_str()));

    // 4. Reconnect via the real cp::reconnect path
    let reconnected = reconnect(&loaded).await.expect("reconnect should succeed");

    // 5. Upload: write a local file, then upload it to the sandbox
    let upload_content = b"hello from fabro cp e2e test\n";
    let local_upload = tmp.path().join("upload.txt");
    std::fs::write(&local_upload, upload_content).unwrap();

    reconnected
        .upload_file_from_local(&local_upload, "cp_test_upload.txt")
        .await
        .expect("upload_file_from_local should succeed");

    // 6. Verify the file exists in the sandbox via the original connection
    assert!(
        env.file_exists("cp_test_upload.txt").await.unwrap(),
        "uploaded file should exist in the sandbox"
    );
    let remote_content = env
        .read_file("cp_test_upload.txt", None, None)
        .await
        .unwrap();
    assert!(
        remote_content.contains("hello from fabro cp e2e test"),
        "expected uploaded content in sandbox, got: {remote_content}"
    );

    // 7. Download: retrieve the file back to local via the reconnected sandbox
    let local_download = tmp.path().join("download.txt");
    reconnected
        .download_file_to_local("cp_test_upload.txt", &local_download)
        .await
        .expect("download_file_to_local should succeed");

    let downloaded = std::fs::read(&local_download).unwrap();
    assert_eq!(downloaded, upload_content);

    // 8. Upload a binary file to test non-UTF-8 content
    let binary_content: Vec<u8> = (0..=255).collect();
    let local_binary = tmp.path().join("binary.bin");
    std::fs::write(&local_binary, &binary_content).unwrap();

    reconnected
        .upload_file_from_local(&local_binary, "cp_test_binary.bin")
        .await
        .expect("binary upload should succeed");

    let local_binary_dl = tmp.path().join("binary_dl.bin");
    reconnected
        .download_file_to_local("cp_test_binary.bin", &local_binary_dl)
        .await
        .expect("binary download should succeed");

    let downloaded_binary = std::fs::read(&local_binary_dl).unwrap();
    assert_eq!(
        downloaded_binary, binary_content,
        "binary round-trip should be exact"
    );

    // 9. Cleanup
    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_computer_use_browser_screenshot() {
    use base64::Engine;

    dotenvy::dotenv().ok();
    if let Some(home) = dirs::home_dir() {
        dotenvy::from_path(home.join(".fabro/.env")).ok();
    }
    let config = DaytonaConfig {
        snapshot: Some(DaytonaSnapshotConfig {
            name: "daytona-medium".into(),
            cpu: None,
            memory: None,
            disk: None,
            dockerfile: None,
        }),
        skip_clone: true,
        ..DaytonaConfig::default()
    };
    let env = DaytonaSandbox::new(config, None, None, None)
        .await
        .expect("DAYTONA_API_KEY must be set");
    env.initialize().await.unwrap();

    // 1. Start the computer use desktop environment (Xvfb, xfce4, etc.)
    let cu = env.computer_use().await.unwrap();
    let start_resp = cu.start().await.expect("computer_use.start() failed");
    eprintln!("Computer use started: {:?}", start_resp.message);

    // 2. Find or install a browser
    let check = env
        .exec_command(
            "which chromium || which chromium-browser || which google-chrome || echo NONE",
            30_000,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    eprintln!("Browser check: {}", check.stdout.trim());

    if check.stdout.trim() == "NONE" {
        let install_result = env
            .exec_command(
                "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq chromium 2>&1",
                180_000, None, None, None,
            )
            .await
            .unwrap();
        eprintln!(
            "Browser install exit_code={}, last_line={}",
            install_result.exit_code,
            install_result.stdout.lines().last().unwrap_or("")
        );
        assert_eq!(install_result.exit_code, 0, "Chromium install failed");
    }

    let browser_bin = env
        .exec_command(
            "which chromium || which chromium-browser || which google-chrome",
            10_000,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let browser = browser_bin.stdout.trim().to_string();
    eprintln!("Using browser: {browser}");

    // 3. Detect the DISPLAY that computer use started
    let display_check = env
        .exec_command(
            "ps aux | grep Xvfb | grep -v grep | head -1",
            10_000,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    eprintln!("Xvfb process: {}", display_check.stdout.trim());

    // 4. Launch browser with setsid to fully detach, and log stderr
    let launch_cmd = format!(
        "DISPLAY=:0 setsid {browser} --no-sandbox --disable-gpu \
         --window-size=1024,768 --window-position=0,0 \
         https://example.com > /tmp/chrome_stdout.log 2>/tmp/chrome_stderr.log &\n\
         sleep 2 && echo launched"
    );
    let launch_result = env
        .exec_command(&launch_cmd, 30_000, None, None, None)
        .await
        .unwrap();
    eprintln!("Browser launch exit_code={}", launch_result.exit_code);

    // 5. Wait for the page to load, then check if browser is running
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;

    let ps_check = env
        .exec_command(
            "ps aux | grep -i chrom | grep -v grep",
            10_000,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    eprintln!("Chrome processes:\n{}", ps_check.stdout);

    let stderr_check = env
        .exec_command(
            "cat /tmp/chrome_stderr.log 2>/dev/null | tail -20",
            10_000,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    eprintln!("Chrome stderr:\n{}", stderr_check.stdout);

    // 5. Take a screenshot via the Computer Use API
    let screenshot = cu
        .screenshot()
        .take_full_screen()
        .await
        .expect("screenshot failed");

    let b64_data = screenshot
        .screenshot
        .expect("screenshot response had no data");
    eprintln!(
        "Screenshot captured: {} bytes base64 ({} bytes decoded approx)",
        b64_data.len(),
        b64_data.len() * 3 / 4
    );
    assert!(!b64_data.is_empty(), "screenshot should not be empty");

    // 6. Decode and save to /tmp for manual inspection
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(&b64_data)
        .expect("base64 decode failed");
    let output_path = "/tmp/daytona_browser_screenshot.png";
    std::fs::write(output_path, &png_bytes).expect("failed to write screenshot");
    eprintln!(
        "Screenshot saved to {output_path} ({} bytes)",
        png_bytes.len()
    );

    // 7. Cleanup
    cu.stop().await.ok();
    env.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn daytona_playwright_mcp_sandbox_transport() {
    use fabro_agent::Sandbox;

    // Create sandbox from daytona-medium (has Node.js + Chromium)
    dotenvy::dotenv().ok();
    if let Some(home) = dirs::home_dir() {
        dotenvy::from_path(home.join(".fabro/.env")).ok();
    }
    let config = DaytonaConfig {
        snapshot: Some(DaytonaSnapshotConfig {
            name: "daytona-medium".into(),
            cpu: None,
            memory: None,
            disk: None,
            dockerfile: None,
        }),
        skip_clone: true,
        ..DaytonaConfig::default()
    };
    let sandbox = DaytonaSandbox::new(config, None, None, None)
        .await
        .expect("DAYTONA_API_KEY must be set");
    sandbox.initialize().await.unwrap();

    // 1. Install Playwright MCP server and its browser
    eprintln!("Installing @playwright/mcp and Chromium browser...");
    let install = sandbox
        .exec_command(
            "npm install -g @playwright/mcp@latest 2>&1 && npx playwright install --with-deps chromium 2>&1",
            300_000,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    eprintln!(
        "Install exit_code={}, last_lines:\n{}",
        install.exit_code,
        install
            .stdout
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(install.exit_code, 0, "Playwright install failed");

    // 2. Start the Playwright MCP server via the sandbox transport resolution path
    let mcp_port = 3100u16;
    let mcp_config = fabro_mcp::config::McpServerConfig {
        name: "playwright".into(),
        transport: fabro_mcp::config::McpTransport::Sandbox {
            command: vec![
                "npx".into(),
                "@playwright/mcp@latest".into(),
                "--port".into(),
                mcp_port.to_string(),
                "--headless".into(),
                "--browser".into(),
                "chromium".into(),
            ],
            port: mcp_port,
            env: std::collections::HashMap::new(),
        },
        startup_timeout_secs: 30,
        tool_timeout_secs: 120,
    };

    // Resolve the sandbox transport: start the server, get preview URL, rewrite to HTTP
    let resolved = match &mcp_config.transport {
        fabro_mcp::config::McpTransport::Sandbox { command, port, .. } => {
            let (url, headers) = {
                let cmd_str = command.join(" ");
                let launch_script = format!(
                    "setsid sh -c '{cmd_str} > /tmp/mcp_server_stdout.log 2>/tmp/mcp_server_stderr.log' \
                     </dev/null >/dev/null 2>&1 &\necho $!"
                );
                let launch_result = sandbox
                    .exec_command(&launch_script, 30_000, None, None, None)
                    .await
                    .unwrap();
                eprintln!("MCP server PID: {}", launch_result.stdout.trim());

                // Wait for server to listen
                let poll_cmd = format!(
                    "for i in $(seq 1 30); do ss -tln | grep -q ':{port} ' && echo ready && exit 0; sleep 1; done; echo timeout"
                );
                let poll_result = sandbox
                    .exec_command(&poll_cmd, 60_000, None, None, None)
                    .await
                    .unwrap();
                eprintln!("Server readiness: {}", poll_result.stdout.trim());

                if poll_result.stdout.trim() != "ready" {
                    let stderr = sandbox
                        .exec_command(
                            "cat /tmp/mcp_server_stderr.log 2>/dev/null | tail -20",
                            10_000,
                            None,
                            None,
                            None,
                        )
                        .await
                        .map(|r| r.stdout)
                        .unwrap_or_default();
                    panic!("MCP server did not start on port {port}. stderr:\n{stderr}");
                }

                sandbox
                    .get_preview_url(*port)
                    .await
                    .unwrap()
                    .expect("sandbox should support preview URLs")
            };
            eprintln!("Preview URL: {url}");

            fabro_mcp::config::McpServerConfig {
                name: mcp_config.name.clone(),
                transport: fabro_mcp::config::McpTransport::Http { url, headers },
                startup_timeout_secs: mcp_config.startup_timeout_secs,
                tool_timeout_secs: mcp_config.tool_timeout_secs,
            }
        }
        _ => unreachable!(),
    };

    // 3. Connect the MCP client to the resolved HTTP endpoint
    let mut manager = fabro_mcp::connection_manager::McpConnectionManager::new();
    let results = manager.start_servers(&[resolved]).await;
    for (name, result) in &results {
        match result {
            Ok(count) => eprintln!("MCP server '{name}' ready with {count} tools"),
            Err(e) => panic!("MCP server '{name}' failed: {e}"),
        }
    }

    // 4. List the tools to verify we got Playwright tools
    let tools = manager.all_tools();
    eprintln!("Discovered {} MCP tools:", tools.len());
    for (name, info) in tools {
        eprintln!(
            "  - {name}: {}",
            info.description.chars().take(80).collect::<String>()
        );
    }
    assert!(!tools.is_empty(), "Should have discovered Playwright tools");

    // 5. Install the browser via MCP tool (ensures correct version is available)
    let install_tool = tools
        .keys()
        .find(|k| k.ends_with("browser_install"))
        .expect("no browser_install tool found");
    eprintln!("Calling tool: {install_tool}");
    let install_result = manager
        .call_tool(
            install_tool,
            serde_json::json!({}),
            std::time::Duration::from_secs(120),
        )
        .await;
    match &install_result {
        Ok(result) => eprintln!(
            "Install result: {}",
            result
                .content
                .first()
                .map(|c| format!("{c:?}"))
                .unwrap_or_default()
        ),
        Err(e) => eprintln!("Install error (non-fatal): {e}"),
    }

    // 6. Call the browser_navigate tool to load a page
    let nav_tool = tools
        .keys()
        .find(|k| k.ends_with("browser_navigate"))
        .expect("no browser_navigate tool found");
    eprintln!("Calling tool: {nav_tool}");
    let nav_result = manager
        .call_tool(
            nav_tool,
            serde_json::json!({"url": "https://example.com"}),
            std::time::Duration::from_secs(30),
        )
        .await;
    match &nav_result {
        Ok(result) => eprintln!(
            "Navigate result: {}",
            &result
                .content
                .first()
                .map(|c| format!("{c:?}"))
                .unwrap_or_default()
        ),
        Err(e) => eprintln!("Navigate error: {e}"),
    }
    assert!(nav_result.is_ok(), "Navigate should succeed");

    // 7. Take a snapshot to verify the page loaded
    let snap_tool = tools
        .keys()
        .find(|k| k.contains("snapshot"))
        .expect("no snapshot tool found");
    eprintln!("Calling tool: {snap_tool}");
    let snap_result = manager
        .call_tool(
            snap_tool,
            serde_json::json!({}),
            std::time::Duration::from_secs(30),
        )
        .await;
    match &snap_result {
        Ok(result) => {
            let text = result
                .content
                .first()
                .map(|c| format!("{c:?}"))
                .unwrap_or_default();
            eprintln!(
                "Snapshot result (first 500 chars): {}",
                &text[..text.len().min(500)]
            );
            assert!(
                text.contains("Example Domain"),
                "Snapshot should contain 'Example Domain'"
            );
        }
        Err(e) => panic!("Snapshot failed: {e}"),
    }

    // 8. Cleanup
    sandbox.cleanup().await.unwrap();
}
