pub use fabro_sandbox::test_support::{MockSandbox, MutableMockSandbox};

use crate::agent_profile::AgentProfile;
use crate::config::SessionConfig;
use crate::profiles::EnvContext;
use crate::sandbox::*;
use crate::session::Session;
use crate::skills::{Skill, format_skills_prompt_section};
use crate::tool_registry::{RegisteredTool, ToolRegistry};
use async_trait::async_trait;
use fabro_llm::client::Client;
use fabro_llm::error::SdkError;
use fabro_llm::provider::{ProviderAdapter, StreamEventStream};
use fabro_llm::types::{ContentPart, FinishReason, Message, Request, Response, StreamEvent, Usage};
use fabro_model::Provider;
use futures::stream;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// --- TestProfile ---

pub struct TestProfile {
    pub registry: ToolRegistry,
    pub context_window: usize,
}

impl TestProfile {
    pub fn new() -> Self {
        Self {
            registry: ToolRegistry::new(),
            context_window: 200_000,
        }
    }

    pub fn with_tools(registry: ToolRegistry) -> Self {
        Self {
            registry,
            context_window: 200_000,
        }
    }

    pub fn with_context_window(registry: ToolRegistry, context_window: usize) -> Self {
        Self {
            registry,
            context_window,
        }
    }
}

impl AgentProfile for TestProfile {
    fn provider(&self) -> Provider {
        Provider::Anthropic
    }

    fn model(&self) -> &'static str {
        "mock-model"
    }

    fn tool_registry(&self) -> &ToolRegistry {
        &self.registry
    }

    fn tool_registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.registry
    }

    fn build_system_prompt(
        &self,
        _env: &dyn Sandbox,
        _env_context: &EnvContext,
        _memory: &[String],
        user_instructions: Option<&str>,
        skills: &[Skill],
    ) -> String {
        let skills_section = format_skills_prompt_section(skills);
        let skills_part = if skills_section.is_empty() {
            String::new()
        } else {
            format!("\n\n{skills_section}")
        };
        match user_instructions {
            Some(instructions) => format!(
                "You are a test assistant.{skills_part}\n\n# User Instructions\n{instructions}"
            ),
            None => format!("You are a test assistant.{skills_part}"),
        }
    }

    fn context_window_size(&self) -> usize {
        self.context_window
    }
}

// --- MockLlmProvider ---

pub struct MockLlmProvider {
    pub responses: Vec<Response>,
    pub call_index: AtomicUsize,
}

impl MockLlmProvider {
    pub fn new(responses: Vec<Response>) -> Self {
        Self {
            responses,
            call_index: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ProviderAdapter for MockLlmProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn complete(&self, _request: &Request) -> Result<Response, SdkError> {
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        if idx < self.responses.len() {
            Ok(self.responses[idx].clone())
        } else {
            Ok(self.responses[self.responses.len() - 1].clone())
        }
    }

    async fn stream(&self, _request: &Request) -> Result<StreamEventStream, SdkError> {
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        let response = if idx < self.responses.len() {
            self.responses[idx].clone()
        } else {
            self.responses[self.responses.len() - 1].clone()
        };
        Ok(response_to_stream(response))
    }
}

/// Convert a canned `Response` into a `StreamEventStream` for mock streaming.
pub fn response_to_stream(response: Response) -> StreamEventStream {
    let mut events: Vec<Result<StreamEvent, SdkError>> = Vec::new();

    // Emit text deltas for text content
    let text = response.text();
    if !text.is_empty() {
        events.push(Ok(StreamEvent::text_delta(text, None)));
    }

    // Emit tool call events
    for part in &response.message.content {
        if let ContentPart::ToolCall(tc) = part {
            events.push(Ok(StreamEvent::ToolCallEnd {
                tool_call: tc.clone(),
            }));
        }
    }

    // Emit finish
    events.push(Ok(StreamEvent::finish(
        response.finish_reason.clone(),
        response.usage.clone(),
        response,
    )));

    Box::pin(stream::iter(events))
}

// --- Helper functions ---

pub fn text_response(text: &str) -> Response {
    Response {
        id: format!("resp_{text}"),
        model: "mock-model".into(),
        provider: "mock".into(),
        message: Message::assistant(text),
        finish_reason: FinishReason::Stop,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            ..Default::default()
        },
        raw: None,
        warnings: vec![],
        rate_limit: None,
    }
}

pub async fn make_client(provider: Arc<dyn ProviderAdapter>) -> Client {
    let mut providers = HashMap::new();
    providers.insert(provider.name().to_string(), provider.clone());
    // Also register under "anthropic" so TestProfile (Provider::Anthropic) routes correctly
    providers.insert("anthropic".to_string(), provider);
    Client::new(providers, Some("mock".into()), vec![])
}

pub async fn make_session(responses: Vec<Response>) -> Session {
    let provider = Arc::new(MockLlmProvider::new(responses));
    let client = make_client(provider).await;
    let profile = Arc::new(TestProfile::new());
    let env = Arc::new(MockSandbox::default());
    Session::new(client, profile, env, SessionConfig::default(), None)
}

pub async fn make_session_with_tools(responses: Vec<Response>, registry: ToolRegistry) -> Session {
    let provider = Arc::new(MockLlmProvider::new(responses));
    let client = make_client(provider).await;
    let profile = Arc::new(TestProfile::with_tools(registry));
    let env = Arc::new(MockSandbox::default());
    Session::new(client, profile, env, SessionConfig::default(), None)
}

pub async fn make_session_with_config(responses: Vec<Response>, config: SessionConfig) -> Session {
    let provider = Arc::new(MockLlmProvider::new(responses));
    let client = make_client(provider).await;
    let profile = Arc::new(TestProfile::new());
    let env = Arc::new(MockSandbox::default());
    Session::new(client, profile, env, config, None)
}

pub async fn make_session_with_tools_and_config(
    responses: Vec<Response>,
    registry: ToolRegistry,
    config: SessionConfig,
) -> Session {
    let provider = Arc::new(MockLlmProvider::new(responses));
    let client = make_client(provider).await;
    let profile = Arc::new(TestProfile::with_tools(registry));
    let env = Arc::new(MockSandbox::default());
    Session::new(client, profile, env, config, None)
}

pub fn tool_call_response(
    tool_name: &str,
    tool_call_id: &str,
    args: serde_json::Value,
) -> Response {
    use fabro_llm::types::{ContentPart, Role, ToolCall};
    Response {
        id: format!("resp_{tool_call_id}"),
        model: "mock-model".into(),
        provider: "mock".into(),
        message: Message {
            role: Role::Assistant,
            content: vec![
                ContentPart::text("Let me use a tool."),
                ContentPart::ToolCall(ToolCall::new(tool_call_id, tool_name, args)),
            ],
            name: None,
            tool_call_id: None,
        },
        finish_reason: FinishReason::ToolCalls,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            ..Default::default()
        },
        raw: None,
        warnings: vec![],
        rate_limit: None,
    }
}

pub fn make_echo_tool() -> RegisteredTool {
    use fabro_llm::types::ToolDefinition;
    RegisteredTool {
        definition: ToolDefinition {
            name: "echo".into(),
            description: "Echoes the input".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        },
        executor: Arc::new(|args, _ctx| {
            Box::pin(async move {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("no text");
                Ok(format!("echo: {text}"))
            })
        }),
    }
}

pub fn make_error_tool() -> RegisteredTool {
    use fabro_llm::types::ToolDefinition;
    RegisteredTool {
        definition: ToolDefinition {
            name: "fail_tool".into(),
            description: "Always fails".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
        executor: Arc::new(|_args, _ctx| {
            Box::pin(async move { Err("tool execution failed".to_string()) })
        }),
    }
}

// --- MockErrorProvider ---

pub struct MockErrorProvider {
    pub error: SdkError,
}

#[async_trait]
impl ProviderAdapter for MockErrorProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn complete(&self, _request: &Request) -> Result<Response, SdkError> {
        Err(self.error.clone())
    }

    async fn stream(&self, _request: &Request) -> Result<StreamEventStream, SdkError> {
        Err(self.error.clone())
    }
}

// --- CapturingLlmProvider ---

/// A mock LLM provider that captures the full Request for test assertions.
pub struct CapturingLlmProvider {
    pub captured_request: Mutex<Option<Request>>,
}

impl CapturingLlmProvider {
    pub fn new() -> Self {
        Self {
            captured_request: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ProviderAdapter for CapturingLlmProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn complete(&self, request: &Request) -> Result<Response, SdkError> {
        *self
            .captured_request
            .lock()
            .expect("captured_request lock poisoned") = Some(request.clone());
        Ok(text_response("captured"))
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, SdkError> {
        *self
            .captured_request
            .lock()
            .expect("captured_request lock poisoned") = Some(request.clone());
        Ok(response_to_stream(text_response("captured")))
    }
}

// --- MockMidStreamErrorProvider ---

/// A mock provider that yields some text deltas then an error mid-stream.
pub struct MockMidStreamErrorProvider {
    pub partial_text: String,
    pub error: SdkError,
}

#[async_trait]
impl ProviderAdapter for MockMidStreamErrorProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn complete(&self, _request: &Request) -> Result<Response, SdkError> {
        Err(self.error.clone())
    }

    async fn stream(&self, _request: &Request) -> Result<StreamEventStream, SdkError> {
        let events: Vec<Result<StreamEvent, SdkError>> = vec![
            Ok(StreamEvent::text_delta(self.partial_text.clone(), None)),
            Err(self.error.clone()),
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

pub fn multi_tool_call_response(calls: Vec<(&str, &str, serde_json::Value)>) -> Response {
    use fabro_llm::types::{ContentPart, Role, ToolCall};
    let mut content = vec![ContentPart::text("Let me use multiple tools.")];
    for (tool_name, tool_call_id, args) in calls {
        content.push(ContentPart::ToolCall(ToolCall::new(
            tool_call_id,
            tool_name,
            args,
        )));
    }
    Response {
        id: "resp_multi".into(),
        model: "mock-model".into(),
        provider: "mock".into(),
        message: Message {
            role: Role::Assistant,
            content,
            name: None,
            tool_call_id: None,
        },
        finish_reason: FinishReason::ToolCalls,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            ..Default::default()
        },
        raw: None,
        warnings: vec![],
        rate_limit: None,
    }
}
