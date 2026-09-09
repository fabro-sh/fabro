#![expect(
    clippy::disallowed_methods,
    reason = "Live provider integration tests read required API keys from process env."
)]

use std::collections::HashMap;
use std::sync::Arc;

use fabro_auth::ApiCredential;
use fabro_llm::client::Client;
use fabro_llm::error::ProviderErrorKind;
use fabro_llm::model_test::{ModelTestStatus, run_model_test};
use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::{
    AnthropicAdapter, BedrockAdapter, GeminiAdapter, OpenAiAdapter, OpenAiCompatibleAdapter,
};
use fabro_llm::types::{
    CostSource, FinishReason, Message, ReasoningEffort, Request, ToolChoice, ToolDefinition,
};
use fabro_model::catalog::{LlmCatalogSettings, ProviderCatalogSettings};
use fabro_model::{Catalog, ModelTestMode, ProviderId};
use fabro_static::EnvVars;

fn make_request(model: &str) -> Request {
    Request {
        model:            model.to_string(),
        messages:         vec![Message::user("Say hello in exactly one word")],
        provider:         None,
        tools:            None,
        tool_choice:      None,
        response_format:  None,
        temperature:      Some(0.0),
        top_p:            None,
        max_tokens:       Some(50),
        stop_sequences:   None,
        reasoning_effort: None,
        speed:            None,
        metadata:         None,
        provider_options: None,
    }
}

/// Build the built-in catalog with `provider` enabled, plus an operator base
/// URL for providers such as Modal that do not ship one.
fn enabled_provider_catalog(provider: &ProviderId, base_url: Option<String>) -> Arc<Catalog> {
    let mut settings = LlmCatalogSettings::default();
    settings
        .providers
        .insert(provider.to_string(), ProviderCatalogSettings {
            enabled: Some(true),
            base_url,
            ..ProviderCatalogSettings::default()
        });
    Arc::new(
        Catalog::from_builtin_with_overrides(&settings)
            .unwrap_or_else(|err| panic!("enabled {provider} catalog should build: {err}")),
    )
}

/// Drive the shared deep tool round trip for one catalog offering.
async fn assert_deep_tool_round_trip(
    catalog: &Arc<Catalog>,
    provider: &ProviderId,
    model_id: &str,
    credential: ApiCredential,
) {
    let client = Arc::new(
        Client::from_credentials(vec![credential], Arc::clone(catalog))
            .await
            .unwrap_or_else(|err| panic!("{provider} client should build from the catalog: {err}")),
    );
    let model = catalog
        .get_on_provider(provider, model_id)
        .unwrap_or_else(|| panic!("{provider} {model_id} should be present"));

    let outcome = run_model_test(model, ModelTestMode::Deep, None, client).await;
    assert_eq!(
        outcome.status,
        ModelTestStatus::Ok,
        "{provider} {model_id} deep test failed: {:?}",
        outcome.error_message
    );
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
async fn anthropic_complete() {
    let api_key = std::env::var(EnvVars::ANTHROPIC_API_KEY).expect("ANTHROPIC_API_KEY must be set");
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

#[fabro_macros::e2e_test(twin, live("OPENAI_API_KEY"))]
async fn openai_complete() {
    let (base_url, api_key) = fabro_test::e2e_openai!();
    let adapter = OpenAiAdapter::new(api_key).with_base_url(base_url);
    let request = Request {
        temperature: None,
        ..make_request("gpt-5.2")
    };
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

#[fabro_macros::e2e_test(twin, live("OPENAI_API_KEY"))]
async fn openai_gpt_5_3_codex_complete() {
    let (base_url, api_key) = fabro_test::e2e_openai!();
    let adapter = OpenAiAdapter::new(api_key).with_base_url(base_url);
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

#[fabro_macros::e2e_test(live("OPENAI_API_KEY"))]
async fn openai_gpt_5_5_complete() {
    let api_key = std::env::var(EnvVars::OPENAI_API_KEY).expect("OPENAI_API_KEY must be set");
    let adapter = OpenAiAdapter::new(api_key);
    let request = Request {
        temperature: None,
        ..make_request("gpt-5.5")
    };
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "openai");
}

#[fabro_macros::e2e_test(live("OPENAI_GPT_5_5_PRO_API_KEY"))]
async fn openai_gpt_5_5_pro_complete() {
    let api_key = std::env::var("OPENAI_GPT_5_5_PRO_API_KEY")
        .expect("OPENAI_GPT_5_5_PRO_API_KEY must be set");
    let adapter = OpenAiAdapter::new(api_key);
    let request = Request {
        temperature: None,
        ..make_request("gpt-5.5-pro")
    };
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "openai");
}

#[fabro_macros::e2e_test(live("KIMI_API_KEY"))]
async fn kimi_k3_reasoning_tool_round_trip() {
    let api_key = std::env::var(EnvVars::KIMI_API_KEY).expect("KIMI_API_KEY must be set");
    let adapter = OpenAiCompatibleAdapter::new(api_key, "https://api.moonshot.ai/v1")
        .with_name("moonshot")
        .with_catalog(Arc::new(Catalog::from_builtin().unwrap()));
    let tool = ToolDefinition::function(
        "multiply",
        "Multiply two integers",
        serde_json::json!({
            "type": "object",
            "properties": {
                "a": {"type": "integer"},
                "b": {"type": "integer"}
            },
            "required": ["a", "b"]
        }),
    );
    let request = Request {
        model: "kimi-k3".to_string(),
        messages: vec![Message::user(
            "Use the multiply tool to calculate 19 times 23. Do not calculate it yourself.",
        )],
        tools: Some(vec![tool]),
        tool_choice: Some(ToolChoice::Required),
        temperature: Some(0.0),
        max_tokens: Some(4096),
        reasoning_effort: Some(ReasoningEffort::Low),
        ..make_request("kimi-k3")
    };

    let tool_response = adapter.complete(&request).await.unwrap();
    assert_eq!(tool_response.finish_reason, FinishReason::ToolCalls);
    assert!(
        tool_response.reasoning().is_some(),
        "K3 should return reasoning content before its tool call"
    );
    let tool_call = tool_response
        .tool_calls()
        .into_iter()
        .next()
        .expect("K3 should call the required tool");
    assert_eq!(tool_call.name, "multiply");

    let mut messages = request.messages.clone();
    messages.push(tool_response.message);
    messages.push(Message::tool_result(
        tool_call.id,
        serde_json::json!({"product": 437}),
        false,
    ));
    let final_request = Request {
        model: "kimi-k3".to_string(),
        messages,
        temperature: Some(0.0),
        max_tokens: Some(2048),
        reasoning_effort: Some(ReasoningEffort::Low),
        ..make_request("kimi-k3")
    };

    let final_response = adapter.complete(&final_request).await.unwrap();
    assert_eq!(final_response.finish_reason, FinishReason::Stop);
    assert!(
        final_response.text().contains("437"),
        "K3 should incorporate the replayed tool result"
    );
}

#[fabro_macros::e2e_test(twin)]
async fn openai_server_error() {
    let (base_url, api_key) = fabro_test::e2e_openai!();
    let admin_url = base_url
        .strip_suffix("/v1")
        .expect("OpenAI base URL should end with /v1");

    fabro_test::test_http_client()
        .post(format!("{admin_url}/__admin/scenarios"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({
            "scenarios": [{
                "matcher": { "endpoint": "responses" },
                "script": {
                    "kind": "error",
                    "status": 500,
                    "message": "internal server error",
                    "error_type": "server_error",
                    "code": "server_error"
                }
            }]
        }))
        .send()
        .await
        .unwrap();

    let adapter = OpenAiAdapter::new(api_key).with_base_url(base_url);
    let request = make_request("gpt-4o-mini");
    let err = adapter.complete(&request).await.unwrap_err();

    assert_eq!(err.provider_kind(), Some(ProviderErrorKind::Server));
    assert_eq!(err.status_code(), Some(500));
}

#[fabro_macros::e2e_test(live("GEMINI_API_KEY"))]
async fn gemini_complete() {
    let api_key = std::env::var(EnvVars::GEMINI_API_KEY).expect("GEMINI_API_KEY must be set");
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

#[fabro_macros::e2e_test(live("AWS_BEARER_TOKEN_BEDROCK"))]
async fn bedrock_complete_with_api_key() {
    let token = std::env::var(EnvVars::AWS_BEARER_TOKEN_BEDROCK)
        .expect("AWS_BEARER_TOKEN_BEDROCK must be set");
    let adapter =
        BedrockAdapter::new_api_key(token, "https://bedrock-runtime.us-east-1.amazonaws.com")
            .unwrap()
            .with_name("bedrock");
    // Amazon Nova: first-party, no Anthropic-approval gate and no third-party
    // marketplace subscription, so this runs on any Bedrock-enabled account.
    let request = make_request("us.amazon.nova-2-lite-v1:0");
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "bedrock");
}

#[fabro_macros::e2e_test(live("AWS_ACCESS_KEY_ID"))]
async fn bedrock_complete_with_sigv4() {
    let adapter = BedrockAdapter::new_sigv4("https://bedrock-runtime.us-east-1.amazonaws.com")
        .unwrap()
        .with_name("bedrock");
    // First-party Nova — see bedrock_complete_with_api_key for why.
    let request = make_request("us.amazon.nova-2-lite-v1:0");
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert_eq!(response.provider, "bedrock");
}

#[fabro_macros::e2e_test(live("AWS_BEARER_TOKEN_BEDROCK"))]
async fn bedrock_openai_frontier_complete() {
    let token = std::env::var(EnvVars::AWS_BEARER_TOKEN_BEDROCK)
        .expect("AWS_BEARER_TOKEN_BEDROCK must be set");
    // GPT-5.x on Bedrock is the bedrock-mantle Responses surface: the plain
    // openai adapter pointed at the mantle endpoint with the Bedrock key as
    // the bearer token.
    let adapter = OpenAiAdapter::new(token)
        .with_base_url("https://bedrock-mantle.us-east-1.api.aws/openai/v1")
        .with_name("bedrock-openai");
    let request = Request {
        temperature: None,
        ..make_request("openai.gpt-5.5")
    };
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert_eq!(response.provider, "bedrock-openai");
}

#[fabro_macros::e2e_test(live("POOLSIDE_API_KEY"))]
async fn poolside_laguna_xs_deep_tool_round_trip() {
    let api_key = std::env::var(EnvVars::POOLSIDE_API_KEY).expect("POOLSIDE_API_KEY must be set");
    let provider = ProviderId::new("poolside");
    let catalog = enabled_provider_catalog(&provider, None);
    let credential = ApiCredential::from_api_key(provider.clone(), api_key, &catalog)
        .expect("Poolside credential should resolve from the catalog");

    assert_deep_tool_round_trip(&catalog, &provider, "laguna-xs-2.1", credential).await;
}

#[fabro_macros::e2e_test(live("OPENROUTER_API_KEY"))]
async fn openrouter_complete() {
    let api_key =
        std::env::var(EnvVars::OPENROUTER_API_KEY).expect("OPENROUTER_API_KEY must be set");
    let adapter = OpenAiCompatibleAdapter::new(api_key, "https://openrouter.ai/api/v1")
        .with_name("openrouter");
    let request = make_request("deepseek/deepseek-v4-flash-0731");
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "openrouter");
    assert!(
        response.cost_usd.is_some(),
        "OpenRouter responses should carry an authoritative usage.cost",
    );
    assert_eq!(response.cost_source, Some(CostSource::Authoritative));
}

#[fabro_macros::e2e_test(live("ZAI_API_KEY"))]
async fn zai_glm_5_2_reasoning_tool_round_trip() {
    let api_key = std::env::var(EnvVars::ZAI_API_KEY).expect("ZAI_API_KEY must be set");
    let adapter = OpenAiCompatibleAdapter::new(api_key, "https://api.z.ai/api/coding/paas/v4")
        .with_name("zai")
        .with_catalog(Arc::new(Catalog::from_builtin().unwrap()));
    let tool = ToolDefinition::function(
        "multiply",
        "Multiply two integers",
        serde_json::json!({
            "type": "object",
            "properties": {
                "a": {"type": "integer"},
                "b": {"type": "integer"}
            },
            "required": ["a", "b"]
        }),
    );
    let request = Request {
        model: "glm-5.2".to_string(),
        messages: vec![Message::user(
            "Use the multiply tool to calculate 19 times 23. Do not calculate it yourself.",
        )],
        tools: Some(vec![tool]),
        tool_choice: Some(ToolChoice::Required),
        temperature: Some(0.0),
        max_tokens: Some(4096),
        reasoning_effort: Some(ReasoningEffort::High),
        ..make_request("glm-5.2")
    };

    let tool_response = adapter.complete(&request).await.unwrap();
    assert_eq!(tool_response.finish_reason, FinishReason::ToolCalls);
    let raw_message_keys = tool_response
        .raw
        .as_ref()
        .and_then(|raw| raw.pointer("/choices/0/message"))
        .and_then(serde_json::Value::as_object)
        .map(|message| message.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        tool_response.reasoning().is_some(),
        "GLM 5.2 should return reasoning content before its tool call; raw message keys: \
         {raw_message_keys:?}"
    );
    let tool_call = tool_response
        .tool_calls()
        .into_iter()
        .next()
        .expect("GLM 5.2 should call the required tool");
    assert_eq!(tool_call.name, "multiply");

    let mut messages = request.messages.clone();
    messages.push(tool_response.message);
    messages.push(Message::tool_result(
        tool_call.id,
        serde_json::json!({"product": 437}),
        false,
    ));
    let final_request = Request {
        model: "glm-5.2".to_string(),
        messages,
        temperature: Some(0.0),
        max_tokens: Some(2048),
        reasoning_effort: Some(ReasoningEffort::High),
        ..make_request("glm-5.2")
    };

    let final_response = adapter.complete(&final_request).await.unwrap();
    assert_eq!(final_response.finish_reason, FinishReason::Stop);
    assert!(
        final_response.text().contains("437"),
        "GLM 5.2 should incorporate the replayed tool result"
    );
}

#[fabro_macros::e2e_test(live("OPENROUTER_API_KEY"))]
async fn openrouter_glm_5_2_reasoning_tool_round_trip() {
    let api_key =
        std::env::var(EnvVars::OPENROUTER_API_KEY).expect("OPENROUTER_API_KEY must be set");
    let overrides: LlmCatalogSettings = toml::from_str(
        r"
[providers.openrouter]
enabled = true
",
    )
    .expect("OpenRouter catalog override should parse");
    let catalog = Catalog::from_builtin_with_overrides(&overrides)
        .expect("enabled OpenRouter catalog should build");
    let adapter = OpenAiCompatibleAdapter::new(api_key, "https://openrouter.ai/api/v1")
        .with_name("openrouter")
        .with_catalog(Arc::new(catalog));
    let tool = ToolDefinition::function(
        "multiply",
        "Multiply two integers",
        serde_json::json!({
            "type": "object",
            "properties": {
                "a": {"type": "integer"},
                "b": {"type": "integer"}
            },
            "required": ["a", "b"]
        }),
    );
    let request = Request {
        model: "z-ai/glm-5.2".to_string(),
        messages: vec![Message::user(
            "Use the multiply tool to calculate 19 times 23. Do not calculate it yourself.",
        )],
        tools: Some(vec![tool]),
        tool_choice: Some(ToolChoice::Required),
        temperature: Some(0.0),
        max_tokens: Some(4096),
        reasoning_effort: Some(ReasoningEffort::High),
        ..make_request("z-ai/glm-5.2")
    };

    let tool_response = adapter.complete(&request).await.unwrap();
    assert_eq!(tool_response.finish_reason, FinishReason::ToolCalls);
    let raw_message_keys = tool_response
        .raw
        .as_ref()
        .and_then(|raw| raw.pointer("/choices/0/message"))
        .and_then(serde_json::Value::as_object)
        .map(|message| message.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        tool_response.reasoning().is_some(),
        "GLM 5.2 should return reasoning content before its tool call; raw message keys: \
         {raw_message_keys:?}"
    );
    assert_eq!(tool_response.cost_source, Some(CostSource::Authoritative));
    let tool_call = tool_response
        .tool_calls()
        .into_iter()
        .next()
        .expect("GLM 5.2 should call the required tool");
    assert_eq!(tool_call.name, "multiply");

    let mut messages = request.messages.clone();
    messages.push(tool_response.message);
    messages.push(Message::tool_result(
        tool_call.id,
        serde_json::json!({"product": 437}),
        false,
    ));
    let final_request = Request {
        model: "z-ai/glm-5.2".to_string(),
        messages,
        temperature: Some(0.0),
        max_tokens: Some(2048),
        reasoning_effort: Some(ReasoningEffort::High),
        ..make_request("z-ai/glm-5.2")
    };

    let final_response = adapter.complete(&final_request).await.unwrap();
    assert_eq!(final_response.finish_reason, FinishReason::Stop);
    assert!(
        final_response.text().contains("437"),
        "GLM 5.2 should incorporate the replayed tool result"
    );
    assert_eq!(final_response.cost_source, Some(CostSource::Authoritative));
}

#[fabro_macros::e2e_test(live("ZAI_API_KEY"))]
async fn zai_glm_5_3_reasoning_tool_round_trip() {
    let api_key = std::env::var(EnvVars::ZAI_API_KEY).expect("ZAI_API_KEY must be set");
    let adapter = OpenAiCompatibleAdapter::new(api_key, "https://api.z.ai/api/coding/paas/v4")
        .with_name("zai")
        .with_catalog(Arc::new(Catalog::from_builtin().unwrap()));
    let tool = ToolDefinition::function(
        "multiply",
        "Multiply two integers",
        serde_json::json!({
            "type": "object",
            "properties": {
                "a": {"type": "integer"},
                "b": {"type": "integer"}
            },
            "required": ["a", "b"]
        }),
    );
    let request = Request {
        model: "glm-5.3".to_string(),
        messages: vec![Message::user(
            "Use the multiply tool to calculate 19 times 23. Do not calculate it yourself.",
        )],
        tools: Some(vec![tool]),
        tool_choice: Some(ToolChoice::Required),
        temperature: Some(0.0),
        max_tokens: Some(4096),
        reasoning_effort: Some(ReasoningEffort::High),
        ..make_request("glm-5.3")
    };

    let tool_response = adapter.complete(&request).await.unwrap();
    assert_eq!(tool_response.finish_reason, FinishReason::ToolCalls);
    let raw_message_keys = tool_response
        .raw
        .as_ref()
        .and_then(|raw| raw.pointer("/choices/0/message"))
        .and_then(serde_json::Value::as_object)
        .map(|message| message.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        tool_response.reasoning().is_some(),
        "GLM 5.3 should return reasoning content before its tool call; raw message keys: \
         {raw_message_keys:?}"
    );
    let tool_call = tool_response
        .tool_calls()
        .into_iter()
        .next()
        .expect("GLM 5.3 should call the required tool");
    assert_eq!(tool_call.name, "multiply");

    let mut messages = request.messages.clone();
    messages.push(tool_response.message);
    messages.push(Message::tool_result(
        tool_call.id,
        serde_json::json!({"product": 437}),
        false,
    ));
    let final_request = Request {
        model: "glm-5.3".to_string(),
        messages,
        temperature: Some(0.0),
        max_tokens: Some(2048),
        reasoning_effort: Some(ReasoningEffort::High),
        ..make_request("glm-5.3")
    };

    let final_response = adapter.complete(&final_request).await.unwrap();
    assert_eq!(final_response.finish_reason, FinishReason::Stop);
    assert!(
        final_response.text().contains("437"),
        "GLM 5.3 should incorporate the replayed tool result"
    );
}

#[fabro_macros::e2e_test(live("OPENROUTER_API_KEY"))]
async fn openrouter_glm_5_3_reasoning_tool_round_trip() {
    let api_key =
        std::env::var(EnvVars::OPENROUTER_API_KEY).expect("OPENROUTER_API_KEY must be set");
    let overrides: LlmCatalogSettings = toml::from_str(
        r"
[providers.openrouter]
enabled = true
",
    )
    .expect("OpenRouter catalog override should parse");
    let catalog = Catalog::from_builtin_with_overrides(&overrides)
        .expect("enabled OpenRouter catalog should build");
    let adapter = OpenAiCompatibleAdapter::new(api_key, "https://openrouter.ai/api/v1")
        .with_name("openrouter")
        .with_catalog(Arc::new(catalog));
    let tool = ToolDefinition::function(
        "multiply",
        "Multiply two integers",
        serde_json::json!({
            "type": "object",
            "properties": {
                "a": {"type": "integer"},
                "b": {"type": "integer"}
            },
            "required": ["a", "b"]
        }),
    );
    let request = Request {
        model: "z-ai/glm-5.3".to_string(),
        messages: vec![Message::user(
            "Use the multiply tool to calculate 19 times 23. Do not calculate it yourself.",
        )],
        tools: Some(vec![tool]),
        tool_choice: Some(ToolChoice::Required),
        temperature: Some(0.0),
        max_tokens: Some(4096),
        reasoning_effort: Some(ReasoningEffort::High),
        ..make_request("z-ai/glm-5.3")
    };

    let tool_response = adapter.complete(&request).await.unwrap();
    assert_eq!(tool_response.finish_reason, FinishReason::ToolCalls);
    let raw_message_keys = tool_response
        .raw
        .as_ref()
        .and_then(|raw| raw.pointer("/choices/0/message"))
        .and_then(serde_json::Value::as_object)
        .map(|message| message.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        tool_response.reasoning().is_some(),
        "GLM 5.3 should return reasoning content before its tool call; raw message keys: \
         {raw_message_keys:?}"
    );
    assert_eq!(tool_response.cost_source, Some(CostSource::Authoritative));
    let tool_call = tool_response
        .tool_calls()
        .into_iter()
        .next()
        .expect("GLM 5.3 should call the required tool");
    assert_eq!(tool_call.name, "multiply");

    let mut messages = request.messages.clone();
    messages.push(tool_response.message);
    messages.push(Message::tool_result(
        tool_call.id,
        serde_json::json!({"product": 437}),
        false,
    ));
    let final_request = Request {
        model: "z-ai/glm-5.3".to_string(),
        messages,
        temperature: Some(0.0),
        max_tokens: Some(2048),
        reasoning_effort: Some(ReasoningEffort::High),
        ..make_request("z-ai/glm-5.3")
    };

    let final_response = adapter.complete(&final_request).await.unwrap();
    assert_eq!(final_response.finish_reason, FinishReason::Stop);
    assert!(
        final_response.text().contains("437"),
        "GLM 5.3 should incorporate the replayed tool result"
    );
    assert_eq!(final_response.cost_source, Some(CostSource::Authoritative));
}

#[fabro_macros::e2e_test(live("OPENROUTER_API_KEY"))]
async fn openrouter_poolside_laguna_complete() {
    let api_key =
        std::env::var(EnvVars::OPENROUTER_API_KEY).expect("OPENROUTER_API_KEY must be set");
    let adapter = OpenAiCompatibleAdapter::new(api_key, "https://openrouter.ai/api/v1")
        .with_name("openrouter");
    let request = make_request("poolside/laguna-xs-2.1");
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "openrouter");
    assert!(
        response.cost_usd.is_some(),
        "OpenRouter responses should carry an authoritative usage.cost",
    );
    assert_eq!(response.cost_source, Some(CostSource::Authoritative));
}

#[fabro_macros::e2e_test(live("FIREWORKS_API_KEY"))]
async fn fireworks_complete() {
    let api_key = std::env::var(EnvVars::FIREWORKS_API_KEY).expect("FIREWORKS_API_KEY must be set");
    let adapter = OpenAiCompatibleAdapter::new(api_key, "https://api.fireworks.ai/inference/v1")
        .with_name("fireworks");
    // gpt-oss models spend reasoning tokens before the final text, so the
    // completion budget must cover both.
    let request = Request {
        max_tokens: Some(2048),
        ..make_request("accounts/fireworks/models/gpt-oss-20b")
    };
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    assert_eq!(response.provider, "fireworks");
}

#[fabro_macros::e2e_test(live("DEEPSEEK_API_KEY"))]
async fn deepseek_complete() {
    let api_key = std::env::var(EnvVars::DEEPSEEK_API_KEY).expect("DEEPSEEK_API_KEY must be set");
    let adapter =
        OpenAiCompatibleAdapter::new(api_key, "https://api.deepseek.com").with_name("deepseek");
    let request = Request {
        // Thinking mode is enabled by default and shares this budget with the
        // visible answer.
        max_tokens: Some(1024),
        ..make_request("deepseek-v4-flash")
    };
    let response = adapter.complete(&request).await.unwrap();

    assert!(
        !response.text().is_empty(),
        "response text should not be empty"
    );
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0 || response.usage.reasoning_tokens > 0);
    assert_eq!(response.provider, "deepseek");
}

#[fabro_macros::e2e_test(live("DEEPSEEK_API_KEY"))]
async fn deepseek_v4_flash_deep_tool_round_trip() {
    let api_key = std::env::var(EnvVars::DEEPSEEK_API_KEY).expect("DEEPSEEK_API_KEY must be set");
    let provider = ProviderId::new("deepseek");
    let catalog = enabled_provider_catalog(&provider, None);
    let credential = ApiCredential::from_api_key(provider.clone(), api_key, &catalog)
        .expect("DeepSeek credential should resolve from the catalog");

    assert_deep_tool_round_trip(&catalog, &provider, "deepseek-v4-flash", credential).await;
}

#[fabro_macros::e2e_test(live("FIREWORKS_API_KEY"))]
async fn fireworks_kimi_k2_7_code_deep_tool_round_trip() {
    let api_key = std::env::var(EnvVars::FIREWORKS_API_KEY).expect("FIREWORKS_API_KEY must be set");
    let provider = ProviderId::new("fireworks");
    let catalog = enabled_provider_catalog(&provider, None);
    let credential = ApiCredential::from_api_key(provider.clone(), api_key, &catalog)
        .expect("Fireworks credential should resolve from the catalog");

    assert_deep_tool_round_trip(&catalog, &provider, "kimi-k2.7-code", credential).await;
}

#[fabro_macros::e2e_test(live("FIREWORKS_API_KEY"))]
async fn fireworks_kimi_k3_fast_deep_tool_round_trip() {
    let api_key = std::env::var(EnvVars::FIREWORKS_API_KEY).expect("FIREWORKS_API_KEY must be set");
    let provider = ProviderId::new("fireworks");
    let catalog = enabled_provider_catalog(&provider, None);
    let credential = ApiCredential::from_api_key(provider.clone(), api_key, &catalog)
        .expect("Fireworks credential should resolve from the catalog");

    assert_deep_tool_round_trip(&catalog, &provider, "kimi-k3-fast", credential).await;
}

#[fabro_macros::e2e_test(live("OPENROUTER_API_KEY"))]
async fn openrouter_kimi_k3_deep_tool_round_trip() {
    let api_key =
        std::env::var(EnvVars::OPENROUTER_API_KEY).expect("OPENROUTER_API_KEY must be set");
    let provider = ProviderId::new("openrouter");
    let catalog = enabled_provider_catalog(&provider, None);
    let credential = ApiCredential::from_api_key(provider.clone(), api_key, &catalog)
        .expect("OpenRouter credential should resolve from the catalog");

    assert_deep_tool_round_trip(&catalog, &provider, "kimi-k3", credential).await;
}

#[fabro_macros::e2e_test(
    live("MODAL_KIMI_K3_BASE_URL"),
    live("MODAL_TOKEN_ID"),
    live("MODAL_TOKEN_SECRET")
)]
async fn modal_kimi_k3_deep_tool_round_trip() {
    let base_url =
        std::env::var("MODAL_KIMI_K3_BASE_URL").expect("MODAL_KIMI_K3_BASE_URL must be set");
    let token_id = std::env::var(EnvVars::MODAL_TOKEN_ID).expect("MODAL_TOKEN_ID must be set");
    let token_secret =
        std::env::var(EnvVars::MODAL_TOKEN_SECRET).expect("MODAL_TOKEN_SECRET must be set");
    let provider = ProviderId::new("modal");
    let catalog = enabled_provider_catalog(&provider, Some(base_url));
    let credential = ApiCredential::with_extra_headers(
        provider.clone(),
        HashMap::from([
            ("Modal-Key".to_string(), token_id),
            ("Modal-Secret".to_string(), token_secret),
        ]),
    );

    assert_deep_tool_round_trip(&catalog, &provider, "kimi-k3", credential).await;
}

async fn run_multi_turn_cache_test(
    adapter: &dyn ProviderAdapter,
    model: &str,
    min_cache_ratio: f64,
    temperature: Option<f64>,
) {
    // Claude Haiku 4.5 requires 4096 tokens minimum for prompt caching.
    // Each repeat is ~78 tokens; 70 repeats ≈ 5460 tokens, safely above the
    // threshold.
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
            temperature,
            top_p: None,
            max_tokens: Some(100),
            stop_sequences: None,
            reasoning_effort: None,
            speed: None,
            metadata: None,
            provider_options: None,
        };

        let response = adapter
            .complete(&request)
            .await
            .expect("provider adapter should return a response");
        let text = response.text();
        assert!(
            !text.is_empty(),
            "response text should not be empty on turn {turn}"
        );

        let cache_read = response.usage.cache_read_tokens as f64;
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

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
async fn anthropic_multi_turn_cache() {
    let api_key = std::env::var(EnvVars::ANTHROPIC_API_KEY).expect("ANTHROPIC_API_KEY must be set");
    let adapter =
        AnthropicAdapter::new(api_key).with_catalog(Arc::new(Catalog::from_builtin().unwrap()));
    run_multi_turn_cache_test(&adapter, "claude-haiku-4-5", 0.5, Some(0.0)).await;
}

#[fabro_macros::e2e_test(live("OPENAI_API_KEY"))]
async fn openai_multi_turn_cache() {
    let api_key = std::env::var(EnvVars::OPENAI_API_KEY).expect("OPENAI_API_KEY must be set");
    let adapter = OpenAiAdapter::new(api_key);
    run_multi_turn_cache_test(&adapter, "gpt-5.2", 0.5, None).await;
}

#[fabro_macros::e2e_test(live("GEMINI_API_KEY"))]
async fn gemini_multi_turn_cache() {
    let api_key = std::env::var(EnvVars::GEMINI_API_KEY).expect("GEMINI_API_KEY must be set");
    let adapter = GeminiAdapter::new(api_key);
    run_multi_turn_cache_test(&adapter, "gemini-2.5-flash", 0.5, Some(0.0)).await;
}

/// Prompt caching for Claude routed through OpenRouter: the catalog row opts
/// into explicit `cache_control` breakpoints, and OpenRouter must forward
/// them to Anthropic for cache reads to appear. Guards the end-to-end
/// passthrough the wire tests can't see.
#[fabro_macros::e2e_test(live("OPENROUTER_API_KEY"))]
async fn openrouter_claude_multi_turn_cache() {
    let api_key =
        std::env::var(EnvVars::OPENROUTER_API_KEY).expect("OPENROUTER_API_KEY must be set");
    let overrides: LlmCatalogSettings = toml::from_str(
        r"
[providers.openrouter]
enabled = true
",
    )
    .expect("OpenRouter catalog override should parse");
    let catalog = Catalog::from_builtin_with_overrides(&overrides)
        .expect("enabled OpenRouter catalog should build");
    let adapter = OpenAiCompatibleAdapter::new(api_key, "https://openrouter.ai/api/v1")
        .with_name("openrouter")
        .with_catalog(Arc::new(catalog));
    run_multi_turn_cache_test(&adapter, "claude-haiku-4-5", 0.5, Some(0.0)).await;
}
