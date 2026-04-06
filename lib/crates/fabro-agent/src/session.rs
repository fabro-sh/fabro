use crate::agent_profile::AgentProfile;
use crate::compaction::{check_context_usage, compact_context};
use crate::config::SessionOptions;
use crate::error::{AbortReason, AgentError};
use crate::event::Emitter;
use crate::file_tracker::FileTracker;
use crate::history::History;
use crate::loop_detection::detect_loop;
use crate::mcp_integration;
use crate::memory::discover_memory;
use crate::profiles::EnvContext;
use crate::sandbox::Sandbox;
use crate::skills::{
    ExpandedInput, Skill, default_skill_dirs, discover_skills, expand_skill, make_use_skill_tool,
};
use crate::subagent::{SubAgentCallbackEvent, SubAgentEventCallback, SubAgentManager};
use crate::tool_execution::execute_tool_calls;
use crate::types::{AgentEvent, SessionEvent, SessionState, Turn};
use fabro_llm::client::Client;
use fabro_llm::error::{ProviderErrorKind, SdkError};
use fabro_llm::generate::StreamAccumulator;
use fabro_llm::provider::StreamEventStream;
use fabro_llm::retry;
use fabro_llm::types::{
    ContentPart, Message, ReasoningEffort, Request, RetryPolicy, StreamEvent, ToolChoice,
};
use fabro_mcp::config::{McpServerSettings, McpTransport};
use fabro_mcp::connection_manager::McpConnectionManager;
use futures::StreamExt;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub struct Session {
    id: String,
    config: SessionOptions,
    history: History,
    event_emitter: Emitter,
    state: SessionState,
    llm_client: Client,
    provider_profile: Arc<dyn AgentProfile>,
    sandbox: Arc<dyn Sandbox>,
    steering_queue: Arc<Mutex<VecDeque<String>>>,
    followup_queue: Arc<Mutex<VecDeque<String>>>,
    cancel_token: CancellationToken,
    abort_reason: Arc<Mutex<Option<AbortReason>>>,
    memory: Vec<String>,
    env_context: EnvContext,
    skills: Vec<Skill>,
    system_prompt: String,
    file_tracker: FileTracker,
    tool_env: Option<HashMap<String, String>>,
    subagent_manager: Option<Arc<AsyncMutex<SubAgentManager>>>,
}

impl Session {
    #[must_use]
    pub fn new(
        llm_client: Client,
        provider_profile: Arc<dyn AgentProfile>,
        sandbox: Arc<dyn Sandbox>,
        config: SessionOptions,
        subagent_manager: Option<Arc<AsyncMutex<SubAgentManager>>>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            config,
            history: History::default(),
            event_emitter: Emitter::new(),
            state: SessionState::Idle,
            llm_client,
            provider_profile,
            sandbox,
            steering_queue: Arc::new(Mutex::new(VecDeque::new())),
            followup_queue: Arc::new(Mutex::new(VecDeque::new())),
            cancel_token: CancellationToken::new(),
            abort_reason: Arc::new(Mutex::new(None)),
            memory: Vec::new(),
            env_context: EnvContext::default(),
            skills: Vec::new(),
            system_prompt: String::new(),
            file_tracker: FileTracker::default(),
            tool_env: None,
            subagent_manager,
        }
    }

    pub fn set_tool_env(&mut self, env: HashMap<String, String>) {
        self.tool_env = Some(env);
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Initialize session by discovering project docs and capturing environment context.
    /// Call before `process_input`.
    pub async fn initialize(&mut self) {
        self.event_emitter.emit(
            self.id.clone(),
            AgentEvent::SessionStarted {
                provider: Some(self.provider_profile.provider().to_string()),
                model: Some(self.provider_profile.model().to_string()),
            },
        );

        let doc_root = self
            .config
            .git_root
            .clone()
            .unwrap_or_else(|| self.sandbox.working_directory().to_string());
        self.memory = discover_memory(
            self.sandbox.as_ref(),
            &doc_root,
            self.sandbox.working_directory(),
            self.provider_profile.provider(),
        )
        .await;

        // Discover skills
        let skill_dirs = if let Some(dirs) = &self.config.skill_dirs {
            dirs.clone()
        } else {
            let skills_dir = fabro_util::Home::from_env().skills_dir();
            let skills_str = skills_dir.to_string_lossy().to_string();
            default_skill_dirs(Some(&skills_str), self.config.git_root.as_deref())
        };
        self.skills = discover_skills(self.sandbox.as_ref(), &skill_dirs).await;
        debug!(skill_count = self.skills.len(), "Skills discovered");

        // Register use_skill tool when skills are available
        if !self.skills.is_empty() {
            let skills_arc = Arc::new(self.skills.clone());
            if let Some(profile) = Arc::get_mut(&mut self.provider_profile) {
                profile
                    .tool_registry_mut()
                    .register(make_use_skill_tool(skills_arc));
            }
        }

        // Start MCP servers and register their tools
        if !self.config.mcp_servers.is_empty() {
            // Resolve Sandbox transports: start the server inside the sandbox,
            // then rewrite the config to Http using the sandbox's preview URL.
            let mcp_servers = self.resolve_sandbox_mcp_servers().await;

            let mut manager = McpConnectionManager::new();
            let results = manager.start_servers(&mcp_servers).await;

            for (server_name, result) in &results {
                match result {
                    Ok(tool_count) => {
                        self.event_emitter.emit(
                            self.id.clone(),
                            AgentEvent::McpServerReady {
                                server_name: server_name.clone(),
                                tool_count: *tool_count,
                            },
                        );
                    }
                    Err(e) => {
                        self.event_emitter.emit(
                            self.id.clone(),
                            AgentEvent::McpServerFailed {
                                server_name: server_name.clone(),
                                error: e.to_string(),
                            },
                        );
                    }
                }
            }

            let manager = Arc::new(manager);
            let mcp_tools = mcp_integration::make_mcp_tools(&manager);
            if let Some(profile) = Arc::get_mut(&mut self.provider_profile) {
                for tool in mcp_tools {
                    profile.tool_registry_mut().register(tool);
                }
            }
        }

        // Populate environment context
        self.env_context = self.build_env_context().await;
        debug!(
            is_git_repo = self.env_context.is_git_repo,
            model = %self.env_context.model,
            "Environment context built"
        );

        // Build system prompt once (static for the session lifetime)
        self.system_prompt = self.provider_profile.build_system_prompt(
            self.sandbox.as_ref(),
            &self.env_context,
            &self.memory,
            self.config.user_instructions.as_deref(),
            &self.skills,
        );
    }

    /// Resolve `McpTransport::Sandbox` configs by starting the MCP server inside the
    /// sandbox and rewriting the transport to `Http` with the sandbox's preview URL.
    async fn resolve_sandbox_mcp_servers(&self) -> Vec<McpServerSettings> {
        let mut resolved = Vec::with_capacity(self.config.mcp_servers.len());

        for config in &self.config.mcp_servers {
            match &config.transport {
                McpTransport::Sandbox { command, port, env } => {
                    let port = *port;
                    match self.start_sandbox_mcp_server(command, port, env).await {
                        Ok((url, headers)) => {
                            info!(
                                server = %config.name,
                                url = %url,
                                "Sandbox MCP server started, connecting via HTTP"
                            );
                            resolved.push(McpServerSettings {
                                name: config.name.clone(),
                                transport: McpTransport::Http { url, headers },
                                startup_timeout_secs: config.startup_timeout_secs,
                                tool_timeout_secs: config.tool_timeout_secs,
                            });
                        }
                        Err(e) => {
                            warn!(
                                server = %config.name,
                                error = %e,
                                "Failed to start sandbox MCP server"
                            );
                            self.event_emitter.emit(
                                self.id.clone(),
                                AgentEvent::McpServerFailed {
                                    server_name: config.name.clone(),
                                    error: e,
                                },
                            );
                        }
                    }
                }
                _ => resolved.push(config.clone()),
            }
        }

        resolved
    }

    /// Start an MCP server inside the sandbox and return (url, headers) for HTTP connection.
    async fn start_sandbox_mcp_server(
        &self,
        command: &[String],
        port: u16,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<(String, std::collections::HashMap<String, String>), String> {
        let sandbox = self.sandbox.as_ref();

        let cmd_str = command.join(" ");

        // Launch the server detached with setsid so Daytona's exec doesn't block
        let launch_script = format!(
            "setsid sh -c '{cmd_str} > /tmp/mcp_server_stdout.log 2>/tmp/mcp_server_stderr.log' \
             </dev/null >/dev/null 2>&1 &\necho $!"
        );
        let env_ref = if env.is_empty() { None } else { Some(env) };
        let launch_result = sandbox
            .exec_command(&launch_script, 30_000, None, env_ref, None)
            .await
            .map_err(|e| format!("Failed to launch MCP server: {e}"))?;

        let pid = launch_result.stdout.trim();
        info!(pid, port, "MCP server process launched in sandbox");

        // Wait for the server to start listening on the port
        let poll_cmd = format!(
            "for i in $(seq 1 30); do ss -tln | grep -q ':{port} ' && echo ready && exit 0; sleep 1; done; echo timeout"
        );
        let poll_result = sandbox
            .exec_command(&poll_cmd, 60_000, None, None, None)
            .await
            .map_err(|e| format!("Failed to poll MCP server readiness: {e}"))?;

        if poll_result.stdout.trim() != "ready" {
            // Grab stderr for debugging
            let stderr = sandbox
                .exec_command(
                    "cat /tmp/mcp_server_stderr.log 2>/dev/null | tail -20",
                    10_000,
                    None,
                    None,
                    None,
                )
                .await
                .map(|r| r.stdout)
                .unwrap_or_default();
            return Err(format!(
                "MCP server did not start listening on port {port} within 30s. stderr:\n{stderr}"
            ));
        }

        // Get the preview URL for the port, or fall back to localhost for local sandboxes
        if let Some(url_and_headers) = sandbox.get_preview_url(port).await? {
            Ok(url_and_headers)
        } else {
            info!(port, "No preview URL available, using localhost");
            Ok((
                format!("http://localhost:{port}"),
                std::collections::HashMap::new(),
            ))
        }
    }

    async fn build_env_context(&self) -> EnvContext {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let model_name = self.provider_profile.model().to_string();

        // Detect git info via sandbox
        let git_branch = self
            .sandbox
            .exec_command("git rev-parse --abbrev-ref HEAD", 5000, None, None, None)
            .await
            .ok()
            .filter(|r| r.exit_code == 0)
            .map(|r| r.stdout.trim().to_string());

        let is_git_repo = git_branch.is_some();

        let git_status_short = if is_git_repo {
            self.sandbox
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
            self.sandbox
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
            knowledge_cutoff: self.provider_profile.knowledge_cutoff().unwrap_or_default(),
            git_status_short,
            git_recent_commits,
        }
    }

    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
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
        self.set_abort_reason(AbortReason::Cancelled);
        self.cancel_token.cancel();
    }

    /// Returns a handle that can set the abort reason from another task.
    #[must_use]
    pub fn abort_reason_handle(&self) -> Arc<Mutex<Option<AbortReason>>> {
        self.abort_reason.clone()
    }

    fn set_abort_reason(&self, reason: AbortReason) {
        let mut guard = self
            .abort_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_none() {
            *guard = Some(reason);
        }
    }

    fn aborted_error(&self) -> AgentError {
        let reason = self
            .abort_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or(AbortReason::Cancelled);
        AgentError::Aborted(reason)
    }

    fn emit_llm_error(&mut self, err: SdkError) -> AgentError {
        self.event_emitter.emit(
            self.id.clone(),
            AgentEvent::Error {
                error: AgentError::Llm(err.clone()),
            },
        );
        if is_auth_error(&err) {
            self.transition(SessionState::Closed);
        }
        AgentError::Llm(err)
    }

    async fn open_stream_with_retry(
        &mut self,
        client: &Client,
        request: &Request,
        retry_policy: &RetryPolicy,
    ) -> Result<StreamEventStream, AgentError> {
        let stream_result = retry::retry(retry_policy, || {
            let client = client.clone();
            let request = request.clone();
            async move { client.stream(&request).await }
        })
        .await;

        match stream_result {
            Ok(stream) => Ok(stream),
            Err(err) => Err(self.emit_llm_error(err)),
        }
    }

    #[must_use]
    pub fn followup_queue_handle(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.followup_queue.clone()
    }

    #[must_use]
    pub fn steering_queue_handle(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.steering_queue.clone()
    }

    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Build a callback that forwards sub-agent lifecycle and child session events
    /// through this session's emitter.
    #[must_use]
    pub fn sub_agent_event_callback(&self) -> SubAgentEventCallback {
        let emitter = self.event_emitter.clone();
        let parent_session_id = self.id.clone();
        Arc::new(move |event| match event {
            SubAgentCallbackEvent::Lifecycle(event) => {
                emitter.emit(parent_session_id.clone(), event);
            }
            SubAgentCallbackEvent::Forwarded(mut event) => {
                if event.parent_session_id.is_none() {
                    event.parent_session_id = Some(parent_session_id.clone());
                }
                emitter.forward(event);
            }
        })
    }

    /// Transition the session state machine, emitting events and running
    /// cleanup as appropriate for each transition.
    ///
    /// Valid transitions (matches the Attractor spec):
    /// - Idle → Thinking
    /// - Thinking → Executing
    /// - Thinking → Idle  (emits ProcessingEnd)
    /// - Executing → Thinking
    /// - Thinking → Closed (emits SessionEnded)
    /// - Executing → Closed (emits SessionEnded)
    /// - Idle → Closed (emits SessionEnded)
    /// - any → Closed (abort/error — emits SessionEnded)
    fn transition(&mut self, to: SessionState) {
        let from = self.state;
        if from == to {
            return;
        }

        debug_assert!(
            matches!(
                (from, to),
                (
                    SessionState::Idle | SessionState::Executing,
                    SessionState::Thinking
                ) | (
                    SessionState::Thinking,
                    SessionState::Executing | SessionState::Idle
                ) | (_, SessionState::Closed)
            ),
            "Invalid session state transition: {from:?} -> {to:?}"
        );

        if to == SessionState::Closed && from != SessionState::Closed {
            // Clean up subagents before emitting SessionEnded
            if let Some(ref manager) = self.subagent_manager {
                if let Ok(mut mgr) = manager.try_lock() {
                    mgr.close_all();
                }
            }
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::SessionEnded);
        }

        if matches!(from, SessionState::Thinking | SessionState::Executing)
            && to == SessionState::Idle
        {
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::ProcessingEnd);
        }

        self.state = to;
    }

    pub fn close(&mut self) {
        self.transition(SessionState::Closed);
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffort>) {
        self.config.reasoning_effort = effort;
    }

    pub fn set_speed(&mut self, speed: Option<String>) {
        self.config.speed = speed;
    }

    pub const fn set_max_turns(&mut self, max_turns: usize) {
        self.config.max_turns = max_turns;
    }

    #[must_use]
    pub const fn history(&self) -> &History {
        &self.history
    }

    #[must_use]
    pub const fn file_tracker(&self) -> &FileTracker {
        &self.file_tracker
    }

    pub async fn process_input(&mut self, input: &str) -> Result<(), AgentError> {
        if self.state == SessionState::Closed {
            return Err(AgentError::SessionClosed);
        }

        // Spawn wall-clock timeout task if configured
        let timer_handle = self.config.wall_clock_timeout.map(|duration| {
            let token = self.cancel_token.clone();
            let reason_handle = self.abort_reason.clone();
            tokio::spawn(async move {
                time::sleep(duration).await;
                {
                    let mut guard = reason_handle
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if guard.is_none() {
                        *guard = Some(AbortReason::WallClockTimeout);
                    }
                }
                token.cancel();
            })
        });

        // Process the initial input, then drain any followups
        let mut result = self.run_single_input(input).await;

        if result.is_ok() {
            loop {
                let followup = self
                    .followup_queue
                    .lock()
                    .expect("followup queue lock poisoned")
                    .pop_front();
                let Some(followup) = followup else { break };
                result = self.run_single_input(&followup).await;
                if result.is_err() {
                    break;
                }
            }
        }

        // Abort the timer so it doesn't fire after we're done
        if let Some(handle) = timer_handle {
            handle.abort();
        }

        // Only transition to Idle if the session wasn't closed by an error
        if self.state != SessionState::Closed {
            self.transition(SessionState::Idle);
        }

        result
    }

    async fn run_single_input(&mut self, input: &str) -> Result<(), AgentError> {
        const STREAM_CONSUME_RETRIES: usize = 3;

        if self.state == SessionState::Closed {
            return Err(AgentError::SessionClosed);
        }

        self.transition(SessionState::Thinking);

        // Expand skill references in input
        let expanded = if self.skills.is_empty() {
            ExpandedInput {
                text: input.to_string(),
                skill_name: None,
            }
        } else {
            expand_skill(&self.skills, input).map_err(AgentError::InvalidState)?
        };
        if let Some(ref name) = expanded.skill_name {
            self.event_emitter.emit(
                self.id.clone(),
                AgentEvent::SkillExpanded {
                    skill_name: name.clone(),
                },
            );
        }
        let expanded_input = expanded.text;

        // Append user turn and emit event
        self.history.push(Turn::User {
            content: expanded_input.clone(),
            timestamp: SystemTime::now(),
        });
        self.event_emitter.emit(
            self.id.clone(),
            AgentEvent::UserInput {
                text: expanded_input.clone(),
            },
        );

        // Drain steering queue before first LLM call
        self.drain_steering();

        let mut round_count: usize = 0;

        loop {
            // Check max_tool_rounds_per_input
            if self.config.max_tool_rounds_per_input > 0
                && round_count >= self.config.max_tool_rounds_per_input
            {
                self.event_emitter.emit(
                    self.id.clone(),
                    AgentEvent::TurnLimitReached {
                        max_turns: self.config.max_tool_rounds_per_input,
                    },
                );
                break;
            }

            // Check max_turns
            if self.config.max_turns > 0 && self.history.turns().len() >= self.config.max_turns {
                self.event_emitter.emit(
                    self.id.clone(),
                    AgentEvent::TurnLimitReached {
                        max_turns: self.config.max_turns,
                    },
                );
                break;
            }

            // Check cancellation
            if self.cancel_token.is_cancelled() {
                self.close();
                return Err(self.aborted_error());
            }

            // Pre-turn compaction: trim context before building the request
            self.compact_if_needed().await;

            // Build request
            let request = self.build_request();

            // Emit AssistantTextStart before LLM call
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::AssistantTextStart);

            // Call LLM (streaming) with retry for transient errors
            let retry_emitter = self.event_emitter.clone();
            let retry_session_id = self.id.clone();
            let retry_provider = self.provider_profile.provider().as_str().to_string();
            let retry_model = self.provider_profile.model().to_string();
            let retry_policy = RetryPolicy {
                max_retries: 3,
                on_retry: Some(std::sync::Arc::new(move |err, attempt, delay| {
                    retry_emitter.emit(
                        retry_session_id.clone(),
                        AgentEvent::LlmRetry {
                            provider: retry_provider.clone(),
                            model: retry_model.clone(),
                            attempt: attempt as usize,
                            delay_secs: delay.as_secs_f64(),
                            error: err.clone(),
                        },
                    );
                })),
                ..Default::default()
            };
            let client = self.llm_client.clone();
            let mut event_stream = self
                .open_stream_with_retry(&client, &request, &retry_policy)
                .await?;

            // Consume the stream, retrying up to 3 times if the provider
            // closes the stream without sending a Finish event. If visible
            // output was already emitted, clear it before replaying the turn.
            let mut response = None;

            for stream_attempt in 0..=STREAM_CONSUME_RETRIES {
                let mut accumulator = StreamAccumulator::new();
                let mut emitted_text = String::new();
                let mut emitted_reasoning = String::new();

                while let Some(event_result) = event_stream.next().await {
                    match event_result {
                        Ok(event) => {
                            match &event {
                                StreamEvent::TextDelta { ref delta, .. } => {
                                    emitted_text.push_str(delta);
                                    self.event_emitter.emit(
                                        self.id.clone(),
                                        AgentEvent::TextDelta {
                                            delta: delta.clone(),
                                        },
                                    );
                                }
                                StreamEvent::ReasoningDelta { ref delta } => {
                                    emitted_reasoning.push_str(delta);
                                    self.event_emitter.emit(
                                        self.id.clone(),
                                        AgentEvent::ReasoningDelta {
                                            delta: delta.clone(),
                                        },
                                    );
                                }
                                _ => {}
                            }
                            accumulator.process(&event);
                        }
                        Err(err) => {
                            return Err(self.emit_llm_error(err));
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
                    return Err(self.aborted_error());
                }

                if let Some(resp) = accumulator.response().cloned() {
                    response = Some(resp);
                    break;
                }

                // No Finish event — retry if we have attempts left
                if stream_attempt < STREAM_CONSUME_RETRIES {
                    tracing::warn!(
                        attempt = stream_attempt + 1,
                        max = STREAM_CONSUME_RETRIES,
                        "Stream ended without Finish event, retrying turn"
                    );
                    if !emitted_text.is_empty() || !emitted_reasoning.is_empty() {
                        self.event_emitter.emit(
                            self.id.clone(),
                            AgentEvent::AssistantOutputReplace {
                                text: String::new(),
                                reasoning: None,
                            },
                        );
                    }
                    event_stream = self
                        .open_stream_with_retry(&client, &request, &retry_policy)
                        .await?;
                }
            }

            let Some(response) = response else {
                return Err(self.emit_llm_error(SdkError::Stream {
                    message: "Stream ended without a Finish event (after retries)".into(),
                    source: None,
                }));
            };

            // Record assistant turn
            let text = response.text();
            let tool_calls = response.tool_calls();
            let provider_parts: Vec<_> = response
                .message
                .content
                .iter()
                .filter(|p| matches!(p, ContentPart::Other { .. } | ContentPart::Thinking(_)))
                .cloned()
                .collect();
            let usage = response.usage.clone();

            self.history.push(Turn::Assistant {
                content: text.clone(),
                tool_calls: tool_calls.clone(),
                provider_parts,
                usage: Box::new(usage),
                response_id: response.id.clone(),
                timestamp: SystemTime::now(),
            });

            // Emit AssistantMessage with enriched data from the response
            self.event_emitter.emit(
                self.id.clone(),
                AgentEvent::AssistantMessage {
                    text: text.clone(),
                    model: response.model.clone(),
                    usage: response.usage.clone(),
                    tool_call_count: tool_calls.len(),
                },
            );

            // Post-response compaction: trim context after appending assistant turn
            self.compact_if_needed().await;

            // If no tool calls, natural completion
            if tool_calls.is_empty() {
                break;
            }

            round_count += 1;

            // Execute tool calls (parallel or sequential based on provider)
            self.transition(SessionState::Executing);
            let results = execute_tool_calls(
                &tool_calls,
                true,
                self.provider_profile.tool_registry(),
                self.sandbox.clone(),
                self.config.tool_hooks.as_ref(),
                &self.cancel_token,
                &self.config,
                &self.event_emitter,
                &self.id,
                self.tool_env.as_ref(),
            )
            .await;

            // Track file operations from tool calls
            self.file_tracker
                .record_from_tool_calls(&tool_calls, &results);

            // Check cancellation after tool execution
            if self.cancel_token.is_cancelled() {
                self.history.push(Turn::ToolResults {
                    results,
                    timestamp: SystemTime::now(),
                });
                self.close();
                return Err(self.aborted_error());
            }

            // Record tool results turn
            self.history.push(Turn::ToolResults {
                results,
                timestamp: SystemTime::now(),
            });

            // Drain steering after tool execution
            self.drain_steering();
            self.transition(SessionState::Thinking);

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

    async fn compact_if_needed(&mut self) {
        let over_threshold = check_context_usage(
            &self.system_prompt,
            &self.history,
            self.provider_profile.as_ref(),
            self.config.compaction_threshold_percent,
            &self.event_emitter,
            &self.id,
        );
        if over_threshold && self.config.enable_context_compaction {
            if let Err(e) = compact_context(
                &mut self.history,
                &self.llm_client,
                self.provider_profile.as_ref(),
                &self.system_prompt,
                &self.file_tracker,
                self.config.compaction_preserve_turns,
                &self.event_emitter,
                &self.id,
            )
            .await
            {
                self.event_emitter.emit(
                    self.id.clone(),
                    AgentEvent::Error {
                        error: AgentError::InvalidState(format!("Context compaction failed: {e}")),
                    },
                );
            }
        }
    }

    fn drain_steering(&mut self) {
        let messages: Vec<String> = self
            .steering_queue
            .lock()
            .expect("steering queue lock poisoned")
            .drain(..)
            .collect();
        for msg in messages {
            let text = msg.clone();
            self.history.push(Turn::Steering {
                content: msg,
                timestamp: SystemTime::now(),
            });
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::SteeringInjected { text });
        }
    }

    fn build_request(&self) -> Request {
        let mut messages = Vec::new();
        if !self.system_prompt.trim().is_empty() {
            messages.push(Message::system(self.system_prompt.clone()));
        }
        messages.extend(self.history.convert_to_messages());

        let tools = self.provider_profile.tools();
        let has_tools = !tools.is_empty();

        Request {
            model: self.provider_profile.model().to_string(),
            messages,
            provider: Some(self.provider_profile.provider().as_str().to_string()),
            tools: if has_tools { Some(tools) } else { None },
            tool_choice: if has_tools {
                Some(ToolChoice::Auto)
            } else {
                None
            },
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens: self.config.max_tokens.or_else(|| {
                fabro_model::Catalog::builtin()
                    .get(self.provider_profile.model())
                    .and_then(fabro_model::Model::max_output)
            }),
            stop_sequences: None,
            reasoning_effort: self.config.reasoning_effort,
            speed: self.config.speed.clone(),
            metadata: None,
            provider_options: None,
        }
    }
}

const fn is_auth_error(err: &SdkError) -> bool {
    matches!(
        err.provider_kind(),
        Some(ProviderErrorKind::Authentication | ProviderErrorKind::AccessDenied)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ToolApprovalAdapter;
    use crate::subagent::SubAgentStatus;
    use crate::test_support::*;
    use crate::tool_registry::{RegisteredTool, ToolRegistry};
    use fabro_llm::error::{ProviderErrorDetail, ProviderErrorKind};
    use fabro_llm::provider::{ProviderAdapter, StreamEventStream};
    use fabro_llm::types::{
        ContentPart, ReasoningEffort, Request, Response, Role, StreamEvent, ToolDefinition,
    };
    use futures::stream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    enum ScriptedStreamCall {
        Response(Box<Response>),
        Events(Vec<Result<StreamEvent, SdkError>>),
        Error(SdkError),
    }

    struct ScriptedStreamProvider {
        calls: Vec<ScriptedStreamCall>,
        call_index: AtomicUsize,
    }

    impl ScriptedStreamProvider {
        fn new(calls: Vec<ScriptedStreamCall>) -> Self {
            assert!(
                !calls.is_empty(),
                "scripted stream provider needs at least one call"
            );
            Self {
                calls,
                call_index: AtomicUsize::new(0),
            }
        }

        fn events_for_response(response: Response) -> Vec<Result<StreamEvent, SdkError>> {
            let mut events = Vec::new();
            let text = response.text();
            if !text.is_empty() {
                events.push(Ok(StreamEvent::text_delta(text, None)));
            }

            for part in &response.message.content {
                if let ContentPart::ToolCall(tool_call) = part {
                    events.push(Ok(StreamEvent::ToolCallEnd {
                        tool_call: tool_call.clone(),
                    }));
                }
            }

            events.push(Ok(StreamEvent::finish(
                response.finish_reason.clone(),
                response.usage.clone(),
                response,
            )));
            events
        }
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for ScriptedStreamProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, SdkError> {
            Err(SdkError::Configuration {
                message: "ScriptedStreamProvider does not implement complete()".into(),
                source: None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, SdkError> {
            let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
            let scripted = if idx < self.calls.len() {
                self.calls[idx].clone()
            } else {
                self.calls[self.calls.len() - 1].clone()
            };

            match scripted {
                ScriptedStreamCall::Response(response) => {
                    Ok(Box::pin(stream::iter(Self::events_for_response(*response))))
                }
                ScriptedStreamCall::Events(events) => Ok(Box::pin(stream::iter(events))),
                ScriptedStreamCall::Error(err) => Err(err),
            }
        }
    }

    async fn make_session_with_provider(provider: Arc<dyn ProviderAdapter>) -> Session {
        make_session_with_provider_and_manager(provider, None).await
    }

    async fn make_session_with_provider_and_manager(
        provider: Arc<dyn ProviderAdapter>,
        subagent_manager: Option<Arc<AsyncMutex<SubAgentManager>>>,
    ) -> Session {
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockSandbox::default());
        Session::new(
            client,
            profile,
            env,
            SessionOptions::default(),
            subagent_manager,
        )
    }

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
        assert!(matches!(&turns[1], Turn::Assistant { content, .. } if content == "Hello there!"));
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
        assert!(matches!(&turns[3], Turn::Assistant { content, .. } if content == "Done!"));

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

        let config = SessionOptions {
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

        let config = SessionOptions {
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
        assert!(
            matches!(&turns[1], Turn::Assistant { content, .. } if content == "First response")
        );
        assert!(matches!(&turns[2], Turn::User { content, .. } if content == "followup message"));
        assert!(
            matches!(&turns[3], Turn::Assistant { content, .. } if content == "Followup response")
        );
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

        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, AgentEvent::SessionStarted { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, AgentEvent::UserInput { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, AgentEvent::AssistantMessage { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, AgentEvent::SessionEnded))
        );
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

        let config = SessionOptions {
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
        let has_steering_warning = session.history().turns().iter().any(
            |t| matches!(t, Turn::Steering { content, .. } if content.contains("Loop detected")),
        );
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

        let config = SessionOptions {
            enable_loop_detection: false,
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        // Set abort before processing
        session.abort();
        let result = session.process_input("Do something").await;

        // Should return Aborted error and transition to Closed
        assert!(matches!(result, Err(AgentError::Aborted(_))));
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
            executor: Arc::new(move |_args, _ctx| {
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
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            enable_loop_detection: false,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config, None);

        // Wire the session's cancel_token to our shared one
        session.cancel_token = cancel_token;

        let result = session.process_input("Do something").await;

        // Should return Aborted error and transition to Closed
        assert!(matches!(result, Err(AgentError::Aborted(_))));
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
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);

        let result = session.process_input("Hello").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AgentError::Llm(_)));
        assert_eq!(session.state(), SessionState::Closed);
    }

    #[tokio::test]
    async fn sequential_inputs() {
        let responses = vec![text_response("First"), text_response("Second")];

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
            !events
                .iter()
                .any(|e| matches!(e.event, AgentEvent::SessionStarted { .. })),
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
        let profile = Arc::new(TestProfile::with_tools(registry));
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);
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
        let profile = Arc::new(TestProfile::with_context_window(registry, 100));
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);
        let mut rx = session.subscribe();

        session.process_input(&large_input).await.unwrap();

        let mut found_warning = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::Warning { details, .. } = &event.event {
                found_warning = true;
                assert_eq!(details["context_window_size"], 100);
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
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);

        // Default reasoning_effort is None
        session.set_reasoning_effort(Some(ReasoningEffort::High));
        session.process_input("test").await.unwrap();

        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("request should have been captured");
        assert_eq!(request.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[tokio::test]
    async fn context_window_no_warning_under_threshold() {
        let responses = vec![text_response("OK")];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let registry = ToolRegistry::new();
        // Large context window so short input stays well under 80%
        let profile = Arc::new(TestProfile::with_context_window(registry, 200_000));
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);
        let mut rx = session.subscribe();

        session.process_input("Hi").await.unwrap();

        let mut found_warning = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEvent::Warning { .. }) {
                found_warning = true;
            }
        }
        assert!(!found_warning);
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
            executor: Arc::new(|_args, _ctx| {
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
            executor: Arc::new(|_args, _ctx| {
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
        let responses = vec![text_response("First"), text_response("Second")];

        let mut session = make_session(responses).await;
        let mut rx = session.subscribe();

        session.initialize().await;
        session.process_input("one").await.unwrap();
        session.process_input("two").await.unwrap();
        session.close();

        let mut session_start_count = 0;
        let mut session_end_count = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEvent::SessionStarted { .. }) {
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
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            user_instructions: Some("Always use TDD".into()),
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config, None);
        session.initialize().await;
        session.process_input("test").await.unwrap();

        // Verify user instructions are included in the system prompt
        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("request should have been captured");
        let system_msg = &request.messages[0];
        let system_text = system_msg.text();
        assert!(
            system_text.contains("Always use TDD"),
            "System prompt should contain user instructions"
        );
    }

    #[tokio::test]
    async fn request_omits_system_message_when_prompt_empty() {
        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);

        // Intentionally skip initialize(): system prompt remains empty.
        session.process_input("test").await.unwrap();

        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("request should have been captured");
        assert!(
            request
                .messages
                .iter()
                .all(|message| message.role != Role::System),
            "request should not contain an empty system message"
        );
        assert!(
            matches!(request.messages.first(), Some(message) if message.role == Role::User),
            "first request message should be user input"
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

        let config = SessionOptions {
            tool_hooks: Some(Arc::new(ToolApprovalAdapter(Arc::new(|_name, _args| {
                Err("denied by policy".to_string())
            })))),
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

        let config = SessionOptions {
            tool_hooks: Some(Arc::new(ToolApprovalAdapter(Arc::new(|_name, _args| {
                Ok(())
            })))),
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

        let captured: Arc<Mutex<Option<(String, serde_json::Value)>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();

        let responses = vec![
            tool_call_response("echo", "call_1", serde_json::json!({"text": "world"})),
            text_response("Done"),
        ];

        let config = SessionOptions {
            tool_hooks: Some(Arc::new(ToolApprovalAdapter(Arc::new(
                move |name, args| {
                    *captured_clone.lock().unwrap() = Some((name.to_string(), args.clone()));
                    Ok(())
                },
            )))),
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        session.process_input("Use echo").await.unwrap();

        let captured_value = captured.lock().unwrap();
        let (name, args) = captured_value
            .as_ref()
            .expect("approval fn should have been called");
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

        let config = SessionOptions {
            tool_hooks: None,
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

        let config = SessionOptions {
            tool_hooks: Some(Arc::new(ToolApprovalAdapter(Arc::new(|_name, _args| {
                Err("not allowed".to_string())
            })))),
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
                assert!(
                    is_error,
                    "ToolCallCompleted event should have is_error: true"
                );
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
                source: None,
            },
        });
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);

        let result = session.process_input("Hello").await;
        assert!(matches!(
            result,
            Err(AgentError::Llm(SdkError::Stream { .. }))
        ));
    }

    #[tokio::test]
    async fn stream_retries_when_stream_ends_without_finish_before_any_deltas() {
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![]),
            ScriptedStreamCall::Response(Box::new(text_response("Recovered"))),
        ]));
        let mut session = make_session_with_provider(provider.clone()).await;
        let mut rx = session.subscribe();

        session.process_input("Hello").await.unwrap();

        assert_eq!(provider.call_index.load(Ordering::SeqCst), 2);
        let turns = session.history().turns();
        assert!(matches!(
            turns.last(),
            Some(Turn::Assistant { content, .. }) if content == "Recovered"
        ));

        let mut assistant_text_start_count = 0;
        let mut replace_count = 0;
        let mut deltas = Vec::new();
        let mut assistant_messages = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::AssistantTextStart => assistant_text_start_count += 1,
                AgentEvent::AssistantOutputReplace { .. } => replace_count += 1,
                AgentEvent::TextDelta { delta } => deltas.push(delta),
                AgentEvent::AssistantMessage { text, .. } => assistant_messages.push(text),
                _ => {}
            }
        }

        assert_eq!(assistant_text_start_count, 1);
        assert_eq!(replace_count, 0);
        assert_eq!(deltas, vec!["Recovered".to_string()]);
        assert_eq!(assistant_messages, vec!["Recovered".to_string()]);
    }

    #[tokio::test]
    async fn stream_retries_with_output_replace_after_partial_text() {
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![Ok(StreamEvent::text_delta("Hel", None))]),
            ScriptedStreamCall::Response(Box::new(text_response("Hello"))),
        ]));
        let mut session = make_session_with_provider(provider.clone()).await;
        let mut rx = session.subscribe();

        session.process_input("Hello").await.unwrap();

        assert_eq!(provider.call_index.load(Ordering::SeqCst), 2);
        let turns = session.history().turns();
        assert!(matches!(
            turns.last(),
            Some(Turn::Assistant { content, .. }) if content == "Hello"
        ));

        let mut observed = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::AssistantTextStart => observed.push("start".to_string()),
                AgentEvent::TextDelta { delta } => observed.push(format!("delta:{delta}")),
                AgentEvent::AssistantOutputReplace { text, reasoning } => {
                    observed.push(format!("replace:{text}:{reasoning:?}"));
                }
                AgentEvent::AssistantMessage { text, .. } => {
                    observed.push(format!("message:{text}"));
                }
                _ => {}
            }
        }

        assert_eq!(
            observed,
            vec![
                "start".to_string(),
                "delta:Hel".to_string(),
                "replace::None".to_string(),
                "delta:Hello".to_string(),
                "message:Hello".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn retry_open_auth_error_emits_error_and_closes_session() {
        let auth_error = SdkError::Provider {
            kind: ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(401),
                ..ProviderErrorDetail::new("bad key", "mock")
            }),
        };
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![Ok(StreamEvent::text_delta("Hel", None))]),
            ScriptedStreamCall::Error(auth_error.clone()),
        ]));
        let mut session = make_session_with_provider(provider.clone()).await;
        let mut rx = session.subscribe();

        let result = session.process_input("Hello").await;
        assert!(matches!(
            result,
            Err(AgentError::Llm(SdkError::Provider {
                kind: ProviderErrorKind::Authentication,
                ..
            }))
        ));

        assert_eq!(provider.call_index.load(Ordering::SeqCst), 2);
        assert_eq!(session.state(), SessionState::Closed);

        let mut observed = Vec::new();
        let mut found_auth_error_event = false;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::AssistantTextStart => observed.push("start".to_string()),
                AgentEvent::TextDelta { delta } => observed.push(format!("delta:{delta}")),
                AgentEvent::AssistantOutputReplace { text, reasoning } => {
                    observed.push(format!("replace:{text}:{reasoning:?}"));
                }
                AgentEvent::Error { error } => {
                    observed.push("error".to_string());
                    found_auth_error_event = matches!(
                        error,
                        AgentError::Llm(SdkError::Provider {
                            kind: ProviderErrorKind::Authentication,
                            ..
                        })
                    );
                }
                AgentEvent::AssistantMessage { .. } => observed.push("message".to_string()),
                _ => {}
            }
        }

        assert_eq!(
            observed,
            vec![
                "start".to_string(),
                "delta:Hel".to_string(),
                "replace::None".to_string(),
                "error".to_string(),
            ]
        );
        assert!(found_auth_error_event, "expected auth error event");
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
        let profile = Arc::new(TestProfile::with_context_window(registry, 100));
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            enable_context_compaction: true,
            compaction_preserve_turns: 1,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config, None);
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
        assert!(
            found_completed,
            "CompactionCompleted event should be emitted"
        );

        // History should have been compacted: summary turn + preserved turns
        let turns = session.history().turns();
        assert!(
            turns.iter().any(|t| matches!(t, Turn::System { content, .. } if content.contains("A different assistant began this task"))),
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
        let profile = Arc::new(TestProfile::with_context_window(registry, 100));
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            enable_context_compaction: false,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config, None);
        let mut rx = session.subscribe();

        session.process_input(&large_input).await.unwrap();

        let mut found_compaction = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(
                event.event,
                AgentEvent::CompactionStarted { .. } | AgentEvent::CompactionCompleted { .. }
            ) {
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
            fn name(&self) -> &'static str {
                "mock"
            }

            async fn complete(&self, _request: &Request) -> Result<Response, SdkError> {
                Err(SdkError::Stream {
                    message: "summarization failed".into(),
                    source: None,
                })
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
                    if let ContentPart::ToolCall(tc) = part {
                        events.push(Ok(StreamEvent::ToolCallEnd {
                            tool_call: tc.clone(),
                        }));
                    }
                }
                events.push(Ok(StreamEvent::finish(
                    response.finish_reason.clone(),
                    response.usage.clone(),
                    response,
                )));
                Ok(Box::pin(stream::iter(events)))
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
        let profile = Arc::new(TestProfile::with_context_window(registry, 100));
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            enable_context_compaction: true,
            compaction_preserve_turns: 1,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config, None);
        let mut rx = session.subscribe();

        // Should not return an error even though compaction fails
        let result = session.process_input(&large_input).await;
        assert!(
            result.is_ok(),
            "Session should continue despite compaction failure"
        );

        // Should emit an Error event for the failed compaction
        let mut found_error = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::Error { error } = &event.event {
                let msg = error.to_string();
                if msg.contains("compaction") || msg.contains("summarization") {
                    found_error = true;
                }
            }
        }
        assert!(found_error, "Should emit Error event for failed compaction");
    }

    #[tokio::test]
    async fn compaction_includes_structured_prompt_and_file_tracking() {
        use crate::tool_registry::RegisteredTool;
        use fabro_llm::types::ToolDefinition;

        // Provider that captures complete() requests (compaction) while returning
        // canned responses for stream() calls.
        struct CompactionCapturingProvider {
            stream_responses: Vec<Response>,
            stream_index: AtomicUsize,
            captured_complete: Mutex<Option<Request>>,
        }

        #[async_trait::async_trait]
        impl ProviderAdapter for CompactionCapturingProvider {
            fn name(&self) -> &'static str {
                "mock"
            }

            async fn complete(&self, request: &Request) -> Result<Response, SdkError> {
                *self.captured_complete.lock().unwrap() = Some(request.clone());
                Ok(text_response("## Goal\nSummary goes here."))
            }

            async fn stream(&self, _request: &Request) -> Result<StreamEventStream, SdkError> {
                let idx = self.stream_index.fetch_add(1, Ordering::SeqCst);
                let response = if idx < self.stream_responses.len() {
                    self.stream_responses[idx].clone()
                } else {
                    self.stream_responses[self.stream_responses.len() - 1].clone()
                };
                Ok(response_to_stream(response))
            }
        }

        // read_file tool that always succeeds
        let read_tool = RegisteredTool {
            definition: ToolDefinition {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"file_path": {"type": "string"}}}),
            },
            executor: Arc::new(|_args, _ctx| {
                Box::pin(async move { Ok("file contents".to_string()) })
            }),
        };

        let mut registry = ToolRegistry::new();
        registry.register(read_tool);

        // Stream responses:
        // [0] = tool call to read_file (first process_input)
        // [1] = text "OK" (completes first turn after tool results)
        // [2] = text "OK" (second process_input — triggers compaction)
        // [3] = fallback
        let stream_responses = vec![
            tool_call_response(
                "read_file",
                "tc1",
                serde_json::json!({"file_path": "/src/main.rs"}),
            ),
            text_response("OK"),
            text_response("Done after compaction"),
            text_response("fallback"),
        ];

        let provider = Arc::new(CompactionCapturingProvider {
            stream_responses,
            stream_index: AtomicUsize::new(0),
            captured_complete: Mutex::new(None),
        });

        let client = make_client(provider.clone() as Arc<dyn ProviderAdapter>).await;
        // Tiny context window to force compaction
        let profile = Arc::new(TestProfile::with_context_window(registry, 100));
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            enable_context_compaction: true,
            compaction_preserve_turns: 1,
            ..Default::default()
        };

        let mut session = Session::new(client, profile, env, config, None);
        let mut rx = session.subscribe();

        // First call: tool call executes, files get tracked, no compaction yet
        // (compaction may trigger but file tracker is populated by tool execution)
        session.process_input("Read the file").await.unwrap();
        assert_eq!(
            session.file_tracker().file_count(),
            1,
            "read_file should be tracked"
        );

        // Second call with large input: context is well over threshold, compaction triggers
        let large_input = "x".repeat(400);
        session.process_input(&large_input).await.unwrap();

        // Verify the compaction request has the structured prompt
        let captured = provider.captured_complete.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("compaction request should have been captured");
        let system_text = request.messages[0].text();
        assert!(
            system_text.contains("## Goal"),
            "Compaction system prompt should contain structured '## Goal' section"
        );
        assert!(
            system_text.contains("## File Operations"),
            "Compaction system prompt should contain '## File Operations' section when files were tracked"
        );
        assert!(
            system_text.contains("/src/main.rs"),
            "File operations section should include the tracked file path"
        );
        assert!(
            system_text.contains("COPY THIS SECTION VERBATIM"),
            "File operations section should instruct verbatim copying"
        );

        // Verify CompactionCompleted event has tracked_file_count
        let mut found_tracked_count = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::CompactionCompleted {
                tracked_file_count, ..
            } = &event.event
            {
                assert_eq!(*tracked_file_count, 1, "Should track 1 file (read_file)");
                found_tracked_count = true;
            }
        }
        assert!(
            found_tracked_count,
            "CompactionCompleted event should be emitted"
        );
    }

    #[tokio::test]
    async fn mcp_end_to_end_tool_call() {
        use fabro_mcp::config::{McpServerSettings, McpTransport};
        use std::collections::HashMap;

        let test_server = format!(
            "{}/../fabro-mcp/tests/test_mcp_server.py",
            env!("CARGO_MANIFEST_DIR")
        );
        let config = SessionOptions {
            mcp_servers: vec![McpServerSettings {
                name: "test-echo".into(),
                transport: McpTransport::Stdio {
                    command: vec!["python3".into(), test_server],
                    env: HashMap::new(),
                },
                startup_timeout_secs: 10,
                tool_timeout_secs: 30,
            }],
            enable_loop_detection: false,
            ..Default::default()
        };

        // Mock LLM: first call returns tool call for the MCP tool, second returns text
        let responses = vec![
            tool_call_response(
                "mcp__test_echo__echo",
                "mcp_call_1",
                serde_json::json!({"message": "hello from llm"}),
            ),
            text_response("The echo server replied!"),
        ];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let profile: Arc<dyn AgentProfile> = Arc::new(TestProfile::new());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, config, None);

        // Subscribe to events before initialize
        let mut rx = session.subscribe();

        // Initialize starts the MCP server and registers tools
        session.initialize().await;

        // Verify McpServerReady event was emitted
        let mut mcp_ready = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::McpServerReady {
                server_name,
                tool_count,
            } = &event.event
            {
                assert_eq!(server_name, "test-echo");
                assert_eq!(*tool_count, 1);
                mcp_ready = true;
            }
        }
        assert!(mcp_ready, "McpServerReady event should be emitted");

        // Process input — LLM calls MCP tool, gets result, responds
        session.process_input("Call the echo tool").await.unwrap();

        // Verify turn sequence
        let turns = session.history().turns();
        assert_eq!(
            turns.len(),
            4,
            "Expected User + Assistant(tool) + ToolResults + Assistant(text)"
        );
        assert!(matches!(&turns[0], Turn::User { .. }));
        assert!(matches!(&turns[1], Turn::Assistant { tool_calls, .. } if tool_calls.len() == 1));
        assert!(matches!(&turns[2], Turn::ToolResults { results, .. } if results.len() == 1));
        assert!(
            matches!(&turns[3], Turn::Assistant { content, .. } if content == "The echo server replied!")
        );

        // Verify the MCP tool result content — the echo server returns the message
        if let Turn::ToolResults { results, .. } = &turns[2] {
            assert_eq!(results[0].tool_call_id, "mcp_call_1");
            assert!(!results[0].is_error);
            let output = results[0].content.as_str().unwrap_or("");
            assert_eq!(output, "hello from llm");
        } else {
            panic!("expected ToolResults turn");
        }

        // Verify tool call events
        let mut tool_started = false;
        let mut tool_completed = false;
        while let Ok(event) = rx.try_recv() {
            match &event.event {
                AgentEvent::ToolCallStarted { tool_name, .. } => {
                    assert_eq!(tool_name, "mcp__test_echo__echo");
                    tool_started = true;
                }
                AgentEvent::ToolCallCompleted {
                    tool_name,
                    is_error,
                    ..
                } => {
                    assert_eq!(tool_name, "mcp__test_echo__echo");
                    assert!(!is_error);
                    tool_completed = true;
                }
                _ => {}
            }
        }
        assert!(
            tool_started,
            "ToolCallStarted should be emitted for MCP tool"
        );
        assert!(
            tool_completed,
            "ToolCallCompleted should be emitted for MCP tool"
        );
    }

    #[tokio::test]
    async fn wall_clock_timeout_aborts_session() {
        // Register a tool that loops until the cancel token fires
        let slow_tool = RegisteredTool {
            definition: ToolDefinition {
                name: "slow_tool".into(),
                description: "Waits until cancelled".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            executor: Arc::new(|_args, ctx| {
                Box::pin(async move {
                    ctx.cancel.cancelled().await;
                    Ok("cancelled".to_string())
                })
            }),
        };
        let mut registry = ToolRegistry::new();
        registry.register(slow_tool);

        // LLM will call the slow tool, then (if it ever gets there) respond with text
        let responses = vec![
            tool_call_response("slow_tool", "call_1", serde_json::json!({})),
            text_response("Should not reach this"),
        ];

        let config = SessionOptions {
            wall_clock_timeout: Some(std::time::Duration::from_millis(10)),
            enable_loop_detection: false,
            ..Default::default()
        };

        let mut session = make_session_with_tools_and_config(responses, registry, config).await;
        let result = session.process_input("Do something slow").await;

        assert!(
            matches!(
                result,
                Err(AgentError::Aborted(AbortReason::WallClockTimeout))
            ),
            "expected Aborted(WallClockTimeout), got {result:?}"
        );
        assert_eq!(session.state(), SessionState::Closed);
    }

    #[tokio::test]
    async fn wall_clock_timeout_does_not_fire_when_session_completes_in_time() {
        let responses = vec![text_response("Fast response")];

        let config = SessionOptions {
            wall_clock_timeout: Some(std::time::Duration::from_secs(10)),
            ..Default::default()
        };

        let mut session = make_session_with_config(responses, config).await;
        let result = session.process_input("Hello").await;

        assert!(result.is_ok());
        assert_eq!(session.state(), SessionState::Idle);
        let turns = session.history().turns();
        assert_eq!(turns.len(), 2);
        assert!(matches!(&turns[1], Turn::Assistant { content, .. } if content == "Fast response"));
    }

    #[tokio::test]
    async fn close_cleans_up_subagents_before_emitting_session_ended() {
        use crate::subagent::SubAgentManager;

        let manager = Arc::new(AsyncMutex::new(SubAgentManager::new(3)));

        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Response(Box::new(text_response("done"))),
        ]));
        let mut session =
            make_session_with_provider_and_manager(provider, Some(manager.clone())).await;

        // Wire the manager's event callback to the session's emitter
        manager
            .lock()
            .await
            .set_event_callback(session.sub_agent_event_callback());

        // Spawn a subagent
        let child = make_session(vec![text_response("child done")]).await;
        let agent_id = manager.lock().await.spawn(child, "task".into(), 0).unwrap();

        // Collect events
        let mut rx = session.subscribe();
        session.close();

        // The subagent should have been closed
        assert!(matches!(
            manager.lock().await.status(&agent_id),
            Some(SubAgentStatus::Closed)
        ));

        // Verify event ordering: SubAgentClosed before SessionEnded
        let mut events = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            events.push(envelope.event);
        }
        let closed_idx = events
            .iter()
            .position(|e| matches!(e, AgentEvent::SubAgentClosed { .. }));
        let ended_idx = events
            .iter()
            .position(|e| matches!(e, AgentEvent::SessionEnded));
        assert!(
            closed_idx.is_some(),
            "SubAgentClosed event should be emitted"
        );
        assert!(ended_idx.is_some(), "SessionEnded event should be emitted");
        assert!(
            closed_idx.unwrap() < ended_idx.unwrap(),
            "SubAgentClosed must come before SessionEnded"
        );
    }

    #[tokio::test]
    async fn process_input_emits_processing_end_on_idle_transition() {
        let mut session = make_session(vec![text_response("Hello")]).await;
        session.initialize().await;

        let mut rx = session.subscribe();
        session.process_input("Hi").await.unwrap();

        assert_eq!(session.state(), SessionState::Idle);

        let mut events = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            events.push(envelope.event);
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ProcessingEnd)),
            "ProcessingEnd event should be emitted when returning to Idle"
        );
    }
}
