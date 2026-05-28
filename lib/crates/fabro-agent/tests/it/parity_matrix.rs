#![expect(
    clippy::disallowed_methods,
    reason = "agent parity test harness: sync std::fs for staging fixture trees and reading captured outputs"
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use fabro_agent::subagent::SessionFactory;
use fabro_agent::{
    AgentEvent, AgentProfile, AnthropicProfile, GeminiProfile, LocalSandbox, OpenAiProfile,
    Session, SessionOptions, SubAgentManager, WebFetchSummarizer,
};
use fabro_auth::EnvCredentialSource;
use fabro_llm::client::Client;
use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::{OpenAiAdapter, OpenAiCompatibleAdapter};
use fabro_model::{Catalog, ModelHandle, ProviderId};
use fabro_test::{TwinScenario, TwinScenarios, TwinToolCall, twin_openai};
use tokio::sync::Mutex as AsyncMutex;

type Provider = ProviderId;

#[derive(Clone)]
struct OpenAiTwinOptions {
    base_url: String,
    api_key:  String,
}

fn summarizer_model_id(provider: &Provider) -> ModelHandle {
    match provider.as_str() {
        ProviderId::OPENAI | "kimi" | "zai" | "minimax" | "inception" => ModelHandle::ByName {
            provider: ProviderId::openai(),
            model:    "gpt-5.4-mini".to_string(),
        },
        ProviderId::GEMINI => ModelHandle::ByName {
            provider: ProviderId::gemini(),
            model:    "gemini-3-flash-preview".to_string(),
        },
        ProviderId::ANTHROPIC => ModelHandle::ByName {
            provider: ProviderId::anthropic(),
            model:    "claude-haiku-4-5".to_string(),
        },
        other => panic!("unexpected provider {other}"),
    }
}

fn build_summarizer(provider: &Provider, client: &Client) -> WebFetchSummarizer {
    WebFetchSummarizer {
        client:   client.clone(),
        model_id: summarizer_model_id(provider),
    }
}

fn build_profile(provider: &Provider, model: &str, client: &Client) -> Box<dyn AgentProfile> {
    let summarizer = Some(build_summarizer(provider, client));
    match provider.as_str() {
        ProviderId::ANTHROPIC => Box::new(AnthropicProfile::with_summarizer(model, summarizer)),
        ProviderId::OPENAI => Box::new(OpenAiProfile::with_summarizer(model, summarizer)),
        "kimi" | "zai" | "minimax" | "inception" => Box::new(
            OpenAiProfile::with_summarizer(model, summarizer).with_provider_id(provider.clone()),
        ),
        ProviderId::GEMINI => Box::new(GeminiProfile::with_summarizer(model, summarizer)),
        other => panic!("unexpected provider {other}"),
    }
}

async fn make_session(
    provider: Provider,
    model: &str,
    cwd: &Path,
    twin: Option<OpenAiTwinOptions>,
) -> Session {
    let client = make_client(&provider, twin.as_ref()).await;
    let mut profile = build_profile(&provider, model, &client);
    let env = Arc::new(LocalSandbox::new(cwd.to_path_buf()));

    // Register subagent tools so spawn_agent / wait / send_input / close_agent are
    // available
    let manager = Arc::new(AsyncMutex::new(SubAgentManager::new(3)));
    let factory_client = client.clone();
    let factory_model: String = model.to_string();
    let factory_cwd = cwd.to_path_buf();
    let factory_provider = provider.clone();
    let factory: SessionFactory = Arc::new(move || {
        let sub_profile: Arc<dyn AgentProfile> = {
            let summarizer = Some(build_summarizer(&factory_provider, &factory_client));
            match factory_provider.as_str() {
                ProviderId::ANTHROPIC => Arc::new(AnthropicProfile::with_summarizer(
                    &factory_model,
                    summarizer,
                )),
                ProviderId::OPENAI => {
                    Arc::new(OpenAiProfile::with_summarizer(&factory_model, summarizer))
                }
                "kimi" | "zai" | "minimax" | "inception" => Arc::new(
                    OpenAiProfile::with_summarizer(&factory_model, summarizer)
                        .with_provider_id(factory_provider.clone()),
                ),
                ProviderId::GEMINI => {
                    Arc::new(GeminiProfile::with_summarizer(&factory_model, summarizer))
                }
                other => panic!("unexpected provider {other}"),
            }
        };
        let sub_env = Arc::new(LocalSandbox::new(factory_cwd.clone()));
        Session::new(
            factory_client.clone(),
            sub_profile,
            sub_env,
            SessionOptions::default(),
            None,
        )
    });
    profile.register_subagent_tools(manager, factory, 0);

    let profile: Arc<dyn AgentProfile> = Arc::from(profile);
    let config = SessionOptions {
        max_turns: 20,
        ..SessionOptions::default()
    };
    Session::new(client, profile, env, config, None)
}

async fn make_session_with_config(
    provider: Provider,
    model: &str,
    cwd: &Path,
    config: SessionOptions,
    twin: Option<OpenAiTwinOptions>,
) -> Session {
    let client = make_client(&provider, twin.as_ref()).await;
    let profile: Arc<dyn AgentProfile> = Arc::from(build_profile(&provider, model, &client));
    let env = Arc::new(LocalSandbox::new(cwd.to_path_buf()));
    Session::new(client, profile, env, config, None)
}

async fn make_client(provider: &Provider, twin: Option<&OpenAiTwinOptions>) -> Client {
    if provider == &ProviderId::openai() && fabro_test::TestMode::from_env().is_twin() {
        return make_twin_client(twin.expect("openai twin config should be provided"));
    }

    let source = EnvCredentialSource::new();
    let catalog = Arc::new(Catalog::from_builtin().expect("default catalog should build"));
    Client::from_source(&source, catalog)
        .await
        .expect("Client::from_source failed")
}

fn make_twin_client(twin: &OpenAiTwinOptions) -> Client {
    let adapter: Arc<dyn ProviderAdapter> =
        Arc::new(OpenAiAdapter::new(twin.api_key.clone()).with_base_url(twin.base_url.clone()));
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("openai".to_string(), adapter);
    Client::new(providers, Some("openai".to_string()), Vec::new())
}

fn make_openai_compatible_twin_client(provider: &Provider, twin: &OpenAiTwinOptions) -> Client {
    let provider_name = provider.to_string();
    let adapter: Arc<dyn ProviderAdapter> = Arc::new(
        OpenAiCompatibleAdapter::new(twin.api_key.clone(), twin.base_url.clone())
            .with_name(provider_name.clone()),
    );
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert(provider_name.clone(), adapter);
    Client::new(providers, Some(provider_name), Vec::new())
}

fn make_openai_compatible_twin_session(
    provider: Provider,
    model: &str,
    cwd: &Path,
    config: SessionOptions,
    twin: &OpenAiTwinOptions,
) -> Session {
    let client = make_openai_compatible_twin_client(&provider, twin);
    let profile: Arc<dyn AgentProfile> =
        Arc::new(OpenAiProfile::new(model).with_provider_id(provider));
    let env = Arc::new(LocalSandbox::new(cwd.to_path_buf()));
    Session::new(client, profile, env, config, None)
}

macro_rules! provider_test {
    ($scenario:ident, $provider:expr, $model:expr, $prefix:ident, keys = [$($key:expr),+ $(,)?]) => {
        paste::paste! {
            #[fabro_macros::e2e_test($(live($key)),+)]
            async fn [<$prefix _ $scenario>]() {
                let tmp = tempfile::tempdir().expect("failed to create tempdir");
                let mut session = make_session($provider, $model, tmp.path(), None).await;
                session.initialize().await.unwrap();
                [<scenario_ $scenario>](&mut session, tmp.path()).await;
            }
        }
    };
}

macro_rules! openai_twin_provider_test {
    ($scenario:ident) => {
        paste::paste! {
            #[fabro_macros::e2e_test(twin, live("OPENAI_API_KEY"))]
            async fn [<openai_twin_ $scenario>]() {
                let tmp = tempfile::tempdir().expect("failed to create tempdir");
                let (base_url, api_key) = fabro_test::e2e_openai!();
                let twin = OpenAiTwinOptions { base_url, api_key };
                if fabro_test::TestMode::from_env().is_twin() {
                    load_openai_twin_scenario(stringify!($scenario), &twin.api_key, tmp.path())
                        .await;
                }
                let mut session = make_session(
                    ProviderId::openai(),
                    "gpt-5.4-mini",
                    tmp.path(),
                    Some(twin),
                ).await;
                session.initialize().await.unwrap();
                [<scenario_ $scenario>](&mut session, tmp.path()).await;
            }
        }
    };
}

macro_rules! provider_tests {
    ($scenario:ident) => {
        provider_test!(
            $scenario,
            ProviderId::anthropic(),
            "claude-haiku-4-5",
            anthropic,
            keys = ["ANTHROPIC_API_KEY"]
        );
        provider_test!(
            $scenario,
            ProviderId::gemini(),
            "gemini-3-flash-preview",
            gemini,
            keys = ["GEMINI_API_KEY"]
        );
        provider_test!(
            $scenario,
            ProviderId::new("kimi"),
            "kimi-k2.5",
            kimi,
            keys = ["KIMI_API_KEY"]
        );
        #[cfg(feature = "quarantine")]
        provider_test!(
            $scenario,
            ProviderId::new("zai"),
            "glm-4.7",
            zai,
            keys = ["ZAI_API_KEY"]
        );
        provider_test!(
            $scenario,
            ProviderId::new("minimax"),
            "minimax-m2.5",
            minimax,
            keys = ["MINIMAX_API_KEY"]
        );
        #[cfg(feature = "quarantine")]
        provider_test!(
            $scenario,
            ProviderId::new("inception"),
            "mercury-2",
            inception,
            keys = ["INCEPTION_API_KEY"]
        );
    };
}

#[fabro_macros::e2e_test(twin)]
async fn openai_compatible_twin_preserves_raw_apply_patch_arguments() {
    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let file_path = tmp.path().join("data.txt");
    std::fs::write(&file_path, "old\n").expect("failed to write data.txt");

    let (base_url, api_key) = fabro_test::e2e_openai!();
    let twin = OpenAiTwinOptions { base_url, api_key };
    let patch = "\
*** Begin Patch
*** Update File: data.txt
@@
-old
+new
*** End Patch
";

    TwinScenarios::new(twin.api_key.clone())
        .scenario(
            TwinScenario::chat_completions("gpt-5.4-mini")
                .input_contains("Replace old with new")
                .tool_call(TwinToolCall::apply_patch_raw_arguments(patch)),
        )
        .load(twin_openai().await)
        .await;

    let config = SessionOptions {
        max_turns: 2,
        ..SessionOptions::default()
    };
    let mut session = make_openai_compatible_twin_session(
        ProviderId::new("litellm"),
        "gpt-5.4-mini",
        tmp.path(),
        config,
        &twin,
    );
    session.initialize().await.unwrap();
    let mut rx = session.subscribe();

    session
        .process_input("Replace old with new in data.txt using apply_patch")
        .await
        .expect("process_input failed");

    let mut tool_results = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::ToolCallCompleted {
            tool_name,
            output,
            is_error,
            ..
        } = event.event
        {
            tool_results.push(format!("{tool_name}: is_error={is_error} output={output}"));
        }
    }

    let content = std::fs::read_to_string(file_path).expect("failed to read data.txt");
    assert_eq!(content, "new\n", "tool results: {tool_results:#?}");
}

provider_tests!(simple_file_creation);
openai_twin_provider_test!(simple_file_creation);
provider_tests!(read_and_edit_file);
openai_twin_provider_test!(read_and_edit_file);
provider_tests!(multi_file_edit);
openai_twin_provider_test!(multi_file_edit);
provider_tests!(shell_execution);
openai_twin_provider_test!(shell_execution);
provider_tests!(shell_timeout);
openai_twin_provider_test!(shell_timeout);
provider_tests!(grep_and_glob);
openai_twin_provider_test!(grep_and_glob);
provider_tests!(tool_output_truncation);
openai_twin_provider_test!(tool_output_truncation);
provider_tests!(parallel_tool_calls);
openai_twin_provider_test!(parallel_tool_calls);
provider_tests!(steering_before_input);
openai_twin_provider_test!(steering_before_input);
provider_tests!(steering_mid_task);
provider_tests!(follow_up);
openai_twin_provider_test!(follow_up);
provider_tests!(subagent_spawn);

provider_test!(
    web_fetch,
    ProviderId::anthropic(),
    "claude-haiku-4-5",
    anthropic,
    keys = ["ANTHROPIC_API_KEY"]
);
provider_test!(
    web_fetch,
    ProviderId::openai(),
    "gpt-5.4-mini",
    openai,
    keys = ["OPENAI_API_KEY"]
);
provider_test!(
    web_fetch,
    ProviderId::gemini(),
    "gemini-3-flash-preview",
    gemini,
    keys = ["GEMINI_API_KEY"]
);
provider_test!(
    web_fetch,
    ProviderId::new("kimi"),
    "kimi-k2.5",
    kimi,
    keys = ["KIMI_API_KEY", "OPENAI_API_KEY"]
);
#[cfg(feature = "quarantine")]
provider_test!(
    web_fetch,
    ProviderId::new("zai"),
    "glm-4.7",
    zai,
    keys = ["ZAI_API_KEY", "OPENAI_API_KEY"]
);
provider_test!(
    web_fetch,
    ProviderId::new("minimax"),
    "minimax-m2.5",
    minimax,
    keys = ["MINIMAX_API_KEY", "OPENAI_API_KEY"]
);
#[cfg(feature = "quarantine")]
provider_test!(
    web_fetch,
    ProviderId::new("inception"),
    "mercury-2",
    inception,
    keys = ["INCEPTION_API_KEY", "OPENAI_API_KEY"]
);

provider_test!(
    web_search,
    ProviderId::anthropic(),
    "claude-haiku-4-5",
    anthropic,
    keys = ["ANTHROPIC_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
provider_test!(
    web_search,
    ProviderId::openai(),
    "gpt-5.4-mini",
    openai,
    keys = ["OPENAI_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
provider_test!(
    web_search,
    ProviderId::gemini(),
    "gemini-3-flash-preview",
    gemini,
    keys = ["GEMINI_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
provider_test!(
    web_search,
    ProviderId::new("kimi"),
    "kimi-k2.5",
    kimi,
    keys = ["KIMI_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
#[cfg(feature = "quarantine")]
provider_test!(
    web_search,
    ProviderId::new("zai"),
    "glm-4.7",
    zai,
    keys = ["ZAI_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
provider_test!(
    web_search,
    ProviderId::new("minimax"),
    "minimax-m2.5",
    minimax,
    keys = ["MINIMAX_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
#[cfg(feature = "quarantine")]
provider_test!(
    web_search,
    ProviderId::new("inception"),
    "mercury-2",
    inception,
    keys = ["INCEPTION_API_KEY", "BRAVE_SEARCH_API_KEY"]
);

// Scenarios below are only generated for providers where they are supported.
// - multi_step_read_analyze_edit / provider_specific_editing: gpt-4o-mini is
//   too weak to reliably apply precise file edits (uses apply_patch, not
//   edit_file).
// - reasoning_effort: gpt-4o-mini doesn't support the reasoning.effort
//   parameter.
// - loop_detection: needs custom config, tested separately below.

provider_tests!(error_recovery);
openai_twin_provider_test!(error_recovery);

// gpt-5-mini is too weak to reliably apply precise file edits (uses
// apply_patch, not edit_file).
macro_rules! non_openai_provider_tests {
    ($scenario:ident) => {
        provider_test!(
            $scenario,
            ProviderId::anthropic(),
            "claude-haiku-4-5",
            anthropic,
            keys = ["ANTHROPIC_API_KEY"]
        );
        provider_test!(
            $scenario,
            ProviderId::gemini(),
            "gemini-3-flash-preview",
            gemini,
            keys = ["GEMINI_API_KEY"]
        );
        provider_test!(
            $scenario,
            ProviderId::new("kimi"),
            "kimi-k2.5",
            kimi,
            keys = ["KIMI_API_KEY"]
        );
        #[cfg(feature = "quarantine")]
        provider_test!(
            $scenario,
            ProviderId::new("zai"),
            "glm-4.7",
            zai,
            keys = ["ZAI_API_KEY"]
        );
        provider_test!(
            $scenario,
            ProviderId::new("minimax"),
            "minimax-m2.5",
            minimax,
            keys = ["MINIMAX_API_KEY"]
        );
        #[cfg(feature = "quarantine")]
        provider_test!(
            $scenario,
            ProviderId::new("inception"),
            "mercury-2",
            inception,
            keys = ["INCEPTION_API_KEY"]
        );
    };
}

non_openai_provider_tests!(multi_step_read_analyze_edit);
non_openai_provider_tests!(provider_specific_editing);

// ---------------------------------------------------------------------------
// Scenario 1: simple_file_creation
// ---------------------------------------------------------------------------
async fn scenario_simple_file_creation(session: &mut Session, dir: &Path) {
    session
        .process_input("Create a file called hello.txt containing 'Hello'")
        .await
        .expect("process_input failed");
    assert!(dir.join("hello.txt").exists());
}

// ---------------------------------------------------------------------------
// Scenario 2: read_and_edit_file
// ---------------------------------------------------------------------------
async fn scenario_read_and_edit_file(session: &mut Session, dir: &Path) {
    std::fs::write(dir.join("data.txt"), "old content").expect("failed to write data.txt");
    session
        .process_input("Read data.txt and replace its content with 'new content'")
        .await
        .expect("process_input failed");
    let content = std::fs::read_to_string(dir.join("data.txt")).expect("failed to read data.txt");
    assert!(
        content.contains("new content"),
        "Expected 'new content' in file, got: {content}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: multi_file_edit
// ---------------------------------------------------------------------------
async fn scenario_multi_file_edit(session: &mut Session, dir: &Path) {
    std::fs::write(dir.join("a.txt"), "aaa").expect("failed to write a.txt");
    std::fs::write(dir.join("b.txt"), "bbb").expect("failed to write b.txt");
    session
        .process_input(
            "Read a.txt and b.txt, then replace the content of a.txt with 'AAA' and b.txt with 'BBB'",
        )
        .await
        .expect("process_input failed");
    let a = std::fs::read_to_string(dir.join("a.txt")).expect("failed to read a.txt");
    let b = std::fs::read_to_string(dir.join("b.txt")).expect("failed to read b.txt");
    assert!(a.contains("AAA"), "Expected 'AAA' in a.txt, got: {a}");
    assert!(b.contains("BBB"), "Expected 'BBB' in b.txt, got: {b}");
}

// ---------------------------------------------------------------------------
// Scenario 4: shell_execution
// ---------------------------------------------------------------------------
async fn scenario_shell_execution(session: &mut Session, _dir: &Path) {
    session
        .process_input(
            "Run the command `echo hello_from_shell` in the shell and tell me what it printed",
        )
        .await
        .expect("process_input failed");
}

// ---------------------------------------------------------------------------
// Scenario 5: shell_timeout
// ---------------------------------------------------------------------------
async fn scenario_shell_timeout(session: &mut Session, _dir: &Path) {
    session
        .process_input("Run the command `sleep 999` with a 1-second timeout")
        .await
        .expect("process_input failed");
}

// ---------------------------------------------------------------------------
// Scenario 6: grep_and_glob
// ---------------------------------------------------------------------------
async fn scenario_grep_and_glob(session: &mut Session, dir: &Path) {
    std::fs::write(dir.join("target.txt"), "needle_pattern_xyz")
        .expect("failed to write target.txt");
    std::fs::write(dir.join("other.txt"), "nothing").expect("failed to write other.txt");
    session
        .process_input(
            "Search for files containing 'needle_pattern_xyz' and tell me which file has it",
        )
        .await
        .expect("process_input failed");
}

// ---------------------------------------------------------------------------
// Scenario 7: multi_step_read_analyze_edit
// ---------------------------------------------------------------------------
async fn scenario_multi_step_read_analyze_edit(session: &mut Session, dir: &Path) {
    std::fs::write(
        dir.join("buggy.rs"),
        "fn add(a: i32, b: i32) -> i32 { a - b }",
    )
    .expect("failed to write buggy.rs");
    session
        .process_input("Read buggy.rs, find the bug, and fix it")
        .await
        .expect("process_input failed");
    let content = std::fs::read_to_string(dir.join("buggy.rs")).expect("failed to read buggy.rs");
    assert!(
        content.contains("a + b"),
        "Expected 'a + b' in buggy.rs, got: {content}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 8: tool_output_truncation
// ---------------------------------------------------------------------------
async fn scenario_tool_output_truncation(session: &mut Session, dir: &Path) {
    let lines = (1..=10_000).fold(String::new(), |mut acc, n| {
        let _ = writeln!(acc, "line {n}");
        acc
    });
    std::fs::write(dir.join("big.txt"), lines).expect("failed to write big.txt");
    session
        .process_input("Read the file big.txt and tell me how many lines it has")
        .await
        .expect("process_input failed");
}

// ---------------------------------------------------------------------------
// Scenario 9: parallel_tool_calls
// ---------------------------------------------------------------------------
async fn scenario_parallel_tool_calls(session: &mut Session, dir: &Path) {
    std::fs::write(dir.join("one.txt"), "content_one").expect("failed to write one.txt");
    std::fs::write(dir.join("two.txt"), "content_two").expect("failed to write two.txt");
    std::fs::write(dir.join("three.txt"), "content_three").expect("failed to write three.txt");
    session
        .process_input("Read one.txt, two.txt, and three.txt and tell me what each contains")
        .await
        .expect("process_input failed");
}

// ---------------------------------------------------------------------------
// Scenario 10a: steering_before_input
// ---------------------------------------------------------------------------
async fn scenario_steering_before_input(session: &mut Session, _dir: &Path) {
    session.steer("Stop counting and just say DONE".to_string());
    session
        .process_input("Count from 1 to 100, one number per line")
        .await
        .expect("process_input failed");
}

// ---------------------------------------------------------------------------
// Scenario 10b: steering_mid_task
// ---------------------------------------------------------------------------
async fn scenario_steering_mid_task(session: &mut Session, dir: &Path) {
    // Setup: create a file the LLM will read (triggering a tool call)
    std::fs::write(dir.join("task.txt"), "read me first").expect("write task.txt");

    // Grab handle before process_input borrows &mut self
    let control = session.control_handle();
    let mut rx = session.subscribe();

    // Spawn a task that waits for the first tool call, then injects steering
    let steer_task = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            if matches!(
                event.event,
                fabro_agent::AgentEvent::ToolCallCompleted { .. }
            ) {
                control.steer(
                    "Stop what you are doing. Create a file called steered.txt containing 'steered' and do nothing else.".to_string(),
                    None,
                );
                break;
            }
        }
    });

    session
        .process_input(
            "Read task.txt, then create files a.txt, b.txt, c.txt, d.txt, e.txt each containing their letter",
        )
        .await
        .expect("process_input failed");

    steer_task.await.expect("steer task panicked");

    // The steering message should have redirected the LLM to create steered.txt
    assert!(
        dir.join("steered.txt").exists(),
        "steered.txt should exist — steering mid-task should have redirected the LLM"
    );
}

// ---------------------------------------------------------------------------
// Scenario 10c: follow_up
// ---------------------------------------------------------------------------
async fn scenario_follow_up(session: &mut Session, dir: &Path) {
    session.follow_up("Create a file called second.txt containing 'second'".to_string());
    session
        .process_input("Create a file called first.txt containing 'first'")
        .await
        .expect("process_input failed");

    let first = dir.join("first.txt");
    let second = dir.join("second.txt");
    assert!(first.exists(), "first.txt should exist");
    assert!(second.exists(), "second.txt should exist");
    let first_content = std::fs::read_to_string(&first).expect("read first.txt");
    let second_content = std::fs::read_to_string(&second).expect("read second.txt");
    assert!(
        first_content.contains("first"),
        "first.txt should contain 'first', got: {first_content}"
    );
    assert!(
        second_content.contains("second"),
        "second.txt should contain 'second', got: {second_content}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 11: reasoning_effort
// ---------------------------------------------------------------------------
macro_rules! reasoning_effort_tests {
    ($provider:expr, $model:expr, $test_name:ident, keys = [$($key:expr),+ $(,)?]) => {
        #[fabro_macros::e2e_test($(live($key)),+)]
        async fn $test_name() {
            let tmp = tempfile::tempdir().expect("failed to create tempdir");
            let config = SessionOptions {
                max_turns: 20,
                reasoning_effort: Some(fabro_llm::types::ReasoningEffort::Low),
                ..SessionOptions::default()
            };
            let mut session =
                make_session_with_config($provider, $model, tmp.path(), config, None).await;
            session.initialize().await.unwrap();
            session
                .process_input("Say hello")
                .await
                .expect("process_input failed");
        }
    };
}

reasoning_effort_tests!(
    ProviderId::anthropic(),
    "claude-haiku-4-5",
    anthropic_reasoning_effort,
    keys = ["ANTHROPIC_API_KEY"]
);
// gpt-5-mini does not support the reasoning.effort parameter, so no OpenAI
// test.
reasoning_effort_tests!(
    ProviderId::gemini(),
    "gemini-3-flash-preview",
    gemini_reasoning_effort,
    keys = ["GEMINI_API_KEY"]
);
reasoning_effort_tests!(
    ProviderId::new("kimi"),
    "kimi-k2.5",
    kimi_reasoning_effort,
    keys = ["KIMI_API_KEY"]
);
#[cfg(feature = "quarantine")]
reasoning_effort_tests!(
    ProviderId::new("zai"),
    "glm-4.7",
    zai_reasoning_effort,
    keys = ["ZAI_API_KEY"]
);
reasoning_effort_tests!(
    ProviderId::new("minimax"),
    "minimax-m2.5",
    minimax_reasoning_effort,
    keys = ["MINIMAX_API_KEY"]
);
#[cfg(feature = "quarantine")]
reasoning_effort_tests!(
    ProviderId::new("inception"),
    "mercury-2",
    inception_reasoning_effort,
    keys = ["INCEPTION_API_KEY"]
);

// ---------------------------------------------------------------------------
// Scenario 12: subagent_spawn
// ---------------------------------------------------------------------------
async fn scenario_subagent_spawn(session: &mut Session, dir: &Path) {
    std::fs::write(dir.join("secret.txt"), "the_secret_value").expect("failed to write secret.txt");
    session
        .process_input(
            "Spawn a subagent to read the file secret.txt and report its contents. \
             Wait for the subagent to finish, then tell me what it found.",
        )
        .await
        .expect("process_input failed");
}

// ---------------------------------------------------------------------------
// Scenario 13: loop_detection
// ---------------------------------------------------------------------------
macro_rules! loop_detection_tests {
    ($provider:expr, $model:expr, $test_name:ident, keys = [$($key:expr),+ $(,)?]) => {
        #[fabro_macros::e2e_test($(live($key)),+)]
        async fn $test_name() {
            let tmp = tempfile::tempdir().expect("failed to create tempdir");
            let config = SessionOptions {
                max_turns: 20,
                loop_detection_window: 3,
                ..SessionOptions::default()
            };
            let mut session =
                make_session_with_config($provider, $model, tmp.path(), config, None).await;
            session.initialize().await.unwrap();
            session
                .process_input("Repeatedly read the file /dev/null")
                .await
                .expect("process_input failed");
        }
    };
}

loop_detection_tests!(
    ProviderId::anthropic(),
    "claude-haiku-4-5",
    anthropic_loop_detection,
    keys = ["ANTHROPIC_API_KEY"]
);
loop_detection_tests!(
    ProviderId::openai(),
    "gpt-5.4-mini",
    openai_loop_detection,
    keys = ["OPENAI_API_KEY"]
);
loop_detection_tests!(
    ProviderId::gemini(),
    "gemini-3-flash-preview",
    gemini_loop_detection,
    keys = ["GEMINI_API_KEY"]
);
loop_detection_tests!(
    ProviderId::new("kimi"),
    "kimi-k2.5",
    kimi_loop_detection,
    keys = ["KIMI_API_KEY"]
);
#[cfg(feature = "quarantine")]
loop_detection_tests!(
    ProviderId::new("zai"),
    "glm-4.7",
    zai_loop_detection,
    keys = ["ZAI_API_KEY"]
);
loop_detection_tests!(
    ProviderId::new("minimax"),
    "minimax-m2.5",
    minimax_loop_detection,
    keys = ["MINIMAX_API_KEY"]
);
#[cfg(feature = "quarantine")]
loop_detection_tests!(
    ProviderId::new("inception"),
    "mercury-2",
    inception_loop_detection,
    keys = ["INCEPTION_API_KEY"]
);

async fn load_openai_twin_scenario(name: &str, namespace: &str, cwd: &Path) {
    let scenarios = match name {
        "simple_file_creation" => TwinScenarios::new(namespace.to_string()).scenario(
            TwinScenario::responses("gpt-5.4-mini")
                .input_contains("Create a file called hello.txt containing 'Hello'")
                .tool_call(TwinToolCall::write_file("hello.txt", "Hello"))
                .text("Done."),
        ),
        "read_and_edit_file" => TwinScenarios::new(namespace.to_string())
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .input_contains("Read data.txt and replace its content with 'new content'")
                    .tool_call(TwinToolCall::read_file("data.txt")),
            )
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .tool_call(TwinToolCall::write_file("data.txt", "new content"))
                    .text("Done."),
            ),
        "multi_file_edit" => TwinScenarios::new(namespace.to_string())
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .input_contains(
                        "Read a.txt and b.txt, then replace the content of a.txt with 'AAA' and b.txt with 'BBB'",
                    )
                    .tool_calls(vec![
                        TwinToolCall::read_file("a.txt"),
                        TwinToolCall::read_file("b.txt"),
                    ]),
            )
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .tool_calls(vec![
                        TwinToolCall::write_file("a.txt", "AAA"),
                        TwinToolCall::write_file("b.txt", "BBB"),
                    ])
                    .text("Done."),
            ),
        "shell_execution" => TwinScenarios::new(namespace.to_string()).scenario(
            TwinScenario::responses("gpt-5.4-mini")
                .input_contains(
                    "Run the command `echo hello_from_shell` in the shell and tell me what it printed",
                )
                .tool_call(TwinToolCall::shell("echo hello_from_shell"))
                .text("It printed hello_from_shell."),
        ),
        "shell_timeout" => TwinScenarios::new(namespace.to_string()).scenario(
            TwinScenario::responses("gpt-5.4-mini")
                .input_contains("Run the command `sleep 999` with a 1-second timeout")
                .tool_call(TwinToolCall::shell_with_timeout("sleep 999", 1000))
                .text("The command timed out."),
        ),
        "grep_and_glob" => TwinScenarios::new(namespace.to_string())
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .input_contains(
                        "Search for files containing 'needle_pattern_xyz' and tell me which file has it",
                    )
                    .tool_calls(vec![
                        TwinToolCall::glob_pattern("*.txt", cwd.display().to_string()),
                        TwinToolCall::grep_pattern("needle_pattern_xyz", "."),
                    ]),
            )
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .text("target.txt contains needle_pattern_xyz."),
            ),
        "tool_output_truncation" => TwinScenarios::new(namespace.to_string()).scenario(
            TwinScenario::responses("gpt-5.4-mini")
                .input_contains("Read the file big.txt and tell me how many lines it has")
                .tool_call(TwinToolCall::read_file("big.txt"))
                .text("The file has 10000 lines."),
        ),
        "parallel_tool_calls" => TwinScenarios::new(namespace.to_string()).scenario(
            TwinScenario::responses("gpt-5.4-mini")
                .input_contains("Read one.txt, two.txt, and three.txt and tell me what each contains")
                .tool_calls(vec![
                    TwinToolCall::read_file("one.txt"),
                    TwinToolCall::read_file("two.txt"),
                    TwinToolCall::read_file("three.txt"),
                ])
                .text("one: content_one, two: content_two, three: content_three"),
        ),
        "steering_before_input" => TwinScenarios::new(namespace.to_string()).scenario(
            TwinScenario::responses("gpt-5.4-mini")
                .input_contains("Count from 1 to 100, one number per line")
                .text("DONE"),
        ),
        "follow_up" => TwinScenarios::new(namespace.to_string())
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .input_contains("Create a file called first.txt containing 'first'")
                    .tool_call(TwinToolCall::write_file("first.txt", "first"))
                    .text("Created first.txt."),
            )
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .input_contains("Create a file called second.txt containing 'second'")
                    .tool_call(TwinToolCall::write_file("second.txt", "second"))
                    .text("Created second.txt."),
            ),
        "error_recovery" => TwinScenarios::new(namespace.to_string())
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .input_contains(
                        "Try to read a file called nonexistent_file.txt. If it doesn't exist, create it with the content 'recovered'",
                    )
                    .tool_call(TwinToolCall::read_file("nonexistent_file.txt")),
            )
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .tool_call(TwinToolCall::write_file("nonexistent_file.txt", "recovered"))
                    .text("Created the file."),
            ),
        other => panic!("missing openai twin scenario for {other}"),
    };

    scenarios.load(twin_openai().await).await;
}

// ---------------------------------------------------------------------------
// Scenario 14: error_recovery
// ---------------------------------------------------------------------------
async fn scenario_error_recovery(session: &mut Session, dir: &Path) {
    session
        .process_input(
            "Try to read a file called nonexistent_file.txt. If it doesn't exist, create it with the content 'recovered'",
        )
        .await
        .expect("process_input failed");
    let path = dir.join("nonexistent_file.txt");
    assert!(
        path.exists(),
        "nonexistent_file.txt should have been created"
    );
    let content = std::fs::read_to_string(&path).expect("failed to read nonexistent_file.txt");
    assert!(
        content.contains("recovered"),
        "Expected 'recovered' in file, got: {content}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 15: web_fetch
// ---------------------------------------------------------------------------
async fn scenario_web_fetch(session: &mut Session, dir: &Path) {
    // Test basic fetch (HTML-to-markdown conversion)
    session
        .process_input(
            "Use the web_fetch tool to fetch https://example.com and write its content to a file called fetched.txt",
        )
        .await
        .expect("process_input failed");
    let path = dir.join("fetched.txt");
    assert!(path.exists(), "fetched.txt should have been created");
    let content = std::fs::read_to_string(&path).expect("failed to read fetched.txt");
    let lower = content.to_lowercase();
    assert!(
        lower.contains("example domain")
            || lower.contains("example.com")
            || lower.contains("example")
                && (lower.contains("documentation")
                    || lower.contains("iana")
                    || lower.contains("illustrative")),
        "Expected content related to example.com, got first 200 chars: {}",
        &content[..content.len().min(200)]
    );

    // Test fetch with prompt parameter (LLM summarization)
    session
        .process_input(
            "Use the web_fetch tool with the prompt parameter to fetch https://example.com and answer: 'What is the title heading on this page?' Write only the answer to a file called answer.txt",
        )
        .await
        .expect("process_input failed for prompt test");
    let answer_path = dir.join("answer.txt");
    assert!(answer_path.exists(), "answer.txt should have been created");
    let answer = std::fs::read_to_string(&answer_path).expect("failed to read answer.txt");
    assert!(
        answer.to_lowercase().contains("example domain")
            || answer.to_lowercase().contains("example"),
        "Expected answer to mention 'example domain' or 'example', got: {answer}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 16: web_search
// ---------------------------------------------------------------------------
async fn scenario_web_search(session: &mut Session, dir: &Path) {
    session
        .process_input(
            "Use the web_search tool to search for 'Rust programming language' and write the first result's title and URL to a file called search_results.txt",
        )
        .await
        .expect("process_input failed");
    let path = dir.join("search_results.txt");
    assert!(path.exists(), "search_results.txt should have been created");
    let content = std::fs::read_to_string(&path).expect("failed to read search_results.txt");
    assert!(
        !content.is_empty(),
        "search_results.txt should not be empty"
    );
}

// ---------------------------------------------------------------------------
// Scenario 17: provider_specific_editing
// ---------------------------------------------------------------------------
async fn scenario_provider_specific_editing(session: &mut Session, dir: &Path) {
    std::fs::write(dir.join("target.rs"), "fn greet() { println!(\"hello\"); }")
        .expect("failed to write target.rs");
    session
        .process_input("Edit target.rs to change 'hello' to 'goodbye'")
        .await
        .expect("process_input failed");
    let content = std::fs::read_to_string(dir.join("target.rs")).expect("failed to read target.rs");
    assert!(
        content.contains("goodbye"),
        "Expected 'goodbye' in target.rs, got: {content}"
    );
}
