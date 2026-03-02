//! Integration tests for `DaytonaExecutionEnvironment`.
//!
//! These tests require a `DAYTONA_API_KEY` environment variable and network access.
//! Run with: `cargo test --package arc-workflows -- --ignored daytona`

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use arc_agent::ExecutionEnvironment;
use arc_llm::provider::Provider;
use arc_workflows::artifact::sync_artifacts_to_env;
use arc_workflows::checkpoint::Checkpoint;
use arc_workflows::context::Context;
use arc_workflows::daytona_env::{DaytonaConfig, DaytonaExecutionEnvironment};
use arc_workflows::engine::{PipelineEngine, RunConfig};
use arc_workflows::error::ArcError;
use arc_workflows::event::EventEmitter;
use arc_workflows::graph::{AttrValue, Edge, Graph, Node};
use arc_workflows::handler::exit::ExitHandler;
use arc_workflows::handler::start::StartHandler;
use arc_workflows::handler::{Handler, HandlerRegistry};
use arc_workflows::outcome::{Outcome, StageStatus};

async fn create_env() -> DaytonaExecutionEnvironment {
    dotenvy::dotenv().ok();
    let client = daytona_sdk::Client::new()
        .await
        .expect("Failed to create Daytona client — is DAYTONA_API_KEY set?");
    DaytonaExecutionEnvironment::new(client, DaytonaConfig::default())
}

#[tokio::test]
#[ignore]
async fn daytona_exec_command() {
    let env = create_env().await;
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
    let env = create_env().await;
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
    use arc_workflows::daytona_env::{DaytonaSandboxConfig, DaytonaSnapshotConfig};

    dotenvy::dotenv().ok();
    let client = daytona_sdk::Client::new()
        .await
        .expect("Failed to create Daytona client — is DAYTONA_API_KEY set?");

    let config = DaytonaConfig {
        sandbox: DaytonaSandboxConfig {
            auto_stop_interval: Some(60),
            ..Default::default()
        },
        snapshot: Some(DaytonaSnapshotConfig {
            name: "arc-test-snapshot".to_string(),
            cpu: Some(2),
            memory: Some(4),
            disk: Some(10),
            dockerfile: Some(
                "FROM ubuntu:22.04\nRUN apt-get update && apt-get install -y ripgrep".to_string(),
            ),
        }),
    };

    let env = DaytonaExecutionEnvironment::new(client, config);
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
    let expected_prefix = format!("file://{}/.arc/artifacts/", env.working_directory());
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
        _logs_root: &Path,
        _services: &arc_workflows::handler::EngineServices,
    ) -> Result<Outcome, ArcError> {
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
    let env: Arc<dyn ExecutionEnvironment> = Arc::new(env);

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

    let engine = PipelineEngine::new(registry, Arc::new(EventEmitter::new()), env.clone());
    let config = RunConfig {
        logs_root: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        git_checkpoint: None,
        base_sha: None,
        run_branch: None,
        meta_branch: None,
        labels: std::collections::HashMap::new(),
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
    let expected_prefix = format!("file://{}/.arc/artifacts/", env.working_directory());
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

use arc_workflows::engine::GitCheckpointMode;

// ---------------------------------------------------------------------------
// Git checkpoint E2E on Daytona (Remote mode)
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
        _logs_root: &Path,
        services: &arc_workflows::handler::EngineServices,
    ) -> Result<Outcome, ArcError> {
        let content = format!("output from {}", node.id);
        let cmd = format!("echo '{content}' > {}.txt", node.id);
        let _ = services
            .execution_env
            .exec_command(&cmd, 10_000, None, None, None)
            .await;
        Ok(Outcome::success())
    }
}

/// Set up git inside a Daytona sandbox for checkpoint commits.
/// Returns (run_id, base_sha, branch_name) on success.
async fn setup_daytona_git(exec_env: &dyn ExecutionEnvironment) -> (String, String, String) {
    // Get current HEAD as base SHA
    let sha_result = exec_env
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
    let branch_name = format!("arc/run/{run_id}");

    let checkout_cmd = format!("git checkout -b {branch_name}");
    let checkout_result = exec_env
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
    let env: Arc<dyn ExecutionEnvironment> = Arc::new(env);

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
    let (run_id, base_sha, branch_name) = setup_daytona_git(&*env).await;

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
    let mut emitter = EventEmitter::new();
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

    let engine = PipelineEngine::new(registry, Arc::new(emitter), env.clone());
    let config = RunConfig {
        logs_root: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id,
        git_checkpoint: Some(GitCheckpointMode::Remote(dir.path().to_path_buf())),
        base_sha: Some(base_sha),
        run_branch: Some(branch_name),
        meta_branch: None,
        labels: std::collections::HashMap::new(),
    };

    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Assert GitCheckpoint events were emitted
    {
        let events = events.lock().unwrap();
        let git_events: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let arc_workflows::event::PipelineEvent::GitCheckpoint {
                    node_id,
                    git_commit_sha,
                    ..
                } = e
                {
                    Some((node_id.clone(), git_commit_sha.clone()))
                } else {
                    None
                }
            })
            .collect();
        assert!(
            git_events.len() >= 2,
            "expected at least 2 GitCheckpoint events, got {}",
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
    assert!(
        final_patch.exists(),
        "final.patch should exist in logs_root"
    );
    let patch_content = std::fs::read_to_string(&final_patch).unwrap();
    assert!(!patch_content.is_empty(), "final.patch should not be empty");

    env.cleanup().await.unwrap();
}

// ---------------------------------------------------------------------------
// Parallel git branching on Daytona (Remote mode)
// ---------------------------------------------------------------------------

use arc_workflows::handler::fan_in::FanInHandler;
use arc_workflows::handler::parallel::ParallelHandler;

/// End-to-end: parallel branches get isolated worktrees in Daytona sandbox,
/// fan-in fast-forwards to winner.
#[tokio::test]
#[ignore]
async fn daytona_parallel_git_branching_e2e() {
    let env = create_env().await;
    env.initialize().await.unwrap();
    let env: Arc<dyn ExecutionEnvironment> = Arc::new(env);

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

    let logs_dir = tempfile::tempdir().unwrap();
    let mut emitter = EventEmitter::new();
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

    let engine = PipelineEngine::new(registry, Arc::new(emitter), Arc::clone(&env));

    let config = RunConfig {
        logs_root: logs_dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: run_id.clone(),
        git_checkpoint: Some(GitCheckpointMode::Remote(logs_dir.path().to_path_buf())),
        base_sha: Some(base_sha),
        run_branch: Some(branch_name),
        meta_branch: None,
        labels: std::collections::HashMap::new(),
    };

    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("daytona parallel pipeline should succeed");
    assert_eq!(
        outcome.status,
        StageStatus::Success,
        "pipeline failed: {:?}",
        outcome.failure_reason
    );

    // Verify parallel.results has head_sha for each branch
    let checkpoint =
        Checkpoint::load(&logs_dir.path().join("checkpoint.json")).expect("checkpoint should load");
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
                    arc_workflows::event::PipelineEvent::ParallelStarted { .. }
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
                    arc_workflows::event::PipelineEvent::ParallelCompleted { .. }
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

use arc_workflows::cli::cli_backend::AgentCliBackend;
use arc_workflows::handler::codergen::{CodergenBackend, CodergenResult};

/// Helper: run a real CLI backend test on Daytona.
///
/// Installs the CLI tool in the sandbox, then runs the AgentCliBackend against it.
async fn run_daytona_cli_test(provider: Provider, model: &str, install_command: &str) {
    let env = create_env().await;
    env.initialize().await.unwrap();
    let env: Arc<dyn ExecutionEnvironment> = Arc::new(env);

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
        "curl -fsSL https://claude.ai/install.sh | sh",
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
// Daytona shadow commit E2E — Remote mode with MetadataStore
// ---------------------------------------------------------------------------

use arc_workflows::git::MetadataStore;

/// End-to-end test: pipeline with `GitCheckpointMode::Remote(host_repo_path)` + `meta_branch`
/// writes shadow branch on the host repo and includes `Arc-Checkpoint` trailer in sandbox commits.
#[tokio::test]
#[ignore]
async fn daytona_git_checkpoint_with_shadow_branch() {
    let env = create_env().await;
    env.initialize().await.unwrap();
    let env: Arc<dyn ExecutionEnvironment> = Arc::new(env);

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
    // Write graph.dot so init_run can read it
    std::fs::write(dir.path().join("graph.dot"), "digraph {}").unwrap();

    let mut registry = HandlerRegistry::new(Box::new(FileWriterHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let meta_branch = MetadataStore::branch_name(&run_id);
    let engine = PipelineEngine::new(registry, Arc::new(EventEmitter::new()), env.clone());
    let config = RunConfig {
        logs_root: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: run_id.clone(),
        git_checkpoint: Some(GitCheckpointMode::Remote(host_repo.path().to_path_buf())),
        base_sha: Some(base_sha),
        run_branch: Some(branch_name),
        meta_branch: Some(meta_branch),
        labels: std::collections::HashMap::new(),
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

    // Assert sandbox commit has Arc-Checkpoint trailer
    let log_result = env
        .exec_command("git log --format=%B -1", 10_000, None, None, None)
        .await
        .expect("git log should succeed");
    assert_eq!(log_result.exit_code, 0);
    let commit_msg = log_result.stdout.trim().to_string();
    assert!(
        commit_msg.contains("Arc-Checkpoint:"),
        "sandbox commit should have Arc-Checkpoint trailer, got:\n{commit_msg}"
    );
    assert!(
        commit_msg.contains("Arc-Run:"),
        "sandbox commit should have Arc-Run trailer, got:\n{commit_msg}"
    );

    // Assert final.patch exists
    let final_patch = dir.path().join("final.patch");
    assert!(
        final_patch.exists(),
        "final.patch should exist in logs_root"
    );

    env.cleanup().await.unwrap();
}
