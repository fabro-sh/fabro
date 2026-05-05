use std::sync::Arc;
use std::time::Duration;

use fabro_graphviz::graph::{AttrValue, Node};
use fabro_llm::provider::Provider;
use fabro_workflow::context::Context;
use fabro_workflow::event::Emitter;
use fabro_workflow::handler::agent::{CodergenBackend, CodergenResult};
use fabro_workflow::handler::llm::cli::AgentCliBackend;

/// Run a real CLI tool via LocalSandbox and verify the full flow.
async fn run_real_cli_test(provider: Provider, model: &str) {
    let workspace = tempfile::tempdir().expect("real CLI test workspace should create");
    let env: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
        workspace.path().to_path_buf(),
    ));
    let backend = AgentCliBackend::new_from_env(model.to_string(), provider)
        .with_poll_interval(Duration::from_millis(10));

    let mut node = Node::new("real_cli_test");
    node.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("What is 2+2? Reply with just the number.".to_string()),
    );

    let context = Context::new();
    let emitter = Arc::new(Emitter::default());
    let result = backend
        .run(
            &node,
            "What is 2+2? Reply with just the number.",
            &context,
            None,
            &emitter,
            &env,
            None,
            tokio_util::sync::CancellationToken::new(),
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
            let tokens = usage.tokens();
            assert!(
                tokens.input_tokens > 0,
                "{provider}/{model}: input_tokens should be > 0, got {}",
                tokens.input_tokens
            );
        }
        CodergenResult::Full(_) => panic!("expected Text result from {provider}/{model}"),
    }
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
