use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use fabro_agent::{AgentProfile, LocalSandbox, OpenAiProfile, Session, SessionOptions};
use fabro_llm::client::Client;
use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::OpenAiAdapter;
use fabro_model::ProviderId;
use fabro_test::{TwinScenario, TwinScenarios, TwinToolCall, twin_openai};
use tokio::fs::read_to_string;

const MODEL: &str = "gpt-5.4-mini";

#[expect(
    clippy::disallowed_methods,
    reason = "e2e_openai! expands live-mode environment lookups even for twin-only tests"
)]
#[fabro_macros::e2e_test(twin)]
async fn openai_twin_compaction_preserves_tool_call_pairs() {
    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let (base_url, api_key) = fabro_test::e2e_openai!();

    load_compaction_scenarios(&api_key).await;

    let mut session = make_openai_session(tmp.path(), base_url, api_key);
    session.initialize().await.unwrap();

    let result = session
        .process_input(
            "Trigger the compaction regression by writing four small files, then say done.",
        )
        .await;

    assert!(
        result.is_ok(),
        "session should complete without sending an orphaned function_call_output: {result:?}"
    );
    assert_eq!(
        read_to_string(tmp.path().join("four.txt"))
            .await
            .expect("four.txt should be written"),
        "four"
    );
}

fn make_openai_session(cwd: &Path, base_url: String, api_key: String) -> Session {
    let adapter: Arc<dyn ProviderAdapter> =
        Arc::new(OpenAiAdapter::new(api_key).with_base_url(base_url));
    let mut providers = HashMap::new();
    providers.insert(ProviderId::OPENAI.to_string(), adapter);
    let client = Client::new(providers, Some(ProviderId::OPENAI.to_string()), Vec::new());
    let profile: Arc<dyn AgentProfile> = Arc::new(OpenAiProfile::new(MODEL));
    let sandbox = Arc::new(LocalSandbox::new(cwd.to_path_buf()));
    let options = SessionOptions {
        max_turns: 20,
        enable_context_compaction: true,
        compaction_threshold_percent: 80,
        compaction_preserve_turns: 6,
        ..SessionOptions::default()
    };

    Session::new(client, profile, sandbox, options, None)
}

async fn load_compaction_scenarios(namespace: &str) {
    TwinScenarios::new(namespace.to_string())
        .scenario(
            TwinScenario::responses(MODEL)
                .stream(true)
                .input_contains("Trigger the compaction regression")
                .tool_call(TwinToolCall::write_file("one.txt", "one")),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .stream(true)
                .tool_call(TwinToolCall::write_file("two.txt", "two")),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .stream(true)
                .tool_call(TwinToolCall::write_file("three.txt", "three")),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .stream(true)
                .tool_call(TwinToolCall::write_file("four.txt", "four"))
                .usage(180_000, 5),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .stream(false)
                .input_contains("Here is the conversation to summarize")
                .text("short summary"),
        )
        .scenario(TwinScenario::responses(MODEL).stream(true).text("Done."))
        .load(twin_openai().await)
        .await;
}
