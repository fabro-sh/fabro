use crate::config::SessionConfig;
use crate::execution_env::*;
use crate::profiles::EnvContext;
use crate::provider_profile::{ProfileCapabilities, ProviderProfile};
use crate::session::Session;
use crate::skills::Skill;
use crate::tool_registry::ToolRegistry;
use arc_llm::client::Client;
use arc_llm::error::SdkError;
use arc_llm::provider::{Provider, ProviderAdapter, StreamEventStream};
use arc_llm::types::{ContentPart, FinishReason, Message, Request, Response, StreamEvent, Usage};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

// --- MockExecutionEnvironment ---

pub struct MockExecutionEnvironment {
    pub files: HashMap<String, String>,
    pub exec_result: ExecResult,
    pub grep_results: Vec<String>,
    pub glob_results: Vec<String>,
    pub working_dir: &'static str,
    pub platform_str: &'static str,
    pub os_version_str: String,
    /// When true, `read_file` applies offset/limit by splitting on lines.
    pub apply_read_offset_limit: bool,
    /// Captures (path, content) pairs from `write_file` calls.
    pub written_files: Mutex<Vec<(String, String)>>,
    /// Captures the `timeout_ms` argument from `exec_command` calls.
    pub captured_timeout: Mutex<Option<u64>>,
    /// Captures the `command` argument from `exec_command` calls.
    pub captured_command: Mutex<Option<String>>,
    pub event_callback: Option<crate::execution_env::ExecEnvEventCallback>,
}

impl MockExecutionEnvironment {
    pub fn linux() -> Self {
        Self {
            working_dir: "/home/test",
            platform_str: "linux",
            os_version_str: "Linux 6.1.0".into(),
            ..Default::default()
        }
    }
}

impl MockExecutionEnvironment {
    fn emit(&self, event: crate::execution_env::ExecutionEnvEvent) {
        if let Some(ref cb) = self.event_callback {
            cb(event);
        }
    }
}

impl Default for MockExecutionEnvironment {
    fn default() -> Self {
        Self {
            files: HashMap::new(),
            exec_result: ExecResult {
                stdout: "mock output".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 10,
            },
            grep_results: vec![],
            glob_results: vec![],
            working_dir: "/tmp/test",
            platform_str: "darwin",
            os_version_str: "Darwin 24.0.0".into(),
            apply_read_offset_limit: false,
            written_files: Mutex::new(Vec::new()),
            captured_timeout: Mutex::new(None),
            captured_command: Mutex::new(None),
            event_callback: None,
        }
    }
}

#[async_trait]
impl ExecutionEnvironment for MockExecutionEnvironment {
    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        let content = self
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("File not found: {path}"))?;

        if self.apply_read_offset_limit {
            let lines: Vec<&str> = content.lines().collect();
            let start = offset.unwrap_or(1).saturating_sub(1);
            let count = limit.unwrap_or(2000);
            let selected: Vec<&str> = lines.into_iter().skip(start).take(count).collect();
            Ok(selected.join("\n"))
        } else {
            Ok(content)
        }
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        self.written_files
            .lock()
            .expect("written_files lock poisoned")
            .push((path.to_string(), content.to_string()));
        Ok(())
    }

    async fn delete_file(&self, _path: &str) -> Result<(), String> {
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        Ok(self.files.contains_key(path))
    }

    async fn list_directory(
        &self,
        _path: &str,
        _depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        Ok(vec![])
    }

    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        _working_dir: Option<&str>,
        _env_vars: Option<&std::collections::HashMap<String, String>>,
        _cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        *self
            .captured_timeout
            .lock()
            .expect("captured_timeout lock poisoned") = Some(timeout_ms);
        *self
            .captured_command
            .lock()
            .expect("captured_command lock poisoned") = Some(command.to_string());
        Ok(self.exec_result.clone())
    }

    async fn grep(
        &self,
        _pattern: &str,
        _path: &str,
        _options: &GrepOptions,
    ) -> Result<Vec<String>, String> {
        Ok(self.grep_results.clone())
    }

    async fn glob(&self, _pattern: &str, _path: Option<&str>) -> Result<Vec<String>, String> {
        Ok(self.glob_results.clone())
    }

    async fn initialize(&self) -> Result<(), String> {
        self.emit(crate::execution_env::ExecutionEnvEvent::Initializing {
            env_type: "mock".into(),
        });
        self.emit(crate::execution_env::ExecutionEnvEvent::Ready {
            env_type: "mock".into(),
            duration_ms: 0,
        });
        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        self.emit(crate::execution_env::ExecutionEnvEvent::CleanupStarted {
            env_type: "mock".into(),
        });
        self.emit(crate::execution_env::ExecutionEnvEvent::CleanupCompleted {
            env_type: "mock".into(),
            duration_ms: 0,
        });
        Ok(())
    }

    fn working_directory(&self) -> &str {
        self.working_dir
    }

    fn platform(&self) -> &str {
        self.platform_str
    }

    fn os_version(&self) -> String {
        self.os_version_str.clone()
    }
}

// --- MutableMockExecutionEnvironment ---

/// A mock execution environment with Mutex-protected files for tests that need
/// write operations to be visible to subsequent reads (e.g., `apply_patch` tests).
pub struct MutableMockExecutionEnvironment {
    pub files: Mutex<HashMap<String, String>>,
}

impl MutableMockExecutionEnvironment {
    pub fn new(files: HashMap<String, String>) -> Self {
        Self {
            files: Mutex::new(files),
        }
    }
}

#[async_trait]
impl ExecutionEnvironment for MutableMockExecutionEnvironment {
    async fn read_file(
        &self,
        path: &str,
        _offset: Option<usize>,
        _limit: Option<usize>,
    ) -> Result<String, String> {
        self.files
            .lock()
            .expect("files lock poisoned")
            .get(path)
            .cloned()
            .ok_or_else(|| format!("File not found: {path}"))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        self.files
            .lock()
            .expect("files lock poisoned")
            .insert(path.to_string(), content.to_string());
        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<(), String> {
        self.files.lock().expect("files lock poisoned").remove(path);
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        Ok(self
            .files
            .lock()
            .expect("files lock poisoned")
            .contains_key(path))
    }

    async fn list_directory(
        &self,
        _path: &str,
        _depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        Ok(vec![])
    }

    async fn exec_command(
        &self,
        _command: &str,
        _timeout_ms: u64,
        _working_dir: Option<&str>,
        _env_vars: Option<&std::collections::HashMap<String, String>>,
        _cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        Ok(ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
            duration_ms: 0,
        })
    }

    async fn grep(
        &self,
        _pattern: &str,
        _path: &str,
        _options: &GrepOptions,
    ) -> Result<Vec<String>, String> {
        Ok(vec![])
    }

    async fn glob(&self, _pattern: &str, _path: Option<&str>) -> Result<Vec<String>, String> {
        Ok(vec![])
    }

    async fn initialize(&self) -> Result<(), String> {
        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        Ok(())
    }

    fn working_directory(&self) -> &'static str {
        "/tmp"
    }

    fn platform(&self) -> &'static str {
        "linux"
    }

    fn os_version(&self) -> String {
        "Linux 6.1.0".into()
    }
}

// --- TestProfile ---

pub struct TestProfile {
    pub registry: ToolRegistry,
    pub parallel_tool_calls: bool,
    pub context_window: usize,
}

impl TestProfile {
    pub fn new() -> Self {
        Self {
            registry: ToolRegistry::new(),
            parallel_tool_calls: false,
            context_window: 200_000,
        }
    }

    pub fn with_tools(registry: ToolRegistry) -> Self {
        Self {
            registry,
            parallel_tool_calls: false,
            context_window: 200_000,
        }
    }

    pub fn parallel(registry: ToolRegistry) -> Self {
        Self {
            registry,
            parallel_tool_calls: true,
            context_window: 200_000,
        }
    }

    pub fn parallel_with_context_window(registry: ToolRegistry, context_window: usize) -> Self {
        Self {
            registry,
            parallel_tool_calls: true,
            context_window,
        }
    }
}

impl ProviderProfile for TestProfile {
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
        _env: &dyn ExecutionEnvironment,
        _env_context: &EnvContext,
        _project_docs: &[String],
        user_instructions: Option<&str>,
        skills: &[Skill],
    ) -> String {
        let skills_section = crate::skills::format_skills_prompt_section(skills);
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

    fn capabilities(&self) -> ProfileCapabilities {
        ProfileCapabilities {
            supports_reasoning: false,
            supports_streaming: false,
            supports_parallel_tool_calls: self.parallel_tool_calls,
            context_window_size: self.context_window,
        }
    }

    fn knowledge_cutoff(&self) -> &'static str {
        "May 2025"
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

    Box::pin(futures::stream::iter(events))
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
    let env = Arc::new(MockExecutionEnvironment::default());
    Session::new(client, profile, env, SessionConfig::default())
}

pub async fn make_session_with_tools(responses: Vec<Response>, registry: ToolRegistry) -> Session {
    let provider = Arc::new(MockLlmProvider::new(responses));
    let client = make_client(provider).await;
    let profile = Arc::new(TestProfile::with_tools(registry));
    let env = Arc::new(MockExecutionEnvironment::default());
    Session::new(client, profile, env, SessionConfig::default())
}

pub async fn make_session_with_config(responses: Vec<Response>, config: SessionConfig) -> Session {
    let provider = Arc::new(MockLlmProvider::new(responses));
    let client = make_client(provider).await;
    let profile = Arc::new(TestProfile::new());
    let env = Arc::new(MockExecutionEnvironment::default());
    Session::new(client, profile, env, config)
}

pub async fn make_session_with_tools_and_config(
    responses: Vec<Response>,
    registry: ToolRegistry,
    config: SessionConfig,
) -> Session {
    let provider = Arc::new(MockLlmProvider::new(responses));
    let client = make_client(provider).await;
    let profile = Arc::new(TestProfile::with_tools(registry));
    let env = Arc::new(MockExecutionEnvironment::default());
    Session::new(client, profile, env, config)
}

pub fn tool_call_response(
    tool_name: &str,
    tool_call_id: &str,
    args: serde_json::Value,
) -> Response {
    use arc_llm::types::{ContentPart, Role, ToolCall};
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

pub fn make_echo_tool() -> crate::tool_registry::RegisteredTool {
    use arc_llm::types::ToolDefinition;
    crate::tool_registry::RegisteredTool {
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

pub fn make_error_tool() -> crate::tool_registry::RegisteredTool {
    use arc_llm::types::ToolDefinition;
    crate::tool_registry::RegisteredTool {
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
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

pub fn multi_tool_call_response(calls: Vec<(&str, &str, serde_json::Value)>) -> Response {
    use arc_llm::types::{ContentPart, Role, ToolCall};
    let mut content = vec![ContentPart::text("Let me use multiple tools.")];
    for (tool_name, tool_call_id, args) in &calls {
        content.push(ContentPart::ToolCall(ToolCall::new(
            *tool_call_id,
            *tool_name,
            args.clone(),
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
