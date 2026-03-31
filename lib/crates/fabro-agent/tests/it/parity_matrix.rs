use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use fabro_agent::subagent::SessionFactory;
use fabro_agent::{
    AgentProfile, AnthropicProfile, GeminiProfile, LocalSandbox, OpenAiProfile, Session,
    SessionConfig, SubAgentManager, WebFetchSummarizer,
};
use fabro_llm::client::Client;
use fabro_llm::provider::Provider;
use fabro_model::ModelRef;
use tokio::sync::Mutex as AsyncMutex;

fn summarizer_model_id(provider: Provider) -> ModelRef {
    match provider {
        Provider::OpenAi
        | Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception
        | Provider::OpenAiCompatible => ModelRef::ByName {
            provider: Provider::OpenAi,
            model: "gpt-5.4-mini".to_string(),
        },
        Provider::Gemini => ModelRef::ByName {
            provider: Provider::Gemini,
            model: "gemini-3-flash-preview".to_string(),
        },
        Provider::Anthropic => ModelRef::ByName {
            provider: Provider::Anthropic,
            model: "claude-haiku-4-5".to_string(),
        },
    }
}

fn build_summarizer(provider: Provider, client: &Client) -> WebFetchSummarizer {
    WebFetchSummarizer {
        client: client.clone(),
        model_id: summarizer_model_id(provider),
    }
}

fn build_profile(provider: Provider, model: &str, client: &Client) -> Box<dyn AgentProfile> {
    let summarizer = Some(build_summarizer(provider, client));
    match provider {
        Provider::Anthropic => Box::new(AnthropicProfile::with_summarizer(model, summarizer)),
        Provider::OpenAi => Box::new(OpenAiProfile::with_summarizer(model, summarizer)),
        Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception
        | Provider::OpenAiCompatible => {
            Box::new(OpenAiProfile::with_summarizer(model, summarizer).with_provider(provider))
        }
        Provider::Gemini => Box::new(GeminiProfile::with_summarizer(model, summarizer)),
    }
}

async fn make_session(provider: Provider, model: &str, cwd: &Path) -> Session {
    let client = Client::from_env().await.expect("Client::from_env failed");
    let mut profile = build_profile(provider, model, &client);
    let env = Arc::new(LocalSandbox::new(cwd.to_path_buf()));

    // Register subagent tools so spawn_agent / wait / send_input / close_agent are available
    let manager = Arc::new(AsyncMutex::new(SubAgentManager::new(3)));
    let factory_client = client.clone();
    let factory_model: String = model.to_string();
    let factory_cwd = cwd.to_path_buf();
    let factory: SessionFactory = Arc::new(move || {
        let sub_profile: Arc<dyn AgentProfile> = {
            let summarizer = Some(build_summarizer(provider, &factory_client));
            match provider {
                Provider::Anthropic => Arc::new(AnthropicProfile::with_summarizer(
                    &factory_model,
                    summarizer,
                )),
                Provider::OpenAi => {
                    Arc::new(OpenAiProfile::with_summarizer(&factory_model, summarizer))
                }
                Provider::Kimi
                | Provider::Zai
                | Provider::Minimax
                | Provider::Inception
                | Provider::OpenAiCompatible => Arc::new(
                    OpenAiProfile::with_summarizer(&factory_model, summarizer)
                        .with_provider(provider),
                ),
                Provider::Gemini => {
                    Arc::new(GeminiProfile::with_summarizer(&factory_model, summarizer))
                }
            }
        };
        let sub_env = Arc::new(LocalSandbox::new(factory_cwd.clone()));
        Session::new(
            factory_client.clone(),
            sub_profile,
            sub_env,
            SessionConfig::default(),
            None,
        )
    });
    profile.register_subagent_tools(manager, factory, 0);

    let profile: Arc<dyn AgentProfile> = Arc::from(profile);
    let config = SessionConfig {
        max_turns: 20,
        ..SessionConfig::default()
    };
    Session::new(client, profile, env, config, None)
}

async fn make_session_with_config(
    provider: Provider,
    model: &str,
    cwd: &Path,
    config: SessionConfig,
) -> Session {
    let client = Client::from_env().await.expect("Client::from_env failed");
    let profile: Arc<dyn AgentProfile> = Arc::from(build_profile(provider, model, &client));
    let env = Arc::new(LocalSandbox::new(cwd.to_path_buf()));
    Session::new(client, profile, env, config, None)
}

macro_rules! provider_test {
    ($scenario:ident, $provider:expr, $model:expr, $prefix:ident, keys = [$($key:expr),+ $(,)?]) => {
        paste::paste! {
            #[fabro_macros::e2e_test($(live($key)),+)]
            async fn [<$prefix _ $scenario>]() {
                let tmp = tempfile::tempdir().expect("failed to create tempdir");
                let mut session = make_session($provider, $model, tmp.path()).await;
                session.initialize().await;
                [<scenario_ $scenario>](&mut session, tmp.path()).await;
            }
        }
    };
}

macro_rules! provider_tests {
    ($scenario:ident) => {
        provider_test!(
            $scenario,
            Provider::Anthropic,
            "claude-haiku-4-5",
            anthropic,
            keys = ["ANTHROPIC_API_KEY"]
        );
        provider_test!(
            $scenario,
            Provider::OpenAi,
            "gpt-5.4-mini",
            openai,
            keys = ["OPENAI_API_KEY"]
        );
        provider_test!(
            $scenario,
            Provider::Gemini,
            "gemini-3-flash-preview",
            gemini,
            keys = ["GEMINI_API_KEY"]
        );
        provider_test!(
            $scenario,
            Provider::Kimi,
            "kimi-k2.5",
            kimi,
            keys = ["KIMI_API_KEY"]
        );
        #[cfg(feature = "quarantine")]
        provider_test!(
            $scenario,
            Provider::Zai,
            "glm-4.7",
            zai,
            keys = ["ZAI_API_KEY"]
        );
        provider_test!(
            $scenario,
            Provider::Minimax,
            "minimax-m2.5",
            minimax,
            keys = ["MINIMAX_API_KEY"]
        );
        #[cfg(feature = "quarantine")]
        provider_test!(
            $scenario,
            Provider::Inception,
            "mercury-2",
            inception,
            keys = ["INCEPTION_API_KEY"]
        );
    };
}

provider_tests!(simple_file_creation);
provider_tests!(read_and_edit_file);
provider_tests!(multi_file_edit);
provider_tests!(shell_execution);
provider_tests!(shell_timeout);
provider_tests!(grep_and_glob);
provider_tests!(tool_output_truncation);
provider_tests!(parallel_tool_calls);
provider_tests!(steering_before_input);
provider_tests!(steering_mid_task);
provider_tests!(follow_up);
provider_tests!(subagent_spawn);

provider_test!(
    web_fetch,
    Provider::Anthropic,
    "claude-haiku-4-5",
    anthropic,
    keys = ["ANTHROPIC_API_KEY"]
);
provider_test!(
    web_fetch,
    Provider::OpenAi,
    "gpt-5.4-mini",
    openai,
    keys = ["OPENAI_API_KEY"]
);
provider_test!(
    web_fetch,
    Provider::Gemini,
    "gemini-3-flash-preview",
    gemini,
    keys = ["GEMINI_API_KEY"]
);
provider_test!(
    web_fetch,
    Provider::Kimi,
    "kimi-k2.5",
    kimi,
    keys = ["KIMI_API_KEY", "OPENAI_API_KEY"]
);
#[cfg(feature = "quarantine")]
provider_test!(
    web_fetch,
    Provider::Zai,
    "glm-4.7",
    zai,
    keys = ["ZAI_API_KEY", "OPENAI_API_KEY"]
);
provider_test!(
    web_fetch,
    Provider::Minimax,
    "minimax-m2.5",
    minimax,
    keys = ["MINIMAX_API_KEY", "OPENAI_API_KEY"]
);
#[cfg(feature = "quarantine")]
provider_test!(
    web_fetch,
    Provider::Inception,
    "mercury-2",
    inception,
    keys = ["INCEPTION_API_KEY", "OPENAI_API_KEY"]
);

provider_test!(
    web_search,
    Provider::Anthropic,
    "claude-haiku-4-5",
    anthropic,
    keys = ["ANTHROPIC_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
provider_test!(
    web_search,
    Provider::OpenAi,
    "gpt-5.4-mini",
    openai,
    keys = ["OPENAI_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
provider_test!(
    web_search,
    Provider::Gemini,
    "gemini-3-flash-preview",
    gemini,
    keys = ["GEMINI_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
provider_test!(
    web_search,
    Provider::Kimi,
    "kimi-k2.5",
    kimi,
    keys = ["KIMI_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
#[cfg(feature = "quarantine")]
provider_test!(
    web_search,
    Provider::Zai,
    "glm-4.7",
    zai,
    keys = ["ZAI_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
provider_test!(
    web_search,
    Provider::Minimax,
    "minimax-m2.5",
    minimax,
    keys = ["MINIMAX_API_KEY", "BRAVE_SEARCH_API_KEY"]
);
#[cfg(feature = "quarantine")]
provider_test!(
    web_search,
    Provider::Inception,
    "mercury-2",
    inception,
    keys = ["INCEPTION_API_KEY", "BRAVE_SEARCH_API_KEY"]
);

// Scenarios below are only generated for providers where they are supported.
// - multi_step_read_analyze_edit / provider_specific_editing: gpt-4o-mini is too
//   weak to reliably apply precise file edits (uses apply_patch, not edit_file).
// - reasoning_effort: gpt-4o-mini doesn't support the reasoning.effort parameter.
// - loop_detection: needs custom config, tested separately below.

provider_tests!(error_recovery);

// gpt-5-mini is too weak to reliably apply precise file edits (uses apply_patch, not edit_file).
macro_rules! non_openai_provider_tests {
    ($scenario:ident) => {
        provider_test!(
            $scenario,
            Provider::Anthropic,
            "claude-haiku-4-5",
            anthropic,
            keys = ["ANTHROPIC_API_KEY"]
        );
        provider_test!(
            $scenario,
            Provider::Gemini,
            "gemini-3-flash-preview",
            gemini,
            keys = ["GEMINI_API_KEY"]
        );
        provider_test!(
            $scenario,
            Provider::Kimi,
            "kimi-k2.5",
            kimi,
            keys = ["KIMI_API_KEY"]
        );
        #[cfg(feature = "quarantine")]
        provider_test!(
            $scenario,
            Provider::Zai,
            "glm-4.7",
            zai,
            keys = ["ZAI_API_KEY"]
        );
        provider_test!(
            $scenario,
            Provider::Minimax,
            "minimax-m2.5",
            minimax,
            keys = ["MINIMAX_API_KEY"]
        );
        #[cfg(feature = "quarantine")]
        provider_test!(
            $scenario,
            Provider::Inception,
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

    // Grab handles before process_input borrows &mut self
    let steering_queue = session.steering_queue_handle();
    let mut rx = session.subscribe();

    // Spawn a task that waits for the first tool call, then injects steering
    let steer_task = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            if matches!(
                event.event,
                fabro_agent::AgentEvent::ToolCallCompleted { .. }
            ) {
                steering_queue
                    .lock()
                    .expect("steering queue lock")
                    .push_back(
                        "Stop what you are doing. Create a file called steered.txt containing 'steered' and do nothing else.".to_string(),
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
            let config = SessionConfig {
                max_turns: 20,
                reasoning_effort: Some(fabro_llm::types::ReasoningEffort::Low),
                ..SessionConfig::default()
            };
            let mut session = make_session_with_config($provider, $model, tmp.path(), config).await;
            session.initialize().await;
            session
                .process_input("Say hello")
                .await
                .expect("process_input failed");
        }
    };
}

reasoning_effort_tests!(
    Provider::Anthropic,
    "claude-haiku-4-5",
    anthropic_reasoning_effort,
    keys = ["ANTHROPIC_API_KEY"]
);
// gpt-5-mini does not support the reasoning.effort parameter, so no OpenAI test.
reasoning_effort_tests!(
    Provider::Gemini,
    "gemini-3-flash-preview",
    gemini_reasoning_effort,
    keys = ["GEMINI_API_KEY"]
);
reasoning_effort_tests!(
    Provider::Kimi,
    "kimi-k2.5",
    kimi_reasoning_effort,
    keys = ["KIMI_API_KEY"]
);
#[cfg(feature = "quarantine")]
reasoning_effort_tests!(
    Provider::Zai,
    "glm-4.7",
    zai_reasoning_effort,
    keys = ["ZAI_API_KEY"]
);
reasoning_effort_tests!(
    Provider::Minimax,
    "minimax-m2.5",
    minimax_reasoning_effort,
    keys = ["MINIMAX_API_KEY"]
);
#[cfg(feature = "quarantine")]
reasoning_effort_tests!(
    Provider::Inception,
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
            let config = SessionConfig {
                max_turns: 20,
                loop_detection_window: 3,
                ..SessionConfig::default()
            };
            let mut session = make_session_with_config($provider, $model, tmp.path(), config).await;
            session.initialize().await;
            session
                .process_input("Repeatedly read the file /dev/null")
                .await
                .expect("process_input failed");
        }
    };
}

loop_detection_tests!(
    Provider::Anthropic,
    "claude-haiku-4-5",
    anthropic_loop_detection,
    keys = ["ANTHROPIC_API_KEY"]
);
loop_detection_tests!(
    Provider::OpenAi,
    "gpt-5.4-mini",
    openai_loop_detection,
    keys = ["OPENAI_API_KEY"]
);
loop_detection_tests!(
    Provider::Gemini,
    "gemini-3-flash-preview",
    gemini_loop_detection,
    keys = ["GEMINI_API_KEY"]
);
loop_detection_tests!(
    Provider::Kimi,
    "kimi-k2.5",
    kimi_loop_detection,
    keys = ["KIMI_API_KEY"]
);
#[cfg(feature = "quarantine")]
loop_detection_tests!(
    Provider::Zai,
    "glm-4.7",
    zai_loop_detection,
    keys = ["ZAI_API_KEY"]
);
loop_detection_tests!(
    Provider::Minimax,
    "minimax-m2.5",
    minimax_loop_detection,
    keys = ["MINIMAX_API_KEY"]
);
#[cfg(feature = "quarantine")]
loop_detection_tests!(
    Provider::Inception,
    "mercury-2",
    inception_loop_detection,
    keys = ["INCEPTION_API_KEY"]
);

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
