use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use fabro_auth::CredentialSource;
use fabro_llm::client::Client;
use fabro_llm::error::ProviderErrorKind;
use fabro_llm::generate::StreamAccumulator;
use fabro_llm::provider::StreamEventStream;
use fabro_llm::types::{
    ContentPart, Message as LlmMessage, ReasoningEffort, Request, RetryPolicy, StreamEvent,
    ToolChoice,
};
use fabro_llm::{Error as LlmError, retry};
use fabro_mcp::config::{McpServerSettings, McpTransport};
use fabro_mcp::connection_manager::McpConnectionManager;
use fabro_model::{AgentProfileKind, Catalog, ModelRef, Speed};
use fabro_types::{Principal, SessionMessage, SessionRecord, SteeringMessage};
use futures::StreamExt;
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent_profile::AgentProfile;
use crate::compaction::{check_context_usage, compact_context};
use crate::config::SessionOptions;
use crate::error::{Error, InterruptReason};
use crate::event::Emitter;
use crate::file_tracker::FileTracker;
use crate::history::History;
use crate::loop_detection::detect_loop;
use crate::memory::{BUDGET_BYTES, MemoryDocument, discover_memory};
use crate::profiles::EnvContext;
use crate::question_tools::AgentToolRuntime;
use crate::sandbox::Sandbox;
use crate::skills::{
    ExpandedInput, Skill, default_skill_dirs, discover_skills, expand_skill, make_use_skill_tool,
};
use crate::subagent::{SubAgentCallbackEvent, SubAgentEventCallback, SubAgentManager};
use crate::tool_execution::execute_tool_calls;
use crate::types::{
    AgentEvent, McpToolSummary, MemoryFileSummary, Message, SessionEvent, SessionState,
    SkillActivationSource, SkillSummary,
};
use crate::{mcp_integration, task_reminder};

/// One queued external control item for a live session.
#[derive(Debug, Clone)]
pub enum SteeringItem {
    /// Existing steering behavior: inject a user-role guidance message that
    /// remains visibly distinct from a paired user's message.
    Steering {
        text:  String,
        actor: Option<Principal>,
    },
    User {
        text: String,
    },
    System {
        text: String,
    },
}

impl SteeringItem {
    #[must_use]
    pub fn actor(&self) -> Option<&Principal> {
        match self {
            Self::Steering { actor, .. } => actor.as_ref(),
            Self::User { .. } | Self::System { .. } => None,
        }
    }
}

impl From<SteeringMessage> for SteeringItem {
    fn from(message: SteeringMessage) -> Self {
        Self::Steering {
            text:  message.text,
            actor: message.actor,
        }
    }
}

#[derive(Default)]
struct ControlState {
    queue:             VecDeque<SteeringItem>,
    waiting_for_steer: bool,
}

/// Trait that lets the workflow layer keep an agent in `process_input` when a
/// natural completion (no tool calls) coincides with an unconsumed steering
/// message. The implementation must coordinate with the steering source so
/// that, once it returns `false`, no further steers can race into the queue
/// for this session.
pub trait CompletionCoordinator: Send + Sync {
    /// Called inside the agent loop when the assistant finishes a turn with
    /// no tool calls. Return `true` to continue (the session will iterate
    /// once more and drain pending steering messages); `false` to break out
    /// of the loop normally.
    fn on_natural_completion(&self) -> bool;
}

/// Cheap clone of the parts of a `Session` that an external coordinator
/// (e.g. the workflow `SteeringHub`) needs to deliver steering messages and
/// interrupt the current round without holding the session itself.
#[derive(Clone)]
pub struct SessionControlHandle {
    control:     Arc<Mutex<ControlState>>,
    round_token: Arc<RwLock<CancellationToken>>,
    notify:      Arc<Notify>,
}

impl Default for SessionControlHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionControlHandle {
    /// Build an unattached handle for testing or direct construction by
    /// callers that want to wire a queue into something other than a live
    /// `Session`. Both pieces are independent `Arc` values; cloning the
    /// handle clones the `Arc`s.
    #[must_use]
    pub fn new() -> Self {
        Self {
            control:     Arc::new(Mutex::new(ControlState::default())),
            round_token: Arc::new(RwLock::new(CancellationToken::new())),
            notify:      Arc::new(Notify::new()),
        }
    }

    /// Push a steering message onto the queue and wake a session waiting
    /// after a pure interrupt.
    pub fn steer(&self, text: String, actor: Option<Principal>) {
        self.enqueue(SteeringItem::Steering { text, actor });
    }

    /// Cancel the current round and, if no steering text is queued, park the
    /// session at a steerable wait point.
    pub fn interrupt(&self, _actor: Option<Principal>) {
        {
            let mut control = self.control.lock().expect("control state lock poisoned");
            if control.queue.is_empty() {
                control.waiting_for_steer = true;
            }
        }
        self.cancel_round();
        self.notify.notify_waiters();
    }

    /// Atomically apply interrupt semantics, then enqueue steering text.
    pub fn interrupt_then_steer(&self, text: String, actor: Option<Principal>) {
        self.interrupt_then_enqueue(SteeringItem::Steering { text, actor });
    }

    pub fn park_for_steer(&self) {
        let mut control = self.control.lock().expect("control state lock poisoned");
        if control.queue.is_empty() {
            control.waiting_for_steer = true;
        }
    }

    /// Direct enqueue used by callers such as the hub flushing buffered
    /// steers.
    pub fn enqueue(&self, item: SteeringItem) {
        {
            let mut control = self.control.lock().expect("control state lock poisoned");
            control.waiting_for_steer = false;
            control.queue.push_back(item);
        }
        self.notify.notify_waiters();
    }

    /// Push `item` while enforcing a FIFO cap: if the queue is at or above
    /// `cap`, the oldest entry is evicted and returned. Atomic under a
    /// single lock acquisition.
    #[must_use]
    pub fn enqueue_bounded(&self, item: SteeringItem, cap: usize) -> Option<SteeringItem> {
        self.push_bounded(item, cap)
    }

    /// Push `item` only when the queue is below `cap`. Unlike
    /// `enqueue_bounded`, this preserves all existing queued work and returns
    /// whether the item was accepted.
    #[must_use]
    pub fn try_enqueue_bounded(&self, item: SteeringItem, cap: usize) -> bool {
        {
            let mut control = self.control.lock().expect("control state lock poisoned");
            if control.queue.len() >= cap {
                return false;
            }
            control.queue.push_back(item);
            control.waiting_for_steer = false;
        }
        self.notify.notify_waiters();
        true
    }

    /// Interrupt the current round and push `item` while enforcing a FIFO cap.
    #[must_use]
    pub fn interrupt_then_enqueue_bounded(
        &self,
        item: SteeringItem,
        cap: usize,
    ) -> Option<SteeringItem> {
        let evicted = self.push_bounded(item, cap);
        self.cancel_round();
        evicted
    }

    fn push_bounded(&self, item: SteeringItem, cap: usize) -> Option<SteeringItem> {
        let evicted = {
            let mut control = self.control.lock().expect("control state lock poisoned");
            let evicted = if control.queue.len() >= cap {
                control.queue.pop_front()
            } else {
                None
            };
            control.waiting_for_steer = false;
            control.queue.push_back(item);
            evicted
        };
        self.notify.notify_waiters();
        evicted
    }

    fn interrupt_then_enqueue(&self, item: SteeringItem) {
        {
            let mut control = self.control.lock().expect("control state lock poisoned");
            control.waiting_for_steer = true;
            control.queue.push_back(item);
            control.waiting_for_steer = false;
        }
        self.cancel_round();
        self.notify.notify_waiters();
    }

    fn cancel_round(&self) {
        self.round_token
            .read()
            .expect("round token lock poisoned")
            .cancel();
    }

    /// Whether the steering queue currently has no unconsumed messages.
    #[must_use]
    pub fn queue_is_empty(&self) -> bool {
        self.control
            .lock()
            .expect("control state lock poisoned")
            .queue
            .is_empty()
    }

    /// Whether queue work or an interrupt-induced wait is still pending.
    #[must_use]
    pub fn has_pending_control_work(&self) -> bool {
        let control = self.control.lock().expect("control state lock poisoned");
        !control.queue.is_empty() || control.waiting_for_steer
    }

    #[must_use]
    pub fn is_waiting_for_steer(&self) -> bool {
        self.control
            .lock()
            .expect("control state lock poisoned")
            .waiting_for_steer
    }

    /// Current queue length. Production callers should generally prefer
    /// `queue_is_empty` or `enqueue_bounded`'s atomic eviction; this is
    /// kept for tests and diagnostics.
    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.control
            .lock()
            .expect("control state lock poisoned")
            .queue
            .len()
    }
}

#[async_trait::async_trait]
pub trait ToolEnvProvider: Send + Sync {
    async fn resolve(&self) -> anyhow::Result<HashMap<String, String>>;
}

pub struct StaticEnvProvider(pub HashMap<String, String>);

#[async_trait::async_trait]
impl ToolEnvProvider for StaticEnvProvider {
    async fn resolve(&self) -> anyhow::Result<HashMap<String, String>> {
        Ok(self.0.clone())
    }
}

pub struct Session {
    id:                     String,
    /// Root agent session ID for this session's agent tree. A root session
    /// uses its own `id`; a subagent session inherits its parent's
    /// `root_session_id` so todo tools that scope by root (Anthropic tasks)
    /// share one list across all subagents.
    root_session_id:        String,
    config:                 SessionOptions,
    history:                History,
    event_emitter:          Emitter,
    state:                  SessionState,
    llm_client:             Client,
    provider_profile:       Arc<dyn AgentProfile>,
    sandbox:                Arc<dyn Sandbox>,
    control_state:          Arc<Mutex<ControlState>>,
    control_notify:         Arc<Notify>,
    followup_queue:         Arc<Mutex<VecDeque<String>>>,
    cancel_token:           CancellationToken,
    round_token:            Arc<RwLock<CancellationToken>>,
    interrupt_reason:       Arc<Mutex<Option<InterruptReason>>>,
    memory:                 Vec<MemoryDocument>,
    env_context:            EnvContext,
    skills:                 Vec<Skill>,
    system_prompt:          String,
    file_tracker:           FileTracker,
    tool_env_provider:      Option<Arc<dyn ToolEnvProvider>>,
    subagent_manager:       Option<Arc<AsyncMutex<SubAgentManager>>>,
    completion_coordinator: Option<Arc<dyn CompletionCoordinator>>,
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
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            root_session_id: id.clone(),
            id,
            config,
            history: History::default(),
            event_emitter: Emitter::new(),
            state: SessionState::Idle,
            llm_client,
            provider_profile,
            sandbox,
            control_state: Arc::new(Mutex::new(ControlState::default())),
            control_notify: Arc::new(Notify::new()),
            followup_queue: Arc::new(Mutex::new(VecDeque::new())),
            cancel_token: CancellationToken::new(),
            round_token: Arc::new(RwLock::new(CancellationToken::new())),
            interrupt_reason: Arc::new(Mutex::new(None)),
            memory: Vec::new(),
            env_context: EnvContext::default(),
            skills: Vec::new(),
            system_prompt: String::new(),
            file_tracker: FileTracker::default(),
            tool_env_provider: None,
            subagent_manager,
            completion_coordinator: None,
        }
    }

    /// Build a session from a credential source and catalog. Resolves the LLM
    /// client once at construction and caches it for the session's lifetime.
    /// Sessions are bounded (≤ 1 hour); cached client is fine within that
    /// window. For longer-lived contexts (workflow runs) hold a source and
    /// catalog, not a session.
    ///
    /// # Errors
    ///
    /// Returns an error if `Client::from_source` fails (e.g. vault unreachable,
    /// OAuth refresh failed).
    pub async fn from_source(
        source: &dyn CredentialSource,
        catalog: Arc<Catalog>,
        provider_profile: Arc<dyn AgentProfile>,
        sandbox: Arc<dyn Sandbox>,
        config: SessionOptions,
        subagent_manager: Option<Arc<AsyncMutex<SubAgentManager>>>,
    ) -> Result<Self, LlmError> {
        let client = Client::from_source(source, catalog).await?;
        Ok(Self::new(
            client,
            provider_profile,
            sandbox,
            config,
            subagent_manager,
        ))
    }

    pub fn from_record(
        record: &SessionRecord,
        runtime_context: &[SessionMessage],
        llm_client: Client,
        provider_profile: Arc<dyn AgentProfile>,
        sandbox: Arc<dyn Sandbox>,
        config: SessionOptions,
        subagent_manager: Option<Arc<AsyncMutex<SubAgentManager>>>,
    ) -> Result<Self, Error> {
        let mut session = Self::new(
            llm_client,
            provider_profile,
            sandbox,
            config,
            subagent_manager,
        );
        session.id = record.id.to_string();
        // from_record represents a fresh root session by default; callers
        // that materialize subagent sessions set the root explicitly via
        // `set_root_session_id`.
        session.root_session_id.clone_from(&session.id);
        session.history = History::from_session_messages(runtime_context).map_err(|err| {
            Error::InvalidState(format!("invalid persisted session context: {err}"))
        })?;
        session.state = SessionState::Idle;
        Ok(session)
    }

    pub fn set_tool_env_provider(&mut self, provider: Arc<dyn ToolEnvProvider>) {
        self.tool_env_provider = Some(provider);
    }

    pub fn set_tool_env(&mut self, env: HashMap<String, String>) {
        self.set_tool_env_provider(Arc::new(StaticEnvProvider(env)));
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Root agent session ID for this session's agent tree. Equal to
    /// [`Self::id`] for the root agent.
    #[must_use]
    pub fn root_session_id(&self) -> &str {
        &self.root_session_id
    }

    /// Override the root session ID. Used by subagent construction to
    /// inherit the parent's root.
    pub fn set_root_session_id(&mut self, root: impl Into<String>) {
        self.root_session_id = root.into();
    }

    #[must_use]
    pub fn profile_kind(&self) -> AgentProfileKind {
        self.provider_profile.profile_kind()
    }

    #[must_use]
    pub fn provider_id(&self) -> fabro_model::ProviderId {
        self.provider_profile.provider_id()
    }

    #[must_use]
    pub fn model(&self) -> &str {
        self.provider_profile.model()
    }

    #[must_use]
    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.config.reasoning_effort
    }

    #[must_use]
    pub fn speed(&self) -> Option<Speed> {
        self.config.speed
    }

    /// Initialize session by discovering project docs and capturing environment
    /// context. Call before `process_input`.
    ///
    /// # Errors
    ///
    /// Returns `Error::Interrupted(InterruptReason::Cancelled)` if the
    /// session's cancel token fires during initialization.
    pub async fn initialize(&mut self) -> Result<(), Error> {
        let cancel_token = self.cancel_token.clone();

        self.event_emitter
            .emit(self.id.clone(), AgentEvent::SessionStarted {
                provider: Some(self.provider_profile.provider_id().to_string()),
                model:    Some(self.provider_profile.model().to_string()),
            });

        if cancel_token.is_cancelled() {
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }

        let doc_root = self
            .config
            .git_root
            .clone()
            .unwrap_or_else(|| self.sandbox.working_directory().to_string());
        self.memory = discover_memory(
            self.sandbox.as_ref(),
            &doc_root,
            self.sandbox.working_directory(),
            self.provider_profile.profile_kind(),
            &cancel_token,
        )
        .await?;

        let provider_profile = self.provider_profile.profile_kind().to_string();

        // Emit memory loaded event with file metadata. Contents are deliberately
        // omitted so the durable event stream never carries file bytes.
        let memory_files: Vec<MemoryFileSummary> = self
            .memory
            .iter()
            .map(|doc| MemoryFileSummary {
                path:         doc.path.clone(),
                byte_count:   doc.byte_count,
                loaded_bytes: doc.loaded_bytes,
                truncated:    doc.truncated,
            })
            .collect();
        let total_loaded_bytes = self.memory.iter().map(|doc| doc.loaded_bytes).sum();
        self.event_emitter
            .emit(self.id.clone(), AgentEvent::MemoryLoaded {
                provider_profile: provider_profile.clone(),
                files: memory_files,
                total_loaded_bytes,
                budget_bytes: BUDGET_BYTES,
            });

        // Discover skills
        let skill_dirs = if let Some(dirs) = &self.config.skill_dirs {
            dirs.clone()
        } else {
            let skills_dir = fabro_util::Home::from_env().skills_dir();
            let skills_str = skills_dir.to_string_lossy().to_string();
            default_skill_dirs(Some(&skills_str), self.config.git_root.as_deref())
        };
        self.skills = discover_skills(self.sandbox.as_ref(), &skill_dirs, &cancel_token).await?;
        debug!(skill_count = self.skills.len(), "Skills discovered");

        let skill_summaries: Vec<SkillSummary> = self
            .skills
            .iter()
            .map(|skill| SkillSummary {
                name:        skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect();
        self.event_emitter
            .emit(self.id.clone(), AgentEvent::SkillsDiscovered {
                provider_profile,
                source_dirs: skill_dirs.clone(),
                skills: skill_summaries,
            });

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
            let mcp_servers = self.resolve_sandbox_mcp_servers(&cancel_token).await?;

            let mut manager = McpConnectionManager::new();
            let results = manager.start_servers(&mcp_servers).await;

            for (server_name, result) in &results {
                match result {
                    Ok(tool_count) => {
                        let tools = manager
                            .tool_summaries_for_server(server_name)
                            .into_iter()
                            .map(|(name, original_name)| McpToolSummary {
                                name,
                                original_name,
                            })
                            .collect();
                        self.event_emitter
                            .emit(self.id.clone(), AgentEvent::McpServerReady {
                                server_name: server_name.clone(),
                                tool_count: *tool_count,
                                tools,
                            });
                    }
                    Err(e) => {
                        self.event_emitter
                            .emit(self.id.clone(), AgentEvent::McpServerFailed {
                                server_name: server_name.clone(),
                                error:       e.to_string(),
                            });
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
        self.env_context = self.build_env_context(&cancel_token).await?;
        debug!(
            is_git_repo = self.env_context.is_git_repo,
            model = %self.env_context.model,
            "Environment context built"
        );

        // Build system prompt once (static for the session lifetime). Only
        // the loaded memory text is passed to the profile; the document
        // metadata is already surfaced via the `agent.memory.loaded` event.
        let memory_contents: Vec<String> =
            self.memory.iter().map(|doc| doc.content.clone()).collect();
        self.system_prompt = self.provider_profile.build_system_prompt(
            self.sandbox.as_ref(),
            &self.env_context,
            &memory_contents,
            self.config.user_instructions.as_deref(),
            &self.skills,
        );

        Ok(())
    }

    /// Resolve `McpTransport::Sandbox` configs by starting the MCP server
    /// inside the sandbox and rewriting the transport to `Http` with the
    /// sandbox's preview URL.
    async fn resolve_sandbox_mcp_servers(
        &self,
        cancel_token: &CancellationToken,
    ) -> Result<Vec<McpServerSettings>, Error> {
        let mut resolved = Vec::with_capacity(self.config.mcp_servers.len());

        for config in &self.config.mcp_servers {
            if cancel_token.is_cancelled() {
                return Err(Error::Interrupted(InterruptReason::Cancelled));
            }
            match &config.transport {
                McpTransport::Sandbox { command, port, env } => {
                    let port = *port;
                    match self
                        .start_sandbox_mcp_server(command, port, env, cancel_token)
                        .await?
                    {
                        Ok((url, headers)) => {
                            info!(
                                server = %config.name,
                                url = %url,
                                "Sandbox MCP server started, connecting via HTTP"
                            );
                            resolved.push(McpServerSettings {
                                name:                 config.name.clone(),
                                transport:            McpTransport::Http { url, headers },
                                current_dir:          config.current_dir.clone(),
                                clear_env:            config.clear_env,
                                startup_timeout_secs: config.startup_timeout_secs,
                                tool_timeout_secs:    config.tool_timeout_secs,
                            });
                        }
                        Err(e) => {
                            warn!(
                                server = %config.name,
                                error = %e,
                                "Failed to start sandbox MCP server"
                            );
                            self.event_emitter
                                .emit(self.id.clone(), AgentEvent::McpServerFailed {
                                    server_name: config.name.clone(),
                                    error:       e,
                                });
                        }
                    }
                }
                _ => resolved.push(config.clone()),
            }
        }

        Ok(resolved)
    }

    /// Start an MCP server inside the sandbox and return (url, headers) for
    /// HTTP connection.
    ///
    /// The outer `Result` surfaces fatal cancellation as
    /// `Error::Interrupted(InterruptReason::Cancelled)` (the running MCP
    /// process group is terminated before returning). The inner `Result`
    /// captures non-fatal startup failures that the caller logs and turns
    /// into an `McpServerFailed` event.
    async fn start_sandbox_mcp_server(
        &self,
        command: &[String],
        port: u16,
        env: &std::collections::HashMap<String, String>,
        cancel_token: &CancellationToken,
    ) -> Result<Result<(String, std::collections::HashMap<String, String>), String>, Error> {
        let sandbox = self.sandbox.as_ref();

        let cmd_str = command
            .iter()
            .map(|arg| fabro_sandbox::shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");

        // Launch the server detached with setsid so Daytona's exec doesn't block.
        // shell_quote the inner command for the outer `sh -c` so a single quote
        // or metacharacter in any argv element can't break out of the wrapper.
        let inner = format!("{cmd_str} > /tmp/mcp_server_stdout.log 2>/tmp/mcp_server_stderr.log");
        let launch_script = format!(
            "setsid sh -c {quoted} </dev/null >/dev/null 2>&1 &\necho $!",
            quoted = fabro_sandbox::shell_quote(&inner)
        );
        let env_ref = if env.is_empty() { None } else { Some(env) };

        if cancel_token.is_cancelled() {
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }
        let launch_result = match sandbox
            .exec_command(
                &launch_script,
                30_000,
                None,
                env_ref,
                Some(cancel_token.child_token()),
            )
            .await
        {
            Ok(result) => result,
            Err(e) => {
                if cancel_token.is_cancelled() {
                    return Err(Error::Interrupted(InterruptReason::Cancelled));
                }
                return Ok(Err(format!(
                    "Failed to launch MCP server: {}",
                    e.display_with_causes()
                )));
            }
        };

        let pid = launch_result.stdout.trim().to_string();
        info!(pid = %pid, port, "MCP server process launched in sandbox");

        // Wait for the server to start listening on the port
        let poll_cmd = format!(
            "for i in $(seq 1 30); do ss -tln | grep -q ':{port} ' && echo ready && exit 0; sleep 1; done; echo timeout"
        );
        let poll_result = sandbox
            .exec_command(
                &poll_cmd,
                60_000,
                None,
                None,
                Some(cancel_token.child_token()),
            )
            .await;

        if cancel_token.is_cancelled() {
            kill_mcp_pid(sandbox, &pid).await;
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }

        let poll_result = match poll_result {
            Ok(result) => result,
            Err(e) => {
                return Ok(Err(format!(
                    "Failed to poll MCP server readiness: {}",
                    e.display_with_causes()
                )));
            }
        };

        if poll_result.stdout.trim() != "ready" {
            // Grab stderr for debugging
            let stderr = sandbox
                .exec_command(
                    "cat /tmp/mcp_server_stderr.log 2>/dev/null | tail -20",
                    10_000,
                    None,
                    None,
                    Some(cancel_token.child_token()),
                )
                .await
                .map(|r| r.stdout)
                .unwrap_or_default();
            return Ok(Err(format!(
                "MCP server did not start listening on port {port} within 30s. stderr:\n{stderr}"
            )));
        }

        // Get the preview URL for the port, or fall back to localhost for local
        // sandboxes
        let preview = match sandbox.get_preview_url(port).await {
            Ok(p) => p,
            Err(e) => return Ok(Err(e.display_with_causes())),
        };

        if cancel_token.is_cancelled() {
            kill_mcp_pid(sandbox, &pid).await;
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }

        if let Some(url_and_headers) = preview {
            Ok(Ok(url_and_headers))
        } else {
            info!(port, "No preview URL available, using localhost");
            Ok(Ok((
                format!("http://localhost:{port}"),
                std::collections::HashMap::new(),
            )))
        }
    }

    async fn build_env_context(
        &self,
        cancel_token: &CancellationToken,
    ) -> Result<EnvContext, Error> {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let model_name = self.provider_profile.model().to_string();

        if cancel_token.is_cancelled() {
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }

        // Detect git info via sandbox
        let git_branch = self
            .sandbox
            .exec_command(
                "git rev-parse --abbrev-ref HEAD",
                5000,
                None,
                None,
                Some(cancel_token.child_token()),
            )
            .await
            .ok()
            .filter(fabro_sandbox::ExecResult::is_success)
            .map(|r| r.stdout.trim().to_string());

        if cancel_token.is_cancelled() {
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }

        let is_git_repo = git_branch.is_some();

        let git_status_short = if is_git_repo {
            self.sandbox
                .exec_command(
                    "git status --short",
                    5000,
                    None,
                    None,
                    Some(cancel_token.child_token()),
                )
                .await
                .ok()
                .filter(fabro_sandbox::ExecResult::is_success)
                .map(|r| r.stdout.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        if cancel_token.is_cancelled() {
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }

        let git_recent_commits = if is_git_repo {
            self.sandbox
                .exec_command(
                    "git log --oneline -10",
                    5000,
                    None,
                    None,
                    Some(cancel_token.child_token()),
                )
                .await
                .ok()
                .filter(fabro_sandbox::ExecResult::is_success)
                .map(|r| r.stdout.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        if cancel_token.is_cancelled() {
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }

        Ok(EnvContext {
            git_branch,
            is_git_repo,
            current_date: today,
            model: model_name,
            knowledge_cutoff: self.provider_profile.knowledge_cutoff().unwrap_or_default(),
            git_status_short,
            git_recent_commits,
        })
    }

    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.event_emitter.subscribe()
    }

    /// Push a steer onto the queue (no actor — internal callers like
    /// loop-detection use this).
    pub fn steer(&self, message: String) {
        self.control_handle().steer(message, None);
    }

    /// Cancel the current round and wait for later steering before starting
    /// another LLM round.
    pub fn control_interrupt(&self, actor: Option<Principal>) {
        self.control_handle().interrupt(actor);
    }

    /// Cancel the current round and deliver the message as the next steer.
    pub fn interrupt_then_steer(&self, message: String, actor: Option<Principal>) {
        self.control_handle().interrupt_then_steer(message, actor);
    }

    /// Cheap, cloneable handle that lets external coordinators deliver
    /// steers and trigger interrupts without owning the `Session` itself.
    #[must_use]
    pub fn control_handle(&self) -> SessionControlHandle {
        SessionControlHandle {
            control:     self.control_state.clone(),
            round_token: self.round_token.clone(),
            notify:      self.control_notify.clone(),
        }
    }

    /// Install a coordinator that decides whether `process_input` should
    /// keep iterating after a no-tool turn. Used by the workflow layer to
    /// race-safely include any steers that arrived during the final
    /// response.
    pub fn set_completion_coordinator(&mut self, coordinator: Arc<dyn CompletionCoordinator>) {
        self.completion_coordinator = Some(coordinator);
    }

    pub fn follow_up(&self, message: String) {
        self.followup_queue
            .lock()
            .expect("followup queue lock poisoned")
            .push_back(message);
    }

    pub fn interrupt(&self) {
        self.set_interrupt_reason(InterruptReason::Cancelled);
        self.cancel_token.cancel();
    }

    /// Returns a handle that can set the interrupt reason from another task.
    #[must_use]
    pub fn interrupt_reason_handle(&self) -> Arc<Mutex<Option<InterruptReason>>> {
        self.interrupt_reason.clone()
    }

    fn set_interrupt_reason(&self, reason: InterruptReason) {
        let mut guard = self
            .interrupt_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_none() {
            *guard = Some(reason);
        }
    }

    fn interrupted_error(&self) -> Error {
        let reason = self
            .interrupt_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or(InterruptReason::Cancelled);
        Error::Interrupted(reason)
    }

    fn emit_llm_error(&mut self, err: LlmError) -> Error {
        self.event_emitter.emit(self.id.clone(), AgentEvent::Error {
            error: Error::Llm(err.clone()),
        });
        if is_auth_error(&err) {
            self.transition(SessionState::Closed);
        }
        Error::Llm(err)
    }

    async fn open_stream_with_retry(
        &mut self,
        client: &Client,
        request: &Request,
        retry_policy: &RetryPolicy,
    ) -> Result<StreamEventStream, Error> {
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
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Build a callback that forwards sub-agent lifecycle and child session
    /// events through this session's emitter.
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
    /// - any → Closed (interrupt/error — emits SessionEnded)
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

    pub fn close(&mut self) -> bool {
        let was_open = self.state != SessionState::Closed;
        self.transition(SessionState::Closed);
        was_open
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffort>) {
        self.config.reasoning_effort = effort;
    }

    pub fn set_speed(&mut self, speed: Option<Speed>) {
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

    pub async fn process_input(&mut self, input: &str) -> Result<(), Error> {
        self.process_input_with_runtime(input, AgentToolRuntime::default())
            .await
    }

    pub async fn process_input_with_runtime(
        &mut self,
        input: &str,
        agent_tool_runtime: AgentToolRuntime,
    ) -> Result<(), Error> {
        if self.state == SessionState::Closed {
            return Err(Error::SessionClosed);
        }

        // Spawn wall-clock timeout task if configured
        let timer_handle = self.config.wall_clock_timeout.map(|duration| {
            let token = self.cancel_token.clone();
            let reason_handle = self.interrupt_reason.clone();
            tokio::spawn(async move {
                time::sleep(duration).await;
                {
                    let mut guard = reason_handle
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if guard.is_none() {
                        *guard = Some(InterruptReason::WallClockTimeout);
                    }
                }
                token.cancel();
            })
        });

        // Process the initial input, then drain any followups
        let mut result = self.run_single_input(input, &agent_tool_runtime).await;

        if result.is_ok() {
            loop {
                let followup = self
                    .followup_queue
                    .lock()
                    .expect("followup queue lock poisoned")
                    .pop_front();
                let Some(followup) = followup else { break };
                result = self.run_single_input(&followup, &agent_tool_runtime).await;
                if result.is_err() {
                    break;
                }
            }
        }

        // Stop the timer so it doesn't fire after we're done.
        if let Some(handle) = timer_handle {
            handle.abort();
        }

        // Only transition to Idle if the session wasn't closed by an error
        if self.state != SessionState::Closed {
            self.transition(SessionState::Idle);
        }

        result
    }

    async fn run_single_input(
        &mut self,
        input: &str,
        agent_tool_runtime: &AgentToolRuntime,
    ) -> Result<(), Error> {
        const STREAM_CONSUME_RETRIES: usize = 3;

        if self.state == SessionState::Closed {
            return Err(Error::SessionClosed);
        }

        self.transition(SessionState::Thinking);

        // Expand skill references in input
        let expanded = if self.skills.is_empty() {
            ExpandedInput {
                text:       input.to_string(),
                skill_name: None,
            }
        } else {
            expand_skill(&self.skills, input).map_err(Error::InvalidState)?
        };
        if let Some(ref name) = expanded.skill_name {
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::SkillActivated {
                    skill_name: name.clone(),
                    source:     SkillActivationSource::Slash,
                });
        }
        let expanded_input = expanded.text;

        // Append user turn and emit event
        self.history.push(Message::User {
            content:   expanded_input.clone(),
            timestamp: SystemTime::now(),
        });
        self.event_emitter
            .emit(self.id.clone(), AgentEvent::UserInput {
                text: expanded_input.clone(),
            });

        let mut round_count: usize = 0;

        loop {
            // Top-of-loop: if the previous round's interrupt token fired,
            // swap in a fresh one before draining and rebuilding state.
            // (Terminal cancel via `cancel_token` is handled by the explicit
            // check below and by `interrupted_error()`.)
            {
                let needs_refresh = self
                    .round_token
                    .read()
                    .expect("round token lock poisoned")
                    .is_cancelled();
                if needs_refresh {
                    *self.round_token.write().expect("round token lock poisoned") =
                        CancellationToken::new();
                }
            }

            // Terminal cancellation wins even when a control interrupt has
            // parked the session waiting for steering.
            if self.cancel_token.is_cancelled() {
                self.close();
                return Err(self.interrupted_error());
            }

            // Drain pending steering messages at the top of every iteration
            // so steering pushed mid-round is delivered as the first turn of
            // the next round. A pure interrupt with no queued steer parks the
            // session here until a later steer arrives.
            self.drain_steering();
            self.wait_for_steer_if_needed().await?;
            self.drain_steering();

            // Check max_tool_rounds_per_input
            if self.config.max_tool_rounds_per_input > 0
                && round_count >= self.config.max_tool_rounds_per_input
            {
                self.event_emitter
                    .emit(self.id.clone(), AgentEvent::TurnLimitReached {
                        max_turns: self.config.max_tool_rounds_per_input,
                    });
                break;
            }

            // Check max_turns
            if self.config.max_turns > 0 && self.history.turns().len() >= self.config.max_turns {
                self.event_emitter
                    .emit(self.id.clone(), AgentEvent::TurnLimitReached {
                        max_turns: self.config.max_turns,
                    });
                break;
            }

            // Snapshot the per-round token; it stays stable for this iteration.
            let round_token = self
                .round_token
                .read()
                .expect("round token lock poisoned")
                .clone();

            // Pre-turn compaction: trim context before building the request
            self.compact_if_needed().await;

            self.inject_task_reminder_if_needed();

            // Build request
            let request = self.build_request();

            // Emit AssistantTextStart before LLM call
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::AssistantTextStart);

            // Call LLM (streaming) with retry for transient errors
            let retry_emitter = self.event_emitter.clone();
            let retry_session_id = self.id.clone();
            let retry_provider = self.provider_profile.provider_id().to_string();
            let retry_model = self.provider_profile.model().to_string();
            let retry_policy = RetryPolicy {
                max_retries: 3,
                on_retry: Some(std::sync::Arc::new(move |err, attempt, delay| {
                    retry_emitter.emit(retry_session_id.clone(), AgentEvent::LlmRetry {
                        provider:   retry_provider.clone(),
                        model:      retry_model.clone(),
                        attempt:    attempt as usize,
                        delay_secs: delay.as_secs_f64(),
                        error:      err.clone(),
                    });
                })),
                ..Default::default()
            };
            let client = self.llm_client.clone();
            let cancel_token_for_select = self.cancel_token.clone();
            let stream_outcome: Option<Result<StreamEventStream, Error>> = tokio::select! {
                biased;
                () = round_token.cancelled() => None,
                () = cancel_token_for_select.cancelled() => None,
                stream = self.open_stream_with_retry(&client, &request, &retry_policy) => Some(stream),
            };
            let mut event_stream = if let Some(stream) = stream_outcome {
                stream?
            } else {
                if self.cancel_token.is_cancelled() {
                    self.close();
                    return Err(self.interrupted_error());
                }
                // Round-only cancel before stream opened — re-iterate to
                // pick up the steer.
                continue;
            };

            // Consume the stream, retrying up to 3 times if the provider
            // closes the stream without sending a Finish event. If visible
            // output was already emitted, clear it before replaying the turn.
            let mut response = None;
            // Set true if a steer-interrupt cancelled the round mid-stream so
            // we can clear partial output and `continue` after the loop.
            let mut steer_interrupted = false;
            let mut visible_output_present = false;

            'streamattempts: for stream_attempt in 0..=STREAM_CONSUME_RETRIES {
                let mut accumulator = StreamAccumulator::new();
                let mut attempt_emitted_output = false;
                let mut stream_error = None;

                loop {
                    let chunk = tokio::select! {
                        biased;
                        () = round_token.cancelled() => None,
                        () = self.cancel_token.cancelled() => None,
                        next = event_stream.next() => Some(next),
                    };
                    let Some(event_opt) = chunk else {
                        // One of the cancellation tokens fired.
                        break;
                    };
                    let Some(event_result) = event_opt else {
                        // Stream ended normally.
                        break;
                    };
                    match event_result {
                        Ok(event) => {
                            match &event {
                                StreamEvent::TextDelta { ref delta, .. } => {
                                    attempt_emitted_output = true;
                                    visible_output_present = true;
                                    self.event_emitter.emit(
                                        self.id.clone(),
                                        AgentEvent::TextDelta {
                                            delta: delta.clone(),
                                        },
                                    );
                                }
                                StreamEvent::ReasoningDelta { ref delta } => {
                                    attempt_emitted_output = true;
                                    visible_output_present = true;
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
                            stream_error = Some(err);
                            break;
                        }
                    }
                }

                // If terminal cancel fired, drop the stream and bail out.
                if self.cancel_token.is_cancelled() {
                    drop(event_stream);
                    self.close();
                    return Err(self.interrupted_error());
                }

                // If only the round token fired (steer interrupt), drop the
                // stream now; we'll clear partial output and continue below.
                if round_token.is_cancelled() {
                    drop(event_stream);
                    steer_interrupted = true;
                    break 'streamattempts;
                }

                if let Some(resp) = accumulator.response().cloned() {
                    response = Some(resp);
                    break;
                }

                if let Some(err) = stream_error {
                    let can_retry = err.retryable() && stream_attempt < STREAM_CONSUME_RETRIES;
                    let retry_attempt = u32::try_from(stream_attempt).unwrap_or(u32::MAX);
                    let retry_delay = can_retry
                        .then(|| retry::retry_delay(&retry_policy, &err, retry_attempt))
                        .flatten();

                    if let Some(delay) = retry_delay {
                        tracing::warn!(
                            attempt = stream_attempt + 1,
                            max = STREAM_CONSUME_RETRIES,
                            error = %err,
                            delay_secs = delay.as_secs_f64(),
                            "LLM stream failed mid-turn, retrying turn"
                        );
                        if attempt_emitted_output {
                            self.event_emitter.emit(
                                self.id.clone(),
                                AgentEvent::AssistantOutputReplace {
                                    text:      String::new(),
                                    reasoning: None,
                                },
                            );
                            visible_output_present = false;
                        }
                        if let Some(ref on_retry) = retry_policy.on_retry {
                            on_retry(&err, retry_attempt, delay);
                        }

                        let delay_outcome = tokio::select! {
                            biased;
                            () = round_token.cancelled() => None,
                            () = self.cancel_token.cancelled() => None,
                            () = time::sleep(delay) => Some(()),
                        };
                        if delay_outcome.is_none() {
                            steer_interrupted =
                                round_token.is_cancelled() && !self.cancel_token.is_cancelled();
                            break 'streamattempts;
                        }

                        let cancel_token_for_select = self.cancel_token.clone();
                        let retry_outcome: Option<Result<StreamEventStream, Error>> = tokio::select! {
                            biased;
                            () = round_token.cancelled() => None,
                            () = cancel_token_for_select.cancelled() => None,
                            stream = self.open_stream_with_retry(&client, &request, &retry_policy) => Some(stream),
                        };
                        event_stream = if let Some(stream) = retry_outcome {
                            stream?
                        } else {
                            steer_interrupted =
                                round_token.is_cancelled() && !self.cancel_token.is_cancelled();
                            break 'streamattempts;
                        };
                        continue 'streamattempts;
                    }

                    if visible_output_present {
                        self.event_emitter.emit(
                            self.id.clone(),
                            AgentEvent::AssistantOutputReplace {
                                text:      String::new(),
                                reasoning: None,
                            },
                        );
                    }
                    return Err(self.emit_llm_error(err));
                }

                // No Finish event — retry if we have attempts left
                if stream_attempt < STREAM_CONSUME_RETRIES {
                    tracing::warn!(
                        attempt = stream_attempt + 1,
                        max = STREAM_CONSUME_RETRIES,
                        "Stream ended without Finish event, retrying turn"
                    );
                    if attempt_emitted_output {
                        self.event_emitter.emit(
                            self.id.clone(),
                            AgentEvent::AssistantOutputReplace {
                                text:      String::new(),
                                reasoning: None,
                            },
                        );
                        visible_output_present = false;
                    }
                    let cancel_token_for_select = self.cancel_token.clone();
                    let retry_outcome: Option<Result<StreamEventStream, Error>> = tokio::select! {
                        biased;
                        () = round_token.cancelled() => None,
                        () = cancel_token_for_select.cancelled() => None,
                        stream = self.open_stream_with_retry(&client, &request, &retry_policy) => Some(stream),
                    };
                    event_stream = if let Some(stream) = retry_outcome {
                        stream?
                    } else {
                        steer_interrupted =
                            round_token.is_cancelled() && !self.cancel_token.is_cancelled();
                        break 'streamattempts;
                    };
                }
            }

            // Mid-LLM steer interrupt: drop the unrecorded turn, clear any
            // partial visible output, and re-iterate. The next turn's
            // top-of-loop drain delivers the steer as the next user message.
            if steer_interrupted {
                if visible_output_present {
                    self.event_emitter
                        .emit(self.id.clone(), AgentEvent::AssistantOutputReplace {
                            text:      String::new(),
                            reasoning: None,
                        });
                }
                continue;
            }

            let Some(response) = response else {
                if visible_output_present {
                    self.event_emitter
                        .emit(self.id.clone(), AgentEvent::AssistantOutputReplace {
                            text:      String::new(),
                            reasoning: None,
                        });
                }
                return Err(self.emit_llm_error(LlmError::Stream {
                    message: "Stream ended without a Finish event (after retries)".into(),
                    source:  None,
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

            self.history.push(Message::Assistant {
                content: text.clone(),
                tool_calls: tool_calls.clone(),
                provider_parts,
                usage: Box::new(usage),
                response_id: response.id.clone(),
                timestamp: SystemTime::now(),
            });

            // Emit AssistantMessage with enriched data from the response
            let model = ModelRef {
                provider: self.provider_profile.provider_id(),
                model_id: if response.model.is_empty() {
                    self.provider_profile.model().to_string()
                } else {
                    response.model.clone()
                },
                speed:    self.config.speed,
            };
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::AssistantMessage {
                    text: text.clone(),
                    model,
                    usage: response.usage.clone(),
                    tool_call_count: tool_calls.len(),
                });

            // Post-response compaction: trim context after appending assistant turn
            self.compact_if_needed().await;

            // If no tool calls, natural completion. Consult the optional
            // completion coordinator: it can return `true` to force one more
            // iteration when a steer arrived during the final response.
            if tool_calls.is_empty() {
                let should_continue = self
                    .completion_coordinator
                    .as_ref()
                    .is_some_and(|c| c.on_natural_completion());
                if should_continue {
                    continue;
                }
                break;
            }

            round_count += 1;

            // Build a composite cancellation token covering both terminal
            // cancel and round (steer) interrupt. Tools observe it
            // cooperatively — they synthesize "Cancelled" results rather
            // than being dropped mid-flight, which preserves the
            // tool_use ↔ tool_result invariant.
            let composite_token = CancellationToken::new();
            let composite_for_cancel = composite_token.clone();
            let cancel_token_clone = self.cancel_token.clone();
            let round_token_clone = round_token.clone();
            let composite_watcher = tokio::spawn(async move {
                tokio::select! {
                    () = cancel_token_clone.cancelled() => composite_for_cancel.cancel(),
                    () = round_token_clone.cancelled() => composite_for_cancel.cancel(),
                }
            });

            // Execute tool calls (parallel or sequential based on provider)
            self.transition(SessionState::Executing);
            let results = execute_tool_calls(
                &tool_calls,
                true,
                self.provider_profile.tool_registry(),
                self.sandbox.clone(),
                self.config.tool_hooks.as_ref(),
                &composite_token,
                &self.config,
                &self.event_emitter,
                &self.id,
                &self.root_session_id,
                self.tool_env_provider.as_ref(),
                agent_tool_runtime,
            )
            .await;
            composite_watcher.abort();

            // Track file operations from tool calls
            self.file_tracker
                .record_from_tool_calls(&tool_calls, &results);

            // Always append tool_results so the tool_use ↔ tool_result
            // invariant holds, regardless of which token fired.
            self.history.push(Message::ToolResults {
                results,
                timestamp: SystemTime::now(),
            });

            // Terminal cancel takes precedence: close and return.
            if self.cancel_token.is_cancelled() {
                self.close();
                return Err(self.interrupted_error());
            }

            // Round-only cancel (steer interrupt mid-tool): re-iterate;
            // the next top-of-loop drain delivers the steer.
            if round_token.is_cancelled() {
                self.transition(SessionState::Thinking);
                continue;
            }

            self.transition(SessionState::Thinking);

            // Loop detection
            if self.config.enable_loop_detection
                && detect_loop(&self.history, self.config.loop_detection_window)
            {
                self.history.push(Message::Steering {
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
        let Some(estimate) = check_context_usage(
            &self.system_prompt,
            &self.history,
            self.provider_profile.as_ref(),
            self.config.compaction_threshold_percent,
            &self.event_emitter,
            &self.id,
        ) else {
            return;
        };
        if !self.config.enable_context_compaction {
            return;
        }
        if let Err(e) = compact_context(
            &mut self.history,
            &self.llm_client,
            self.provider_profile.as_ref(),
            &self.file_tracker,
            self.config.compaction_preserve_turns,
            estimate,
            &self.event_emitter,
            &self.id,
        )
        .await
        {
            self.event_emitter.emit(self.id.clone(), AgentEvent::Error {
                error: Error::InvalidState(format!("Context compaction failed: {e}")),
            });
        }
    }

    fn drain_steering(&mut self) {
        let messages: Vec<SteeringItem> = {
            let mut control = self
                .control_state
                .lock()
                .expect("control state lock poisoned");
            control.queue.drain(..).collect()
        };
        for item in messages {
            match item {
                SteeringItem::Steering { text, actor } => {
                    self.history.push(Message::Steering {
                        content:   text.clone(),
                        timestamp: SystemTime::now(),
                    });
                    self.event_emitter
                        .emit(self.id.clone(), AgentEvent::SteeringInjected {
                            text,
                            actor,
                        });
                }
                SteeringItem::User { text } => {
                    self.history.push(Message::User {
                        content:   text.clone(),
                        timestamp: SystemTime::now(),
                    });
                }
                SteeringItem::System { text } => {
                    self.history.push(Message::System {
                        content:   text.clone(),
                        timestamp: SystemTime::now(),
                    });
                }
            }
        }
    }

    async fn wait_for_steer_if_needed(&mut self) -> Result<(), Error> {
        loop {
            let notified = self.control_notify.notified();
            let should_wait = {
                let control = self
                    .control_state
                    .lock()
                    .expect("control state lock poisoned");
                control.waiting_for_steer && control.queue.is_empty()
            };
            if !should_wait {
                return Ok(());
            }

            tokio::select! {
                biased;
                () = self.cancel_token.cancelled() => {
                    self.close();
                    return Err(self.interrupted_error());
                }
                () = notified => {}
            }
        }
    }

    fn build_request(&self) -> Request {
        let mut messages = Vec::new();
        if !self.system_prompt.trim().is_empty() {
            messages.push(LlmMessage::system(self.system_prompt.clone()));
        }
        messages.extend(self.history.convert_to_messages());

        let tools = self
            .provider_profile
            .tool_registry()
            .definitions_for_policy(
                self.config.tool_access_policy.as_deref(),
                self.config.tool_exposure_mode,
            );
        let has_tools = !tools.is_empty();

        Request {
            model: self.provider_profile.model().to_string(),
            messages,
            provider: Some(self.provider_profile.provider_id().to_string()),
            tools: if has_tools { Some(tools) } else { None },
            tool_choice: if has_tools {
                Some(ToolChoice::Auto)
            } else {
                None
            },
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens: self
                .config
                .max_tokens
                .or_else(|| self.provider_profile.max_output_tokens()),
            stop_sequences: None,
            reasoning_effort: self.config.reasoning_effort,
            speed: self.config.speed,
            metadata: None,
            provider_options: None,
        }
    }

    fn inject_task_reminder_if_needed(&mut self) {
        let tools = self
            .provider_profile
            .tool_registry()
            .definitions_for_policy(
                self.config.tool_access_policy.as_deref(),
                self.config.tool_exposure_mode,
            );
        let tool_names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        if let Some(reminder) = task_reminder::maybe_reminder(&self.history, &tool_names) {
            self.history.push(Message::System {
                content:   reminder,
                timestamp: SystemTime::now(),
            });
        }
    }
}

const fn is_auth_error(err: &LlmError) -> bool {
    matches!(
        err.provider_kind(),
        Some(ProviderErrorKind::Authentication | ProviderErrorKind::AccessDenied)
    )
}

/// Best-effort kill of a sandbox MCP server process group. Used when
/// `start_sandbox_mcp_server` is cancelled after spawning a detached
/// `setsid` child but before reporting readiness. Errors from the sandbox
/// are logged and swallowed; the caller is already returning a Cancelled
/// error.
async fn kill_mcp_pid(sandbox: &dyn Sandbox, pid: &str) {
    let pid = pid.trim();
    if pid.is_empty() {
        return;
    }
    let script =
        format!("kill -TERM -{pid} 2>/dev/null; sleep 1; kill -KILL -{pid} 2>/dev/null; true");
    if let Err(err) = sandbox.exec_command(&script, 5_000, None, None, None).await {
        warn!(pid, error = %err.display_with_causes(), "Failed to kill MCP server process group during cancellation");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use anyhow::Context as _;
    use fabro_llm::error::{ProviderErrorDetail, ProviderErrorKind};
    use fabro_llm::provider::{ProviderAdapter, StreamEventStream};
    use fabro_llm::types::{
        ContentPart, ReasoningEffort, Request, Response, Role, StreamEvent, TokenCounts, ToolCall,
        ToolDefinition,
    };
    use futures::stream;
    use tokio::time::{sleep, timeout};

    use super::*;
    use crate::config::{ToolAccess, ToolAccessPolicy, ToolApprovalAdapter, ToolExposureMode};
    use crate::skills::{Skill, make_use_skill_tool};
    use crate::subagent::SubAgentStatus;
    use crate::test_support::*;
    use crate::tool_registry::{RegisteredTool, ToolContext, ToolRegistry};

    struct NamedToolAccessPolicy {
        decisions: Vec<(&'static str, ToolAccess)>,
    }

    impl NamedToolAccessPolicy {
        fn new(decisions: Vec<(&'static str, ToolAccess)>) -> Self {
            Self { decisions }
        }
    }

    impl ToolAccessPolicy for NamedToolAccessPolicy {
        fn access_for_tool(&self, tool_name: &str) -> ToolAccess {
            self.decisions
                .iter()
                .find_map(|(name, access)| (*name == tool_name).then_some(*access))
                .unwrap_or(ToolAccess::Denied)
        }
    }

    fn make_named_noop_tool(name: &str) -> RegisteredTool {
        RegisteredTool {
            definition: ToolDefinition {
                name:        name.to_string(),
                description: format!("Tool {name}"),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(|_args, _ctx| Box::pin(async { Ok("ok".to_string()) })),
        }
    }

    #[derive(Clone)]
    enum ScriptedStreamCall {
        Response(Box<Response>),
        Events(Vec<Result<StreamEvent, LlmError>>),
        Error(LlmError),
    }

    struct ScriptedStreamProvider {
        calls:      Vec<ScriptedStreamCall>,
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

        fn events_for_response(response: Response) -> Vec<Result<StreamEvent, LlmError>> {
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

        async fn complete(&self, _request: &Request) -> Result<Response, LlmError> {
            Err(LlmError::Configuration {
                message: "ScriptedStreamProvider does not implement complete()".into(),
                source:  None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
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
        assert!(matches!(&turns[0], Message::User { content, .. } if content == "Hi"));
        assert!(
            matches!(&turns[1], Message::Assistant { content, .. } if content == "Hello there!")
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
        assert!(matches!(&turns[0], Message::User { .. }));
        assert!(
            matches!(&turns[1], Message::Assistant { tool_calls, .. } if tool_calls.len() == 1)
        );
        assert!(matches!(&turns[2], Message::ToolResults { results, .. } if results.len() == 1));
        assert!(matches!(&turns[3], Message::Assistant { content, .. } if content == "Done!"));

        // Verify tool result content
        if let Message::ToolResults { results, .. } = &turns[2] {
            assert_eq!(results[0].tool_call_id, "call_1");
            assert!(!results[0].is_error);
        }
    }

    struct SequenceToolEnvProvider {
        values: Mutex<VecDeque<HashMap<String, String>>>,
    }

    #[async_trait::async_trait]
    impl ToolEnvProvider for SequenceToolEnvProvider {
        async fn resolve(&self) -> anyhow::Result<HashMap<String, String>> {
            self.values
                .lock()
                .unwrap()
                .pop_front()
                .context("env script exhausted")
        }
    }

    #[tokio::test]
    async fn session_passes_tool_env_provider_to_each_tool_round() {
        let seen_tokens = Arc::new(Mutex::new(Vec::new()));
        let seen_tokens_for_tool = Arc::clone(&seen_tokens);
        let record_env_tool = RegisteredTool {
            definition: ToolDefinition {
                name:        "record_env".into(),
                description: "Records resolved env".into(),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(move |_args, ctx| {
                let seen_tokens = Arc::clone(&seen_tokens_for_tool);
                Box::pin(async move {
                    let env = ctx
                        .resolve_tool_env()
                        .await
                        .map_err(|err| format!("{err:#}"))?
                        .unwrap_or_default();
                    seen_tokens.lock().unwrap().push(
                        env.get("GITHUB_TOKEN")
                            .cloned()
                            .unwrap_or_else(|| "<missing>".to_string()),
                    );
                    Ok("recorded".to_string())
                })
            }),
        };

        let mut registry = ToolRegistry::new();
        registry.register(record_env_tool);
        let responses = vec![
            tool_call_response("record_env", "call_1", serde_json::json!({})),
            tool_call_response("record_env", "call_2", serde_json::json!({})),
            text_response("Done!"),
        ];
        let mut session = make_session_with_tools(responses, registry).await;
        session.set_tool_env_provider(Arc::new(SequenceToolEnvProvider {
            values: Mutex::new(VecDeque::from([
                HashMap::from([("GITHUB_TOKEN".to_string(), "t1".to_string())]),
                HashMap::from([("GITHUB_TOKEN".to_string(), "t2".to_string())]),
            ])),
        }));

        session.process_input("Use tools").await.unwrap();

        assert_eq!(seen_tokens.lock().unwrap().as_slice(), [
            "t1".to_string(),
            "t2".to_string()
        ]);
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
        assert!(matches!(&turns[0], Message::User { .. }));
        assert!(
            matches!(&turns[1], Message::Steering { content, .. } if content == "Focus on the task")
        );
        assert!(matches!(&turns[2], Message::Assistant { .. }));
    }

    #[tokio::test]
    async fn steer_event_carries_text() {
        let mut session = make_session(vec![text_response("OK")]).await;
        let mut rx = session.subscribe();
        session.steer("hi there".to_string());
        session.process_input("Do something").await.unwrap();

        let mut found_text = None;
        while let Ok(ev) = rx.try_recv() {
            if let AgentEvent::SteeringInjected { text, .. } = ev.event {
                found_text = Some(text);
                break;
            }
        }
        assert_eq!(found_text.as_deref(), Some("hi there"));
    }

    #[tokio::test]
    async fn pure_interrupt_enters_waiting_for_steer_without_queueing_text() {
        let handle = SessionControlHandle::new();

        handle.interrupt(None);
        handle.interrupt(None);

        assert!(handle.is_waiting_for_steer());
        assert_eq!(handle.queue_len(), 0);
        assert!(handle.has_pending_control_work());
    }

    #[tokio::test]
    async fn pure_interrupt_waits_until_later_steer() {
        let mut session = make_session(vec![text_response("OK")]).await;
        let handle = session.control_handle();
        handle.interrupt(None);

        let wake_handle = handle.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(10)).await;
            wake_handle.steer("resume now".to_string(), None);
        });

        timeout(Duration::from_secs(1), session.process_input("start"))
            .await
            .expect("session should wake when steering arrives")
            .unwrap();

        let turns = session.history().turns();
        assert!(matches!(&turns[1], Message::Steering { content, .. } if content == "resume now"));
        assert!(!handle.is_waiting_for_steer());
    }

    #[tokio::test]
    async fn interrupt_then_steer_injects_steering_text() {
        let mut session = make_session(vec![text_response("OK")]).await;
        let mut rx = session.subscribe();

        let handle = session.control_handle();
        handle.interrupt_then_steer("stop now".to_string(), None);
        session.process_input("start").await.unwrap();

        let mut found_text = None;
        while let Ok(ev) = rx.try_recv() {
            if let AgentEvent::SteeringInjected { text, .. } = ev.event {
                found_text = Some(text);
                break;
            }
        }
        assert_eq!(found_text.as_deref(), Some("stop now"));
    }

    #[tokio::test]
    async fn append_during_final_response_triggers_extra_round_when_coordinator_returns_true() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct OnceCoordinator {
            calls:  AtomicUsize,
            handle: SessionControlHandle,
        }
        impl CompletionCoordinator for OnceCoordinator {
            fn on_natural_completion(&self) -> bool {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // Simulate a steer that arrived during the first
                    // completion: enqueue and report "keep going".
                    self.handle
                        .steer("after-completion steer".to_string(), None);
                    true
                } else {
                    false
                }
            }
        }

        // First scripted response is a no-tool natural completion; second
        // also natural completion. The completion coordinator forces the
        // loop to iterate once more — that iteration must drain the queued
        // steer and produce a second Assistant turn.
        let responses = vec![
            text_response("First reply"),
            text_response("Second reply, after steer"),
        ];
        let mut session = make_session(responses).await;
        let handle = session.control_handle();
        session.set_completion_coordinator(Arc::new(OnceCoordinator {
            calls: AtomicUsize::new(0),
            handle,
        }));

        session.process_input("hi").await.unwrap();
        let turns = session.history().turns();
        // User + Assistant + Steering + Assistant = 4
        assert_eq!(turns.len(), 4);
        assert!(matches!(&turns[0], Message::User { .. }));
        assert!(
            matches!(&turns[1], Message::Assistant { content, .. } if content == "First reply")
        );
        assert!(matches!(&turns[2], Message::Steering { content, .. }
                if content == "after-completion steer"));
        assert!(matches!(&turns[3], Message::Assistant { content, .. }
                if content == "Second reply, after steer"));
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
        assert!(matches!(&turns[0], Message::User { content, .. } if content == "initial message"));
        assert!(
            matches!(&turns[1], Message::Assistant { content, .. } if content == "First response")
        );
        assert!(
            matches!(&turns[2], Message::User { content, .. } if content == "followup message")
        );
        assert!(
            matches!(&turns[3], Message::Assistant { content, .. } if content == "Followup response")
        );
    }

    #[tokio::test]
    async fn events_emitted() {
        let mut session = make_session(vec![text_response("Hello")]).await;
        let mut rx = session.subscribe();

        session.initialize().await.unwrap();
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
        if let Message::ToolResults { results, .. } = &turns[2] {
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
        if let Message::ToolResults { results, .. } = &turns[2] {
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
            |t| matches!(t, Message::Steering { content, .. } if content.contains("Loop detected")),
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
        // Set interrupt before processing
        session.interrupt();
        let result = session.process_input("Do something").await;

        // Should return Interrupted error and transition to Closed
        assert!(matches!(result, Err(Error::Interrupted(_))));
        assert_eq!(session.state(), SessionState::Closed);

        // Should have stopped immediately: User turn only, no LLM call
        let turns = session.history().turns();
        assert_eq!(turns.len(), 1);
        assert!(matches!(&turns[0], Message::User { .. }));
    }

    #[tokio::test]
    async fn abort_transitions_to_closed() {
        let cancel_token = CancellationToken::new();
        let cancel_token_for_tool = cancel_token.clone();

        // Tool that cancels the token when executed
        let abort_tool = RegisteredTool {
            definition: ToolDefinition {
                name:        "set_abort".into(),
                description: "Sets interrupt flag".into(),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(move |_args, _ctx| {
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

        // Should return Interrupted error and transition to Closed
        assert!(matches!(result, Err(Error::Interrupted(_))));
        assert_eq!(session.state(), SessionState::Closed);

        // Should have processed: User + Assistant(tool_call) + ToolResults = 3 turns
        // The tool cancelled the token, so the loop breaks before the next LLM call
        let turns = session.history().turns();
        assert_eq!(turns.len(), 3);
        assert!(matches!(&turns[0], Message::User { .. }));
        assert!(
            matches!(&turns[1], Message::Assistant { tool_calls, .. } if tool_calls.len() == 1)
        );
        assert!(matches!(&turns[2], Message::ToolResults { .. }));
    }

    #[tokio::test]
    async fn auth_error_closes_session() {
        let error_provider = Arc::new(MockErrorProvider {
            error: LlmError::Provider {
                kind:   ProviderErrorKind::Authentication,
                detail: Box::new(ProviderErrorDetail::new("invalid api key", "mock")),
            },
        });
        let client = make_client(error_provider).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);

        let result = session.process_input("Hello").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Llm(_)));
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
        assert!(matches!(&turns[0], Message::User { content, .. } if content == "one"));
        assert!(matches!(&turns[1], Message::Assistant { content, .. } if content == "First"));
        assert!(matches!(&turns[2], Message::User { content, .. } if content == "two"));
        assert!(matches!(&turns[3], Message::Assistant { content, .. } if content == "Second"));
    }

    #[tokio::test]
    async fn closed_session_rejects_input() {
        let mut session = make_session(vec![]).await;
        session.close();
        assert_eq!(session.state(), SessionState::Closed);

        let result = session.process_input("Hello").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::SessionClosed));
    }

    #[tokio::test]
    async fn close_reports_whether_it_transitioned_to_closed() {
        let mut session = make_session(vec![]).await;
        let mut rx = session.subscribe();

        assert!(session.close());
        assert!(!session.close());

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.event, AgentEvent::SessionEnded))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn closed_session_does_not_emit_session_start() {
        let mut session = make_session(vec![]).await;
        session.close();

        let mut rx = session.subscribe();
        let result = session.process_input("Hello").await;
        assert!(matches!(result, Err(Error::SessionClosed)));

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
        if let Message::ToolResults { results, .. } = &turns[2] {
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
                name:        "strict_tool".into(),
                description: "Tool with required params".into(),
                parameters:  serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"]
                }),
            },
            executor:   Arc::new(|_args, _ctx| {
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
        if let Message::ToolResults { results, .. } = &turns[2] {
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
                name:        "strict_tool".into(),
                description: "Tool with required params".into(),
                parameters:  serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"]
                }),
            },
            executor:   Arc::new(|_args, _ctx| {
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
        if let Message::ToolResults { results, .. } = &turns[2] {
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

        session.initialize().await.unwrap();
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
        // SessionStarted is emitted once during initialize(), SessionEnded once during
        // close()
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
        session.initialize().await.unwrap();
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
    async fn request_exposes_all_registered_tools_when_no_access_policy_is_set() {
        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let mut registry = ToolRegistry::new();
        registry.register(make_named_noop_tool("read_file"));
        registry.register(make_named_noop_tool("write_file"));
        let profile = Arc::new(TestProfile::with_tools(registry));
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);

        session.process_input("test").await.unwrap();

        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("request should have been captured");
        let tools = request.tools.as_ref().expect("tools should be exposed");
        let tool_names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(tool_names.len(), 2);
        assert!(tool_names.contains(&"read_file"));
        assert!(tool_names.contains(&"write_file"));
    }

    #[tokio::test]
    async fn request_injects_task_reminder_after_ten_unused_assistant_turns() {
        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let mut registry = ToolRegistry::new();
        registry.register(make_named_noop_tool("TaskCreate"));
        registry.register(make_named_noop_tool("TaskUpdate"));
        let profile = Arc::new(TestProfile::with_tools(registry));
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);

        for index in 0..10 {
            session
                .process_input(&format!("turn {index}"))
                .await
                .unwrap();
        }
        session.process_input("turn 10").await.unwrap();

        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("request should have been captured");
        assert!(
            request.messages.iter().any(|message| {
                message.role == Role::System
                    && message.text().contains("<system-reminder>")
                    && message.text().contains("TaskCreate")
                    && message.text().contains("TaskUpdate")
            }),
            "request should include task reminder system message"
        );
    }

    #[tokio::test]
    async fn request_omits_tools_denied_by_access_policy() {
        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let mut registry = ToolRegistry::new();
        registry.register(make_named_noop_tool("read_file"));
        registry.register(make_named_noop_tool("write_file"));
        let profile = Arc::new(TestProfile::with_tools(registry));
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            tool_access_policy: Some(Arc::new(NamedToolAccessPolicy::new(vec![
                ("read_file", ToolAccess::Allowed),
                ("write_file", ToolAccess::Denied),
            ]))),
            tool_exposure_mode: ToolExposureMode::IncludeRequiresApproval,
            ..SessionOptions::default()
        };
        let mut session = Session::new(client, profile, env, config, None);

        session.process_input("test").await.unwrap();

        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("request should have been captured");
        let tools = request.tools.as_ref().expect("tools should be exposed");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");
    }

    #[tokio::test]
    async fn request_exposes_approval_required_tools_when_mode_allows_them() {
        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let mut registry = ToolRegistry::new();
        registry.register(make_named_noop_tool("read_file"));
        registry.register(make_named_noop_tool("shell"));
        let profile = Arc::new(TestProfile::with_tools(registry));
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            tool_access_policy: Some(Arc::new(NamedToolAccessPolicy::new(vec![
                ("read_file", ToolAccess::Allowed),
                ("shell", ToolAccess::RequiresApproval),
            ]))),
            tool_exposure_mode: ToolExposureMode::IncludeRequiresApproval,
            ..SessionOptions::default()
        };
        let mut session = Session::new(client, profile, env, config, None);

        session.process_input("test").await.unwrap();

        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("request should have been captured");
        let tools = request.tools.as_ref().expect("tools should be exposed");
        let tool_names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(tool_names.len(), 2);
        assert!(tool_names.contains(&"read_file"));
        assert!(tool_names.contains(&"shell"));
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

        if let Message::ToolResults { results, .. } = &turns[2] {
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
            matches!(&turns[3], Message::Assistant { content, .. } if content == "OK after denial")
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
        if let Message::ToolResults { results, .. } = &turns[2] {
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
        if let Message::ToolResults { results, .. } = &turns[2] {
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

    #[tokio::test(start_paused = true)]
    async fn stream_retries_retryable_mid_stream_error_and_records_recovered_response() {
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![
                Ok(StreamEvent::text_delta("partial", None)),
                Err(LlmError::Stream {
                    message: "connection reset".into(),
                    source:  None,
                }),
            ]),
            ScriptedStreamCall::Response(Box::new(text_response("Recovered"))),
        ]));
        let mut session = make_session_with_provider(provider.clone()).await;
        let mut rx = session.subscribe();

        session.process_input("Hello").await.unwrap();

        assert_eq!(provider.call_index.load(Ordering::SeqCst), 2);
        assert_eq!(session.history().turns().len(), 2);
        assert!(matches!(
            session.history().turns().last(),
            Some(Message::Assistant { content, .. }) if content == "Recovered"
        ));

        let mut observed = Vec::new();
        let mut retry_count = 0;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::TextDelta { delta } => observed.push(format!("delta:{delta}")),
                AgentEvent::AssistantOutputReplace { text, reasoning } => {
                    observed.push(format!("replace:{text}:{reasoning:?}"));
                }
                AgentEvent::LlmRetry { error, .. } => {
                    retry_count += 1;
                    assert!(error.retryable());
                }
                AgentEvent::AssistantMessage { text, .. } => {
                    observed.push(format!("message:{text}"));
                }
                AgentEvent::Error { .. } => observed.push("error".to_string()),
                _ => {}
            }
        }

        assert_eq!(retry_count, 1);
        assert_eq!(observed, vec![
            "delta:partial".to_string(),
            "replace::None".to_string(),
            "delta:Recovered".to_string(),
            "message:Recovered".to_string(),
        ]);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_quota_error_does_not_replay() {
        let quota_error = LlmError::Provider {
            kind:   ProviderErrorKind::QuotaExceeded,
            detail: Box::new(ProviderErrorDetail {
                error_code: Some("insufficient_quota".into()),
                ..ProviderErrorDetail::new("You exceeded your current quota", "mock")
            }),
        };
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![
                Ok(StreamEvent::text_delta("partial", None)),
                Err(quota_error.clone()),
            ]),
        ]));
        let mut session = make_session_with_provider(provider.clone()).await;

        let result = session.process_input("Hello").await;

        assert!(matches!(
            result,
            Err(Error::Llm(LlmError::Provider {
                kind: ProviderErrorKind::QuotaExceeded,
                ..
            }))
        ));
        assert_eq!(provider.call_index.load(Ordering::SeqCst), 1);
    }

    async fn assert_non_retryable_mid_stream_provider_error_does_not_replay(
        kind: ProviderErrorKind,
    ) {
        let llm_error = LlmError::Provider {
            kind,
            detail: Box::new(ProviderErrorDetail::new(
                format!("deterministic provider error: {kind:?}"),
                "mock",
            )),
        };
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![
                Ok(StreamEvent::text_delta("partial", None)),
                Err(llm_error.clone()),
            ]),
            ScriptedStreamCall::Response(Box::new(text_response("should not replay"))),
        ]));
        let mut session = make_session_with_provider(provider.clone()).await;
        let mut rx = session.subscribe();

        let result = session.process_input("Hello").await;

        assert!(matches!(
            result,
            Err(Error::Llm(LlmError::Provider {
                kind: actual_kind,
                ..
            })) if actual_kind == kind
        ));
        assert_eq!(provider.call_index.load(Ordering::SeqCst), 1);
        assert_eq!(session.history().turns().len(), 1);

        let mut observed = Vec::new();
        let mut retry_count = 0;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::TextDelta { delta } => observed.push(format!("delta:{delta}")),
                AgentEvent::AssistantOutputReplace { text, reasoning } => {
                    observed.push(format!("replace:{text}:{reasoning:?}"));
                }
                AgentEvent::LlmRetry { .. } => retry_count += 1,
                AgentEvent::Error { error } => {
                    assert!(matches!(
                        error,
                        Error::Llm(LlmError::Provider {
                            kind: actual_kind,
                            ..
                        }) if actual_kind == kind
                    ));
                    observed.push("error".to_string());
                }
                AgentEvent::AssistantMessage { .. } => observed.push("message".to_string()),
                _ => {}
            }
        }

        assert_eq!(retry_count, 0);
        assert_eq!(observed, vec![
            "delta:partial".to_string(),
            "replace::None".to_string(),
            "error".to_string(),
        ]);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_non_retryable_mid_stream_errors_do_not_replay() {
        assert_non_retryable_mid_stream_provider_error_does_not_replay(
            ProviderErrorKind::Authentication,
        )
        .await;
        assert_non_retryable_mid_stream_provider_error_does_not_replay(
            ProviderErrorKind::ContextLength,
        )
        .await;
        assert_non_retryable_mid_stream_provider_error_does_not_replay(
            ProviderErrorKind::QuotaExceeded,
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn stream_retry_exhaustion_emits_one_error_without_committing_assistant_or_tools() {
        let retryable_error = LlmError::Stream {
            message: "connection reset".into(),
            source:  None,
        };
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![
                Ok(StreamEvent::text_delta("partial", None)),
                Ok(StreamEvent::ToolCallEnd {
                    tool_call: ToolCall::new(
                        "call_1",
                        "echo",
                        serde_json::json!({"text": "should not run"}),
                    ),
                }),
                Err(retryable_error.clone()),
            ]),
        ]));
        let mut session = make_session_with_provider(provider.clone()).await;
        let mut rx = session.subscribe();

        let result = session.process_input("Hello").await;

        assert!(matches!(result, Err(Error::Llm(LlmError::Stream { .. }))));
        assert_eq!(provider.call_index.load(Ordering::SeqCst), 4);
        assert_eq!(session.history().turns().len(), 1);

        let mut retry_count = 0;
        let mut error_count = 0;
        let mut replace_count = 0;
        let mut assistant_message_count = 0;
        let mut tool_started_count = 0;
        let mut tool_completed_count = 0;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::LlmRetry { error, .. } => {
                    retry_count += 1;
                    assert!(error.retryable());
                }
                AgentEvent::AssistantOutputReplace { text, reasoning } => {
                    assert_eq!(text, "");
                    assert!(reasoning.is_none());
                    replace_count += 1;
                }
                AgentEvent::Error { error } => {
                    assert!(matches!(error, Error::Llm(LlmError::Stream { .. })));
                    error_count += 1;
                }
                AgentEvent::AssistantMessage { .. } => assistant_message_count += 1,
                AgentEvent::ToolCallStarted { .. } => tool_started_count += 1,
                AgentEvent::ToolCallCompleted { .. } => tool_completed_count += 1,
                _ => {}
            }
        }

        assert_eq!(retry_count, 3);
        assert_eq!(replace_count, 4);
        assert_eq!(error_count, 1);
        assert_eq!(assistant_message_count, 0);
        assert_eq!(tool_started_count, 0);
        assert_eq!(tool_completed_count, 0);
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
            Some(Message::Assistant { content, .. }) if content == "Recovered"
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
            Some(Message::Assistant { content, .. }) if content == "Hello"
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

        assert_eq!(observed, vec![
            "start".to_string(),
            "delta:Hel".to_string(),
            "replace::None".to_string(),
            "delta:Hello".to_string(),
            "message:Hello".to_string(),
        ]);
    }

    #[tokio::test]
    async fn retry_open_auth_error_emits_error_and_closes_session() {
        let auth_error = LlmError::Provider {
            kind:   ProviderErrorKind::Authentication,
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
            Err(Error::Llm(LlmError::Provider {
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
                        Error::Llm(LlmError::Provider {
                            kind: ProviderErrorKind::Authentication,
                            ..
                        })
                    );
                }
                AgentEvent::AssistantMessage { .. } => observed.push("message".to_string()),
                _ => {}
            }
        }

        assert_eq!(observed, vec![
            "start".to_string(),
            "delta:Hel".to_string(),
            "replace::None".to_string(),
            "error".to_string(),
        ]);
        assert!(found_auth_error_event, "expected auth error event");
    }

    fn response_with_usage(mut response: Response, usage: TokenCounts) -> Response {
        response.usage = usage;
        response
    }

    fn response_with_input_tokens(response: Response, input_tokens: i64) -> Response {
        response_with_usage(response, TokenCounts {
            input_tokens,
            ..TokenCounts::default()
        })
    }

    #[tokio::test]
    async fn compaction_triggered_when_over_threshold() {
        // Tiny context window to trigger compaction
        // Responses: [0] conversation response (stream), [1] summarization (complete),
        // [2] unused fallback
        let responses = vec![
            response_with_usage(text_response("OK"), TokenCounts::default()),
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
            turns.iter().any(|t| matches!(t, Message::System { content, .. } if content.contains("A different assistant began this task"))),
            "Should contain a summary system turn"
        );
    }

    #[tokio::test]
    async fn compaction_uses_assistant_usage_baseline_for_short_response() {
        let responses = vec![
            response_with_input_tokens(text_response("OK"), 90),
            text_response("Here is the summary of the conversation so far."),
            text_response("fallback"),
        ];

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

        session.process_input("hi").await.unwrap();

        let mut started = None;
        let mut found_completed = false;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::CompactionStarted {
                    estimated_tokens,
                    context_window_size,
                } => started = Some((estimated_tokens, context_window_size)),
                AgentEvent::CompactionCompleted { .. } => found_completed = true,
                _ => {}
            }
        }

        assert_eq!(started, Some((90, 100)));
        assert!(
            found_completed,
            "CompactionCompleted event should be emitted"
        );
    }

    #[tokio::test]
    async fn compaction_noop_does_not_emit_started() {
        let large_input = "x".repeat(400);
        let responses = vec![text_response("OK")];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let registry = ToolRegistry::new();
        let profile = Arc::new(TestProfile::with_context_window(registry, 100));
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            enable_context_compaction: true,
            compaction_preserve_turns: 10,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config, None);
        let mut rx = session.subscribe();

        session.process_input(&large_input).await.unwrap();

        let mut found_warning = false;
        let mut found_compaction = false;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::Warning { kind, .. } if kind == "context_window" => {
                    found_warning = true;
                }
                AgentEvent::CompactionStarted { .. } | AgentEvent::CompactionCompleted { .. } => {
                    found_compaction = true;
                }
                _ => {}
            }
        }

        assert!(found_warning, "threshold should have been exceeded");
        assert!(
            !found_compaction,
            "no-op compaction should not emit started or completed events"
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
    async fn compaction_disabled_blocks_api_usage_baseline_compaction() {
        let responses = vec![response_with_input_tokens(text_response("OK"), 90)];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let registry = ToolRegistry::new();
        let profile = Arc::new(TestProfile::with_context_window(registry, 100));
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            enable_context_compaction: false,
            compaction_preserve_turns: 1,
            ..Default::default()
        };
        let mut session = Session::new(client, profile, env, config, None);
        let mut rx = session.subscribe();

        session.process_input("hi").await.unwrap();

        let mut found_api_usage_warning = false;
        let mut found_compaction = false;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::Warning { details, .. }
                    if details["estimated_tokens"] == 90
                        && details["estimate_method"] == "api_usage_plus_local_delta" =>
                {
                    found_api_usage_warning = true;
                }
                AgentEvent::CompactionStarted { .. } | AgentEvent::CompactionCompleted { .. } => {
                    found_compaction = true;
                }
                _ => {}
            }
        }

        assert!(
            found_api_usage_warning,
            "API usage baseline should still drive context warning"
        );
        assert!(!found_compaction, "compaction must remain disabled");
    }

    #[tokio::test]
    async fn compaction_failure_is_non_fatal() {
        // Response [0] = conversation response (stream), [1] will be used for
        // summarization (complete) but we need it to error. We'll use a special
        // provider that errors on complete() but succeeds on stream().

        struct StreamOnlyProvider {
            responses:  Vec<Response>,
            call_index: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl ProviderAdapter for StreamOnlyProvider {
            fn name(&self) -> &'static str {
                "mock"
            }

            async fn complete(&self, _request: &Request) -> Result<Response, LlmError> {
                Err(LlmError::Stream {
                    message: "summarization failed".into(),
                    source:  None,
                })
            }

            async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
                let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
                let response = if idx < self.responses.len() {
                    self.responses[idx].clone()
                } else {
                    self.responses[self.responses.len() - 1].clone()
                };
                // Reuse response_to_stream helper from test_support
                let mut events: Vec<Result<StreamEvent, LlmError>> = Vec::new();
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
        let responses = vec![response_with_usage(
            text_response("OK"),
            TokenCounts::default(),
        )];

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
        use fabro_llm::types::ToolDefinition;

        use crate::tool_registry::RegisteredTool;

        // Provider that captures complete() requests (compaction) while returning
        // canned responses for stream() calls.
        struct CompactionCapturingProvider {
            stream_responses:  Vec<Response>,
            stream_index:      AtomicUsize,
            captured_complete: Mutex<Option<Request>>,
        }

        #[async_trait::async_trait]
        impl ProviderAdapter for CompactionCapturingProvider {
            fn name(&self) -> &'static str {
                "mock"
            }

            async fn complete(&self, request: &Request) -> Result<Response, LlmError> {
                *self.captured_complete.lock().unwrap() = Some(request.clone());
                Ok(text_response("## Goal\nSummary goes here."))
            }

            async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
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
                name:        "read_file".into(),
                description: "Read a file".into(),
                parameters:  serde_json::json!({"type": "object", "properties": {"file_path": {"type": "string"}}}),
            },
            executor:   Arc::new(|_args, _ctx| {
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

        // Second call with large input: context is well over threshold, compaction
        // triggers
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
        use std::collections::HashMap;

        use fabro_mcp::config::{McpServerSettings, McpTransport};

        let test_server = format!(
            "{}/../fabro-mcp/tests/test_mcp_server.py",
            env!("CARGO_MANIFEST_DIR")
        );
        let config = SessionOptions {
            mcp_servers: vec![McpServerSettings {
                name:                 "test-echo".into(),
                transport:            McpTransport::Stdio {
                    command: vec!["python3".into(), test_server],
                    env:     HashMap::new(),
                },
                current_dir:          None,
                clear_env:            false,
                startup_timeout_secs: 10,
                tool_timeout_secs:    30,
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
        session.initialize().await.unwrap();

        // Verify McpServerReady event was emitted with deterministic tool
        // summaries pulled from the connection manager.
        let mut mcp_ready = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::McpServerReady {
                server_name, tools, ..
            } = &event.event
            {
                assert_eq!(server_name, "test-echo");
                assert_eq!(tools.len(), 1);
                assert_eq!(tools[0].name, "mcp__test_echo__echo");
                assert_eq!(tools[0].original_name, "echo");
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
        assert!(matches!(&turns[0], Message::User { .. }));
        assert!(
            matches!(&turns[1], Message::Assistant { tool_calls, .. } if tool_calls.len() == 1)
        );
        assert!(matches!(&turns[2], Message::ToolResults { results, .. } if results.len() == 1));
        assert!(
            matches!(&turns[3], Message::Assistant { content, .. } if content == "The echo server replied!")
        );

        // Verify the MCP tool result content — the echo server returns the message
        if let Message::ToolResults { results, .. } = &turns[2] {
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
                name:        "slow_tool".into(),
                description: "Waits until cancelled".into(),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(|_args, ctx| {
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
                Err(Error::Interrupted(InterruptReason::WallClockTimeout))
            ),
            "expected Interrupted(WallClockTimeout), got {result:?}"
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
        assert!(
            matches!(&turns[1], Message::Assistant { content, .. } if content == "Fast response")
        );
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
        session.initialize().await.unwrap();

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

    async fn build_initialized_session(
        sandbox: Arc<MockSandbox>,
        config: SessionOptions,
    ) -> Session {
        let provider = Arc::new(MockLlmProvider::new(vec![text_response("ok")]));
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::new());
        Session::new(client, profile, sandbox, config, None)
    }

    #[tokio::test]
    async fn initialize_emits_memory_loaded_with_file_metadata() {
        let mut files = std::collections::HashMap::new();
        files.insert("/home/test/AGENTS.md".into(), "Hello world".into());
        let sandbox = Arc::new(MockSandbox {
            files,
            ..MockSandbox::linux()
        });
        let config = SessionOptions {
            git_root: Some("/home/test".into()),
            skill_dirs: Some(Vec::new()),
            ..Default::default()
        };
        let mut session = build_initialized_session(sandbox, config).await;
        let mut rx = session.subscribe();
        session.initialize().await.unwrap();

        let mut memory_event = None;
        while let Ok(envelope) = rx.try_recv() {
            if let AgentEvent::MemoryLoaded {
                files,
                budget_bytes,
                provider_profile,
                ..
            } = envelope.event
            {
                memory_event = Some((files, budget_bytes, provider_profile));
                break;
            }
        }
        let (files, budget_bytes, provider_profile) =
            memory_event.expect("MemoryLoaded should be emitted");
        assert_eq!(provider_profile, "anthropic");
        assert_eq!(budget_bytes, 32768);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/home/test/AGENTS.md");
        assert_eq!(files[0].byte_count, "Hello world".len());
        assert_eq!(files[0].loaded_bytes, "Hello world".len());
        assert!(!files[0].truncated);
    }

    #[tokio::test]
    async fn initialize_emits_memory_loaded_event_with_empty_files_when_no_memory() {
        let sandbox = Arc::new(MockSandbox::linux());
        let config = SessionOptions {
            git_root: Some("/home/test".into()),
            skill_dirs: Some(Vec::new()),
            ..Default::default()
        };
        let mut session = build_initialized_session(sandbox, config).await;
        let mut rx = session.subscribe();
        session.initialize().await.unwrap();

        let mut saw_memory = false;
        while let Ok(envelope) = rx.try_recv() {
            if let AgentEvent::MemoryLoaded { files, .. } = envelope.event {
                assert!(files.is_empty());
                saw_memory = true;
                break;
            }
        }
        assert!(
            saw_memory,
            "MemoryLoaded must be emitted even when no memory files are loaded"
        );
    }

    #[tokio::test]
    async fn initialize_emits_skills_discovered_with_summaries() {
        let mut files = std::collections::HashMap::new();
        files.insert(
            "/skills/commit/SKILL.md".into(),
            "---\nname: commit\ndescription: Make a commit\n---\nDo commit".into(),
        );
        let sandbox = Arc::new(MockSandbox {
            files,
            glob_results: vec!["/skills/commit/SKILL.md".into()],
            ..MockSandbox::linux()
        });
        let config = SessionOptions {
            git_root: Some("/home/test".into()),
            skill_dirs: Some(vec!["/skills".into()]),
            ..Default::default()
        };
        let mut session = build_initialized_session(sandbox, config).await;
        let mut rx = session.subscribe();
        session.initialize().await.unwrap();

        let mut got = None;
        while let Ok(envelope) = rx.try_recv() {
            if let AgentEvent::SkillsDiscovered {
                provider_profile,
                source_dirs,
                skills,
            } = envelope.event
            {
                got = Some((provider_profile, source_dirs, skills));
                break;
            }
        }
        let (provider_profile, source_dirs, skills) =
            got.expect("SkillsDiscovered must be emitted");
        assert_eq!(provider_profile, "anthropic");
        assert_eq!(source_dirs, vec!["/skills".to_string()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "commit");
        assert_eq!(skills[0].description, "Make a commit");
    }

    #[tokio::test]
    async fn initialize_emits_skills_discovered_event_when_no_skills() {
        let sandbox = Arc::new(MockSandbox::linux());
        let config = SessionOptions {
            git_root: Some("/home/test".into()),
            skill_dirs: Some(Vec::new()),
            ..Default::default()
        };
        let mut session = build_initialized_session(sandbox, config).await;
        let mut rx = session.subscribe();
        session.initialize().await.unwrap();

        let mut saw_skills = false;
        while let Ok(envelope) = rx.try_recv() {
            if let AgentEvent::SkillsDiscovered { skills, .. } = envelope.event {
                assert!(skills.is_empty());
                saw_skills = true;
                break;
            }
        }
        assert!(
            saw_skills,
            "SkillsDiscovered must be emitted even when no skills are present"
        );
    }

    #[tokio::test]
    async fn slash_skill_expansion_emits_skill_activated_with_slash_source() {
        let mut files = std::collections::HashMap::new();
        files.insert(
            "/skills/commit/SKILL.md".into(),
            "---\nname: commit\ndescription: Make a commit\n---\nRun commit. {{user_input}}".into(),
        );
        let sandbox = Arc::new(MockSandbox {
            files,
            glob_results: vec!["/skills/commit/SKILL.md".into()],
            ..MockSandbox::linux()
        });
        let config = SessionOptions {
            git_root: Some("/home/test".into()),
            skill_dirs: Some(vec!["/skills".into()]),
            ..Default::default()
        };
        let provider = Arc::new(MockLlmProvider::new(vec![text_response("ok")]));
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::new());
        let mut session = Session::new(client, profile, sandbox, config, None);
        session.initialize().await.unwrap();

        let mut rx = session.subscribe();
        session.process_input("/commit fix things").await.unwrap();

        let mut activations: Vec<(String, SkillActivationSource)> = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            if let AgentEvent::SkillActivated { skill_name, source } = envelope.event {
                activations.push((skill_name, source));
            }
        }
        assert!(
            activations
                .iter()
                .any(|(name, source)| name == "commit" && *source == SkillActivationSource::Slash),
            "expected slash skill activation, got {activations:?}"
        );
    }

    #[tokio::test]
    async fn use_skill_tool_success_emits_skill_activated_with_tool_source() {
        let mut files = std::collections::HashMap::new();
        files.insert(
            "/skills/commit/SKILL.md".into(),
            "---\nname: commit\ndescription: Make a commit\n---\nRun commit.".into(),
        );
        let sandbox = Arc::new(MockSandbox {
            files,
            glob_results: vec!["/skills/commit/SKILL.md".into()],
            ..MockSandbox::linux()
        });
        let config = SessionOptions {
            git_root: Some("/home/test".into()),
            skill_dirs: Some(vec!["/skills".into()]),
            enable_loop_detection: false,
            ..Default::default()
        };
        let responses = vec![
            tool_call_response(
                "use_skill",
                "call_1",
                serde_json::json!({"skill_name": "commit"}),
            ),
            text_response("done"),
        ];
        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::new());
        let mut session = Session::new(client, profile, sandbox, config, None);
        session.initialize().await.unwrap();

        let mut rx = session.subscribe();
        session.process_input("please commit").await.unwrap();

        let mut tool_activations = 0;
        while let Ok(envelope) = rx.try_recv() {
            if let AgentEvent::SkillActivated { source, skill_name } = envelope.event {
                if source == SkillActivationSource::Tool && skill_name == "commit" {
                    tool_activations += 1;
                }
            }
        }
        assert_eq!(
            tool_activations, 1,
            "expected exactly one tool-sourced skill activation"
        );
    }

    #[tokio::test]
    async fn use_skill_tool_failed_lookup_does_not_emit_activation() {
        let sandbox = Arc::new(MockSandbox::linux());
        let config = SessionOptions {
            git_root: Some("/home/test".into()),
            skill_dirs: Some(Vec::new()),
            ..Default::default()
        };
        let provider = Arc::new(MockLlmProvider::new(vec![text_response("ok")]));
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::new());
        let mut session = Session::new(client, profile, sandbox, config, None);
        session.initialize().await.unwrap();

        // Build a use_skill tool with an empty skill list, then invoke it
        // directly with a missing name. We must NOT see a SkillActivated event.
        let skills_arc = Arc::new(Vec::<Skill>::new());
        let tool = make_use_skill_tool(skills_arc);
        let mut rx = session.subscribe();
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let ctx = ToolContext {
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: Some(session.id().to_string()),
            root_session_id: Some(session.id().to_string()),
            tool_call_id: None,
            agent_event_emitter: None,
        };
        let result = (tool.executor)(serde_json::json!({"skill_name": "nope"}), ctx).await;
        assert!(result.is_err());

        while let Ok(envelope) = rx.try_recv() {
            if matches!(envelope.event, AgentEvent::SkillActivated { .. }) {
                panic!("failed use_skill should not emit SkillActivated");
            }
        }
    }
}
