use crate::config::{SessionConfig, ToolApprovalFn};
use crate::error::AgentError;
use crate::event::EventEmitter;
use crate::execution_env::ExecutionEnvironment;
use crate::history::History;
use crate::loop_detection::detect_loop;
use crate::profiles::EnvContext;
use crate::project_docs::discover_project_docs;
use crate::provider_profile::ProviderProfile;
use crate::tool_registry::ToolRegistry;
use crate::truncation::truncate_tool_output;
use crate::types::{AgentEvent, SessionState, Turn};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use futures::StreamExt;
use llm::client::Client;
use llm::error::{ProviderErrorKind, SdkError};
use llm::generate::StreamAccumulator;
use llm::types::{Message, Request, StreamEvent, ToolChoice, ToolResult};
use tokio_util::sync::CancellationToken;

pub struct Session {
    id: String,
    config: SessionConfig,
    history: History,
    event_emitter: EventEmitter,
    state: SessionState,
    llm_client: Client,
    provider_profile: Arc<dyn ProviderProfile>,
    execution_env: Arc<dyn ExecutionEnvironment>,
    steering_queue: Arc<Mutex<VecDeque<String>>>,
    followup_queue: Arc<Mutex<VecDeque<String>>>,
    cancel_token: CancellationToken,
    project_docs: Vec<String>,
    env_context: EnvContext,
}

impl Session {
    #[must_use]
    pub fn new(
        llm_client: Client,
        provider_profile: Arc<dyn ProviderProfile>,
        execution_env: Arc<dyn ExecutionEnvironment>,
        config: SessionConfig,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            config,
            history: History::default(),
            event_emitter: EventEmitter::new(),
            state: SessionState::Idle,
            llm_client,
            provider_profile,
            execution_env,
            steering_queue: Arc::new(Mutex::new(VecDeque::new())),
            followup_queue: Arc::new(Mutex::new(VecDeque::new())),
            cancel_token: CancellationToken::new(),
            project_docs: Vec::new(),
            env_context: EnvContext::default(),
        }
    }

    /// Initialize session by discovering project docs and capturing environment context.
    /// Call before `process_input`.
    pub async fn initialize(&mut self) {
        self.event_emitter
            .emit(self.id.clone(), AgentEvent::SessionStarted);

        let doc_root = self
            .config
            .git_root
            .clone()
            .unwrap_or_else(|| self.execution_env.working_directory().to_string());
        self.project_docs = discover_project_docs(
            self.execution_env.as_ref(),
            &doc_root,
            self.execution_env.working_directory(),
            self.provider_profile.id(),
        )
        .await;

        // Populate environment context
        self.env_context = self.build_env_context().await;
    }

    async fn build_env_context(&self) -> EnvContext {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let model_name = self.provider_profile.model().to_string();

        // Detect git info via execution environment
        let git_branch = self
            .execution_env
            .exec_command("git rev-parse --abbrev-ref HEAD", 5000, None, None, None)
            .await
            .ok()
            .filter(|r| r.exit_code == 0)
            .map(|r| r.stdout.trim().to_string());

        let is_git_repo = git_branch.is_some();

        let git_status_short = if is_git_repo {
            self.execution_env
                .exec_command("git status --short", 5000, None, None, None)
                .await
                .ok()
                .filter(|r| r.exit_code == 0)
                .map(|r| r.stdout.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        let git_recent_commits = if is_git_repo {
            self.execution_env
                .exec_command("git log --oneline -10", 5000, None, None, None)
                .await
                .ok()
                .filter(|r| r.exit_code == 0)
                .map(|r| r.stdout.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        EnvContext {
            git_branch,
            is_git_repo,
            current_date: today,
            model: model_name,
            knowledge_cutoff: self.provider_profile.knowledge_cutoff().to_string(),
            git_status_short,
            git_recent_commits,
        }
    }

    #[must_use] 
    pub const fn state(&self) -> SessionState {
        self.state
    }

    #[must_use] 
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<crate::types::SessionEvent> {
        self.event_emitter.subscribe()
    }

    pub fn steer(&self, message: String) {
        self.steering_queue
            .lock()
            .expect("steering queue lock poisoned")
            .push_back(message);
    }

    pub fn follow_up(&self, message: String) {
        self.followup_queue
            .lock()
            .expect("followup queue lock poisoned")
            .push_back(message);
    }

    pub fn abort(&self) {
        self.cancel_token.cancel();
    }

    #[must_use] 
    pub fn followup_queue_handle(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.followup_queue.clone()
    }

    #[must_use] 
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    pub fn close(&mut self) {
        if self.state != SessionState::Closed {
            self.state = SessionState::Closed;
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::SessionEnded);
        }
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.config.reasoning_effort = effort;
    }

    pub const fn set_max_turns(&mut self, max_turns: usize) {
        self.config.max_turns = max_turns;
    }

    #[must_use] 
    pub const fn history(&self) -> &History {
        &self.history
    }

    pub async fn process_input(&mut self, input: &str) -> Result<(), AgentError> {
        if self.state == SessionState::Closed {
            return Err(AgentError::SessionClosed);
        }

        // Process the initial input, then drain any followups
        self.run_single_input(input).await?;
        loop {
            let followup = self
                .followup_queue
                .lock()
                .expect("followup queue lock poisoned")
                .pop_front();
            let Some(followup) = followup else { break };
            self.run_single_input(&followup).await?;
        }

        self.state = SessionState::Idle;

        Ok(())
    }

    async fn run_single_input(&mut self, input: &str) -> Result<(), AgentError> {
        if self.state == SessionState::Closed {
            return Err(AgentError::SessionClosed);
        }

        self.state = SessionState::Processing;

        // Append user turn and emit event
        self.history.push(Turn::User {
            content: input.to_string(),
            timestamp: SystemTime::now(),
        });
        self.event_emitter
            .emit(self.id.clone(), AgentEvent::UserInput);

        // Drain steering queue before first LLM call
        self.drain_steering();

        // Cache system prompt for this input cycle (it doesn't change during tool rounds)
        let system_prompt = self.provider_profile.build_system_prompt(
            self.execution_env.as_ref(),
            &self.env_context,
            &self.project_docs,
            self.config.user_instructions.as_deref(),
        );

        let mut round_count: usize = 0;

        loop {
            // Check max_tool_rounds_per_input
            if round_count >= self.config.max_tool_rounds_per_input {
                self.event_emitter
                    .emit(self.id.clone(), AgentEvent::TurnLimitReached);
                break;
            }

            // Check max_turns
            if self.config.max_turns > 0 && self.history.turns().len() >= self.config.max_turns {
                self.event_emitter
                    .emit(self.id.clone(), AgentEvent::TurnLimitReached);
                break;
            }

            // Check cancellation
            if self.cancel_token.is_cancelled() {
                self.close();
                return Err(AgentError::Aborted);
            }

            // Build request
            let request = self.build_request(&system_prompt);

            // Emit AssistantTextStart before LLM call
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::AssistantTextStart);

            // Call LLM (streaming)
            let mut event_stream = match self.llm_client.stream(&request).await {
                Ok(stream) => stream,
                Err(err) => {
                    self.event_emitter.emit(
                        self.id.clone(),
                        AgentEvent::Error {
                            error: err.to_string(),
                        },
                    );
                    if is_auth_error(&err) {
                        self.state = SessionState::Closed;
                    }
                    return Err(AgentError::Llm(err));
                }
            };

            let mut accumulator = StreamAccumulator::new();

            while let Some(event_result) = event_stream.next().await {
                match event_result {
                    Ok(event) => {
                        if let StreamEvent::TextDelta { ref delta, .. } = event {
                            self.event_emitter.emit(
                                self.id.clone(),
                                AgentEvent::TextDelta {
                                    delta: delta.clone(),
                                },
                            );
                        }
                        accumulator.process(&event);
                    }
                    Err(err) => {
                        self.event_emitter.emit(
                            self.id.clone(),
                            AgentEvent::Error {
                                error: err.to_string(),
                            },
                        );
                        return Err(AgentError::Llm(err));
                    }
                }

                // Check cancellation between chunks
                if self.cancel_token.is_cancelled() {
                    break;
                }
            }

            // If aborted during streaming, drop the stream to cancel the HTTP
            // connection, then close the session before returning.
            if self.cancel_token.is_cancelled() {
                drop(event_stream);
                self.close();
                return Err(AgentError::Aborted);
            }

            let response = accumulator.response().cloned().ok_or_else(|| {
                AgentError::Llm(SdkError::Stream {
                    message: "Stream ended without a Finish event".into(),
                })
            })?;

            // Record assistant turn
            let text = response.text();
            let tool_calls = response.tool_calls();
            let reasoning = response.reasoning();
            let provider_parts: Vec<_> = response
                .message
                .content
                .iter()
                .filter(|p| {
                    matches!(
                        p,
                        llm::types::ContentPart::Other { .. }
                            | llm::types::ContentPart::Thinking(_)
                            | llm::types::ContentPart::RedactedThinking(_)
                    )
                })
                .cloned()
                .collect();
            let usage = response.usage.clone();

            self.history.push(Turn::Assistant {
                content: text.clone(),
                tool_calls: tool_calls.clone(),
                reasoning,
                provider_parts,
                usage,
                response_id: response.id.clone(),
                timestamp: SystemTime::now(),
            });

            // Emit AssistantMessage with enriched data from the response
            self.event_emitter.emit(
                self.id.clone(),
                AgentEvent::AssistantMessage {
                    text: text.clone(),
                    model: response.model.clone(),
                    input_tokens: response.usage.input_tokens,
                    output_tokens: response.usage.output_tokens,
                    tool_call_count: tool_calls.len(),
                },
            );

            // Check context window usage and compact if needed
            let over_threshold = self.check_context_usage(&system_prompt);
            if over_threshold && self.config.enable_context_compaction {
                if let Err(e) = self.compact_context(&system_prompt).await {
                    self.event_emitter.emit(
                        self.id.clone(),
                        AgentEvent::Error {
                            error: format!("Context compaction failed: {e}"),
                        },
                    );
                }
            }

            // If no tool calls, natural completion
            if tool_calls.is_empty() {
                break;
            }

            round_count += 1;

            // Execute tool calls (parallel or sequential based on provider)
            let results = self.execute_tool_calls(&tool_calls).await;

            // Check cancellation after tool execution
            if self.cancel_token.is_cancelled() {
                self.history.push(Turn::ToolResults {
                    results,
                    timestamp: SystemTime::now(),
                });
                self.close();
                return Err(AgentError::Aborted);
            }

            // Record tool results turn
            self.history.push(Turn::ToolResults {
                results,
                timestamp: SystemTime::now(),
            });

            // Drain steering after tool execution
            self.drain_steering();

            // Loop detection
            if self.config.enable_loop_detection
                && detect_loop(&self.history, self.config.loop_detection_window)
            {
                self.history.push(Turn::Steering {
                    content: "WARNING: Loop detected. You appear to be repeating the same tool calls. Please try a different approach or ask for clarification.".to_string(),
                    timestamp: SystemTime::now(),
                });
                self.event_emitter
                    .emit(self.id.clone(), AgentEvent::LoopDetected);
            }
        }

        Ok(())
    }

    async fn compact_context(&mut self, system_prompt: &str) -> Result<(), AgentError> {
        let estimated_tokens = self.estimate_token_count(system_prompt);
        let context_window = self.provider_profile.context_window_size();
        let original_turn_count = self.history.turns().len();

        self.event_emitter.emit(
            self.id.clone(),
            AgentEvent::CompactionStarted {
                estimated_tokens,
                context_window_size: context_window,
            },
        );

        let preserve_count = self.config.compaction_preserve_turns;

        // Determine turns to summarize
        if original_turn_count <= preserve_count {
            return Ok(());
        }
        let turns_to_summarize = &self.history.turns()[..original_turn_count - preserve_count];
        let rendered = render_turns_for_summary(turns_to_summarize);

        // Build summarization request
        let summary_request = Request {
            model: self.provider_profile.model().to_string(),
            messages: vec![
                Message::system("You are summarizing a coding assistant conversation to provide continuity. A new context \
window will continue this work with only your summary and the most recent messages. Write \
a summary covering:\n\n\
1. Task & Goal: What the user asked for and any constraints or preferences stated.\n\
2. Completed Work: What was accomplished, with file paths and key decisions.\n\
3. Current State: What is in progress or partially done right now.\n\
4. Failed Approaches: What was tried and didn't work, and why.\n\
5. Open Issues: Bugs, edge cases, or TODOs that remain.\n\
6. Next Steps: What should happen next to make progress.\n\n\
Be specific — include file paths, function names, and error messages. Omit pleasantries \
and conversational filler.".to_string()),
                Message::user(format!("Here is the conversation to summarize:\n\n{rendered}")),
            ],
            provider: Some(self.provider_profile.id().to_string()),
            tools: None,
            tool_choice: None,
            response_format: None,
            temperature: Some(0.0),
            top_p: None,
            max_tokens: Some(4096),
            stop_sequences: None,
            reasoning_effort: None,
            metadata: None,
            provider_options: None,
        };

        let response = self.llm_client.complete(&summary_request).await
            .map_err(AgentError::Llm)?;

        let summary_text = response.text();
        let summary_content = format!("[Context Summary]\n{summary_text}");
        let summary_token_estimate = summary_content.len() / 4;

        self.history.compact(preserve_count, summary_content);

        self.event_emitter.emit(
            self.id.clone(),
            AgentEvent::CompactionCompleted {
                original_turn_count,
                preserved_turn_count: preserve_count,
                summary_token_estimate,
            },
        );

        Ok(())
    }

    fn drain_steering(&mut self) {
        let messages: Vec<String> = self
            .steering_queue
            .lock()
            .expect("steering queue lock poisoned")
            .drain(..)
            .collect();
        for msg in messages {
            self.history.push(Turn::Steering {
                content: msg,
                timestamp: SystemTime::now(),
            });
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::SteeringInjected);
        }
    }

    fn build_request(&self, system_prompt: &str) -> Request {
        let mut messages = vec![Message::system(system_prompt.to_string())];
        messages.extend(self.history.convert_to_messages());

        let tools = self.provider_profile.tools();
        let has_tools = !tools.is_empty();

        Request {
            model: self.provider_profile.model().to_string(),
            messages,
            provider: Some(self.provider_profile.id().to_string()),
            tools: if has_tools { Some(tools) } else { None },
            tool_choice: if has_tools {
                Some(ToolChoice::Auto)
            } else {
                None
            },
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens: llm::catalog::get_model_info(self.provider_profile.model())
                .and_then(|m| m.max_output),
            stop_sequences: None,
            reasoning_effort: self.config.reasoning_effort.clone(),
            metadata: None,
            provider_options: self.provider_profile.provider_options(),
        }
    }

    async fn execute_tool_calls(
        &mut self,
        tool_calls: &[llm::types::ToolCall],
    ) -> Vec<ToolResult> {
        if self.provider_profile.supports_parallel_tool_calls() && tool_calls.len() > 1 {
            self.execute_tool_calls_parallel(tool_calls).await
        } else {
            self.execute_tool_calls_sequential(tool_calls).await
        }
    }

    async fn execute_tool_calls_sequential(
        &self,
        tool_calls: &[llm::types::ToolCall],
    ) -> Vec<ToolResult> {
        let mut results = Vec::new();
        for tc in tool_calls {
            if self.cancel_token.is_cancelled() {
                results.push(ToolResult {
                    tool_call_id: tc.id.clone(),
                    content: serde_json::json!("Cancelled"),
                    is_error: true,
                    image_data: None,
                    image_media_type: None,
                });
                continue;
            }

            self.event_emitter.emit(
                self.id.clone(),
                AgentEvent::ToolCallStarted {
                    tool_name: tc.name.clone(),
                    tool_call_id: tc.id.clone(),
                    arguments: tc.arguments.clone(),
                },
            );

            let result = execute_one_tool(
                &tc.id,
                &tc.name,
                &tc.arguments,
                self.provider_profile.tool_registry(),
                self.execution_env.clone(),
                self.config.tool_approval.as_ref(),
                self.cancel_token.child_token(),
            )
            .await;

            self.event_emitter.emit(
                self.id.clone(),
                AgentEvent::ToolCallOutputDelta {
                    delta: result.content.to_string(),
                },
            );

            self.event_emitter.emit(
                self.id.clone(),
                AgentEvent::ToolCallCompleted {
                    tool_name: tc.name.clone(),
                    tool_call_id: tc.id.clone(),
                    output: result.content.clone(),
                    is_error: result.is_error,
                },
            );

            let truncated = truncate_tool_result(&result, &tc.name, &self.config);
            results.push(truncated);
        }
        results
    }

    async fn execute_tool_calls_parallel(
        &self,
        tool_calls: &[llm::types::ToolCall],
    ) -> Vec<ToolResult> {
        let emitter = self.event_emitter.clone();
        let env = self.execution_env.clone();
        let profile = self.provider_profile.clone();
        let session_id = self.id.clone();
        let config = self.config.clone();
        let cancel_token = self.cancel_token.clone();

        let futures: Vec<_> = tool_calls
            .iter()
            .map(|tc| {
                let emitter = emitter.clone();
                let env = env.clone();
                let profile = profile.clone();
                let session_id = session_id.clone();
                let config = config.clone();
                let cancel_token = cancel_token.clone();
                let tc = tc.clone();
                async move {
                    emitter.emit(
                        session_id.clone(),
                        AgentEvent::ToolCallStarted {
                            tool_name: tc.name.clone(),
                            tool_call_id: tc.id.clone(),
                            arguments: tc.arguments.clone(),
                        },
                    );

                    let result = execute_one_tool(
                        &tc.id,
                        &tc.name,
                        &tc.arguments,
                        profile.tool_registry(),
                        env,
                        config.tool_approval.as_ref(),
                        cancel_token.child_token(),
                    )
                    .await;

                    emitter.emit(
                        session_id.clone(),
                        AgentEvent::ToolCallOutputDelta {
                            delta: result.content.to_string(),
                        },
                    );

                    emitter.emit(
                        session_id,
                        AgentEvent::ToolCallCompleted {
                            tool_name: tc.name.clone(),
                            tool_call_id: tc.id.clone(),
                            output: result.content.clone(),
                            is_error: result.is_error,
                        },
                    );

                    truncate_tool_result(&result, &tc.name, &config)
                }
            })
            .collect();

        futures::future::join_all(futures).await
    }

    fn estimate_token_count(&self, system_prompt: &str) -> usize {
        let mut total_chars = system_prompt.len();

        for turn in self.history.turns() {
            match turn {
                Turn::User { content, .. } => total_chars += content.len(),
                Turn::Assistant {
                    content,
                    tool_calls,
                    reasoning,
                    ..
                } => {
                    total_chars += content.len();
                    if let Some(r) = reasoning {
                        total_chars += r.len();
                    }
                    for tc in tool_calls {
                        total_chars += tc.name.len();
                        total_chars += tc.arguments.to_string().len();
                    }
                }
                Turn::ToolResults { results, .. } => {
                    for r in results {
                        total_chars += r.content.to_string().len();
                    }
                }
                Turn::System { content, .. } | Turn::Steering { content, .. } => {
                    total_chars += content.len();
                }
            }
        }

        total_chars / 4 // rough estimate: ~4 chars per token
    }

    fn check_context_usage(&self, system_prompt: &str) -> bool {
        let estimated_tokens = self.estimate_token_count(system_prompt);
        let context_window = self.provider_profile.context_window_size();
        let threshold = context_window * self.config.compaction_threshold_percent / 100;

        if estimated_tokens > threshold {
            self.event_emitter.emit(
                self.id.clone(),
                AgentEvent::ContextWindowWarning {
                    estimated_tokens,
                    context_window_size: context_window,
                    usage_percent: estimated_tokens * 100 / context_window,
                },
            );
            true
        } else {
            false
        }
    }
}

/// Execute a single tool call: registry lookup, argument validation, and execution.
/// Shared by both sequential and parallel execution paths.
async fn execute_one_tool(
    tool_call_id: &str,
    tool_name: &str,
    arguments: &serde_json::Value,
    registry: &ToolRegistry,
    env: Arc<dyn ExecutionEnvironment>,
    tool_approval: Option<&ToolApprovalFn>,
    cancel_token: CancellationToken,
) -> ToolResult {
    if let Some(approval_fn) = tool_approval {
        if let Err(denial_message) = approval_fn(tool_name, arguments) {
            return ToolResult {
                tool_call_id: tool_call_id.to_string(),
                content: serde_json::json!(denial_message),
                is_error: true,
                image_data: None,
                image_media_type: None,
            };
        }
    }

    match registry.get(tool_name) {
        Some(registered_tool) => {
            if let Err(validation_error) =
                validate_tool_args(&registered_tool.definition.parameters, arguments)
            {
                return ToolResult {
                    tool_call_id: tool_call_id.to_string(),
                    content: serde_json::json!(validation_error),
                    is_error: true,
                    image_data: None,
                    image_media_type: None,
                };
            }

            match (registered_tool.executor)(arguments.clone(), env, cancel_token).await {
                Ok(output) => ToolResult {
                    tool_call_id: tool_call_id.to_string(),
                    content: serde_json::json!(output),
                    is_error: false,
                    image_data: None,
                    image_media_type: None,
                },
                Err(err) => ToolResult {
                    tool_call_id: tool_call_id.to_string(),
                    content: serde_json::json!(err),
                    is_error: true,
                    image_data: None,
                    image_media_type: None,
                },
            }
        }
        None => ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: serde_json::json!(format!("Unknown tool: {tool_name}")),
            is_error: true,
            image_data: None,
            image_media_type: None,
        },
    }
}

/// Truncate tool output for history storage while preserving identity fields.
fn truncate_tool_result(
    result: &ToolResult,
    tool_name: &str,
    config: &SessionConfig,
) -> ToolResult {
    let truncated_content = match &result.content {
        serde_json::Value::String(s) => {
            serde_json::json!(truncate_tool_output(s, tool_name, config))
        }
        other => other.clone(),
    };

    ToolResult {
        tool_call_id: result.tool_call_id.clone(),
        content: truncated_content,
        is_error: result.is_error,
        image_data: result.image_data.clone(),
        image_media_type: result.image_media_type.clone(),
    }
}

const fn is_auth_error(err: &SdkError) -> bool {
    matches!(
        err.provider_kind(),
        Some(ProviderErrorKind::Authentication | ProviderErrorKind::AccessDenied)
    )
}

fn render_turns_for_summary(turns: &[Turn]) -> String {
    let mut out = String::new();
    for turn in turns {
        match turn {
            Turn::User { content, .. } => {
                out.push_str(&format!("User: {content}\n"));
            }
            Turn::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if !content.is_empty() {
                    out.push_str(&format!("Assistant: {content}\n"));
                }
                for tc in tool_calls {
                    let args_str = tc.arguments.to_string();
                    let truncated = if args_str.len() > 500 {
                        format!("{}...", &args_str[..500])
                    } else {
                        args_str
                    };
                    out.push_str(&format!("[Tool call: {}] {truncated}\n", tc.name));
                }
            }
            Turn::ToolResults { results, .. } => {
                for r in results {
                    let content_str = r.content.to_string();
                    let truncated = if content_str.len() > 500 {
                        format!("{}...", &content_str[..500])
                    } else {
                        content_str
                    };
                    out.push_str(&format!("[Tool result: {}] {truncated}\n", r.tool_call_id));
                }
            }
            Turn::System { content, .. } => {
                out.push_str(&format!("System: {content}\n"));
            }
            Turn::Steering { content, .. } => {
                out.push_str(&format!("Steering: {content}\n"));
            }
        }
    }
    out
}

fn validate_tool_args(schema: &serde_json::Value, args: &serde_json::Value) -> Result<(), String> {
    // Skip validation for empty/trivial schemas
    if schema.is_null() {
        return Ok(());
    }
    if let Some(obj) = schema.as_object() {
        if obj.is_empty() {
            return Ok(());
        }
    }

    let validator = jsonschema::validator_for(schema)
        .map_err(|e| format!("Invalid tool schema: {e}"))?;

    let errors: Vec<String> = validator.iter_errors(args).map(|e| e.to_string()).collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Tool argument validation failed: {}",
            errors.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use crate::tool_registry::{RegisteredTool, ToolRegistry};
    use llm::error::ProviderErrorDetail;
    use llm::provider::{ProviderAdapter, StreamEventStream};
    use llm::types::{Response, ToolDefinition};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // --- Tests ---

    #[tokio::test]
    async fn new_session_starts_idle() {
        let session = make_session(vec![]).await;
        assert_eq!(session.state(), SessionState::Idle);
    }

    #[tokio::test]
    async fn text_only_response_natural_completion() {
        let mut session = make_session(vec![text_response("Hello there!")]).await;
        session.process_input("Hi").await.unwrap();

        assert_eq!(session.state(), SessionState::Idle);
        let turns = session.history().turns();
        // UserTurn + AssistantTurn = 2
        assert_eq!(turns.len(), 2);
        assert!(matches!(&turns[0], Turn::User { content, .. } if content == "Hi"));
        assert!(
            matches!(&turns[1], Turn::Assistant { content, .. } if content == "Hello there!")
        );
    }

    #[tokio::test]
    async fn tool_call_then_text() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "hello"})),
            text_response("Done!"),
        ];

        let mut session = make_session_with_tools(responses, registry).await;
        session.process_input("Use echo tool").await.unwrap();

        assert_eq!(session.state(), SessionState::Idle);
        let turns = session.history().turns();
        // UserTurn + AssistantTurn(tool_call) + ToolResults + AssistantTurn(text) = 4
        assert_eq!(turns.len(), 4);
        assert!(matches!(&turns[0], Turn::User { .. }));
        assert!(matches!(&turns[1], Turn::Assistant { tool_calls, .. } if tool_calls.len() == 1));
        assert!(matches!(&turns[2], Turn::ToolResults { results, .. } if results.len() == 1));
        assert!(
            matches!(&turns[3], Turn::Assistant { content, .. } if content == "Done!")
        );

        // Verify tool result content
        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert_eq!(results[0].tool_call_id, "call_1");
            assert!(!results[0].is_error);
        }
    }

    #[tokio::test]
    async fn max_tool_rounds_enforced() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        // Respond with tool calls indefinitely
        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "a"})),
            tool_call_response("echo", "call_2", serde_json::json!({"text": "b"})),
            tool_call_response("echo", "call_3", serde_json::json!({"text": "c"})),
        ];

        let config = SessionConfig {
            max_tool_rounds_per_input: 2,
            enable_loop_detection: false,
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        session.process_input("Keep using tools").await.unwrap();

        // Should stop after 2 rounds: User + (Asst+ToolResult) * 2 = 5 turns
        assert_eq!(session.state(), SessionState::Idle);
        let turns = session.history().turns();
        assert_eq!(turns.len(), 5);
    }

    #[tokio::test]
    async fn max_turns_enforced() {
        let responses = vec![
            text_response("first"),
            text_response("second"),
            text_response("should not reach"),
        ];

        let config = SessionConfig {
            max_turns: 3,
            ..Default::default()
        };

        let mut session = make_session_with_config(responses, config).await;

        // First input: adds User + Assistant = 2 turns
        session.process_input("one").await.unwrap();
        assert_eq!(session.history().turns().len(), 2);

        // Second input: adds User (now 3 turns), then max_turns check triggers
        session.process_input("two").await.unwrap();
        // Should have 3 turns total (User + Asst + User), max_turns hit before LLM call
        assert_eq!(session.history().turns().len(), 3);
    }

    #[tokio::test]
    async fn steer_injects_steering_turn() {
        let mut session = make_session(vec![text_response("OK")]).await;
        session.steer("Focus on the task".to_string());
        session.process_input("Do something").await.unwrap();

        let turns = session.history().turns();
        // User + Steering + Assistant = 3
        assert_eq!(turns.len(), 3);
        assert!(matches!(&turns[0], Turn::User { .. }));
        assert!(
            matches!(&turns[1], Turn::Steering { content, .. } if content == "Focus on the task")
        );
        assert!(matches!(&turns[2], Turn::Assistant { .. }));
    }

    #[tokio::test]
    async fn follow_up_triggers_new_cycle() {
        let responses = vec![
            text_response("First response"),
            text_response("Followup response"),
        ];

        let mut session = make_session(responses).await;
        session.follow_up("followup message".to_string());
        session.process_input("initial message").await.unwrap();

        let turns = session.history().turns();
        // First cycle: User + Assistant = 2
        // Second cycle: User + Assistant = 2
        // Total = 4
        assert_eq!(turns.len(), 4);
        assert!(matches!(&turns[0], Turn::User { content, .. } if content == "initial message"));
        assert!(matches!(&turns[1], Turn::Assistant { content, .. } if content == "First response"));
        assert!(matches!(&turns[2], Turn::User { content, .. } if content == "followup message"));
        assert!(matches!(&turns[3], Turn::Assistant { content, .. } if content == "Followup response"));
    }

    #[tokio::test]
    async fn events_emitted() {
        let mut session = make_session(vec![text_response("Hello")]).await;
        let mut rx = session.subscribe();

        session.initialize().await;
        session.process_input("Hi").await.unwrap();
        session.close();

        // Collect events
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        assert!(events.iter().any(|e| matches!(e.event, AgentEvent::SessionStarted)));
        assert!(events.iter().any(|e| matches!(e.event, AgentEvent::UserInput)));
        assert!(events.iter().any(|e| matches!(e.event, AgentEvent::AssistantMessage { .. })));
        assert!(events.iter().any(|e| matches!(e.event, AgentEvent::SessionEnded)));
    }

    #[tokio::test]
    async fn tool_call_end_has_untruncated_output() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "hello world"})),
            text_response("Done"),
        ];

        let mut session = make_session_with_tools(responses, registry).await;
        let mut rx = session.subscribe();

        session.process_input("Use echo").await.unwrap();

        let mut tool_end_events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEvent::ToolCallCompleted { .. }) {
                tool_end_events.push(event);
            }
        }

        assert_eq!(tool_end_events.len(), 1);
        match &tool_end_events[0].event {
            AgentEvent::ToolCallCompleted { output, .. } => {
                assert_eq!(output, &serde_json::json!("echo: hello world"));
            }
            _ => panic!("Expected ToolCallCompleted event"),
        }
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        // No tools registered, but LLM returns a tool call
        let responses = vec![
            tool_call_response("nonexistent_tool", "call_1", serde_json::json!({})),
            text_response("OK"),
        ];

        let mut session = make_session(responses).await;
        session.process_input("Do something").await.unwrap();

        let turns = session.history().turns();
        // User + Asst(tool_call) + ToolResults + Asst(text) = 4
        assert_eq!(turns.len(), 4);
        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert!(results[0].is_error);
            assert_eq!(
                results[0].content,
                serde_json::json!("Unknown tool: nonexistent_tool")
            );
        } else {
            panic!("Expected ToolResults turn at index 2");
        }
    }

    #[tokio::test]
    async fn tool_execution_error() {
        let mut registry = ToolRegistry::new();
        registry.register(make_error_tool());

        let responses = vec![
            tool_call_response("fail_tool", "call_1", serde_json::json!({})),
            text_response("OK"),
        ];

        let mut session = make_session_with_tools(responses, registry).await;
        session.process_input("Use fail tool").await.unwrap();

        let turns = session.history().turns();
        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert!(results[0].is_error);
            assert_eq!(
                results[0].content,
                serde_json::json!("tool execution failed")
            );
        } else {
            panic!("Expected ToolResults turn at index 2");
        }
    }

    #[tokio::test]
    async fn loop_detection_injects_warning() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        // Same tool call repeated multiple times to trigger loop detection
        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "same"})),
            tool_call_response("echo", "call_2", serde_json::json!({"text": "same"})),
            tool_call_response("echo", "call_3", serde_json::json!({"text": "same"})),
            text_response("Done"),
        ];

        let config = SessionConfig {
            enable_loop_detection: true,
            loop_detection_window: 3,
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        let mut rx = session.subscribe();

        session.process_input("Keep echoing").await.unwrap();

        // Check for LoopDetected event
        let mut found_loop_detection = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEvent::LoopDetected) {
                found_loop_detection = true;
            }
        }
        assert!(found_loop_detection);

        // Check for Steering turn with warning in history
        let has_steering_warning = session.history().turns().iter().any(|t| {
            matches!(t, Turn::Steering { content, .. } if content.contains("Loop detected"))
        });
        assert!(has_steering_warning);
    }

    #[tokio::test]
    async fn abort_stops_processing() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "a"})),
            tool_call_response("echo", "call_2", serde_json::json!({"text": "b"})),
        ];

        let config = SessionConfig {
            enable_loop_detection: false,
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        // Set abort before processing
        session.abort();
        let result = session.process_input("Do something").await;

        // Should return Aborted error and transition to Closed
        assert!(matches!(result, Err(AgentError::Aborted)));
        assert_eq!(session.state(), SessionState::Closed);

        // Should have stopped immediately: User turn only, no LLM call
        let turns = session.history().turns();
        assert_eq!(turns.len(), 1);
        assert!(matches!(&turns[0], Turn::User { .. }));
    }

    #[tokio::test]
    async fn abort_transitions_to_closed() {
        let cancel_token = CancellationToken::new();
        let cancel_token_for_tool = cancel_token.clone();

        // Tool that cancels the token when executed
        let abort_tool = RegisteredTool {
            definition: ToolDefinition {
                name: "set_abort".into(),
                description: "Sets abort flag".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            executor: Arc::new(move |_args, _env, _cancel| {
                let token = cancel_token_for_tool.clone();
                Box::pin(async move {
                    token.cancel();
                    Ok("done".to_string())
                })
            }),
        };

        let mut registry = ToolRegistry::new();
        registry.register(abort_tool);

        let responses = vec![
            tool_call_response("set_abort", "call_1", serde_json::json!({})),
            text_response("Should not reach this"),
        ];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::with_tools(registry));
        let env = Arc::new(MockExecutionEnvironment::default());
        let config = SessionConfig {
            enable_loop_detection: false,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config);

        // Wire the session's cancel_token to our shared one
        session.cancel_token = cancel_token;

        let result = session.process_input("Do something").await;

        // Should return Aborted error and transition to Closed
        assert!(matches!(result, Err(AgentError::Aborted)));
        assert_eq!(session.state(), SessionState::Closed);

        // Should have processed: User + Assistant(tool_call) + ToolResults = 3 turns
        // The tool cancelled the token, so the loop breaks before the next LLM call
        let turns = session.history().turns();
        assert_eq!(turns.len(), 3);
        assert!(matches!(&turns[0], Turn::User { .. }));
        assert!(matches!(&turns[1], Turn::Assistant { tool_calls, .. } if tool_calls.len() == 1));
        assert!(matches!(&turns[2], Turn::ToolResults { .. }));
    }

    #[tokio::test]
    async fn auth_error_closes_session() {
        let error_provider = Arc::new(MockErrorProvider {
            error: SdkError::Provider {
                kind: ProviderErrorKind::Authentication,
                detail: Box::new(ProviderErrorDetail::new("invalid api key", "mock")),
            },
        });
        let client = make_client(error_provider).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockExecutionEnvironment::default());
        let mut session = Session::new(client, profile, env, SessionConfig::default());

        let result = session.process_input("Hello").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AgentError::Llm(_)));
        assert_eq!(session.state(), SessionState::Closed);
    }

    #[tokio::test]
    async fn sequential_inputs() {
        let responses = vec![
            text_response("First"),
            text_response("Second"),
        ];

        let mut session = make_session(responses).await;

        session.process_input("one").await.unwrap();
        assert_eq!(session.state(), SessionState::Idle);

        session.process_input("two").await.unwrap();
        assert_eq!(session.state(), SessionState::Idle);

        let turns = session.history().turns();
        assert_eq!(turns.len(), 4);
        assert!(matches!(&turns[0], Turn::User { content, .. } if content == "one"));
        assert!(matches!(&turns[1], Turn::Assistant { content, .. } if content == "First"));
        assert!(matches!(&turns[2], Turn::User { content, .. } if content == "two"));
        assert!(matches!(&turns[3], Turn::Assistant { content, .. } if content == "Second"));
    }

    #[tokio::test]
    async fn closed_session_rejects_input() {
        let mut session = make_session(vec![]).await;
        session.close();
        assert_eq!(session.state(), SessionState::Closed);

        let result = session.process_input("Hello").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AgentError::SessionClosed));
    }

    #[tokio::test]
    async fn closed_session_does_not_emit_session_start() {
        let mut session = make_session(vec![]).await;
        session.close();

        let mut rx = session.subscribe();
        let result = session.process_input("Hello").await;
        assert!(matches!(result, Err(AgentError::SessionClosed)));

        // No SessionStarted event should have been emitted
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(
            !events.iter().any(|e| matches!(e.event, AgentEvent::SessionStarted)),
            "SessionStarted should not be emitted for a closed session"
        );
    }

    #[tokio::test]
    async fn parallel_tool_execution_all_results_returned() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let responses = vec![
            multi_tool_call_response(vec![
                ("echo", "call_1", serde_json::json!({"text": "first"})),
                ("echo", "call_2", serde_json::json!({"text": "second"})),
                ("echo", "call_3", serde_json::json!({"text": "third"})),
            ]),
            text_response("All done!"),
        ];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::parallel(registry));
        let env = Arc::new(MockExecutionEnvironment::default());
        let mut session = Session::new(client, profile, env, SessionConfig::default());
        let mut rx = session.subscribe();

        session.process_input("Use echo three times").await.unwrap();

        let turns = session.history().turns();
        // User + Assistant(3 tool calls) + ToolResults + Assistant(text) = 4
        assert_eq!(turns.len(), 4);

        // Verify all 3 tool results collected
        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert_eq!(results.len(), 3);
            assert_eq!(results[0].tool_call_id, "call_1");
            assert_eq!(results[1].tool_call_id, "call_2");
            assert_eq!(results[2].tool_call_id, "call_3");
            assert!(!results[0].is_error);
            assert!(!results[1].is_error);
            assert!(!results[2].is_error);
        } else {
            panic!("Expected ToolResults turn at index 2");
        }

        // Verify ToolCallStarted and ToolCallCompleted events for all 3 calls
        let mut start_count = 0;
        let mut end_count = 0;
        while let Ok(event) = rx.try_recv() {
            match &event.event {
                AgentEvent::ToolCallStarted { .. } => start_count += 1,
                AgentEvent::ToolCallCompleted { .. } => end_count += 1,
                _ => {}
            }
        }
        assert_eq!(start_count, 3);
        assert_eq!(end_count, 3);
    }

    #[tokio::test]
    async fn context_window_warning_emitted_at_threshold() {
        // Use a very small context window (100 tokens = 400 chars)
        // System prompt "You are a test assistant." = 26 chars = ~6 tokens
        // We need total > 80 tokens (80% of 100)
        // So we need ~320+ chars of content beyond system prompt
        let large_input = "x".repeat(400);

        let responses = vec![text_response("OK")];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let registry = ToolRegistry::new();
        let profile = Arc::new(TestProfile::parallel_with_context_window(
            registry, 100,
        ));
        let env = Arc::new(MockExecutionEnvironment::default());
        let mut session = Session::new(client, profile, env, SessionConfig::default());
        let mut rx = session.subscribe();

        session.process_input(&large_input).await.unwrap();

        let mut found_warning = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::ContextWindowWarning {
                context_window_size,
                ..
            } = &event.event
            {
                found_warning = true;
                assert_eq!(*context_window_size, 100);
            }
        }
        assert!(found_warning);
    }

    #[tokio::test]
    async fn set_reasoning_effort_mid_session() {
        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockExecutionEnvironment::default());
        let mut session = Session::new(client, profile, env, SessionConfig::default());

        // Default reasoning_effort is None
        session.set_reasoning_effort(Some("high".to_string()));
        session.process_input("test").await.unwrap();

        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured.as_ref().expect("request should have been captured");
        assert_eq!(request.reasoning_effort, Some("high".to_string()));
    }

    #[tokio::test]
    async fn context_window_no_warning_under_threshold() {
        let responses = vec![text_response("OK")];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let registry = ToolRegistry::new();
        // Large context window so short input stays well under 80%
        let profile = Arc::new(TestProfile::parallel_with_context_window(
            registry, 200_000,
        ));
        let env = Arc::new(MockExecutionEnvironment::default());
        let mut session = Session::new(client, profile, env, SessionConfig::default());
        let mut rx = session.subscribe();

        session.process_input("Hi").await.unwrap();

        let mut found_warning = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEvent::ContextWindowWarning { .. }) {
                found_warning = true;
            }
        }
        assert!(!found_warning);
    }

    #[test]
    fn render_turns_produces_labeled_text() {
        use llm::types::{ToolCall, ToolResult, Usage};

        let turns = vec![
            Turn::User {
                content: "Hello".into(),
                timestamp: SystemTime::now(),
            },
            Turn::Assistant {
                content: "Let me check".into(),
                tool_calls: vec![ToolCall::new("c1", "read_file", serde_json::json!({"path": "foo.rs"}))],
                reasoning: None,
                provider_parts: vec![],
                usage: Usage::default(),
                response_id: "resp_1".into(),
                timestamp: SystemTime::now(),
            },
            Turn::ToolResults {
                results: vec![ToolResult {
                    tool_call_id: "c1".into(),
                    content: serde_json::json!("file contents here"),
                    is_error: false,
                    image_data: None,
                    image_media_type: None,
                }],
                timestamp: SystemTime::now(),
            },
        ];
        let rendered = render_turns_for_summary(&turns);
        assert!(rendered.contains("User:"));
        assert!(rendered.contains("Hello"));
        assert!(rendered.contains("Assistant:"));
        assert!(rendered.contains("Let me check"));
        assert!(rendered.contains("[Tool call: read_file]"));
        assert!(rendered.contains("[Tool result: c1]"));
    }

    #[test]
    fn render_turns_truncates_long_tool_output() {
        use llm::types::{ToolResult, Usage};

        let long_output = "x".repeat(1000);
        let turns = vec![Turn::ToolResults {
            results: vec![ToolResult {
                tool_call_id: "c1".into(),
                content: serde_json::json!(long_output),
                is_error: false,
                image_data: None,
                image_media_type: None,
            }],
            timestamp: SystemTime::now(),
        }];
        let rendered = render_turns_for_summary(&turns);
        // Should be truncated to 500 chars + "..."
        assert!(rendered.len() < 1000);
        assert!(rendered.contains("..."));
    }

    #[tokio::test]
    async fn check_context_usage_returns_true_over_threshold() {
        let large_input = "x".repeat(400);
        let responses = vec![text_response("OK")];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let registry = ToolRegistry::new();
        let profile = Arc::new(TestProfile::parallel_with_context_window(registry, 100));
        let env = Arc::new(MockExecutionEnvironment::default());
        let config = SessionConfig {
            enable_context_compaction: false, // disable compaction to isolate check
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config);

        // Push a user turn to populate history
        session.process_input(&large_input).await.unwrap();

        let system_prompt = "You are a test assistant.";
        assert!(session.check_context_usage(system_prompt));
    }

    #[tokio::test]
    async fn invalid_tool_args_returns_validation_error() {
        let mut registry = ToolRegistry::new();
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name: "strict_tool".into(),
                description: "Tool with required params".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"]
                }),
            },
            executor: Arc::new(|_args, _env, _cancel| {
                Box::pin(async move { Ok("should not reach".to_string()) })
            }),
        });

        let responses = vec![
            tool_call_response("strict_tool", "call_1", serde_json::json!({})),
            text_response("Done"),
        ];

        let mut session = make_session_with_tools(responses, registry).await;
        session.process_input("Use strict tool").await.unwrap();

        let turns = session.history().turns();
        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert!(results[0].is_error);
            let content_str = results[0].content.to_string();
            assert!(
                content_str.contains("text") && content_str.contains("required"),
                "Expected validation error mentioning 'text' and 'required', got: {content_str}"
            );
        } else {
            panic!("Expected ToolResults turn at index 2");
        }
    }

    #[tokio::test]
    async fn valid_tool_args_passes_validation() {
        let mut registry = ToolRegistry::new();
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name: "strict_tool".into(),
                description: "Tool with required params".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"]
                }),
            },
            executor: Arc::new(|_args, _env, _cancel| {
                Box::pin(async move { Ok("tool executed".to_string()) })
            }),
        });

        let responses = vec![
            tool_call_response(
                "strict_tool",
                "call_1",
                serde_json::json!({"text": "hello"}),
            ),
            text_response("Done"),
        ];

        let mut session = make_session_with_tools(responses, registry).await;
        session.process_input("Use strict tool").await.unwrap();

        let turns = session.history().turns();
        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert!(!results[0].is_error);
        } else {
            panic!("Expected ToolResults turn at index 2");
        }
    }

    #[tokio::test]
    async fn session_start_emitted_once_for_multiple_inputs() {
        let responses = vec![
            text_response("First"),
            text_response("Second"),
        ];

        let mut session = make_session(responses).await;
        let mut rx = session.subscribe();

        session.initialize().await;
        session.process_input("one").await.unwrap();
        session.process_input("two").await.unwrap();
        session.close();

        let mut session_start_count = 0;
        let mut session_end_count = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEvent::SessionStarted) {
                session_start_count += 1;
            }
            if matches!(event.event, AgentEvent::SessionEnded) {
                session_end_count += 1;
            }
        }
        // SessionStarted is emitted once during initialize(), SessionEnded once during close()
        assert_eq!(session_start_count, 1);
        assert_eq!(session_end_count, 1);
    }

    #[tokio::test]
    async fn user_instructions_in_system_prompt() {
        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockExecutionEnvironment::default());
        let config = SessionConfig {
            user_instructions: Some("Always use TDD".into()),
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config);
        session.process_input("test").await.unwrap();

        // Verify user instructions are included in the system prompt
        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured.as_ref().expect("request should have been captured");
        let system_msg = &request.messages[0];
        let system_text = system_msg.text();
        assert!(
            system_text.contains("Always use TDD"),
            "System prompt should contain user instructions"
        );
    }

    #[tokio::test]
    async fn tool_approval_denies_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "hello"})),
            text_response("OK after denial"),
        ];

        let config = SessionConfig {
            tool_approval: Some(Arc::new(|_name, _args| {
                Err("denied by policy".to_string())
            })),
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        session.process_input("Use echo").await.unwrap();

        assert_eq!(session.state(), SessionState::Idle);
        let turns = session.history().turns();
        // User + Assistant(tool_call) + ToolResults + Assistant(text) = 4
        assert_eq!(turns.len(), 4);

        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert!(results[0].is_error);
            let content_str = results[0].content.to_string();
            assert!(
                content_str.contains("denied by policy"),
                "Expected denial message in content, got: {content_str}"
            );
        } else {
            panic!("Expected ToolResults turn at index 2");
        }

        assert!(
            matches!(&turns[3], Turn::Assistant { content, .. } if content == "OK after denial")
        );
    }

    #[tokio::test]
    async fn tool_approval_allows_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "hello"})),
            text_response("Done"),
        ];

        let config = SessionConfig {
            tool_approval: Some(Arc::new(|_name, _args| Ok(()))),
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        session.process_input("Use echo").await.unwrap();

        let turns = session.history().turns();
        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert!(!results[0].is_error);
            let content_str = results[0].content.to_string();
            assert!(
                content_str.contains("echo: hello"),
                "Expected echo output in content, got: {content_str}"
            );
        } else {
            panic!("Expected ToolResults turn at index 2");
        }
    }

    #[tokio::test]
    async fn tool_approval_receives_correct_args() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let captured: Arc<Mutex<Option<(String, serde_json::Value)>>> =
            Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();

        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "world"})),
            text_response("Done"),
        ];

        let config = SessionConfig {
            tool_approval: Some(Arc::new(move |name, args| {
                *captured_clone.lock().unwrap() = Some((name.to_string(), args.clone()));
                Ok(())
            })),
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        session.process_input("Use echo").await.unwrap();

        let captured_value = captured.lock().unwrap();
        let (name, args) = captured_value.as_ref().expect("approval fn should have been called");
        assert_eq!(name, "echo");
        assert_eq!(args, &serde_json::json!({"text": "world"}));
    }

    #[tokio::test]
    async fn tool_approval_none_skips_check() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "hello"})),
            text_response("Done"),
        ];

        let config = SessionConfig {
            tool_approval: None,
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        session.process_input("Use echo").await.unwrap();

        let turns = session.history().turns();
        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert!(!results[0].is_error);
            let content_str = results[0].content.to_string();
            assert!(
                content_str.contains("echo: hello"),
                "Expected echo output in content, got: {content_str}"
            );
        } else {
            panic!("Expected ToolResults turn at index 2");
        }
    }

    #[tokio::test]
    async fn tool_approval_denial_emits_error_event() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "hello"})),
            text_response("Done"),
        ];

        let config = SessionConfig {
            tool_approval: Some(Arc::new(|_name, _args| {
                Err("not allowed".to_string())
            })),
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        let mut rx = session.subscribe();

        session.process_input("Use echo").await.unwrap();

        let mut tool_end_events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEvent::ToolCallCompleted { .. }) {
                tool_end_events.push(event);
            }
        }

        assert_eq!(tool_end_events.len(), 1);
        match &tool_end_events[0].event {
            AgentEvent::ToolCallCompleted { is_error, .. } => {
                assert!(is_error, "ToolCallCompleted event should have is_error: true");
            }
            _ => panic!("Expected ToolCallCompleted event"),
        }
    }

    #[tokio::test]
    async fn stream_emits_text_delta_events() {
        let mut session = make_session(vec![text_response("Hello there!")]).await;
        let mut rx = session.subscribe();

        session.process_input("Hi").await.unwrap();

        let mut deltas = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::TextDelta { delta } = &event.event {
                deltas.push(delta.clone());
            }
        }

        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0], "Hello there!");
    }

    #[tokio::test]
    async fn stream_mid_stream_error() {
        let provider = Arc::new(MockMidStreamErrorProvider {
            partial_text: "partial".into(),
            error: SdkError::Stream {
                message: "connection reset".into(),
            },
        });
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockExecutionEnvironment::default());
        let mut session = Session::new(client, profile, env, SessionConfig::default());

        let result = session.process_input("Hello").await;
        assert!(matches!(result, Err(AgentError::Llm(SdkError::Stream { .. }))));
    }

    #[tokio::test]
    async fn compaction_triggered_when_over_threshold() {
        // Tiny context window to trigger compaction
        // Responses: [0] conversation response (stream), [1] summarization (complete), [2] unused fallback
        let responses = vec![
            text_response("OK"),
            text_response("Here is the summary of the conversation so far."),
            text_response("fallback"),
        ];

        let large_input = "x".repeat(400);

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let registry = ToolRegistry::new();
        let profile = Arc::new(TestProfile::parallel_with_context_window(registry, 100));
        let env = Arc::new(MockExecutionEnvironment::default());
        let config = SessionConfig {
            enable_context_compaction: true,
            compaction_preserve_turns: 1,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config);
        let mut rx = session.subscribe();

        session.process_input(&large_input).await.unwrap();

        let mut found_started = false;
        let mut found_completed = false;
        while let Ok(event) = rx.try_recv() {
            match &event.event {
                AgentEvent::CompactionStarted { .. } => found_started = true,
                AgentEvent::CompactionCompleted { .. } => found_completed = true,
                _ => {}
            }
        }
        assert!(found_started, "CompactionStarted event should be emitted");
        assert!(found_completed, "CompactionCompleted event should be emitted");

        // History should have been compacted: summary turn + preserved turns
        let turns = session.history().turns();
        assert!(
            turns.iter().any(|t| matches!(t, Turn::System { content, .. } if content.contains("[Context Summary]"))),
            "Should contain a summary system turn"
        );
    }

    #[tokio::test]
    async fn compaction_not_triggered_when_disabled() {
        let large_input = "x".repeat(400);
        let responses = vec![text_response("OK")];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let registry = ToolRegistry::new();
        let profile = Arc::new(TestProfile::parallel_with_context_window(registry, 100));
        let env = Arc::new(MockExecutionEnvironment::default());
        let config = SessionConfig {
            enable_context_compaction: false,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config);
        let mut rx = session.subscribe();

        session.process_input(&large_input).await.unwrap();

        let mut found_compaction = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEvent::CompactionStarted { .. } | AgentEvent::CompactionCompleted { .. }) {
                found_compaction = true;
            }
        }
        assert!(!found_compaction, "No compaction events when disabled");
    }

    #[tokio::test]
    async fn compaction_failure_is_non_fatal() {
        // Response [0] = conversation response (stream), [1] will be used for summarization (complete) but we
        // need it to error. We'll use a special provider that errors on complete() but succeeds on stream().

        struct StreamOnlyProvider {
            responses: Vec<Response>,
            call_index: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl ProviderAdapter for StreamOnlyProvider {
            fn name(&self) -> &'static str { "mock" }

            async fn complete(&self, _request: &Request) -> Result<Response, SdkError> {
                Err(SdkError::Stream { message: "summarization failed".into() })
            }

            async fn stream(&self, _request: &Request) -> Result<StreamEventStream, SdkError> {
                let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
                let response = if idx < self.responses.len() {
                    self.responses[idx].clone()
                } else {
                    self.responses[self.responses.len() - 1].clone()
                };
                // Reuse response_to_stream helper from test_support
                let mut events: Vec<Result<StreamEvent, SdkError>> = Vec::new();
                let text = response.text();
                if !text.is_empty() {
                    events.push(Ok(StreamEvent::text_delta(text, None)));
                }
                for part in &response.message.content {
                    if let llm::types::ContentPart::ToolCall(tc) = part {
                        events.push(Ok(StreamEvent::ToolCallEnd { tool_call: tc.clone() }));
                    }
                }
                events.push(Ok(StreamEvent::finish(
                    response.finish_reason.clone(),
                    response.usage.clone(),
                    response,
                )));
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let large_input = "x".repeat(400);
        let responses = vec![text_response("OK")];

        let provider = Arc::new(StreamOnlyProvider {
            responses,
            call_index: AtomicUsize::new(0),
        });
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let registry = ToolRegistry::new();
        let profile = Arc::new(TestProfile::parallel_with_context_window(registry, 100));
        let env = Arc::new(MockExecutionEnvironment::default());
        let config = SessionConfig {
            enable_context_compaction: true,
            compaction_preserve_turns: 1,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config);
        let mut rx = session.subscribe();

        // Should not return an error even though compaction fails
        let result = session.process_input(&large_input).await;
        assert!(result.is_ok(), "Session should continue despite compaction failure");

        // Should emit an Error event for the failed compaction
        let mut found_error = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::Error { error } = &event.event {
                if error.contains("compaction") || error.contains("summarization") {
                    found_error = true;
                }
            }
        }
        assert!(found_error, "Should emit Error event for failed compaction");
    }
}