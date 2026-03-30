use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::{AnthropicAdapter, GeminiAdapter, OpenAiAdapter};
use fabro_llm::types::{FinishReason, Message, Request};

fn make_request(model: &str) -> Request {
    Request {
        model: model.to_string(),
        messages: vec![Message::user("Say hello in exactly one word")],
        provider: None,
        tools: None,
        tool_choice: None,
        response_format: None,
        temperature: Some(0.0),
        top_p: None,
        max_tokens: Some(50),
        stop_sequences: None,
        reasoning_effort: None,
        speed: None,
        metadata: None,
        provider_options: None,
    }
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn anthropic_complete() {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");
    let adapter = AnthropicAdapter::new(api_key);
    let request = make_request("claude-haiku-4-5");
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "anthropic");
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn openai_complete() {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let adapter = OpenAiAdapter::new(api_key);
    let request = make_request("gpt-4o-mini");
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "openai");
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn openai_gpt_5_3_codex_complete() {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let adapter = OpenAiAdapter::new(api_key);
    let request = make_request("gpt-5.3-codex");
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "openai");
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY"]
async fn gemini_complete() {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");
    let adapter = GeminiAdapter::new(api_key);
    let request = make_request("gemini-2.5-flash");
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "gemini");
}

async fn run_multi_turn_cache_test(
    adapter: &dyn ProviderAdapter,
    model: &str,
    min_cache_ratio: f64,
) {
    // Claude Haiku 4.5 requires 4096 tokens minimum for prompt caching.
    // Each repeat is ~78 tokens; 70 repeats ≈ 5460 tokens, safely above the threshold.
    let padding = "This is a detailed context paragraph that provides background information \
        about the conversation. It contains various facts and details that the model should \
        remember throughout the multi-turn interaction. The purpose of this padding is to \
        ensure the system prompt exceeds the minimum cache threshold for the provider. \
        We include information about mathematics, science, history, and general knowledge. \
        The model should use this context when answering questions. "
        .repeat(70);

    let system_message = Message::system(format!(
        "You are a helpful math assistant. Answer briefly.\n\n{padding}"
    ));

    let questions = [
        "What is 1+1?",
        "What is 2+2?",
        "What is 3+3?",
        "What is 4+4?",
        "What is 5+5?",
        "What is 6+6?",
    ];

    let mut messages = vec![system_message, Message::user(questions[0])];
    let mut best_cache_ratio = 0.0_f64;

    for turn in 0..6 {
        let request = Request {
            model: model.to_string(),
            messages: messages.clone(),
            provider: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            temperature: Some(0.0),
            top_p: None,
            max_tokens: Some(100),
            stop_sequences: None,
            reasoning_effort: None,
            speed: None,
            metadata: None,
            provider_options: None,
        };

        let response = adapter.complete(&request).await.unwrap();
        let text = response.text();
        assert!(
            !text.is_empty(),
            "response text should not be empty on turn {turn}"
        );

        let cache_read = response.usage.cache_read_tokens.unwrap_or(0) as f64;
        let input = response.usage.input_tokens as f64;
        let ratio = cache_read / input;
        best_cache_ratio = best_cache_ratio.max(ratio);

        messages.push(Message::assistant(text));
        if turn < 5 {
            messages.push(Message::user(questions[turn + 1]));
        }
    }

    assert!(
        best_cache_ratio >= min_cache_ratio,
        "best cache ratio {best_cache_ratio:.3} should be at least {min_cache_ratio} across all turns"
    );
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn anthropic_multi_turn_cache() {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");
    let adapter = AnthropicAdapter::new(api_key);
    run_multi_turn_cache_test(&adapter, "claude-haiku-4-5", 0.5).await;
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn openai_multi_turn_cache() {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let adapter = OpenAiAdapter::new(api_key);
    run_multi_turn_cache_test(&adapter, "gpt-4o-mini", 0.5).await;
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY"]
async fn gemini_multi_turn_cache() {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");
    let adapter = GeminiAdapter::new(api_key);
    run_multi_turn_cache_test(&adapter, "gemini-2.5-flash", 0.5).await;
}
