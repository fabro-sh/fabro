use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use fabro_auth::CredentialSource;
use fabro_llm::client::Client;
use fabro_llm::error::ProviderErrorKind;
use fabro_llm::generate::StreamAccumulator;
use fabro_llm::provider::StreamEventStream;
use fabro_llm::types::{
    ContentPart, Message as LlmMessage, ReasoningEffort, Request, RetryPolicy, StreamEvent,
    TokenCounts, ToolChoice,
};
use fabro_llm::{Error as LlmError, retry};
use fabro_mcp::config::{McpServerSettings, McpTransport};
use fabro_mcp::connection_manager::McpConnectionManager;
use fabro_mcp::http_transport;
use fabro_model::{AgentProfileKind, Catalog, ModelId, ModelRef, Speed, UsdMicros};
use fabro_types::{
    AgentToolSummary, LlmOutputKind, LlmRetryPhase, PermissionLevel, Principal, SessionMessage,
    SessionRecord, StageContextWindowProjection, SteeringMessage,
};
use fabro_util::shell;
use futures::StreamExt;
use tokio::sync::{Notify, broadcast};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent_profile::AgentProfile;
use crate::compaction::{check_context_usage, compact_context};
use crate::config::SessionOptions;
use crate::context_window::{
    ContextWindowInput, build_local_snapshot, context_window_from_response_usage,
};
use crate::error::{Error, InterruptReason};
use crate::event::Emitter;
use crate::file_tracker::FileTracker;
use crate::history::History;
use crate::loop_detection::detect_loop;
use crate::memory::{BUDGET_BYTES, MemoryDocument, discover_memory};
use crate::native_tool::NativeTool;
use crate::profiles::EnvContext;
use crate::question_tools::AgentToolRuntime;
use crate::sandbox::Sandbox;
use crate::skills::{
    ExpandedInput, Skill, default_skill_dirs, discover_skills, expand_skill,
    make_use_skill_tool_for_vocabulary,
};
use crate::subagent::{SubAgentCallbackEvent, SubAgentEventCallback, SubAgentSupervisor};
use crate::tool_execution::execute_tool_calls;
use crate::tool_permissions::canonical_tool_name;
use crate::tool_registry::ToolDefinitionWithSource;
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionInputTiming {
    pub inference: Duration,
    pub tool:      Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionShutdownReason {
    Completed,
    Cancelled,
    Error,
}

/// Take the value out of `start`, add its elapsed time to `total`. Used by
/// `run_single_input` to accumulate inference and tool spans at well-defined
/// boundaries (stream open, retry, error, cancel, end-of-loop).
fn record_elapsed(start: &mut Option<Instant>, total: &mut Duration) {
    if let Some(s) = start.take() {
        *total = total.saturating_add(s.elapsed());
    }
}

/// Classify a stream event as the first unit of provider output, or `None`
/// when it carries no output.
///
/// `StreamStart` is deliberately excluded because it proves only that the
/// provider responded, not what kind of output followed. The start/delta/end
/// events below identify the first observed content kind.
fn first_output_kind(event: &StreamEvent) -> Option<LlmOutputKind> {
    match event {
        StreamEvent::ReasoningStart | StreamEvent::ReasoningDelta { .. } => {
            Some(LlmOutputKind::Reasoning)
        }
        StreamEvent::TextStart { .. } | StreamEvent::TextDelta { .. } => Some(LlmOutputKind::Text),
        StreamEvent::ToolCallStart { .. }
        | StreamEvent::ToolCallDelta { .. }
        | StreamEvent::ToolCallEnd { .. } => Some(LlmOutputKind::ToolCall),
        _ => None,
    }
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
    queue: VecDeque<SteeringItem>,
    waiting_for_steer: bool,
    interrupt_generation: u64,
    settled_interrupt_generation: u64,
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
            control.interrupt_generation = control.interrupt_generation.saturating_add(1);
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
        let evicted = {
            let mut control = self.control.lock().expect("control state lock poisoned");
            let evicted = if control.queue.len() >= cap {
                control.queue.pop_front()
            } else {
                None
            };
            control.interrupt_generation = control.interrupt_generation.saturating_add(1);
            control.queue.push_back(item);
            control.waiting_for_steer = false;
            evicted
        };
        self.cancel_round();
        self.notify.notify_waiters();
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
            control.interrupt_generation = control.interrupt_generation.saturating_add(1);
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

struct BuiltRequest {
    request:        Request,
    context_window: StageContextWindowProjection,
}

/// Whether an input's `/name` tokens should be treated as skill references.
///
/// Only text the user actually typed can invoke a skill. Harness-synthesized
/// input carries whatever a child agent wrote, where `/tmp` is a path rather
/// than an invocation: expanding it would either fail the parent turn on an
/// unknown name or splice a skill template in place of the envelope.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SkillExpansion {
    Apply,
    Skip,
}

pub struct Session {
    id: String,
    /// Root agent session ID for this session's agent tree. A root session
    /// uses its own `id`; a subagent session inherits its parent's
    /// `root_session_id` so todo tools that scope by root (Anthropic tasks)
    /// share one list across all subagents.
    root_session_id: String,
    config: SessionOptions,
    history: History,
    event_emitter: Emitter,
    state: SessionState,
    ended: bool,
    llm_client: Client,
    provider_profile: Arc<dyn AgentProfile>,
    sandbox: Arc<dyn Sandbox>,
    control_state: Arc<Mutex<ControlState>>,
    control_notify: Arc<Notify>,
    followup_queue: Arc<Mutex<VecDeque<String>>>,
    cancel_token: CancellationToken,
    round_token: Arc<RwLock<CancellationToken>>,
    interrupt_reason: Arc<Mutex<Option<InterruptReason>>>,
    memory: Vec<MemoryDocument>,
    env_context: EnvContext,
    skills: Vec<Skill>,
    system_prompt: String,
    activated_skill_context_observed: bool,
    file_tracker: FileTracker,
    tool_env_provider: Option<Arc<dyn ToolEnvProvider>>,
    subagent_supervisor: Option<SubAgentSupervisor>,
    completion_coordinator: Option<Arc<dyn CompletionCoordinator>>,
    last_input_timing: SessionInputTiming,
    last_input_usage: TokenCounts,
    last_input_cost: Option<UsdMicros>,
}

impl Session {
    #[must_use]
    pub fn new(
        llm_client: Client,
        provider_profile: Arc<dyn AgentProfile>,
        sandbox: Arc<dyn Sandbox>,
        config: SessionOptions,
        subagent_supervisor: Option<SubAgentSupervisor>,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            root_session_id: id.clone(),
            id,
            config,
            history: History::default(),
            event_emitter: Emitter::new(),
            state: SessionState::Idle,
            ended: false,
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
            activated_skill_context_observed: false,
            file_tracker: FileTracker::default(),
            tool_env_provider: None,
            subagent_supervisor,
            completion_coordinator: None,
            last_input_timing: SessionInputTiming::default(),
            last_input_usage: TokenCounts::default(),
            last_input_cost: None,
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
        subagent_supervisor: Option<SubAgentSupervisor>,
    ) -> Result<Self, LlmError> {
        let client = Client::from_source(source, catalog).await?;
        Ok(Self::new(
            client,
            provider_profile,
            sandbox,
            config,
            subagent_supervisor,
        ))
    }

    pub fn from_record(
        record: &SessionRecord,
        runtime_context: &[SessionMessage],
        llm_client: Client,
        provider_profile: Arc<dyn AgentProfile>,
        sandbox: Arc<dyn Sandbox>,
        config: SessionOptions,
        subagent_supervisor: Option<SubAgentSupervisor>,
    ) -> Result<Self, Error> {
        let mut session = Self::new(
            llm_client,
            provider_profile,
            sandbox,
            config,
            subagent_supervisor,
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

    #[must_use]
    pub fn permission_level(&self) -> Option<PermissionLevel> {
        self.config.permission_level
    }

    /// Effective tool list the model is exposed to after provider-profile
    /// setup, optional registrations, MCP integration, and access-policy
    /// filtering. This is the same path used to build outbound requests.
    #[must_use]
    pub fn effective_tools(&self) -> Vec<ToolDefinitionWithSource> {
        self.provider_profile
            .tool_registry()
            .definitions_with_source_for_policy(
                self.config.tool_access_policy.as_deref(),
                self.config.tool_exposure_mode,
            )
    }

    /// Public projection of `effective_tools()` for
    /// `StageProjection.agent_tools` and the `agent.tools.available` event.
    /// Sorted by name for deterministic snapshots; the underlying registry
    /// stores tools in a `HashMap`.
    #[must_use]
    pub fn agent_tool_summaries(&self) -> Vec<AgentToolSummary> {
        let mut summaries: Vec<_> = self
            .effective_tools()
            .iter()
            .map(ToolDefinitionWithSource::to_agent_tool_summary)
            .collect();
        summaries.sort_by(|left, right| left.name.cmp(&right.name));
        summaries
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
            default_skill_dirs(Some(&skills_str), Some(&doc_root))
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
                let vocabulary = profile.tool_registry().vocabulary();
                profile
                    .tool_registry_mut()
                    .register(make_use_skill_tool_for_vocabulary(skills_arc, vocabulary));
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
                McpTransport::Sandbox {
                    protocol,
                    command,
                    port,
                    env,
                } => {
                    let port = *port;
                    match self
                        .start_sandbox_mcp_server(command, port, env, cancel_token)
                        .await?
                    {
                        Ok((url, headers)) => {
                            let url = http_transport::sandbox_mcp_http_url(*protocol, &url)
                                .map_err(|err| Error::InvalidState(err.to_string()))?;
                            info!(
                                server = %config.name,
                                url = %url,
                                "Sandbox MCP server started, connecting via HTTP"
                            );
                            resolved.push(McpServerSettings {
                                name:                 config.name.clone(),
                                transport:            McpTransport::Http {
                                    protocol: *protocol,
                                    url,
                                    headers,
                                },
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

        let launch_script = sandbox_mcp_launch_script(command);
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

    /// True when the shared run budget is exhausted. On the first
    /// observation, marks the session interrupted with `BudgetExhausted` and
    /// cancels its token so in-flight work stops too.
    fn budget_exceeded(&self) -> bool {
        let Some(budget) = &self.config.run_budget else {
            return false;
        };
        if budget.exceeded().is_none() {
            return false;
        }
        self.set_interrupt_reason(InterruptReason::BudgetExhausted);
        self.cancel_token.cancel();
        true
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

    /// Transition the in-memory session state machine.
    ///
    /// Valid transitions (matches the Attractor spec):
    /// - Idle → Thinking
    /// - Thinking → Executing
    /// - Thinking → Idle  (emits ProcessingEnd)
    /// - Executing → Thinking
    /// - Thinking → Closed
    /// - Executing → Closed
    /// - Idle → Closed
    /// - any → Closed (interrupt/error)
    ///
    /// Async resource cleanup and `SessionEnded` emission belong to
    /// [`Self::shutdown`], never to this synchronous transition helper.
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

        if matches!(from, SessionState::Thinking | SessionState::Executing)
            && to == SessionState::Idle
        {
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::ProcessingEnd);
        }

        self.state = to;
    }

    /// Close the session and resolve all owned child tasks before emitting
    /// `SessionEnded`. Returns `true` only for the call that performs shutdown.
    pub async fn shutdown(&mut self, reason: SessionShutdownReason) -> bool {
        if self.ended {
            return false;
        }
        if reason == SessionShutdownReason::Cancelled {
            self.set_interrupt_reason(InterruptReason::Cancelled);
            self.cancel_token.cancel();
        }
        self.transition(SessionState::Closed);
        if let Some(supervisor) = &self.subagent_supervisor {
            supervisor.shutdown_all().await;
        }
        self.ended = true;
        self.event_emitter
            .emit(self.id.clone(), AgentEvent::SessionEnded);
        true
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffort>) {
        self.config.reasoning_effort = effort;
    }

    pub fn set_speed(&mut self, speed: Option<Speed>) {
        self.config.speed = speed;
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
        self.process_input_with_output(input).await.map(drop)
    }

    pub(crate) async fn process_input_with_output(
        &mut self,
        input: &str,
    ) -> Result<Option<String>, Error> {
        self.process_input_with_runtime_and_output(input, AgentToolRuntime::default())
            .await
    }

    #[must_use]
    pub const fn last_input_timing(&self) -> SessionInputTiming {
        self.last_input_timing
    }

    #[must_use]
    pub fn last_input_usage(&self) -> TokenCounts {
        self.last_input_usage.clone()
    }

    #[must_use]
    pub const fn last_input_cost(&self) -> Option<UsdMicros> {
        self.last_input_cost
    }

    /// Process an input. The inference/tool timing accumulated during the call
    /// is available via [`Self::last_input_timing`] after this returns, even on
    /// error.
    pub async fn process_input_with_runtime(
        &mut self,
        input: &str,
        agent_tool_runtime: AgentToolRuntime,
    ) -> Result<(), Error> {
        self.process_input_with_runtime_and_output(input, agent_tool_runtime)
            .await
            .map(drop)
    }

    async fn process_input_with_runtime_and_output(
        &mut self,
        input: &str,
        agent_tool_runtime: AgentToolRuntime,
    ) -> Result<Option<String>, Error> {
        let mut timing = SessionInputTiming::default();
        let mut usage = TokenCounts::default();
        let mut cost = None;
        self.last_input_timing = timing;
        self.last_input_usage = TokenCounts::default();
        self.last_input_cost = None;
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

        // Process the initial input, then drain followups. Claude-compatible
        // background-agent results join this same boundary queue: they never
        // interrupt inference or a tool call, and all results already ready at
        // a boundary are delivered in one additional parent turn.
        let mut result = self
            .run_single_input(
                input,
                SkillExpansion::Apply,
                &agent_tool_runtime,
                &mut timing,
                &mut usage,
                &mut cost,
            )
            .await;

        if result.is_ok() {
            loop {
                let followup = self
                    .followup_queue
                    .lock()
                    .expect("followup queue lock poisoned")
                    .pop_front();
                let next_input = if let Some(followup) = followup {
                    Some((followup, SkillExpansion::Apply))
                } else if let Some(supervisor) = self.subagent_supervisor.clone() {
                    match supervisor
                        .next_parent_notification_turn(&self.cancel_token)
                        .await
                    {
                        Ok(Some(turn)) => Some((turn, SkillExpansion::Skip)),
                        Ok(None) => None,
                        Err(Error::Interrupted(InterruptReason::Cancelled)) => {
                            result = Err(self.interrupted_error());
                            None
                        }
                        Err(error) => {
                            result = Err(error);
                            None
                        }
                    }
                } else {
                    None
                };
                let Some((next_input, skill_expansion)) = next_input else {
                    break;
                };
                result = self
                    .run_single_input(
                        &next_input,
                        skill_expansion,
                        &agent_tool_runtime,
                        &mut timing,
                        &mut usage,
                        &mut cost,
                    )
                    .await;
                if result.is_err() {
                    break;
                }
            }
        }

        // Stop the timer so it doesn't fire after we're done.
        if let Some(handle) = timer_handle {
            handle.abort();
        }

        if self.state == SessionState::Closed {
            let reason = if self.cancel_token.is_cancelled() {
                SessionShutdownReason::Cancelled
            } else {
                SessionShutdownReason::Error
            };
            self.shutdown(reason).await;
        } else {
            self.transition(SessionState::Idle);
        }

        self.last_input_timing = timing;
        self.last_input_usage = usage;
        self.last_input_cost = cost;
        result
    }

    async fn run_single_input(
        &mut self,
        input: &str,
        skill_expansion: SkillExpansion,
        agent_tool_runtime: &AgentToolRuntime,
        timing: &mut SessionInputTiming,
        usage_accumulator: &mut TokenCounts,
        cost_accumulator: &mut Option<UsdMicros>,
    ) -> Result<Option<String>, Error> {
        const STREAM_CONSUME_RETRIES: usize = 3;

        if self.state == SessionState::Closed {
            return Err(Error::SessionClosed);
        }

        self.transition(SessionState::Thinking);

        // Expand skill references in input
        let expanded = if self.skills.is_empty() || skill_expansion == SkillExpansion::Skip {
            ExpandedInput {
                text:       input.to_string(),
                skill_name: None,
            }
        } else {
            expand_skill(&self.skills, input).map_err(Error::InvalidState)?
        };
        if let Some(ref name) = expanded.skill_name {
            self.activated_skill_context_observed = true;
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

        // A failed summarization is unlikely to improve within the same agent
        // turn. Suppress further attempts until the next user/follow-up input
        // so a provider returning empty responses cannot create a paid retry
        // loop at both compaction checkpoints.
        let mut compaction_failed = false;

        loop {
            // Top-of-loop: if the previous round's interrupt token fired,
            // swap in a fresh one before draining and rebuilding state.
            // (Terminal cancel via `cancel_token` is handled by the explicit
            // check below and by `interrupted_error()`.)
            let round_was_interrupted = {
                let needs_refresh = self
                    .round_token
                    .read()
                    .expect("round token lock poisoned")
                    .is_cancelled();
                if needs_refresh {
                    *self.round_token.write().expect("round token lock poisoned") =
                        CancellationToken::new();
                }
                needs_refresh
            };

            // Terminal cancellation wins even when a control interrupt has
            // parked the session waiting for steering.
            if self.cancel_token.is_cancelled() {
                self.shutdown(SessionShutdownReason::Cancelled).await;
                return Err(self.interrupted_error());
            }

            // A budget exhausted outside this session (a parallel stage, a
            // prior input, seeded resume spend) stops it before the next
            // request.
            if self.budget_exceeded() {
                self.shutdown(SessionShutdownReason::Cancelled).await;
                return Err(self.interrupted_error());
            }

            if round_was_interrupted {
                let generations = {
                    let mut control = self
                        .control_state
                        .lock()
                        .expect("control state lock poisoned");
                    let first = control.settled_interrupt_generation.saturating_add(1);
                    let last = control.interrupt_generation;
                    control.settled_interrupt_generation = last;
                    if first <= last {
                        (first..=last).collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    }
                };
                for generation in generations {
                    self.event_emitter
                        .emit(self.id.clone(), AgentEvent::RoundInterrupted { generation });
                }
            }

            // Drain pending steering messages at the top of every iteration
            // so steering pushed mid-round is delivered as the first turn of
            // the next round. A pure interrupt with no queued steer parks the
            // session here until a later steer arrives.
            self.drain_steering();
            self.wait_for_steer_if_needed().await?;
            self.drain_steering();

            // Snapshot the per-round token; it stays stable for this iteration.
            let round_token = self
                .round_token
                .read()
                .expect("round token lock poisoned")
                .clone();

            // Pre-turn compaction: trim context before building the request
            if !compaction_failed {
                compaction_failed = self.compact_if_needed().await;
            }

            // Keep generated directives local to the round until its assistant
            // response commits. An interrupted round must not leave a system
            // message behind for later steering to follow.
            let pending_task_reminder = self.task_reminder_if_needed();

            // Build request
            let built_request = self.build_request(pending_task_reminder.as_ref());
            let local_context_window = built_request.context_window.clone();
            let request = built_request.request;

            let requested_model = ModelRef {
                provider: self.provider_profile.provider_id(),
                model_id: ModelId::new(self.provider_profile.model()),
                speed:    self.config.speed,
            };

            // Open the inference bracket for this round. The request is built
            // and compaction has run, so this is the last point before the
            // provider is contacted at which we still know nothing about the
            // response.
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::LlmRequestStarted {
                    requested_model: requested_model.clone(),
                });

            // Call LLM (streaming) with retry for transient errors
            let retry_emitter = self.event_emitter.clone();
            let retry_session_id = self.id.clone();
            let retry_provider = requested_model.provider.to_string();
            let retry_model = requested_model.model_id.to_string();
            let retry_policy = RetryPolicy {
                max_retries: 3,
                on_retry: Some(std::sync::Arc::new(move |err, attempt, delay| {
                    retry_emitter.emit(retry_session_id.clone(), AgentEvent::LlmRetry {
                        provider:   retry_provider.clone(),
                        model:      retry_model.clone(),
                        attempt:    attempt as usize,
                        delay_secs: delay.as_secs_f64(),
                        error:      err.clone(),
                        phase:      LlmRetryPhase::Open,
                    });
                })),
                ..Default::default()
            };
            let client = self.llm_client.clone();
            let cancel_token_for_select = self.cancel_token.clone();
            let mut inference_start = Some(Instant::now());
            let stream_outcome: Option<Result<StreamEventStream, Error>> = tokio::select! {
                biased;
                () = round_token.cancelled() => None,
                () = cancel_token_for_select.cancelled() => None,
                stream = self.open_stream_with_retry(&client, &request, &retry_policy) => Some(stream),
            };
            let mut event_stream = if let Some(stream) = stream_outcome {
                match stream {
                    Ok(stream) => stream,
                    Err(err) => {
                        record_elapsed(&mut inference_start, &mut timing.inference);
                        return Err(err);
                    }
                }
            } else {
                record_elapsed(&mut inference_start, &mut timing.inference);
                if self.cancel_token.is_cancelled() {
                    self.shutdown(SessionShutdownReason::Cancelled).await;
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
                // Re-armed per attempt: a replayed turn discards everything
                // the previous attempt produced, so its first output is a new
                // observation rather than a continuation.
                let mut first_output_emitted = false;

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
                            if !first_output_emitted {
                                if let Some(kind) = first_output_kind(&event) {
                                    first_output_emitted = true;
                                    self.event_emitter
                                        .emit(self.id.clone(), AgentEvent::LlmFirstOutput { kind });
                                }
                            }
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
                    record_elapsed(&mut inference_start, &mut timing.inference);
                    self.shutdown(SessionShutdownReason::Cancelled).await;
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
                        // Emitted directly rather than through
                        // `retry_policy.on_retry` so the event can name the
                        // consume loop as the source of `attempt`; the policy
                        // callback only ever runs for stream-open failures.
                        self.event_emitter
                            .emit(self.id.clone(), AgentEvent::LlmRetry {
                                provider:   requested_model.provider.to_string(),
                                model:      requested_model.model_id.to_string(),
                                attempt:    stream_attempt,
                                delay_secs: delay.as_secs_f64(),
                                error:      err,
                                phase:      LlmRetryPhase::Consume,
                            });

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
                            match stream {
                                Ok(stream) => stream,
                                Err(err) => {
                                    record_elapsed(&mut inference_start, &mut timing.inference);
                                    return Err(err);
                                }
                            }
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
                    record_elapsed(&mut inference_start, &mut timing.inference);
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
                    // The only mid-turn restart that reaches no error handler:
                    // without this the round replays and discards its output
                    // with nothing on the durable stream to show for it.
                    self.event_emitter
                        .emit(self.id.clone(), AgentEvent::LlmRetry {
                            provider:   requested_model.provider.to_string(),
                            model:      requested_model.model_id.to_string(),
                            attempt:    stream_attempt,
                            delay_secs: 0.0,
                            error:      LlmError::Stream {
                                message: "Stream ended without a finish event".to_string(),
                                source:  None,
                            },
                            phase:      LlmRetryPhase::Consume,
                        });
                    let cancel_token_for_select = self.cancel_token.clone();
                    let retry_outcome: Option<Result<StreamEventStream, Error>> = tokio::select! {
                        biased;
                        () = round_token.cancelled() => None,
                        () = cancel_token_for_select.cancelled() => None,
                        stream = self.open_stream_with_retry(&client, &request, &retry_policy) => Some(stream),
                    };
                    event_stream = if let Some(stream) = retry_outcome {
                        match stream {
                            Ok(stream) => stream,
                            Err(err) => {
                                record_elapsed(&mut inference_start, &mut timing.inference);
                                return Err(err);
                            }
                        }
                    } else {
                        steer_interrupted =
                            round_token.is_cancelled() && !self.cancel_token.is_cancelled();
                        break 'streamattempts;
                    };
                }
            }
            record_elapsed(&mut inference_start, &mut timing.inference);

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
            // Normalize before the response's content moves into history.
            let reasoning = response.reasoning_output();
            let provider_parts: Vec<_> = response
                .message
                .content
                .iter()
                .filter(|p| matches!(p, ContentPart::Other { .. } | ContentPart::Thinking(_)))
                .cloned()
                .collect();
            let usage = response.usage.clone();
            let context_window = Some(context_window_from_response_usage(
                &local_context_window,
                &usage,
            ));
            *usage_accumulator += usage.clone();
            UsdMicros::accumulate(cost_accumulator, response.cost_usd.map(UsdMicros::from_usd));
            if let Some(budget) = &self.config.run_budget {
                let response_cost = response.cost_usd.map(UsdMicros::from_usd);
                if budget.charge(usage.total_tokens(), response_cost).is_some() {
                    // Halt like the wall-clock timeout: the cancelled token
                    // stops tool execution and the next turn, and the reason
                    // makes `interrupted_error()` report the budget.
                    self.set_interrupt_reason(InterruptReason::BudgetExhausted);
                    self.cancel_token.cancel();
                }
            }

            if let Some(reminder) = pending_task_reminder {
                self.history.push(reminder);
            }
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
                    self.provider_profile.model().into()
                } else {
                    response.model.clone().into()
                },
                speed:    self.config.speed,
            };
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::AssistantMessage {
                    text: text.clone(),
                    model,
                    usage: response.usage.clone(),
                    cost_usd: response.cost_usd,
                    cost_source: response.cost_source,
                    tool_call_count: tool_calls.len(),
                    context_window,
                    reasoning,
                });

            // Post-response compaction: trim context after appending assistant turn
            if !compaction_failed {
                compaction_failed = self.compact_if_needed().await;
            }

            // If no tool calls, natural completion. Consult the optional
            // completion coordinator: it can return `true` to force one more
            // iteration when a steer arrived during the final response.
            if tool_calls.is_empty() {
                if round_token.is_cancelled() {
                    continue;
                }
                let should_continue = self
                    .completion_coordinator
                    .as_ref()
                    .is_some_and(|c| c.on_natural_completion());
                if should_continue {
                    continue;
                }
                return Ok((!text.trim().is_empty()).then_some(text));
            }

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
            let tool_start = Instant::now();
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
            timing.tool = timing.tool.saturating_add(tool_start.elapsed());
            composite_watcher.abort();
            if tool_calls.iter().zip(&results).any(|(tool_call, result)| {
                !result.is_error
                    && canonical_tool_name(&tool_call.name) == NativeTool::UseSkill.canonical_name()
            }) {
                self.activated_skill_context_observed = true;
            }

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
                self.shutdown(SessionShutdownReason::Cancelled).await;
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
    }

    /// Attempt context compaction when the configured threshold is exceeded.
    ///
    /// Returns `true` when an attempted compaction failed so the current input
    /// loop can suppress repeated paid summary calls. The next input starts
    /// with a fresh retry opportunity.
    async fn compact_if_needed(&mut self) -> bool {
        let Some(estimate) = check_context_usage(
            &self.system_prompt,
            &self.history,
            self.provider_profile.as_ref(),
            self.config.compaction_threshold_percent,
            &self.event_emitter,
            &self.id,
        ) else {
            return false;
        };
        if !self.config.enable_context_compaction {
            return false;
        }
        if let Err(error) = compact_context(
            &mut self.history,
            &self.llm_client,
            self.provider_profile.as_ref(),
            &self.file_tracker,
            self.config.compaction_preserve_turns,
            estimate,
            &self.event_emitter,
            &self.id,
            self.config.run_budget.as_deref(),
        )
        .await
        {
            self.event_emitter
                .emit(self.id.clone(), AgentEvent::Error { error });
            return true;
        }
        false
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
                    self.shutdown(SessionShutdownReason::Cancelled).await;
                    return Err(self.interrupted_error());
                }
                () = notified => {}
            }
        }
    }

    fn build_request(&self, pending_task_reminder: Option<&Message>) -> BuiltRequest {
        let mut messages = Vec::new();
        if !self.system_prompt.trim().is_empty() {
            messages.push(LlmMessage::system(self.system_prompt.clone()));
        }
        messages.extend(self.history.convert_to_messages());
        if let Some(reminder) = pending_task_reminder {
            messages.push(reminder.to_llm_message());
        }

        let tools_with_source = self.effective_tools();
        let tools: Vec<_> = tools_with_source
            .iter()
            .map(|tool| tool.definition.clone())
            .collect();
        let has_tools = !tools.is_empty();

        let request = Request {
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
        };
        let provider = self.provider_profile.provider_id().to_string();
        let model = self.provider_profile.model().to_string();
        let context_window = build_local_snapshot(ContextWindowInput {
            request: &request,
            tools: &tools_with_source,
            system_prompt: &self.system_prompt,
            memory: &self.memory,
            skills: &self.skills,
            tool_vocabulary: self.provider_profile.tool_registry().vocabulary(),
            activated_skill_context_observed: self.activated_skill_context_observed,
            provider: &provider,
            model: &model,
            context_window_tokens: self.provider_profile.context_window_size(),
        });
        BuiltRequest {
            request,
            context_window,
        }
    }

    fn task_reminder_if_needed(&self) -> Option<Message> {
        let tools = self.effective_tools();
        let tool_names: Vec<&str> = tools
            .iter()
            .map(|tool| tool.definition.name.as_str())
            .collect();
        task_reminder::maybe_reminder(&self.history, &tool_names).map(|content| Message::System {
            content,
            timestamp: SystemTime::now(),
        })
    }
}

const fn is_auth_error(err: &LlmError) -> bool {
    matches!(
        err.provider_kind(),
        Some(ProviderErrorKind::Authentication | ProviderErrorKind::AccessDenied)
    )
}

/// Build the script that launches a sandbox MCP server detached and echoes its
/// PID.
///
/// `setsid` fully detaches the server so Daytona's exec doesn't block on it.
/// The inner command is shell-quoted for the wrapper so a single quote or
/// metacharacter in any argv element can't break out, and the wrapper itself is
/// the current `$BASH` because the sandbox evaluates this string as non-login
/// Bash and may resolve that executable outside `/bin` (for example on NixOS).
fn sandbox_mcp_launch_script(command: &[String]) -> String {
    let command_source = match command {
        // Sandbox MCP `script` entries resolve to this exact argv shape. The
        // surrounding launcher is already the provider-selected Bash, so
        // evaluate the source in that process instead of PATH-resolving a
        // second interpreter. Grouping keeps the log redirections scoped to
        // the whole script, including multi-command and trailing-comment
        // forms.
        [interpreter, flag, source] if interpreter == "bash" && flag == "-c" => {
            format!("{{\n{source}\n}}")
        }
        _ => shell::shell_join(command),
    };
    let inner =
        format!("{command_source} > /tmp/mcp_server_stdout.log 2>/tmp/mcp_server_stderr.log");
    format!(
        "setsid \"$BASH\" -c {quoted} </dev/null >/dev/null 2>&1 &\necho $!",
        quoted = shell::shell_quote(&inner)
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
    use fabro_types::{ReasoningOutput, StageContextWindowCountMethod};
    use futures::stream;
    use tokio::time::{sleep, timeout};

    use super::*;
    use crate::config::{ToolAccess, ToolAccessPolicy, ToolApprovalAdapter, ToolExposureMode};
    use crate::error::CompactionError;
    use crate::skills::{Skill, make_use_skill_tool};
    use crate::subagent::{SubAgentStatus, make_wait_tool};
    use crate::test_support::*;
    use crate::tool_registry::{RegisteredTool, ToolContext, ToolRegistry, ToolSource};

    #[test]
    fn sandbox_mcp_launch_wrapper_uses_bash() {
        // The sandbox evaluates this string as non-login Bash, so the detached
        // wrapper reuses the executable selected by the provider.
        let script = sandbox_mcp_launch_script(&[
            "npx".to_string(),
            "@playwright/mcp@latest".to_string(),
            "--port".to_string(),
            "3100".to_string(),
        ]);

        assert!(
            script.starts_with("setsid \"$BASH\" -c "),
            "launch wrapper should detach through the provider-selected Bash: {script}"
        );
        assert!(
            script.ends_with(" </dev/null >/dev/null 2>&1 &\necho $!"),
            "launch wrapper should stay detached and report its PID: {script}"
        );
        assert!(
            script.contains("/tmp/mcp_server_stdout.log")
                && script.contains("2>/tmp/mcp_server_stderr.log"),
            "launch wrapper should keep its log redirection: {script}"
        );
    }

    #[test]
    fn sandbox_mcp_launch_wrapper_evaluates_scripts_in_the_selected_bash() {
        let source =
            "PATH=/mcp-only\nprintf 'starting server\\n'\nexec my-server --port 3100 # ready";
        let script =
            sandbox_mcp_launch_script(&["bash".to_string(), "-c".to_string(), source.to_string()]);

        let wrapper_argument = script
            .strip_prefix("setsid \"$BASH\" -c ")
            .and_then(|rest| rest.strip_suffix(" </dev/null >/dev/null 2>&1 &\necho $!"))
            .expect("launch wrapper should have the canonical shape");
        let unwrapped = shlex::split(wrapper_argument).expect("wrapper argument should parse");

        assert_eq!(unwrapped, vec![format!(
            "{{\n{source}\n}} > /tmp/mcp_server_stdout.log 2>/tmp/mcp_server_stderr.log"
        )]);
        assert!(
            !unwrapped[0].contains("bash -c"),
            "script entries must not PATH-resolve a nested Bash: {}",
            unwrapped[0]
        );
    }

    #[test]
    fn sandbox_mcp_launch_wrapper_quotes_arbitrary_argv() {
        // A quote or metacharacter in any argv element must not break out of
        // the wrapper; it has to arrive as one argument.
        let script = sandbox_mcp_launch_script(&[
            "my-server".to_string(),
            "--flag=it's a value".to_string(),
            "$(touch /tmp/pwned)".to_string(),
        ]);

        let wrapper_argument = script
            .strip_prefix("setsid \"$BASH\" -c ")
            .and_then(|rest| rest.strip_suffix(" </dev/null >/dev/null 2>&1 &\necho $!"))
            .expect("launch wrapper should have the canonical shape");

        // Unwrap the wrapper's own quoting: the whole inner script must arrive
        // as one argument to `bash -c`, with each argv element still quoted so
        // the substitution stays inert.
        let unwrapped = shlex::split(wrapper_argument).expect("wrapper argument should parse");
        assert_eq!(
            unwrapped.len(),
            1,
            "the command must stay a single argument"
        );
        assert_eq!(
            unwrapped[0],
            "my-server \"--flag=it's a value\" '$(touch /tmp/pwned)' > \
             /tmp/mcp_server_stdout.log 2>/tmp/mcp_server_stderr.log"
        );
    }

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
            source:     ToolSource::Native,
        }
    }

    #[derive(Clone)]
    enum ScriptedStreamCall {
        Response(Box<Response>),
        Events(Vec<Result<StreamEvent, LlmError>>),
        /// Emit the events, then hang until the round is cancelled.
        EventsThenPending(Vec<Result<StreamEvent, LlmError>>),
        Error(LlmError),
    }

    struct ScriptedStreamProvider {
        calls:      Vec<ScriptedStreamCall>,
        requests:   Mutex<Vec<Request>>,
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
                requests: Mutex::new(Vec::new()),
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

        async fn stream(&self, request: &Request) -> Result<StreamEventStream, LlmError> {
            self.requests
                .lock()
                .expect("request capture lock poisoned")
                .push(request.clone());
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
                ScriptedStreamCall::EventsThenPending(events) => {
                    Ok(Box::pin(stream::iter(events).chain(stream::pending())))
                }
                ScriptedStreamCall::Error(err) => Err(err),
            }
        }
    }

    struct DelayedStreamProvider {
        responses:  Vec<Response>,
        delay:      Duration,
        call_index: AtomicUsize,
    }

    impl DelayedStreamProvider {
        fn new(responses: Vec<Response>, delay: Duration) -> Self {
            Self {
                responses,
                delay,
                call_index: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for DelayedStreamProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, LlmError> {
            Err(LlmError::Configuration {
                message: "DelayedStreamProvider does not implement complete()".into(),
                source:  None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
            sleep(self.delay).await;
            let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
            let response = if idx < self.responses.len() {
                self.responses[idx].clone()
            } else {
                self.responses[self.responses.len() - 1].clone()
            };
            Ok(response_to_stream(response))
        }
    }

    struct BlockingFirstStreamProvider {
        first_started: Arc<Notify>,
        response:      Response,
        call_index:    AtomicUsize,
    }

    impl BlockingFirstStreamProvider {
        fn new(response: Response) -> Self {
            Self {
                first_started: Arc::new(Notify::new()),
                response,
                call_index: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for BlockingFirstStreamProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, LlmError> {
            Err(LlmError::Configuration {
                message: "BlockingFirstStreamProvider does not implement complete()".into(),
                source:  None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
            if self.call_index.fetch_add(1, Ordering::SeqCst) == 0 {
                self.first_started.notify_one();
                return std::future::pending().await;
            }
            Ok(response_to_stream(self.response.clone()))
        }
    }

    async fn make_session_with_provider(provider: Arc<dyn ProviderAdapter>) -> Session {
        make_session_with_provider_and_manager(provider, None).await
    }

    async fn make_session_with_provider_and_manager(
        provider: Arc<dyn ProviderAdapter>,
        subagent_supervisor: Option<SubAgentSupervisor>,
    ) -> Session {
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockSandbox::default());
        Session::new(
            client,
            profile,
            env,
            SessionOptions::default(),
            subagent_supervisor,
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
        let output = session.process_input_with_output("Hi").await.unwrap();

        assert_eq!(output.as_deref(), Some("Hello there!"));
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

    #[tokio::test]
    async fn last_input_cost_sums_each_response_in_a_multi_turn_input() {
        let mut registry = ToolRegistry::new();
        registry.register(make_echo_tool());

        let responses = vec![
            response_with_cost(
                tool_call_response("echo", "call_1", serde_json::json!({"text": "hello"})),
                0.04,
            ),
            response_with_cost(text_response("Done!"), 0.06),
        ];

        let mut session = make_session_with_tools(responses, registry).await;
        session.process_input("Use echo tool").await.unwrap();

        assert_eq!(session.last_input_cost(), Some(UsdMicros(100_000)));
    }

    #[tokio::test]
    async fn last_input_timing_reports_inference_and_tool_per_call() {
        let mut registry = ToolRegistry::new();
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name:        "slow_tool".into(),
                description: "Sleeps before returning".into(),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(|_args, _ctx| {
                Box::pin(async move {
                    sleep(Duration::from_millis(30)).await;
                    Ok("slept".to_string())
                })
            }),
            source:     ToolSource::Native,
        });
        let provider = Arc::new(DelayedStreamProvider::new(
            vec![
                tool_call_response("slow_tool", "call_1", serde_json::json!({})),
                text_response("Done!"),
                text_response("Second response"),
            ],
            Duration::from_millis(20),
        ));
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::with_tools(registry));
        let env = Arc::new(MockSandbox::default());
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);

        let result = session
            .process_input_with_runtime("use the slow tool", AgentToolRuntime::default())
            .await;
        result.unwrap();
        let first = session.last_input_timing();
        assert!(
            first.inference >= Duration::from_millis(35),
            "expected non-zero inference timing for first input, got {first:?}"
        );
        assert!(
            first.tool >= Duration::from_millis(20),
            "expected non-zero tool timing for first input, got {first:?}"
        );

        let result = session
            .process_input_with_runtime("no tools this time", AgentToolRuntime::default())
            .await;
        result.unwrap();
        let second = session.last_input_timing();
        assert!(
            second.inference >= Duration::from_millis(15),
            "expected per-input inference timing for second input, got {second:?}"
        );
        assert_eq!(second.tool, Duration::ZERO);
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
            source:     ToolSource::Native,
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
    async fn empty_natural_completion_has_no_output() {
        let mut session = make_session(vec![text_response("  ")]).await;

        let output = session.process_input_with_output("Hi").await.unwrap();

        assert_eq!(output, None);
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
        let mut events = session.subscribe();
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
        let generations = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event.event {
                AgentEvent::RoundInterrupted { generation } => Some(generation),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(generations, vec![1]);
    }

    #[tokio::test]
    async fn interrupt_then_steer_injects_steering_text() {
        let mut session = make_session(vec![text_response("OK")]).await;
        let mut rx = session.subscribe();

        let handle = session.control_handle();
        handle.interrupt_then_steer("stop now".to_string(), None);
        session.process_input("start").await.unwrap();

        let events = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|event| event.event)
            .collect::<Vec<_>>();
        let settled = events
            .iter()
            .position(|event| matches!(event, AgentEvent::RoundInterrupted { generation: 1 }))
            .unwrap();
        let steered = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentEvent::SteeringInjected { text, .. } if text == "stop now"
                )
            })
            .unwrap();
        assert!(settled < steered);
        assert!(!handle.is_waiting_for_steer());
    }

    #[tokio::test]
    async fn interrupt_during_inference_settles_once_before_steering_resumes() {
        let provider = Arc::new(BlockingFirstStreamProvider::new(text_response("resumed")));
        let first_started = Arc::clone(&provider.first_started);
        let mut session = make_session_with_provider(provider.clone()).await;
        let control = session.control_handle();
        let mut controller_events = session.subscribe();
        let mut recorded_events = session.subscribe();
        let control_for_controller = control.clone();
        let controller = tokio::spawn(async move {
            first_started.notified().await;
            control_for_controller.interrupt(None);
            wait_for_agent_event(&mut controller_events, |event| {
                matches!(event, AgentEvent::RoundInterrupted { generation: 1 })
            })
            .await;
            assert!(control_for_controller.is_waiting_for_steer());
            control_for_controller.steer("resume inference".into(), None);
        });

        timeout(Duration::from_secs(1), session.process_input("start"))
            .await
            .expect("inference interrupt should settle and resume")
            .unwrap();
        controller.await.unwrap();

        let events = std::iter::from_fn(|| recorded_events.try_recv().ok())
            .map(|event| event.event)
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::RoundInterrupted { .. }))
                .count(),
            1
        );
        let settled = events
            .iter()
            .position(|event| matches!(event, AgentEvent::RoundInterrupted { .. }))
            .unwrap();
        let steered = events
            .iter()
            .position(|event| matches!(event, AgentEvent::SteeringInjected { .. }))
            .unwrap();
        assert!(settled < steered);
        assert_eq!(provider.call_index.load(Ordering::SeqCst), 2);
        assert!(!control.is_waiting_for_steer());
    }

    #[tokio::test]
    async fn interrupted_round_does_not_commit_task_reminder() {
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::EventsThenPending(vec![Ok(StreamEvent::ToolCallStart {
                tool_call: ToolCall::new(
                    "call_1",
                    "TaskUpdate",
                    serde_json::json!({"taskId": "1", "status": "completed"}),
                ),
            })]),
            ScriptedStreamCall::Response(Box::new(text_response("resumed"))),
        ]));
        let mut registry = ToolRegistry::new();
        registry.register(make_named_noop_tool("TaskCreate"));
        registry.register(make_named_noop_tool("TaskUpdate"));
        let mut session = make_session_with_provider_and_tools(provider.clone(), registry).await;
        for index in 0..10 {
            session.history.push(Message::User {
                content:   format!("turn {index}"),
                timestamp: SystemTime::now(),
            });
            session.history.push(Message::Assistant {
                content:        "done".into(),
                tool_calls:     Vec::new(),
                provider_parts: Vec::new(),
                usage:          Box::<TokenCounts>::default(),
                response_id:    format!("response_{index}"),
                timestamp:      SystemTime::now(),
            });
        }

        let control = session.control_handle();
        let mut events = session.subscribe();
        let control_for_controller = control.clone();
        let controller = tokio::spawn(async move {
            wait_for_agent_event(&mut events, |event| {
                matches!(event, AgentEvent::LlmFirstOutput {
                    kind: LlmOutputKind::ToolCall,
                })
            })
            .await;
            control_for_controller.interrupt(None);
            wait_for_agent_event(&mut events, |event| {
                matches!(event, AgentEvent::RoundInterrupted { generation: 1 })
            })
            .await;
            control_for_controller.steer("wrap up now".into(), None);
        });

        timeout(Duration::from_secs(1), session.process_input("continue"))
            .await
            .expect("interrupted session should resume after steering")
            .unwrap();
        controller.await.unwrap();

        let requests = provider
            .requests
            .lock()
            .expect("request capture lock poisoned");
        let interrupted = requests
            .first()
            .expect("the interrupted request should be captured");
        let staged = interrupted
            .messages
            .last()
            .expect("the interrupted request should not be empty");
        assert_eq!(staged.role, Role::System);
        assert_eq!(staged.text(), task_reminder::TASK_REMINDER_TEXT);

        let resumed = requests
            .get(1)
            .expect("steering should trigger a second provider request");
        let [.., steering, reminder] = resumed.messages.as_slice() else {
            panic!(
                "the resumed request should end with steering and a restaged reminder: {:?}",
                resumed.messages
            );
        };
        assert_eq!(steering.role, Role::User);
        assert_eq!(steering.text(), "wrap up now");
        assert_eq!(reminder.role, Role::System);
        assert_eq!(reminder.text(), task_reminder::TASK_REMINDER_TEXT);

        let [
            ..,
            Message::System {
                content: committed, ..
            },
            Message::Assistant { content, .. },
        ] = session.history.turns()
        else {
            panic!(
                "the reminder should commit with the successful assistant turn: {:?}",
                session.history.turns()
            );
        };
        assert_eq!(committed, task_reminder::TASK_REMINDER_TEXT);
        assert_eq!(content, "resumed");
    }

    #[tokio::test]
    async fn interrupt_during_tool_settles_once_after_balancing_tool_result() {
        let blocking_tool = RegisteredTool {
            definition: ToolDefinition {
                name:        "block".into(),
                description: "Blocks until interrupted".into(),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(|_args, ctx| {
                Box::pin(async move {
                    ctx.cancel.cancelled().await;
                    Err("Cancelled".to_string())
                })
            }),
            source:     ToolSource::Native,
        };
        let mut registry = ToolRegistry::new();
        registry.register(blocking_tool);
        let responses = vec![
            tool_call_response("block", "call_block", serde_json::json!({})),
            text_response("resumed"),
        ];
        let mut session = make_session_with_tools(responses, registry).await;
        let control = session.control_handle();
        let mut controller_events = session.subscribe();
        let mut recorded_events = session.subscribe();
        let control_for_controller = control.clone();
        let controller = tokio::spawn(async move {
            wait_for_agent_event(&mut controller_events, |event| {
                matches!(
                    event,
                    AgentEvent::ToolCallStarted { tool_name, .. } if tool_name == "block"
                )
            })
            .await;
            control_for_controller.interrupt(None);
            wait_for_agent_event(&mut controller_events, |event| {
                matches!(event, AgentEvent::RoundInterrupted { generation: 1 })
            })
            .await;
            assert!(control_for_controller.is_waiting_for_steer());
            control_for_controller.steer("resume after tool".into(), None);
        });

        timeout(
            Duration::from_secs(1),
            session.process_input("use the tool"),
        )
        .await
        .expect("tool interrupt should settle and resume")
        .unwrap();
        controller.await.unwrap();

        let events = std::iter::from_fn(|| recorded_events.try_recv().ok())
            .map(|event| event.event)
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::RoundInterrupted { .. }))
                .count(),
            1
        );
        let tool_completed = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolCallCompleted { .. }))
            .unwrap();
        let settled = events
            .iter()
            .position(|event| matches!(event, AgentEvent::RoundInterrupted { .. }))
            .unwrap();
        assert!(tool_completed < settled);
        assert!(matches!(
            session.history().turns().get(2),
            Some(Message::ToolResults { .. })
        ));
        assert!(!control.is_waiting_for_steer());
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
    async fn background_agent_notifications_are_batched_into_one_parent_turn() {
        let supervisor = SubAgentSupervisor::new(3);
        let first = make_session(vec![text_response("first result")]).await;
        let second = make_session(vec![text_response("second result")]).await;
        let first_id = supervisor
            .spawn_with_parent_notification(
                first,
                "first task".to_string(),
                "Inspect first".to_string(),
                0,
            )
            .unwrap();
        let second_id = supervisor
            .spawn_with_parent_notification(
                second,
                "second task".to_string(),
                "Inspect second".to_string(),
                0,
            )
            .unwrap();

        // Make both results ready before the parent reaches its safe turn
        // boundary so batching is deterministic.
        supervisor
            .wait_with_cancel(&first_id, &CancellationToken::new())
            .await
            .unwrap();
        supervisor
            .wait_with_cancel(&second_id, &CancellationToken::new())
            .await
            .unwrap();

        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Response(Box::new(text_response("Parent is waiting"))),
            ScriptedStreamCall::Response(Box::new(text_response("Synthesized both results"))),
        ]));
        let mut parent =
            make_session_with_provider_and_manager(provider, Some(supervisor.clone())).await;

        let output = parent
            .process_input_with_output("Delegate both tasks")
            .await
            .unwrap();

        assert_eq!(output.as_deref(), Some("Synthesized both results"));
        let turns = parent.history().turns();
        assert_eq!(turns.len(), 4);
        let Message::User {
            content: notification,
            ..
        } = &turns[2]
        else {
            panic!("third turn should deliver the background results");
        };
        assert_eq!(notification.matches("<task-notification>").count(), 2);
        assert!(notification.contains(&first_id));
        assert!(notification.contains(&second_id));
        assert!(notification.contains("first result"));
        assert!(notification.contains("second result"));

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn background_agent_output_is_not_parsed_for_skill_references() {
        let supervisor = SubAgentSupervisor::new(3);
        let child = make_session(vec![text_response("Cleaned up /tmp and exited")]).await;
        let child_id = supervisor
            .spawn_with_parent_notification(
                child,
                "clean up".to_string(),
                "Clean scratch files".to_string(),
                0,
            )
            .unwrap();
        supervisor
            .wait_with_cancel(&child_id, &CancellationToken::new())
            .await
            .unwrap();

        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Response(Box::new(text_response("Delegated"))),
            ScriptedStreamCall::Response(Box::new(text_response("Acknowledged"))),
        ]));
        let mut parent =
            make_session_with_provider_and_manager(provider, Some(supervisor.clone())).await;
        parent.skills = vec![Skill {
            name:        "commit".to_string(),
            description: "Make a commit".to_string(),
            template:    "Review changes and commit.".to_string(),
        }];

        // A child that mentions a bare path must not fail the parent turn on
        // `Unknown skill: /tmp`, nor have its report replaced by a skill body.
        let output = parent
            .process_input_with_output("Delegate the cleanup")
            .await
            .unwrap();

        assert_eq!(output.as_deref(), Some("Acknowledged"));
        let turns = parent.history().turns();
        let Message::User {
            content: notification,
            ..
        } = &turns[2]
        else {
            panic!("third turn should deliver the background result");
        };
        assert!(notification.contains("Cleaned up /tmp and exited"));
        assert!(!notification.contains("Review changes and commit."));

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn events_emitted() {
        let mut session = make_session(vec![text_response("Hello")]).await;
        let mut rx = session.subscribe();

        session.initialize().await.unwrap();
        session.process_input("Hi").await.unwrap();
        session.shutdown(SessionShutdownReason::Completed).await;

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
        let assistant_context_window = events.iter().find_map(|e| match &e.event {
            AgentEvent::AssistantMessage { context_window, .. } => context_window.as_ref(),
            _ => None,
        });
        let context_window =
            assistant_context_window.expect("assistant message should carry context window data");
        assert_eq!(
            context_window.count_method,
            StageContextWindowCountMethod::ResponseUsageScaledBreakdown
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, AgentEvent::SessionEnded))
        );
    }

    #[tokio::test]
    async fn assistant_message_context_window_uses_local_estimate_without_response_usage() {
        let mut session = make_session(vec![response_with_usage(
            text_response("Hello"),
            TokenCounts::default(),
        )])
        .await;
        let mut rx = session.subscribe();

        session.process_input("Hi").await.unwrap();

        let context_window = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|event| {
            if let AgentEvent::AssistantMessage { context_window, .. } = event.event {
                context_window
            } else {
                None
            }
        });

        let context_window = context_window.expect("assistant message should carry context window");
        assert_eq!(
            context_window.count_method,
            StageContextWindowCountMethod::LocalEstimate
        );
        assert!(context_window.input_tokens > 0);
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
            source:     ToolSource::Native,
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
        session.shutdown(SessionShutdownReason::Completed).await;
        assert_eq!(session.state(), SessionState::Closed);

        let result = session.process_input("Hello").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::SessionClosed));
    }

    #[tokio::test]
    async fn close_reports_whether_it_transitioned_to_closed() {
        let mut session = make_session(vec![]).await;
        let mut rx = session.subscribe();

        assert!(session.shutdown(SessionShutdownReason::Completed).await);
        assert!(!session.shutdown(SessionShutdownReason::Completed).await);

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
        session.shutdown(SessionShutdownReason::Completed).await;

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
            source:     ToolSource::Native,
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
            source:     ToolSource::Native,
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
        session.shutdown(SessionShutdownReason::Completed).await;

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
        let mut registry = ToolRegistry::new();
        registry.register(make_named_noop_tool("TaskCreate"));
        registry.register(make_named_noop_tool("TaskUpdate"));
        let mut session = make_session_with_provider_and_tools(provider, registry).await;

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
                message.role == Role::System && message.text() == task_reminder::TASK_REMINDER_TEXT
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
    async fn effective_tools_match_request_tool_filtering() {
        let provider = Arc::new(CapturingLlmProvider::new());
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let mut registry = ToolRegistry::new();
        registry.register(make_named_noop_tool("read_file"));
        registry.register(make_named_noop_tool("apply_patch"));
        registry.register(make_named_noop_tool("shell"));
        let profile = Arc::new(TestProfile::with_tools(registry));
        let env = Arc::new(MockSandbox::default());
        let config = SessionOptions {
            tool_access_policy: Some(Arc::new(NamedToolAccessPolicy::new(vec![
                ("read_file", ToolAccess::Allowed),
                ("apply_patch", ToolAccess::RequiresApproval),
                ("shell", ToolAccess::Denied),
            ]))),
            tool_exposure_mode: ToolExposureMode::IncludeRequiresApproval,
            ..SessionOptions::default()
        };
        let session = Session::new(client, profile, env, config, None);

        let tools = session.effective_tools();
        let mut tool_names: Vec<&str> = tools
            .iter()
            .map(|tool| tool.definition.name.as_str())
            .collect();
        tool_names.sort_unstable();

        assert_eq!(tool_names, vec!["apply_patch", "read_file"]);
        assert!(tools.iter().all(|tool| tool.source == ToolSource::Native));
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

    /// Builds a response whose provider parts carry both reasoning channels.
    fn reasoning_response(text: &str, summary: &str, trace: &str) -> Response {
        let mut response = text_response(text);
        let mut content = vec![ContentPart::Other {
            kind: ContentPart::OPENAI_COMPAT_REASONING_DETAILS.to_string(),
            data: serde_json::json!([
                {"type": "reasoning.summary", "summary": summary},
                {"type": "reasoning.text", "text": trace},
            ]),
        }];
        content.extend(response.message.content);
        response.message.content = content;
        response
    }

    fn collect_message_reasoning(
        rx: &mut broadcast::Receiver<SessionEvent>,
    ) -> Vec<Option<ReasoningOutput>> {
        let mut collected = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::AssistantMessage { reasoning, .. } = event.event {
                collected.push(reasoning);
            }
        }
        collected
    }

    #[tokio::test]
    async fn completed_response_emits_normalized_reasoning_once() {
        let mut session = make_session(vec![reasoning_response(
            "4.",
            "the user wants 2+2",
            "2+2 is 4",
        )])
        .await;
        let mut rx = session.subscribe();

        session.process_input("What is 2+2?").await.unwrap();

        let reasoning = collect_message_reasoning(&mut rx);
        assert_eq!(reasoning, vec![Some(ReasoningOutput::new(
            "the user wants 2+2",
            "2+2 is 4",
        ))]);
    }

    #[tokio::test]
    async fn tool_call_response_with_no_visible_text_still_carries_reasoning() {
        let mut tool_call = tool_call_response("nonexistent_tool", "call_1", serde_json::json!({}));
        // Drop the visible text so only the tool call and reasoning remain.
        tool_call.message.content = vec![
            ContentPart::Other {
                kind: ContentPart::OPENAI_COMPAT_REASONING_DETAILS.to_string(),
                data: serde_json::json!([{"type": "reasoning.summary", "summary": "call the tool"}]),
            },
            ContentPart::ToolCall(ToolCall::new(
                "call_1",
                "nonexistent_tool",
                serde_json::json!({}),
            )),
        ];

        let mut session = make_session(vec![tool_call, text_response("OK")]).await;
        let mut rx = session.subscribe();

        session.process_input("Do something").await.unwrap();

        let reasoning = collect_message_reasoning(&mut rx);
        assert_eq!(reasoning, vec![
            Some(ReasoningOutput::from_summary("call the tool")),
            None,
        ]);
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_final_response_contributes_reasoning_after_a_retry() {
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![
                Ok(StreamEvent::ReasoningDelta {
                    delta: "discarded thinking".to_string(),
                }),
                Err(LlmError::Stream {
                    message: "connection reset".into(),
                    source:  None,
                }),
            ]),
            ScriptedStreamCall::Response(Box::new(reasoning_response(
                "Recovered",
                "final summary",
                "final trace",
            ))),
        ]));
        let mut session = make_session_with_provider(provider.clone()).await;
        let mut rx = session.subscribe();

        session.process_input("Hello").await.unwrap();

        assert_eq!(provider.call_index.load(Ordering::SeqCst), 2);
        let reasoning = collect_message_reasoning(&mut rx);
        assert_eq!(reasoning, vec![Some(ReasoningOutput::new(
            "final summary",
            "final trace"
        ))]);
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

    /// Drain the receiver into `(label, detail)` pairs for the inference
    /// bracket events, ignoring everything else.
    fn collect_bracket_events(rx: &mut broadcast::Receiver<SessionEvent>) -> Vec<(String, String)> {
        let mut observed = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::LlmRequestStarted { requested_model } => {
                    observed.push((
                        "started".to_string(),
                        format!("{}/{}", requested_model.provider, requested_model.model_id),
                    ));
                }
                AgentEvent::LlmFirstOutput { kind } => {
                    observed.push(("first_output".to_string(), kind.to_string()));
                }
                AgentEvent::AssistantMessage { text, .. } => {
                    observed.push(("message".to_string(), text));
                }
                _ => {}
            }
        }
        observed
    }

    #[tokio::test]
    async fn inference_bracket_wraps_a_text_first_turn() {
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Response(Box::new(text_response("Hello"))),
        ]));
        let mut session = make_session_with_provider(provider).await;
        let mut rx = session.subscribe();

        session.process_input("Hi").await.unwrap();

        // `started` carries the requested provider/model, and precedes any
        // knowledge of what the response will contain.
        assert_eq!(collect_bracket_events(&mut rx), vec![
            ("started".to_string(), "anthropic/mock-model".to_string()),
            ("first_output".to_string(), "text".to_string()),
            ("message".to_string(), "Hello".to_string()),
        ]);
    }

    #[tokio::test]
    async fn first_output_reports_reasoning_when_reasoning_arrives_first() {
        let response = text_response("Hello");
        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![
                Ok(StreamEvent::ReasoningDelta {
                    delta: "weighing options".to_string(),
                }),
                Ok(StreamEvent::text_delta("Hello", None)),
                Ok(StreamEvent::finish(
                    response.finish_reason.clone(),
                    response.usage.clone(),
                    response,
                )),
            ]),
        ]));
        let mut session = make_session_with_provider(provider).await;
        let mut rx = session.subscribe();

        session.process_input("Hi").await.unwrap();

        // Edge-triggered: the later text delta does not re-fire the latch.
        assert_eq!(collect_bracket_events(&mut rx), vec![
            ("started".to_string(), "anthropic/mock-model".to_string()),
            ("first_output".to_string(), "reasoning".to_string()),
            ("message".to_string(), "Hello".to_string()),
        ]);
    }

    #[tokio::test]
    async fn first_output_reports_tool_call_for_a_turn_with_no_text_or_reasoning() {
        let tool_call = ToolCall::new("call_1", "nonexistent_tool", serde_json::json!({}));
        let mut response = tool_call_response("nonexistent_tool", "call_1", serde_json::json!({}));
        // Strip the visible text so the turn produces neither a text nor a
        // reasoning delta — the case a latch keyed on those two would miss
        // entirely, leaving tool-heavy rounds silent.
        response.message.content = vec![ContentPart::ToolCall(tool_call.clone())];

        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Events(vec![
                Ok(StreamEvent::ToolCallStart {
                    tool_call: tool_call.clone(),
                }),
                Ok(StreamEvent::ToolCallEnd {
                    tool_call: tool_call.clone(),
                }),
                Ok(StreamEvent::finish(
                    response.finish_reason.clone(),
                    response.usage.clone(),
                    response,
                )),
            ]),
            ScriptedStreamCall::Response(Box::new(text_response("Done"))),
        ]));
        let mut session = make_session_with_provider(provider).await;
        let mut rx = session.subscribe();

        session.process_input("Use the tool").await.unwrap();

        let observed = collect_bracket_events(&mut rx);
        let kinds: Vec<&str> = observed
            .iter()
            .filter(|(label, _)| label == "first_output")
            .map(|(_, kind)| kind.as_str())
            .collect();
        assert_eq!(kinds, vec!["tool_call", "text"]);
        // One bracket per round: the tool round and the round that follows it.
        assert_eq!(
            observed
                .iter()
                .filter(|(label, _)| label == "started")
                .count(),
            2
        );
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

        let mut request_started_count = 0;
        let mut replace_count = 0;
        let mut deltas = Vec::new();
        let mut assistant_messages = Vec::new();
        let mut consume_retries = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEvent::LlmRequestStarted { .. } => request_started_count += 1,
                AgentEvent::AssistantOutputReplace { .. } => replace_count += 1,
                AgentEvent::TextDelta { delta } => deltas.push(delta),
                AgentEvent::AssistantMessage { text, .. } => assistant_messages.push(text),
                AgentEvent::LlmRetry { attempt, phase, .. } => {
                    consume_retries.push((attempt, phase));
                }
                _ => {}
            }
        }

        // One round, so one bracket open — the finish-less stream is replayed
        // inside the round rather than starting a new one.
        assert_eq!(request_started_count, 1);
        assert_eq!(replace_count, 0);
        assert_eq!(deltas, vec!["Recovered".to_string()]);
        assert_eq!(assistant_messages, vec!["Recovered".to_string()]);
        // The finish-less restart is the one mid-turn path with no error to
        // report; without this event it would be invisible downstream.
        assert_eq!(consume_retries, vec![(0, LlmRetryPhase::Consume)]);
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
                AgentEvent::LlmRequestStarted { .. } => observed.push("start".to_string()),
                AgentEvent::LlmFirstOutput { kind } => observed.push(format!("first:{kind}")),
                AgentEvent::TextDelta { delta } => observed.push(format!("delta:{delta}")),
                AgentEvent::AssistantOutputReplace { text, reasoning } => {
                    observed.push(format!("replace:{text}:{reasoning:?}"));
                }
                AgentEvent::LlmRetry { phase, .. } => {
                    observed.push(format!("retry:{phase}"));
                }
                AgentEvent::AssistantMessage { text, .. } => {
                    observed.push(format!("message:{text}"));
                }
                _ => {}
            }
        }

        // The latch re-arms on restart: the replayed attempt's first delta is
        // a fresh observation, not a continuation of the discarded one.
        assert_eq!(observed, vec![
            "start".to_string(),
            "first:text".to_string(),
            "delta:Hel".to_string(),
            "replace::None".to_string(),
            "retry:consume".to_string(),
            "first:text".to_string(),
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
                AgentEvent::LlmRequestStarted { .. } => observed.push("start".to_string()),
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

    fn response_with_cost(mut response: Response, cost_usd: f64) -> Response {
        response.cost_usd = Some(cost_usd);
        response.cost_source = Some(fabro_model::CostSource::Authoritative);
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
            responses:      Vec<Response>,
            stream_index:   AtomicUsize,
            complete_calls: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl ProviderAdapter for StreamOnlyProvider {
            fn name(&self) -> &'static str {
                "mock"
            }

            async fn complete(&self, _request: &Request) -> Result<Response, LlmError> {
                self.complete_calls.fetch_add(1, Ordering::SeqCst);
                Err(LlmError::Stream {
                    message: "summarization failed".into(),
                    source:  None,
                })
            }

            async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
                let idx = self.stream_index.fetch_add(1, Ordering::SeqCst);
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
        let responses = vec![
            response_with_input_tokens(
                tool_call_response("nonexistent_tool", "call_1", serde_json::json!({})),
                90,
            ),
            text_response("OK"),
        ];

        let provider = Arc::new(StreamOnlyProvider {
            responses,
            stream_index: AtomicUsize::new(0),
            complete_calls: AtomicUsize::new(0),
        });
        let client = make_client(provider.clone() as Arc<dyn ProviderAdapter>).await;
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
        assert_eq!(
            provider.complete_calls.load(Ordering::SeqCst),
            1,
            "a failed compaction should suppress retries for the rest of the input"
        );

        // Should emit the structured compaction error without flattening the
        // underlying LLM failure.
        let mut found_error = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEvent::Error {
                error: Error::Compaction(CompactionError::Llm(_)),
            }) {
                found_error = true;
            }
        }
        assert!(found_error, "Should emit Error event for failed compaction");
    }

    #[tokio::test]
    async fn compaction_includes_structured_prompt_and_file_tracking() {
        use fabro_llm::types::ToolDefinition;

        use crate::tool_registry::{RegisteredTool, ToolSource};

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
            source:     ToolSource::Native,
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
            source:     ToolSource::Native,
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

    async fn make_parent_waiting_on_blocked_subagent()
    -> (Session, SubAgentSupervisor, String, CancellationToken) {
        let block_until_cancelled = RegisteredTool {
            definition: ToolDefinition {
                name:        "block_until_cancelled".into(),
                description: "Waits until cancelled".into(),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(|_args, ctx| {
                Box::pin(async move {
                    ctx.cancel.cancelled().await;
                    Ok("cancelled".to_string())
                })
            }),
            source:     ToolSource::Native,
        };
        let mut child_registry = ToolRegistry::new();
        child_registry.register(block_until_cancelled);
        let child = make_session_with_tools(
            vec![tool_call_response(
                "block_until_cancelled",
                "child_call",
                serde_json::json!({}),
            )],
            child_registry,
        )
        .await;
        let child_cancel = child.cancel_token();

        let supervisor = SubAgentSupervisor::new(3);
        let agent_id = supervisor
            .spawn(child, "block until cancelled".into(), 0)
            .unwrap();

        let mut parent_registry = ToolRegistry::new();
        parent_registry.register(make_wait_tool(supervisor.clone()));
        let parent_provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Response(Box::new(tool_call_response(
                "wait",
                "parent_wait_call",
                serde_json::json!({ "agent_id": agent_id }),
            ))),
            ScriptedStreamCall::Response(Box::new(text_response("resumed"))),
        ]));
        let client = make_client(parent_provider).await;
        let profile = Arc::new(TestProfile::with_tools(parent_registry));
        let env = Arc::new(MockSandbox::default());
        let session = Session::new(
            client,
            profile,
            env,
            SessionOptions::default(),
            Some(supervisor.clone()),
        );
        supervisor.set_event_callback(session.sub_agent_event_callback());

        (session, supervisor, agent_id, child_cancel)
    }

    async fn wait_for_agent_event(
        rx: &mut broadcast::Receiver<SessionEvent>,
        predicate: impl Fn(&AgentEvent) -> bool,
    ) {
        loop {
            let event = rx
                .recv()
                .await
                .expect("session event stream should remain open");
            if predicate(&event.event) {
                return;
            }
        }
    }

    #[tokio::test]
    async fn control_interrupt_during_subagent_wait_closes_child_and_resumes_after_steer() {
        let (mut session, manager, agent_id, child_cancel) =
            make_parent_waiting_on_blocked_subagent().await;
        let control = session.control_handle();
        let mut events = session.subscribe();
        let mut recorded_events = session.subscribe();
        let control_for_controller = control.clone();
        let controller = tokio::spawn(async move {
            wait_for_agent_event(&mut events, |event| {
                matches!(
                    event,
                    AgentEvent::ToolCallStarted { tool_name, .. } if tool_name == "wait"
                )
            })
            .await;
            control_for_controller.interrupt(None);
            wait_for_agent_event(&mut events, |event| {
                matches!(event, AgentEvent::SubAgentClosed { .. })
            })
            .await;
            wait_for_agent_event(&mut events, |event| {
                matches!(event, AgentEvent::RoundInterrupted { generation: 1 })
            })
            .await;
            assert!(control_for_controller.is_waiting_for_steer());
            control_for_controller.steer("resume after interrupt".into(), None);
        });

        timeout(
            Duration::from_secs(1),
            session.process_input("wait for the child"),
        )
        .await
        .expect("interrupt should unblock the subagent wait")
        .unwrap();
        controller.await.unwrap();

        assert_eq!(session.state(), SessionState::Idle);
        assert!(child_cancel.is_cancelled());
        assert!(matches!(
            manager.status(&agent_id),
            Some(SubAgentStatus::Closed)
        ));
        let events = std::iter::from_fn(|| recorded_events.try_recv().ok())
            .map(|event| event.event)
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::RoundInterrupted { .. }))
                .count(),
            1
        );
        let child_closed = events
            .iter()
            .position(|event| matches!(event, AgentEvent::SubAgentClosed { .. }))
            .unwrap();
        let settled = events
            .iter()
            .position(|event| matches!(event, AgentEvent::RoundInterrupted { .. }))
            .unwrap();
        assert!(child_closed < settled);
        assert!(!control.is_waiting_for_steer());
    }

    #[tokio::test]
    async fn terminal_cancel_during_subagent_wait_closes_child_and_session() {
        let (mut session, manager, agent_id, child_cancel) =
            make_parent_waiting_on_blocked_subagent().await;
        let cancel = session.cancel_token();
        let mut events = session.subscribe();
        let controller = tokio::spawn(async move {
            wait_for_agent_event(&mut events, |event| {
                matches!(
                    event,
                    AgentEvent::ToolCallStarted { tool_name, .. } if tool_name == "wait"
                )
            })
            .await;
            cancel.cancel();
        });

        let result = timeout(
            Duration::from_secs(1),
            session.process_input("wait for the child"),
        )
        .await
        .expect("terminal cancellation should unblock the subagent wait");
        controller.await.unwrap();

        assert!(matches!(
            result,
            Err(Error::Interrupted(InterruptReason::Cancelled))
        ));
        assert_eq!(session.state(), SessionState::Closed);
        assert!(child_cancel.is_cancelled());
        assert!(matches!(
            manager.status(&agent_id),
            Some(SubAgentStatus::Closed)
        ));
    }

    #[tokio::test]
    async fn shutdown_cleans_up_subagents_before_emitting_session_ended() {
        let supervisor = SubAgentSupervisor::new(3);

        let provider = Arc::new(ScriptedStreamProvider::new(vec![
            ScriptedStreamCall::Response(Box::new(text_response("done"))),
        ]));
        let mut session =
            make_session_with_provider_and_manager(provider, Some(supervisor.clone())).await;

        supervisor.set_event_callback(session.sub_agent_event_callback());

        let child_provider = Arc::new(DelayedStreamProvider::new(
            vec![text_response("child done")],
            Duration::from_mins(1),
        ));
        let child = make_session_with_provider(child_provider).await;
        let agent_id = supervisor.spawn(child, "task".into(), 0).unwrap();

        // Collect events
        let mut rx = session.subscribe();
        session.shutdown(SessionShutdownReason::Completed).await;

        // The subagent should have been closed
        assert!(matches!(
            supervisor.status(&agent_id),
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
