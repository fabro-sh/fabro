//! Integration tests for `DaytonaExecutionEnvironment`.
//!
//! These tests require a `DAYTONA_API_KEY` environment variable and network access.
//! Run with: `cargo test --package attractor -- --ignored daytona`

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use agent::ExecutionEnvironment;
use attractor::artifact::sync_artifacts_to_env;
use attractor::checkpoint::Checkpoint;
use attractor::context::Context;
use attractor::daytona_env::{DaytonaConfig, DaytonaExecutionEnvironment};
use attractor::engine::{PipelineEngine, RunConfig};
use attractor::error::AttractorError;
use attractor::event::EventEmitter;
use attractor::graph::{AttrValue, Edge, Graph, Node};
use attractor::handler::exit::ExitHandler;
use attractor::handler::start::StartHandler;
use attractor::handler::{Handler, HandlerRegistry};
use attractor::outcome::{Outcome, StageStatus};
use llm::provider::Provider;

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
    use attractor::daytona_env::{DaytonaSnapshotConfig, DaytonaSandboxConfig};

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
            name: "attractor-test-snapshot".to_string(),
            cpu: Some(2),
            memory: Some(4),
            disk: Some(10),
            dockerfile: Some("FROM ubuntu:22.04\nRUN apt-get update && apt-get install -y ripgrep".to_string()),
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
    std::fs::write(&artifact_file, serde_json::to_string(&artifact_json).unwrap()).unwrap();

    // Build updates with a file:// pointer (as offload_large_values would)
    let pointer = format!("file://{}", artifact_file.display());
    let mut updates = HashMap::new();
    updates.insert(
        "response.plan".to_string(),
        serde_json::json!(pointer),
    );

    // Sync — the local file doesn't exist in the Daytona sandbox, so it should upload
    sync_artifacts_to_env(&mut updates, &env).await.unwrap();

    // Pointer should be rewritten to the Daytona working directory
    let new_pointer = updates["response.plan"].as_str().unwrap();
    let expected_prefix = format!(
        "file://{}/.attractor/artifacts/",
        env.working_directory()
    );
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
        _services: &attractor::handler::EngineServices,
    ) -> Result<Outcome, AttractorError> {
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
    start.attrs.insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
    graph.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert("shape".to_string(), AttrValue::String("Msquare".to_string()));
    graph.nodes.insert("exit".to_string(), exit);

    let mut big_output = Node::new("big_output");
    big_output.attrs.insert("label".to_string(), AttrValue::String("Big Output".to_string()));
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
    };

    let outcome = engine.run(&graph, &config).await.expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Checkpoint should have a pointer rewritten for Daytona
    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json"))
        .expect("checkpoint should load");
    let pointer_value = checkpoint
        .context_values
        .get("response.big_output")
        .expect("context should have response.big_output");
    let pointer_str = pointer_value.as_str().expect("pointer should be a string");
    let expected_prefix = format!(
        "file://{}/.attractor/artifacts/",
        env.working_directory()
    );
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

use attractor::cli::cli_backend::CliBackend;
use attractor::handler::codergen::{CodergenBackend, CodergenResult};

/// Helper: run a real CLI backend test on Daytona.
///
/// Installs the CLI tool in the sandbox, then runs the CliBackend against it.
async fn run_daytona_cli_test(
    provider: Provider,
    model: &str,
    install_command: &str,
) {
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

    let backend = CliBackend::new(model.to_string(), provider);
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
    let provider_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&provider_path).unwrap(),
    )
    .unwrap();
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
    run_daytona_cli_test(
        Provider::OpenAi,
        "o4-mini",
        "npm install -g @openai/codex",
    )
    .await;
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
