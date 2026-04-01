use std::sync::Arc;
use std::time::Duration;

use fabro_graphviz::graph::{AttrValue, Node};
use fabro_llm::provider::Provider;
use fabro_workflow::context::Context;
use fabro_workflow::event::EventEmitter;
use fabro_workflow::handler::agent::{CodergenBackend, CodergenResult};
use fabro_workflow::handler::llm::cli::AgentCliBackend;

/// Run a real CLI tool via LocalSandbox and verify the full flow.
async fn run_real_cli_test(provider: Provider, model: &str) {
    let workspace = tempfile::tempdir().unwrap();
    let env: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
        workspace.path().to_path_buf(),
    ));
    let backend = AgentCliBackend::new(model.to_string(), provider)
        .with_poll_interval(Duration::from_millis(10));

    let mut node = Node::new("real_cli_test");
    node.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("What is 2+2? Reply with just the number.".to_string()),
    );

    let context = Context::new();
    let emitter = Arc::new(EventEmitter::default());
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
        .await
        .unwrap_or_else(|_| panic!("CLI backend ({provider}/{model}) should succeed"));

    match result {
        CodergenResult::Text { text, usage, .. } => {
            assert!(
                text.contains('4'),
                "{provider}/{model}: expected response to contain '4', got: {text}"
            );
            let usage = usage.unwrap_or_else(|| panic!("{provider}/{model}: should have usage"));
            assert!(
                usage.input_tokens > 0,
                "{provider}/{model}: input_tokens should be > 0, got {}",
                usage.input_tokens
            );
        }
        CodergenResult::Full(_) => panic!("expected Text result from {provider}/{model}"),
    }

    // Verify log files were written
    let provider_path = dir.path().join("provider_used.json");
    assert!(
        provider_path.exists(),
        "{provider}/{model}: provider_used.json should exist"
    );
    let provider_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&provider_path).unwrap()).unwrap();
    assert_eq!(provider_json["mode"], "cli");
    assert_eq!(provider_json["provider"], provider.as_str());

    // Verify CLI output was streamed to stage_dir during poll
    let stdout_log = dir.path().join("cli_stdout.log");
    assert!(
        stdout_log.exists(),
        "{provider}/{model}: cli_stdout.log should be written during poll"
    );
    let stdout_content = std::fs::read_to_string(&stdout_log).unwrap();
    assert!(
        !stdout_content.is_empty(),
        "{provider}/{model}: cli_stdout.log should not be empty"
    );
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
async fn real_cli_claude() {
    run_real_cli_test(Provider::Anthropic, "haiku").await;
}

#[fabro_macros::e2e_test(live("OPENAI_API_KEY"))]
async fn real_cli_codex() {
    run_real_cli_test(Provider::OpenAi, "").await;
}

#[fabro_macros::e2e_test(live("GEMINI_API_KEY"))]
async fn real_cli_gemini() {
    run_real_cli_test(Provider::Gemini, "gemini-2.5-flash").await;
}
