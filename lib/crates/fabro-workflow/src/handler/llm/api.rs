use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fabro_agent::subagent::{SessionFactory, SubAgentManager};
use fabro_agent::tool_registry::{RegisteredTool, ToolContext, ToolRegistry, ToolSource};
use fabro_agent::{
    AgentEvent, AgentProfile, AnthropicProfile, CompletionCoordinator, GeminiProfile,
    Message as AgentMessage, OpenAiProfile, Sandbox, Session, SessionOptions, StaticEnvProvider,
    ToolEnvProvider, ToolSecrets, register_question_tools,
};
use fabro_auth::{CredentialSource, EnvCredentialSource};
use fabro_graphviz::graph::{AttrValue, Node};
use fabro_llm::client::Client;
use fabro_llm::types::{
    Message, ReasoningEffort, Request, Response, Speed, TokenCounts,
    ToolDefinition as LlmToolDefinition,
};
use fabro_mcp::config::McpServerSettings;
#[cfg(test)]
use fabro_model::catalog::LlmCatalogSettings;
use fabro_model::{AgentProfileKind, Catalog, FallbackTarget, ModelRef, ProviderId};
use fabro_types::settings::run::RunModelControls;
use fabro_types::{PermissionLevel, RunId, SessionCapability, StageId, StageTiming};
use serde::de::DeserializeOwned;
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::super::agent::{
    CodergenBackend, CodergenResult, CodergenRunRequest, OneShotRequest,
    validate_agent_output_sources,
};
use super::super::structured_output;
use super::activation_lease::{ActivationLease, ActivationLeaseOptions};
use super::routing;
use super::routing::ProviderContext;
use crate::context::WorkflowContext;
use crate::context::keys::Fidelity;
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope};
use crate::outcome::billed_model_usage_from_llm;
use crate::services::FabroRunToolServices;
use crate::steering_hub::{ActiveControlHandle, SteeringHub};

/// Spawn a task that, when the run-level token cancels, sets the agent
/// `Session`'s interrupt reason to `Cancelled` and cancels the session token.
///
/// Factored out of `SessionCancelBridgeGuard::replace` so it can be unit-tested
/// without constructing a real `Session`.
fn spawn_bridge_task(
    run_token: CancellationToken,
    interrupt_reason: Arc<Mutex<Option<fabro_agent::InterruptReason>>>,
    session_token: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_token.cancelled().await;
        {
            let mut guard = interrupt_reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_none() {
                *guard = Some(fabro_agent::InterruptReason::Cancelled);
            }
        }
        session_token.cancel();
    })
}

/// Per-invocation guard that maps a run-level `CancellationToken` to an agent
/// `Session`'s interrupt reason and cancel token.
///
/// Dropping the guard aborts the spawned bridge task so a still-cached session
/// (after success) is not left wired to a stale run token.
struct SessionCancelBridgeGuard {
    handle: Option<JoinHandle<()>>,
}

impl SessionCancelBridgeGuard {
    fn new() -> Self {
        Self { handle: None }
    }

    fn replace(&mut self, run_token: CancellationToken, session: &Session) {
        self.abort();
        self.handle = Some(spawn_bridge_task(
            run_token,
            session.interrupt_reason_handle(),
            session.cancel_token(),
        ));
    }

    fn abort(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Drop for SessionCancelBridgeGuard {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Classification of an `fabro_agent::Error` for the API backend's `run` path.
enum AgentApiErrorDisposition {
    /// Session was interrupted via cancellation; surface as `Error::Cancelled`.
    Cancelled,
    /// Underlying LLM error eligible for provider failover.
    FailoverEligible(fabro_llm::Error),
    /// Terminal error; abort the invocation with this workflow `Error`.
    Terminal(Error),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EffectiveRequestControls {
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) speed:            Option<Speed>,
}

fn classify_agent_error(err: fabro_agent::Error, allow_failover: bool) -> AgentApiErrorDisposition {
    match err {
        fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::Cancelled) => {
            AgentApiErrorDisposition::Cancelled
        }
        fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::WallClockTimeout) => {
            AgentApiErrorDisposition::Terminal(Error::Precondition(
                "Agent session hit its wall-clock timeout".to_string(),
            ))
        }
        fabro_agent::Error::Llm(err) if allow_failover && err.failover_eligible() => {
            AgentApiErrorDisposition::FailoverEligible(err)
        }
        fabro_agent::Error::Llm(err) => AgentApiErrorDisposition::Terminal(Error::Llm(err)),
        other @ (fabro_agent::Error::SessionClosed
        | fabro_agent::Error::InvalidState(_)
        | fabro_agent::Error::ToolExecution(_)) => AgentApiErrorDisposition::Terminal(
            Error::Precondition(format!("Agent session failed: {other}")),
        ),
    }
}

fn begin_session_lifecycle(
    session: &Session,
    emitter: &Arc<Emitter>,
    parent_session_id: Option<String>,
) {
    emitter.emit(&Event::AgentSessionStarted {
        session_id: session.id().to_string(),
        parent_session_id,
        provider: Some(session.provider_id().to_string()),
        model: Some(session.model().to_string()),
    });
}

fn discard_session(
    session: &mut Session,
    lease: &mut Option<Arc<ActivationLease>>,
    emitter: &Arc<Emitter>,
) {
    if let Some(lease) = lease.take() {
        lease.release();
    }
    let session_id = session.id().to_string();
    if session.close() {
        emitter.emit(&Event::AgentSessionEnded {
            session_id,
            parent_session_id: None,
        });
    }
}

fn build_profile(
    model: &str,
    provider_id: ProviderId,
    profile_kind: AgentProfileKind,
    catalog: Arc<Catalog>,
) -> Box<dyn AgentProfile> {
    match profile_kind {
        AgentProfileKind::OpenAi => Box::new(
            OpenAiProfile::new(model)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
        AgentProfileKind::Gemini => Box::new(
            GeminiProfile::new(model)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
        AgentProfileKind::Anthropic => Box::new(
            AnthropicProfile::new(model)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
    }
}

pub fn register_fabro_run_tools(registry: &mut ToolRegistry, services: &FabroRunToolServices) {
    for definition in fabro_tool::tool_definitions() {
        registry.register(fabro_run_tool(definition, services.clone()));
    }
}

/// Register only the Fabro run tools whose names appear in `names`.
///
/// Unknown names are silently ignored so callers can list every tool they
/// care about without depending on the current `fabro_tool` catalog.
pub fn register_named_fabro_run_tools(
    registry: &mut ToolRegistry,
    services: &FabroRunToolServices,
    names: &[&str],
) {
    for definition in fabro_tool::tool_definitions() {
        if names.contains(&definition.name) {
            registry.register(fabro_run_tool(definition, services.clone()));
        }
    }
}

fn fabro_run_tool(
    definition: &fabro_tool::ToolDefinition,
    services: FabroRunToolServices,
) -> RegisteredTool {
    let name = definition.name.to_string();
    RegisteredTool {
        definition: LlmToolDefinition {
            name:        name.clone(),
            description: definition.description.to_string(),
            parameters:  definition.parameters.clone(),
        },
        executor:   Arc::new(move |args, _context: ToolContext| {
            let name = name.clone();
            let services = services.clone();
            Box::pin(async move {
                execute_fabro_run_tool(&name, args, services)
                    .await
                    .map_err(|err| err.to_string())
            })
        }),
        source:     ToolSource::Native,
    }
}

async fn execute_fabro_run_tool(
    name: &str,
    args: serde_json::Value,
    services: FabroRunToolServices,
) -> fabro_tool::ToolResult<String> {
    match name {
        fabro_tool::FABRO_RUN_CREATE_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunCreateParams>(name, args)?;
            ensure_current_run_parent(&params, services.current_run_id)?;
            let validated = fabro_tool::ValidatedCreateRuns::try_from(params)?;
            let result = fabro_tool::create_runs_with_options(
                Arc::clone(&services.backend),
                &services.base_cwd,
                &services.user_settings_path,
                validated,
                fabro_tool::CreateRunOptions {
                    forced_parent_id: Some(services.current_run_id),
                },
            )
            .await?;
            let summary = fabro_tool::create_runs_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_SEARCH_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunSearchParams>(name, args)?;
            let result = fabro_tool::search_runs(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedSearchRuns::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::search_runs_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_GET_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunGetParams>(name, args)?;
            let result = fabro_tool::run_get(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedRunGet::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::run_get_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_INTERACT_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunInteractParams>(name, args)?;
            let validated = fabro_tool::ValidatedInteractRun::try_from(params)?;
            if validated.action.requires_user() {
                return Err(fabro_tool::ToolError::message(
                    "Run approval must be performed by a user through the API, CLI, web UI, or human MCP server.",
                ));
            }
            let result = fabro_tool::interact_run(Arc::clone(&services.backend), validated).await?;
            let summary = fabro_tool::interact_run_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_GATHER_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunGatherParams>(name, args)?;
            let result = fabro_tool::gather_runs(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedGatherRuns::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::gather_runs_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunEventsParams>(name, args)?;
            let result = fabro_tool::run_events(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedRunEvents::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::run_events_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_PAIR_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunPairParams>(name, args)?;
            let result = fabro_tool::pair_run(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedPairRun::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::pair_run_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        _ => Err(fabro_tool::ToolError::message(format!(
            "unknown Fabro run tool `{name}`"
        ))),
    }
}

fn parse_fabro_tool_args<T>(name: &str, args: serde_json::Value) -> fabro_tool::ToolResult<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(args)
        .map_err(|err| fabro_tool::ToolError::message(format!("invalid {name} arguments: {err}")))
}

fn ensure_current_run_parent(
    params: &fabro_tool::FabroRunCreateParams,
    current_run_id: RunId,
) -> fabro_tool::ToolResult<()> {
    let current_parent = current_run_id.to_string();
    for run in &params.runs {
        let parent_id = match run {
            fabro_tool::CreateRunSpecInput::Workflow(_) => None,
            fabro_tool::CreateRunSpecInput::Spec(spec) => spec.parent_id.as_deref().map(str::trim),
        };
        match parent_id {
            None => {}
            Some("") => {
                return Err(fabro_tool::ToolError::message(
                    "parent_id must be omitted or match the current run; blank parent_id is invalid",
                ));
            }
            Some(parent_id) if parent_id == current_parent => {}
            Some(parent_id) => {
                return Err(fabro_tool::ToolError::message(format!(
                    "parent_id must be omitted or match the current run {current_parent}; got {parent_id}"
                )));
            }
        }
    }
    Ok(())
}

fn render_fabro_tool_result<T>(summary: &str, result: &T) -> fabro_tool::ToolResult<String>
where
    T: serde::Serialize,
{
    let json = serde_json::to_string_pretty(result).map_err(|err| {
        fabro_tool::ToolError::message(format!("failed to serialize tool result: {err}"))
    })?;
    Ok(format!("{summary}\n{json}"))
}

pub(crate) fn effective_request_controls(
    run_model_controls: &RunModelControls,
    node: &Node,
) -> Result<EffectiveRequestControls, Error> {
    let reasoning_effort = match control_attr(node, "reasoning_effort")
        .or(run_model_controls.reasoning_effort.as_deref())
    {
        Some(value) => Some(parse_reasoning_effort(node, value)?),
        None => None,
    };
    let speed = control_attr(node, "speed")
        .or(run_model_controls.speed.as_deref())
        .map(|value| parse_speed(node, value))
        .transpose()?;

    Ok(EffectiveRequestControls {
        reasoning_effort,
        speed,
    })
}

fn control_attr<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    node.attrs.get(key).and_then(AttrValue::as_str)
}

fn parse_reasoning_effort(node: &Node, value: &str) -> Result<ReasoningEffort, Error> {
    value.parse::<ReasoningEffort>().map_err(|source| {
        Error::handler_with_source(
            format!(
                "Invalid reasoning_effort \"{value}\" for node \"{}\"; expected one of: {}",
                node.id,
                expected_values(ReasoningEffort::variants()),
            ),
            source,
        )
    })
}

fn parse_speed(node: &Node, value: &str) -> Result<Speed, Error> {
    value.parse::<Speed>().map_err(|source| {
        Error::handler_with_source(
            format!(
                "Invalid speed \"{value}\" for node \"{}\"; expected one of: {}",
                node.id,
                expected_values(Speed::variants()),
            ),
            source,
        )
    })
}

fn expected_values<T>(values: &[T]) -> String
where
    T: ToString,
{
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Shared state for tracking file modifications from agent tool calls.
struct FileTracking {
    /// Maps tool_call_id → file_path for in-flight write/edit calls.
    pending: HashMap<String, String>,
    /// Set of all file paths successfully written/edited.
    touched: HashSet<String>,
    /// Most recently modified file path.
    last:    Option<String>,
}

fn track_file_event(event: &AgentEvent, state: &mut FileTracking) {
    match event {
        AgentEvent::ToolCallStarted {
            tool_name,
            tool_call_id,
            arguments,
        } if tool_name == "write_file" || tool_name == "edit_file" => {
            if let Some(path) = arguments.get("file_path").and_then(|v| v.as_str()) {
                state.pending.insert(tool_call_id.clone(), path.to_string());
            }
        }
        AgentEvent::ToolCallCompleted {
            tool_call_id,
            is_error,
            ..
        } => {
            if let Some(path) = state.pending.remove(tool_call_id) {
                if !*is_error {
                    state.touched.insert(path.clone());
                    state.last = Some(path);
                }
            }
        }
        _ => {}
    }
}

fn file_tracking_snapshot(
    file_tracking: &Arc<Mutex<FileTracking>>,
) -> (Vec<String>, Option<String>) {
    let state = file_tracking
        .lock()
        .expect("file_tracking mutex is never poisoned: no code panics while holding this lock");
    let mut files: Vec<String> = state.touched.iter().cloned().collect();
    files.sort();
    (files, state.last.clone())
}

fn last_touched_file(file_tracking: &Arc<Mutex<FileTracking>>) -> Option<String> {
    file_tracking
        .lock()
        .expect("file_tracking mutex is never poisoned: no code panics while holding this lock")
        .last
        .clone()
}

fn last_assistant_response(session: &Session) -> String {
    session
        .history()
        .turns()
        .iter()
        .rev()
        .find_map(|turn| {
            if let AgentMessage::Assistant { content, .. } = turn {
                if !content.is_empty() {
                    return Some(content.clone());
                }
            }
            None
        })
        .unwrap_or_default()
}

fn emit_agent_tools_available(
    session: &Session,
    node_id: &str,
    stage_id: &StageId,
    emitter: &Arc<Emitter>,
) {
    emitter.emit(&Event::AgentToolsAvailable {
        node_id:    node_id.to_string(),
        visit:      stage_id.visit(),
        session_id: session.id().to_string(),
        tools:      session.agent_tool_summaries(),
    });
}

/// Spawn a task that subscribes to session events and:
/// 1. Tracks file changes (write_file/edit_file tool calls) into shared state.
/// 2. Forwards non-streaming agent events to the pipeline emitter.
fn spawn_event_forwarder(
    session: &Session,
    node_id: String,
    scope: StageScope,
    emitter: Arc<Emitter>,
    file_tracking: Arc<Mutex<FileTracking>>,
) {
    let mut rx = session.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            // Reset watchdog on every event, including streaming deltas
            emitter.touch();

            // Track file changes from tool calls (including sub-agent events)
            track_file_event(
                &event.event,
                &mut file_tracking.lock().expect(
                    "file_tracking mutex is never poisoned: no code panics while holding this lock",
                ),
            );

            // Forward non-streaming agent events to pipeline
            if !event.event.is_streaming_noise()
                && !matches!(&event.event, AgentEvent::ProcessingEnd)
                && !matches!(
                    &event.event,
                    AgentEvent::SessionStarted { .. } | AgentEvent::SessionEnded
                )
            {
                emitter.emit_scoped(
                    &Event::Agent {
                        stage:             node_id.clone(),
                        visit:             scope.visit,
                        event:             event.event.clone(),
                        session_id:        Some(event.session_id.clone()),
                        parent_session_id: event.parent_session_id.clone(),
                        tool_call_id:      event.tool_call_id.clone(),
                    },
                    &scope,
                );
            }
        }
    });
}

/// LLM backend that delegates to an `agent` Session per invocation.
///
/// For `full` fidelity nodes sharing a thread key, sessions are cached
/// and reused so the LLM sees the full conversation history.
pub struct AgentApiBackend {
    model:              String,
    provider_id:        ProviderId,
    fallback_chain:     Vec<FallbackTarget>,
    sessions:           Mutex<HashMap<String, Session>>,
    tool_env:           Option<Arc<dyn ToolEnvProvider>>,
    mcp_servers:        Vec<McpServerSettings>,
    tool_secrets:       ToolSecrets,
    run_model_controls: RunModelControls,
    source:             Arc<dyn CredentialSource>,
    steering_hub:       Arc<SteeringHub>,
    catalog:            Arc<Catalog>,
    fabro_run_tools:    Option<FabroRunToolServices>,
}

struct OneShotCompletion {
    response: Response,
    model:    ModelRef,
}

impl AgentApiBackend {
    #[must_use]
    pub fn new(
        model: String,
        provider_id: impl Into<ProviderId>,
        fallback_chain: Vec<FallbackTarget>,
        source: Arc<dyn CredentialSource>,
        steering_hub: Arc<SteeringHub>,
    ) -> Self {
        let catalog = Arc::new(Catalog::from_builtin().expect("default catalog should build"));
        Self::new_with_catalog(
            model,
            provider_id.into(),
            fallback_chain,
            source,
            steering_hub,
            catalog,
        )
    }

    #[must_use]
    pub fn new_with_catalog(
        model: String,
        provider_id: ProviderId,
        fallback_chain: Vec<FallbackTarget>,
        source: Arc<dyn CredentialSource>,
        steering_hub: Arc<SteeringHub>,
        catalog: Arc<Catalog>,
    ) -> Self {
        Self {
            model,
            provider_id,
            fallback_chain,
            sessions: Mutex::new(HashMap::new()),
            tool_env: None,
            mcp_servers: Vec::new(),
            tool_secrets: ToolSecrets::default(),
            run_model_controls: RunModelControls::default(),
            source,
            steering_hub,
            catalog,
            fabro_run_tools: None,
        }
    }

    #[must_use]
    pub fn new_from_env(
        model: String,
        provider_id: impl Into<ProviderId>,
        fallback_chain: Vec<FallbackTarget>,
        steering_hub: Arc<SteeringHub>,
    ) -> Self {
        Self::new(
            model,
            provider_id,
            fallback_chain,
            Arc::new(EnvCredentialSource::new()),
            steering_hub,
        )
    }

    #[must_use]
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.tool_env = Some(Arc::new(StaticEnvProvider(env)));
        self
    }

    #[must_use]
    pub fn with_tool_env_provider(mut self, provider: Arc<dyn ToolEnvProvider>) -> Self {
        self.tool_env = Some(provider);
        self
    }

    #[must_use]
    pub fn with_mcp_servers(mut self, servers: Vec<McpServerSettings>) -> Self {
        self.mcp_servers = servers;
        self
    }

    #[must_use]
    pub fn with_tool_secrets(mut self, tool_secrets: ToolSecrets) -> Self {
        self.tool_secrets = tool_secrets;
        self
    }

    #[must_use]
    pub fn with_run_model_controls(mut self, controls: RunModelControls) -> Self {
        self.run_model_controls = controls;
        self
    }

    #[must_use]
    pub fn with_fabro_run_tools(mut self, services: FabroRunToolServices) -> Self {
        self.fabro_run_tools = Some(services);
        self
    }

    fn resolve_effective_request_controls(
        &self,
        node: &Node,
    ) -> Result<EffectiveRequestControls, Error> {
        effective_request_controls(&self.run_model_controls, node)
    }

    fn resolve_provider_context(
        &self,
        model: &str,
        provider_attr: Option<&str>,
    ) -> Result<ProviderContext, Error> {
        routing::resolve_provider_context(
            self.catalog.as_ref(),
            &self.provider_id,
            model,
            provider_attr,
        )
    }

    async fn create_session(
        &self,
        node: &Node,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<Session, Error> {
        let model = node.model().unwrap_or(&self.model);
        let provider = routing::resolve_node_provider_context(
            self.catalog.as_ref(),
            &self.provider_id,
            &self.model,
            node,
        )?;
        Self::create_session_for(
            model,
            provider,
            node,
            sandbox,
            self.source.as_ref(),
            Arc::clone(&self.catalog),
            &self.run_model_controls,
            self.tool_env.as_ref(),
            tool_hooks,
            self.mcp_servers.clone(),
            self.tool_secrets.clone(),
            self.fabro_run_tools.clone(),
        )
        .await
    }

    async fn create_session_for(
        model: &str,
        provider: ProviderContext,
        node: &Node,
        sandbox: &Arc<dyn Sandbox>,
        source: &dyn CredentialSource,
        catalog: Arc<Catalog>,
        run_model_controls: &RunModelControls,
        tool_env: Option<&Arc<dyn ToolEnvProvider>>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
        mcp_servers: Vec<McpServerSettings>,
        tool_secrets: ToolSecrets,
        fabro_run_tools: Option<FabroRunToolServices>,
    ) -> Result<Session, Error> {
        let controls = effective_request_controls(run_model_controls, node)?;
        let client = Client::from_source(source, Arc::clone(&catalog))
            .await
            .map_err(|e| Error::handler_with_source("Failed to create LLM client", e))?;

        let mut profile = build_profile(
            model,
            provider.provider_id.clone(),
            provider.profile_kind,
            Arc::clone(&catalog),
        );

        let config = SessionOptions {
            max_tokens: node.max_tokens(),
            reasoning_effort: controls.reasoning_effort,
            speed: controls.speed,
            tool_hooks,
            mcp_servers,
            tool_secrets,
            // Workflow agents run with no `tool_access_policy`, which exposes
            // the entire tool registry (read, write, shell, subagent, MCP) and
            // skips approval gating. Report that truthfully so the UI doesn't
            // render "Unknown" for every workflow stage. Override per-stage if
            // a future workflow attribute narrows the scope.
            permission_level: Some(PermissionLevel::Full),
            ..SessionOptions::default()
        };

        let manager = Arc::new(TokioMutex::new(SubAgentManager::new(
            config.max_subagent_depth,
        )));
        let manager_for_callback = manager.clone();

        // Build factory that creates child sessions WITHOUT subagent tools
        let factory_client = client.clone();
        let factory_model = model.to_string();
        let factory_provider = provider.clone();
        let factory_catalog = Arc::clone(&catalog);
        let factory_env = Arc::clone(sandbox);
        let factory_tool_env = tool_env.cloned();
        let factory_fabro_run_tools = fabro_run_tools.clone();
        let factory_permission_level = config.permission_level;
        let factory_tool_secrets = config.tool_secrets.clone();
        let factory: SessionFactory = Arc::new(move || {
            let mut child_profile = build_profile(
                &factory_model,
                factory_provider.provider_id.clone(),
                factory_provider.profile_kind,
                Arc::clone(&factory_catalog),
            );
            if let Some(services) = factory_fabro_run_tools.clone() {
                register_fabro_run_tools(child_profile.tool_registry_mut(), &services);
            }
            let child_profile: Arc<dyn AgentProfile> = Arc::from(child_profile);
            let mut session = Session::new(
                factory_client.clone(),
                child_profile,
                Arc::clone(&factory_env),
                SessionOptions {
                    reasoning_effort: controls.reasoning_effort,
                    speed: controls.speed,
                    permission_level: factory_permission_level,
                    tool_secrets: factory_tool_secrets.clone(),
                    ..SessionOptions::default()
                },
                None,
            );
            if let Some(provider) = &factory_tool_env {
                session.set_tool_env_provider(Arc::clone(provider));
            }
            session
        });

        profile.register_subagent_tools(manager, factory, 0);
        register_question_tools(provider.profile_kind, profile.tool_registry_mut());
        if let Some(services) = fabro_run_tools {
            register_fabro_run_tools(profile.tool_registry_mut(), &services);
        }
        let profile: Arc<dyn AgentProfile> = Arc::from(profile);

        let mut session = Session::new(
            client,
            profile,
            Arc::clone(sandbox),
            config,
            Some(manager_for_callback.clone()),
        );
        if let Some(provider) = tool_env {
            session.set_tool_env_provider(Arc::clone(provider));
        }

        // Wire subagent event callback to parent session's emitter
        manager_for_callback
            .lock()
            .await
            .set_event_callback(session.sub_agent_event_callback());

        Ok(session)
    }

    /// Activate `session` with the steering hub under `stage_id` and wire up
    /// the completion coordinator.
    fn attach_session_to_hub(
        &self,
        session: &mut Session,
        stage_id: &StageId,
        thread_id: Option<&str>,
        emitter: &Arc<Emitter>,
    ) -> Result<Arc<ActivationLease>, Error> {
        let handle = Arc::new(session.control_handle()) as Arc<dyn ActiveControlHandle>;
        let lease = ActivationLease::activate(
            ActivationLeaseOptions {
                stage_id:         stage_id.clone(),
                session_id:       session.id().to_string(),
                thread_id:        thread_id.map(str::to_string),
                provider:         Some(session.provider_id().to_string()),
                model:            Some(session.model().to_string()),
                reasoning_effort: session.reasoning_effort(),
                speed:            session.speed(),
                permission_level: session.permission_level(),
                capabilities:     vec![SessionCapability::Steer],
                hub:              Arc::clone(&self.steering_hub),
                emitter:          Arc::clone(emitter),
            },
            &handle,
        )?;
        session.set_completion_coordinator(Arc::new(SteeringCompletionCoordinator {
            handle,
            lease: Mutex::new(Some(Arc::clone(&lease))),
        }));
        Ok(lease)
    }

    fn shutdown_cached_sessions(&self, emitter: &Arc<Emitter>) {
        let sessions: Vec<Session> = self
            .sessions
            .lock()
            .expect("sessions mutex is never poisoned: no code panics while holding this lock")
            .drain()
            .map(|(_, s)| s)
            .collect();
        for mut session in sessions {
            let session_id = session.id().to_string();
            if session.close() {
                emitter.emit(&Event::AgentSessionEnded {
                    session_id,
                    parent_session_id: None,
                });
            }
        }
    }

    async fn complete_one_shot_request(
        &self,
        client: &Client,
        node: &Node,
        emitter: &Arc<Emitter>,
        stage_scope: &StageScope,
        request: &Request,
        controls: EffectiveRequestControls,
        fallback_chain: &[FallbackTarget],
    ) -> Result<OneShotCompletion, Error> {
        let result = client.complete(request).await;
        let default_provider = self.provider_id.to_string();

        let (response, model) = match result {
            Ok(resp) => (resp, ModelRef {
                provider: ProviderId::from(
                    request
                        .provider
                        .clone()
                        .unwrap_or_else(|| default_provider.clone()),
                ),
                model_id: request.model.clone(),
                speed:    controls.speed,
            }),
            Err(sdk_err) if sdk_err.failover_eligible() && !fallback_chain.is_empty() => {
                let error_msg = sdk_err.to_string();
                let from_provider = request
                    .provider
                    .clone()
                    .unwrap_or_else(|| default_provider.clone());
                let from_model = request.model.clone();

                let mut last_err = sdk_err;
                let mut found = None;

                for target in fallback_chain {
                    emitter.emit_scoped(
                        &Event::Failover {
                            stage:         node.id.clone(),
                            from_provider: from_provider.clone(),
                            from_model:    from_model.clone(),
                            to_provider:   target.provider.clone(),
                            to_model:      target.model.clone(),
                            error:         error_msg.clone(),
                        },
                        stage_scope,
                    );

                    let max_tokens = node.max_tokens().or_else(|| {
                        self.catalog
                            .get(&target.model)
                            .and_then(|model| model.limits.max_output)
                    });

                    let fallback_request = Request {
                        model: target.model.clone(),
                        provider: Some(target.provider.clone()),
                        max_tokens,
                        reasoning_effort: controls.reasoning_effort,
                        speed: controls.speed,
                        ..request.clone()
                    };

                    match client.complete(&fallback_request).await {
                        Ok(resp) => {
                            found = Some(OneShotCompletion {
                                response: resp,
                                model:    ModelRef {
                                    provider: ProviderId::from(target.provider.clone()),
                                    model_id: target.model.clone(),
                                    speed:    controls.speed,
                                },
                            });
                            break;
                        }
                        Err(err) if err.failover_eligible() => {
                            last_err = err;
                        }
                        Err(err) => return Err(Error::Llm(err)),
                    }
                }

                match found {
                    Some(completion) => return Ok(completion),
                    None => return Err(Error::Llm(last_err)),
                }
            }
            Err(sdk_err) => return Err(Error::Llm(sdk_err)),
        };

        Ok(OneShotCompletion { response, model })
    }
}

#[async_trait]
impl CodergenBackend for AgentApiBackend {
    async fn shutdown(&self, emitter: &Arc<Emitter>) {
        self.shutdown_cached_sessions(emitter);
    }

    fn effective_request_controls(&self, node: &Node) -> Result<EffectiveRequestControls, Error> {
        self.resolve_effective_request_controls(node)
    }

    async fn one_shot(&self, request: OneShotRequest<'_>) -> Result<CodergenResult, Error> {
        let node = request.node;
        let prompt = request.prompt;
        let system_prompt = request.system_prompt;
        let emitter = request.emitter;
        let stage_scope = request.stage_scope;

        let client = Client::from_source(self.source.as_ref(), Arc::clone(&self.catalog))
            .await
            .map_err(|e| Error::handler_with_source("Failed to create LLM client", e))?;

        let model = node.model().unwrap_or(&self.model);
        let provider = self.resolve_provider_context(model, node.provider())?;
        let provider_id = provider.provider_id.to_string();
        let controls = self.resolve_effective_request_controls(node)?;

        let max_tokens = node
            .max_tokens()
            .or_else(|| self.catalog.get(model).and_then(|m| m.limits.max_output));

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(Message::system(sys));
        }
        messages.push(Message::user(prompt));

        // Build per-request fallback chain: if the node overrides the provider,
        // no failover is available; otherwise use the backend's.
        let fallback_chain: &[FallbackTarget] = if node.provider().is_some() {
            &[]
        } else {
            &self.fallback_chain
        };

        let output_schema = structured_output::parse_node_output_schema(node)?;
        let response_format = output_schema
            .as_ref()
            .map(structured_output::prompt_response_format);
        let mut repair_attempts = 0_i64;
        let mut total_usage = TokenCounts::default();
        let mut inference_duration = Duration::ZERO;

        loop {
            let request = Request {
                model: model.to_string(),
                messages: messages.clone(),
                provider: Some(provider_id.clone()),
                reasoning_effort: controls.reasoning_effort,
                speed: controls.speed,
                tools: None,
                tool_choice: None,
                response_format: response_format.clone(),
                temperature: None,
                top_p: None,
                max_tokens,
                stop_sequences: None,
                metadata: None,
                provider_options: None,
            };

            let inference_start = Instant::now();
            let completion_result = self
                .complete_one_shot_request(
                    &client,
                    node,
                    emitter,
                    stage_scope,
                    &request,
                    controls,
                    fallback_chain,
                )
                .await;
            inference_duration = inference_duration.saturating_add(inference_start.elapsed());
            let completion = completion_result?;
            total_usage += completion.response.usage.clone();
            let response_text = completion.response.text();

            let validation_error = if let Some(schema) = &output_schema {
                match structured_output::validate_response_text(schema, &response_text) {
                    Ok(_) => None,
                    Err(error) => Some((schema, error)),
                }
            } else {
                None
            };

            if let Some((schema, error)) = validation_error {
                if repair_attempts >= node.output_retries() {
                    return Err(Error::OutputSchemaValidation(
                        structured_output::exhausted_failure_reason(node.output_retries()),
                    ));
                }
                messages.push(Message::assistant(response_text));
                messages.push(Message::user(error.repair_message(schema)));
                repair_attempts += 1;
                continue;
            }

            let stage_usage = billed_model_usage_from_llm(
                self.catalog.as_ref(),
                &completion.model,
                &total_usage,
            )?;

            return Ok(CodergenResult::Text {
                text:              response_text,
                usage:             Some(stage_usage),
                files_touched:     Vec::new(),
                last_file_touched: None,
                timing:            StageTiming::active_only(
                    crate::millis_u64(inference_duration),
                    0,
                ),
            });
        }
    }

    async fn run(&self, request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
        let node = request.node;
        let prompt = request.prompt;
        let context = request.context;
        let thread_id = request.thread_id;
        let emitter = request.emitter;
        let sandbox = request.sandbox;
        let tool_hooks = request.tool_hooks;
        let cancel_token = request.cancel_token;
        let agent_tool_runtime = request.agent_tool_runtime;
        let output_schema = structured_output::parse_node_output_schema(node)?;

        let fidelity = context.fidelity();
        let reuse_key = if fidelity == Fidelity::Full {
            thread_id.map(String::from)
        } else {
            None
        };

        let mut bridge = SessionCancelBridgeGuard::new();

        // Take a cached session if reusing, otherwise create a new one. Cancel
        // checks bracket `Client::from_source(...)` so cancellation arriving
        // during credential refresh is not lost.
        if cancel_token.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let (mut session, is_reused) = if let Some(ref key) = reuse_key {
            let existing = self
                .sessions
                .lock()
                .expect("sessions mutex is never poisoned: no code panics while holding this lock")
                .remove(key);
            if let Some(s) = existing {
                (s, true)
            } else {
                let created = self.create_session(node, sandbox, tool_hooks.clone()).await;
                if cancel_token.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                (created?, false)
            }
        } else {
            let created = self.create_session(node, sandbox, tool_hooks.clone()).await;
            if cancel_token.is_cancelled() {
                return Err(Error::Cancelled);
            }
            (created?, false)
        };
        if cancel_token.is_cancelled() {
            return Err(Error::Cancelled);
        }
        bridge.replace(cancel_token.clone(), &session);

        tracing::info!(
            node = %node.id,
            fidelity = %fidelity,
            reused = is_reused,
            "Agent session ready"
        );

        // File change tracking: shared between spawned task and main fn.
        let file_tracking = Arc::new(Mutex::new(FileTracking {
            pending: HashMap::new(),
            touched: HashSet::new(),
            last:    None,
        }));
        let stage_scope = StageScope::for_handler(context, &node.id);

        // Subscribe to session events: forward to pipeline emitter + track files.
        spawn_event_forwarder(
            &session,
            node.id.clone(),
            stage_scope.clone(),
            Arc::clone(emitter),
            Arc::clone(&file_tracking),
        );

        let mut total_usage = TokenCounts::default();
        let mut inference_duration = Duration::ZERO;
        let mut tool_duration = Duration::ZERO;

        // Activate with the steering hub after initialization so HTTP
        // `POST /runs/{id}/steer` calls reach this session. The activation
        // lease is shared with the natural-completion coordinator and is
        // released on every exit path.
        let stage_id = stage_scope.stage_id();
        let mut lease: Option<Arc<ActivationLease>> = None;

        let allow_failover_primary = !self.fallback_chain.is_empty();
        let init_result = if is_reused {
            Ok(())
        } else {
            begin_session_lifecycle(&session, emitter, None);
            match session.initialize().await {
                Ok(()) => Ok(()),
                Err(err) => match classify_agent_error(err, allow_failover_primary) {
                    AgentApiErrorDisposition::Cancelled => {
                        bridge.abort();
                        discard_session(&mut session, &mut lease, emitter);
                        return Err(Error::Cancelled);
                    }
                    AgentApiErrorDisposition::Terminal(err) => {
                        bridge.abort();
                        discard_session(&mut session, &mut lease, emitter);
                        return Err(err);
                    }
                    AgentApiErrorDisposition::FailoverEligible(sdk_err) => {
                        Err(fabro_agent::Error::Llm(sdk_err))
                    }
                },
            }
        };

        // If initialize failed with a failover-eligible error, treat as a
        // process_input failover trigger; otherwise run process_input.
        let result = match init_result {
            Ok(()) => {
                match self.attach_session_to_hub(&mut session, &stage_id, thread_id, emitter) {
                    Ok(active_lease) => lease = Some(active_lease),
                    Err(err) => {
                        bridge.abort();
                        discard_session(&mut session, &mut lease, emitter);
                        return Err(err);
                    }
                }
                // Reused steerable sessions already emitted their effective
                // tool list on first activation; the registry, access policy,
                // and exposure mode are immutable for the session's lifetime,
                // so re-emitting on every subsequent prompt is wasted work.
                if !is_reused {
                    emit_agent_tools_available(&session, &node.id, &stage_id, emitter);
                }
                let process_result = session
                    .process_input_with_runtime(prompt, agent_tool_runtime.clone())
                    .await;
                let timing = session.last_input_timing();
                inference_duration = inference_duration.saturating_add(timing.inference);
                tool_duration = tool_duration.saturating_add(timing.tool);
                if process_result.is_ok() {
                    total_usage += session.last_input_usage();
                }
                process_result
            }
            Err(err) => Err(err),
        };

        // On failover-eligible error, try fallback providers.
        let result: Result<(), Error> = match result {
            Ok(()) => Ok(()),
            Err(err) => match classify_agent_error(err, allow_failover_primary) {
                AgentApiErrorDisposition::Cancelled => {
                    bridge.abort();
                    discard_session(&mut session, &mut lease, emitter);
                    return Err(Error::Cancelled);
                }
                AgentApiErrorDisposition::Terminal(err) => {
                    bridge.abort();
                    discard_session(&mut session, &mut lease, emitter);
                    return Err(err);
                }
                AgentApiErrorDisposition::FailoverEligible(sdk_err) => {
                    let error_msg = sdk_err.to_string();
                    let from_provider = self.provider_id.to_string();
                    let from_model = self.model.clone();

                    let mut last_err = Error::Llm(sdk_err);
                    let mut succeeded = false;

                    bridge.abort();
                    discard_session(&mut session, &mut lease, emitter);

                    for (index, target) in self.fallback_chain.iter().enumerate() {
                        emitter.emit_scoped(
                            &Event::Failover {
                                stage:         node.id.clone(),
                                from_provider: from_provider.clone(),
                                from_model:    from_model.clone(),
                                to_provider:   target.provider.clone(),
                                to_model:      target.model.clone(),
                                error:         error_msg.clone(),
                            },
                            &stage_scope,
                        );

                        let Ok(target_provider) =
                            self.resolve_provider_context(&target.model, Some(&target.provider))
                        else {
                            continue;
                        };

                        if cancel_token.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        let new_session_result = Self::create_session_for(
                            &target.model,
                            target_provider,
                            node,
                            sandbox,
                            self.source.as_ref(),
                            Arc::clone(&self.catalog),
                            &self.run_model_controls,
                            self.tool_env.as_ref(),
                            tool_hooks.clone(),
                            self.mcp_servers.clone(),
                            self.tool_secrets.clone(),
                            self.fabro_run_tools.clone(),
                        )
                        .await;
                        if cancel_token.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        let new_session = match new_session_result {
                            Ok(s) => s,
                            Err(e) => {
                                last_err = e;
                                continue;
                            }
                        };
                        session = new_session;
                        bridge.replace(cancel_token.clone(), &session);

                        // Re-subscribe to forward events + track files from the new session
                        spawn_event_forwarder(
                            &session,
                            node.id.clone(),
                            stage_scope.clone(),
                            Arc::clone(emitter),
                            Arc::clone(&file_tracking),
                        );

                        let allow_failover_next = index + 1 < self.fallback_chain.len();
                        begin_session_lifecycle(&session, emitter, None);
                        if let Err(err) = session.initialize().await {
                            match classify_agent_error(err, allow_failover_next) {
                                AgentApiErrorDisposition::Cancelled => {
                                    bridge.abort();
                                    discard_session(&mut session, &mut lease, emitter);
                                    return Err(Error::Cancelled);
                                }
                                AgentApiErrorDisposition::Terminal(err) => {
                                    bridge.abort();
                                    discard_session(&mut session, &mut lease, emitter);
                                    return Err(err);
                                }
                                AgentApiErrorDisposition::FailoverEligible(sdk_err) => {
                                    last_err = Error::Llm(sdk_err);
                                    bridge.abort();
                                    discard_session(&mut session, &mut lease, emitter);
                                    continue;
                                }
                            }
                        }
                        match self.attach_session_to_hub(
                            &mut session,
                            &stage_id,
                            thread_id,
                            emitter,
                        ) {
                            Ok(active_lease) => lease = Some(active_lease),
                            Err(err) => {
                                bridge.abort();
                                discard_session(&mut session, &mut lease, emitter);
                                return Err(err);
                            }
                        }
                        emit_agent_tools_available(&session, &node.id, &stage_id, emitter);
                        let process_result = session
                            .process_input_with_runtime(prompt, agent_tool_runtime.clone())
                            .await;
                        let timing = session.last_input_timing();
                        inference_duration = inference_duration.saturating_add(timing.inference);
                        tool_duration = tool_duration.saturating_add(timing.tool);
                        match process_result {
                            Ok(()) => {
                                total_usage += session.last_input_usage();
                                succeeded = true;
                                break;
                            }
                            Err(err) => match classify_agent_error(err, allow_failover_next) {
                                AgentApiErrorDisposition::Cancelled => {
                                    bridge.abort();
                                    discard_session(&mut session, &mut lease, emitter);
                                    return Err(Error::Cancelled);
                                }
                                AgentApiErrorDisposition::Terminal(err) => {
                                    bridge.abort();
                                    discard_session(&mut session, &mut lease, emitter);
                                    return Err(err);
                                }
                                AgentApiErrorDisposition::FailoverEligible(sdk_err) => {
                                    last_err = Error::Llm(sdk_err);
                                    bridge.abort();
                                    discard_session(&mut session, &mut lease, emitter);
                                }
                            },
                        }
                    }

                    if succeeded { Ok(()) } else { Err(last_err) }
                }
            },
        };

        // On error, discard the session (don't cache failed state). The
        // bridge's `Drop` will abort the spawned task on early return.
        if let Err(err) = result {
            bridge.abort();
            discard_session(&mut session, &mut lease, emitter);
            return Err(err);
        }

        let mut response = last_assistant_response(&session);
        if let Some(schema) = &output_schema {
            let mut repair_attempts = 0_i64;
            loop {
                let last_file_touched = last_touched_file(&file_tracking);
                match validate_agent_output_sources(
                    schema,
                    &response,
                    sandbox,
                    last_file_touched.as_deref(),
                )
                .await
                {
                    Ok(_) => break,
                    Err(error) => {
                        if repair_attempts >= node.output_retries() {
                            bridge.abort();
                            discard_session(&mut session, &mut lease, emitter);
                            return Err(Error::OutputSchemaValidation(
                                structured_output::exhausted_failure_reason(node.output_retries()),
                            ));
                        }
                        let repair_message = error.repair_message(schema);
                        let repair_result = session
                            .process_input_with_runtime(
                                &repair_message,
                                fabro_agent::AgentToolRuntime::default(),
                            )
                            .await;
                        let timing = session.last_input_timing();
                        inference_duration = inference_duration.saturating_add(timing.inference);
                        tool_duration = tool_duration.saturating_add(timing.tool);
                        match repair_result {
                            Ok(()) => {
                                total_usage += session.last_input_usage();
                                repair_attempts += 1;
                                response = last_assistant_response(&session);
                            }
                            Err(err) => match classify_agent_error(err, false) {
                                AgentApiErrorDisposition::Cancelled => {
                                    bridge.abort();
                                    discard_session(&mut session, &mut lease, emitter);
                                    return Err(Error::Cancelled);
                                }
                                AgentApiErrorDisposition::Terminal(err) => {
                                    bridge.abort();
                                    discard_session(&mut session, &mut lease, emitter);
                                    return Err(err);
                                }
                                AgentApiErrorDisposition::FailoverEligible(sdk_err) => {
                                    bridge.abort();
                                    discard_session(&mut session, &mut lease, emitter);
                                    return Err(Error::Llm(sdk_err));
                                }
                            },
                        }
                    }
                }
            }
        }

        let billing_controls = self.resolve_effective_request_controls(node)?;
        let stage_usage = billed_model_usage_from_llm(
            self.catalog.as_ref(),
            &ModelRef {
                provider: session.provider_id(),
                model_id: session.model().to_string(),
                speed:    billing_controls.speed,
            },
            &total_usage,
        )?;

        // Collect files_touched from the shared tracking state.
        let (files_touched, last_file_touched) = file_tracking_snapshot(&file_tracking);

        if let Some(lease) = lease.take() {
            lease.release();
        }

        // Cache session back for reuse on success. Detach the bridge first so
        // the cached session is not left wired to this run's cancel token.
        if let Some(key) = reuse_key {
            bridge.abort();
            self.sessions
                .lock()
                .expect("sessions mutex is never poisoned: no code panics while holding this lock")
                .insert(key, session);
        } else {
            let session_id = session.id().to_string();
            if session.close() {
                emitter.emit(&Event::AgentSessionEnded {
                    session_id,
                    parent_session_id: None,
                });
            }
        }

        Ok(CodergenResult::Text {
            text: response,
            usage: Some(stage_usage),
            files_touched,
            last_file_touched,
            timing: StageTiming::active_only(
                crate::millis_u64(inference_duration),
                crate::millis_u64(tool_duration),
            ),
        })
    }
}

/// Coordinator that lets the agent loop ask the workflow layer whether to
/// keep iterating after a no-tool natural completion. Implements the
/// "close-the-door" pattern: detach only if the queue is empty, otherwise
/// report `true` so the loop drains.
struct SteeringCompletionCoordinator {
    handle: Arc<dyn ActiveControlHandle>,
    lease:  Mutex<Option<Arc<ActivationLease>>>,
}

impl CompletionCoordinator for SteeringCompletionCoordinator {
    fn on_natural_completion(&self) -> bool {
        let mut lease = self.lease.lock().expect("activation lease lock poisoned");
        let Some(active_lease) = lease.as_ref() else {
            return false;
        };
        if active_lease.is_pair_active() {
            self.handle.park_for_steer();
            return true;
        }
        if active_lease.release_if_no_pending_control_work(self.handle.as_ref()) {
            lease.take();
            false
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use chrono::TimeZone;
    use fabro_agent::subagent::SessionFactory;
    use fabro_agent::{AgentProfile, LocalSandbox, ToolRegistry};
    use fabro_api::types;
    use fabro_auth::{EnvCredentialSource, VaultCredentialSource};
    use fabro_llm::provider::{ProviderAdapter, StreamEventStream};
    use fabro_llm::{Error as LlmError, ProviderErrorDetail, ProviderErrorKind};
    use fabro_tool::FabroToolBackend;
    use fabro_types::{
        EventEnvelope, FailureReason, Run, RunId, RunLifecycle, RunLinks, RunOrigin,
        RunPairStatusResponse, RunProjection, RunStatus, RunTimestamps, SuccessReason, WorkflowRef,
        test_support,
    };
    use fabro_vault::{SecretType, Vault};
    use futures::stream;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use tokio::sync::RwLock as AsyncRwLock;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::context::Context;
    use crate::services::FabroRunToolServices;

    struct ShutdownTestProfile {
        registry: ToolRegistry,
    }

    impl ShutdownTestProfile {
        fn new() -> Self {
            Self {
                registry: ToolRegistry::new(),
            }
        }
    }

    impl AgentProfile for ShutdownTestProfile {
        fn profile_kind(&self) -> AgentProfileKind {
            AgentProfileKind::OpenAi
        }

        fn provider_id(&self) -> ProviderId {
            ProviderId::openai()
        }

        fn model(&self) -> &str {
            "gpt-5.4"
        }

        fn tool_registry(&self) -> &ToolRegistry {
            &self.registry
        }

        fn tool_registry_mut(&mut self) -> &mut ToolRegistry {
            &mut self.registry
        }

        fn build_system_prompt(
            &self,
            _env: &dyn fabro_agent::Sandbox,
            _env_context: &fabro_agent::EnvContext,
            _memory: &[String],
            _user_instructions: Option<&str>,
            _skills: &[fabro_agent::Skill],
        ) -> String {
            "test".to_string()
        }
    }

    struct ShutdownTestProvider;

    #[async_trait]
    impl ProviderAdapter for ShutdownTestProvider {
        fn name(&self) -> &str {
            "openai"
        }

        async fn complete(
            &self,
            _request: &Request,
        ) -> Result<fabro_llm::types::Response, LlmError> {
            unreachable!("shutdown test never calls LLM completion")
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
            Ok(Box::pin(stream::empty()))
        }
    }

    fn mock_llm_catalog(server: &MockServer) -> Arc<Catalog> {
        let settings: LlmCatalogSettings = toml::from_str(&format!(
            r#"
[providers.mock]
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "{}"

[providers.mock.auth]
credentials = ["env:MOCK_API_KEY"]

[models.mock-model]
provider = "mock"
display_name = "Mock Model"
family = "mock"
default = true

[models.mock-model.limits]
context_window = 8192
max_output = 1024

[models.mock-model.features]
tools = true
vision = false
reasoning = false
"#,
            server.base_url()
        ))
        .unwrap();
        Arc::new(Catalog::from_builtin_with_overrides(&settings).unwrap())
    }

    fn mock_api_backend(server: &MockServer) -> AgentApiBackend {
        let source = EnvCredentialSource::with_env_lookup(Arc::new(|name| {
            if name == "MOCK_API_KEY" {
                Some("sk-test".to_string())
            } else {
                None
            }
        }));
        AgentApiBackend::new_with_catalog(
            "mock-model".to_string(),
            ProviderId::from("mock"),
            Vec::new(),
            Arc::new(source),
            SteeringHub::for_tests(),
            mock_llm_catalog(server),
        )
    }

    fn chat_completion_response(
        text: &str,
        input_tokens: i64,
        output_tokens: i64,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "model": "mock-model",
            "choices": [{
                "message": {
                    "content": text
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": input_tokens,
                "completion_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens
            }
        })
    }

    fn chat_completion_stream(text: &str, input_tokens: i64, output_tokens: i64) -> String {
        let text_chunk = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "model": "mock-model",
            "choices": [{
                "delta": {
                    "content": text
                },
                "finish_reason": null
            }]
        });
        let usage_chunk = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "model": "mock-model",
            "choices": [],
            "usage": {
                "prompt_tokens": input_tokens,
                "completion_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens
            }
        });
        format!("data: {text_chunk}\n\ndata: {usage_chunk}\n\ndata: [DONE]\n\n")
    }

    fn custom_output_schema_attr() -> AttrValue {
        AttrValue::String(
            r#"{"type":"object","required":["passed"],"properties":{"passed":{"type":"boolean"}}}"#
                .to_string(),
        )
    }

    #[test]
    fn agent_backend_stores_config() {
        let backend = AgentApiBackend::new_from_env(
            "claude-opus-4-6".to_string(),
            ProviderId::openai(),
            Vec::new(),
            SteeringHub::for_tests(),
        );
        assert_eq!(backend.model, "claude-opus-4-6");
        assert_eq!(backend.provider_id, ProviderId::openai());
    }

    #[test]
    fn agent_backend_initializes_empty_sessions() {
        let backend = AgentApiBackend::new_from_env(
            "claude-opus-4-6".to_string(),
            ProviderId::anthropic(),
            Vec::new(),
            SteeringHub::for_tests(),
        );
        assert!(backend.sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn agent_run_tools_register_exact_shared_definitions() {
        let mut registry = ToolRegistry::new();
        let (services, _backend) = fabro_run_tool_services();
        register_fabro_run_tools(&mut registry, &services);

        let mut registered = registry
            .names()
            .into_iter()
            .filter(|name| name.starts_with("fabro_run_"))
            .collect::<Vec<_>>();
        registered.sort();
        assert_eq!(registered, vec![
            fabro_tool::FABRO_RUN_CREATE_TOOL_NAME,
            fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME,
            fabro_tool::FABRO_RUN_GATHER_TOOL_NAME,
            fabro_tool::FABRO_RUN_GET_TOOL_NAME,
            fabro_tool::FABRO_RUN_INTERACT_TOOL_NAME,
            fabro_tool::FABRO_RUN_PAIR_TOOL_NAME,
            fabro_tool::FABRO_RUN_SEARCH_TOOL_NAME,
        ]);

        for definition in fabro_tool::tool_definitions() {
            let registered = registry
                .get(definition.name)
                .expect("shared Fabro run tool should be registered");
            assert_eq!(registered.definition.description, definition.description);
            assert_eq!(registered.definition.parameters, definition.parameters);
        }
    }

    #[test]
    fn register_named_fabro_run_tools_registers_only_listed_tools() {
        let mut registry = ToolRegistry::new();
        let (services, _backend) = fabro_run_tool_services();
        register_named_fabro_run_tools(&mut registry, &services, &[
            fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME,
            fabro_tool::FABRO_RUN_INTERACT_TOOL_NAME,
        ]);

        let mut registered = registry
            .names()
            .into_iter()
            .filter(|name| name.starts_with("fabro_run_"))
            .collect::<Vec<_>>();
        registered.sort();
        assert_eq!(registered, vec![
            fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME,
            fabro_tool::FABRO_RUN_INTERACT_TOOL_NAME,
        ]);
    }

    #[test]
    fn register_named_fabro_run_tools_ignores_unknown_names() {
        let mut registry = ToolRegistry::new();
        let (services, _backend) = fabro_run_tool_services();
        register_named_fabro_run_tools(&mut registry, &services, &[
            fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME,
            "not_a_real_tool",
        ]);

        let registered = registry
            .names()
            .into_iter()
            .filter(|name| name.starts_with("fabro_run_"))
            .collect::<Vec<_>>();
        assert_eq!(registered, vec![fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME]);
    }

    #[tokio::test]
    async fn agent_run_create_injects_current_run_as_parent() {
        let (services, backend) = fabro_run_tool_services();
        let mut registry = ToolRegistry::new();
        register_fabro_run_tools(&mut registry, &services);
        let tool = registry
            .get(fabro_tool::FABRO_RUN_CREATE_TOOL_NAME)
            .expect("create tool should be registered");

        let output = (tool.executor)(
            serde_json::json!({
                "runs": [{
                    "workflow": "child.fabro",
                    "start": false
                }]
            }),
            tool_context(),
        )
        .await
        .expect("create tool should succeed");

        assert!(output.contains("created 1 Fabro run(s)"));
        assert_eq!(backend.created_parent_ids.lock().unwrap().as_slice(), &[
            Some(current_run_id())
        ]);
    }

    #[tokio::test]
    async fn agent_run_create_defaults_to_start_request_and_reports_pending_child() {
        let (services, backend) = fabro_run_tool_services();
        let mut registry = ToolRegistry::new();
        register_fabro_run_tools(&mut registry, &services);
        let tool = registry
            .get(fabro_tool::FABRO_RUN_CREATE_TOOL_NAME)
            .expect("create tool should be registered");

        let output = (tool.executor)(
            serde_json::json!({
                "runs": [{
                    "workflow": "child.fabro"
                }]
            }),
            tool_context(),
        )
        .await
        .expect("create tool should succeed");

        assert!(output.contains("created 1 Fabro run(s), start requested for 1"));
        assert_eq!(backend.started_run_ids.lock().unwrap().as_slice(), &[
            child_run_id()
        ]);
    }

    #[tokio::test]
    async fn agent_run_create_rejects_conflicting_parent_id() {
        let mut registry = ToolRegistry::new();
        let (services, _backend) = fabro_run_tool_services();
        register_fabro_run_tools(&mut registry, &services);
        let tool = registry
            .get(fabro_tool::FABRO_RUN_CREATE_TOOL_NAME)
            .expect("create tool should be registered");

        let err = (tool.executor)(
            serde_json::json!({
                "runs": [{
                    "workflow": "child.fabro",
                    "parent_id": "01KRBZW4DW0000000000000002",
                    "start": false
                }]
            }),
            tool_context(),
        )
        .await
        .expect_err("conflicting parent should be rejected");

        assert!(err.contains("parent_id"));
        assert!(err.contains("current run"));
    }

    #[tokio::test]
    async fn agent_run_tools_share_create_gather_and_events_backend() {
        let (services, backend) = fabro_run_tool_services();
        let mut registry = ToolRegistry::new();
        register_fabro_run_tools(&mut registry, &services);

        let create = registry
            .get(fabro_tool::FABRO_RUN_CREATE_TOOL_NAME)
            .unwrap();
        (create.executor)(
            serde_json::json!({
                "runs": [{
                    "workflow": "child.fabro",
                    "start": false
                }]
            }),
            tool_context(),
        )
        .await
        .expect("create should succeed");

        let gather = registry
            .get(fabro_tool::FABRO_RUN_GATHER_TOOL_NAME)
            .unwrap();
        let gathered = (gather.executor)(
            serde_json::json!({
                "run_ids": [child_run_id().to_string()],
                "timeout_seconds": 0
            }),
            tool_context(),
        )
        .await
        .expect("gather should succeed");

        let events = registry
            .get(fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME)
            .unwrap();
        let listed = (events.executor)(
            serde_json::json!({
                "action": "list",
                "run_id": child_run_id().to_string(),
                "first": 5
            }),
            tool_context(),
        )
        .await
        .expect("events should succeed");

        assert!(gathered.contains("gathered 1 Fabro run(s)"));
        assert!(listed.contains("returned 0 Fabro event(s)"));
        assert_eq!(backend.created_parent_ids.lock().unwrap().as_slice(), &[
            Some(current_run_id())
        ]);
    }

    #[tokio::test]
    async fn agent_run_interact_rejects_approval_actions_before_backend_dispatch() {
        for action in ["approve", "deny"] {
            let (services, backend) = fabro_run_tool_services();
            let mut registry = ToolRegistry::new();
            register_fabro_run_tools(&mut registry, &services);
            let tool = registry
                .get(fabro_tool::FABRO_RUN_INTERACT_TOOL_NAME)
                .expect("interact tool should be registered");

            let err = (tool.executor)(
                serde_json::json!({
                    "run_id": child_run_id().to_string(),
                    "action": action
                }),
                tool_context(),
            )
            .await
            .expect_err("workflow agents must not approve or deny runs");

            assert!(err.contains("must be performed by a user"), "{err}");
            assert!(
                backend.approved_run_ids.lock().unwrap().is_empty(),
                "approve backend should not be called for {action}"
            );
            assert!(
                backend.denied_run_ids.lock().unwrap().is_empty(),
                "deny backend should not be called for {action}"
            );
        }
    }

    #[tokio::test]
    async fn agent_run_pair_dispatches_to_shared_backend() {
        let (services, backend) = fabro_run_tool_services();
        let mut registry = ToolRegistry::new();
        register_fabro_run_tools(&mut registry, &services);
        let tool = registry
            .get(fabro_tool::FABRO_RUN_PAIR_TOOL_NAME)
            .expect("pair tool should be registered");

        let output = (tool.executor)(
            serde_json::json!({
                "action": "status",
                "run_id": child_run_id().to_string()
            }),
            tool_context(),
        )
        .await
        .expect("pair status should succeed");

        assert!(output.contains("read pair status for Fabro run"));
        assert!(output.contains("\"action\": \"status\""));
        assert_eq!(backend.pair_status_run_ids.lock().unwrap().as_slice(), &[
            child_run_id()
        ]);
    }

    fn fabro_run_tool_services() -> (FabroRunToolServices, Arc<MockRunToolBackend>) {
        let backend = Arc::new(MockRunToolBackend {
            child_id:            child_run_id(),
            created_parent_ids:  Mutex::new(Vec::new()),
            started_run_ids:     Mutex::new(Vec::new()),
            approved_run_ids:    Mutex::new(Vec::new()),
            denied_run_ids:      Mutex::new(Vec::new()),
            pair_status_run_ids: Mutex::new(Vec::new()),
        });
        let services = FabroRunToolServices {
            backend:            backend.clone(),
            current_run_id:     current_run_id(),
            base_cwd:           PathBuf::from("/tmp/fabro-test"),
            user_settings_path: PathBuf::from("/tmp/fabro-test/settings.toml"),
        };
        (services, backend)
    }

    fn tool_context() -> ToolContext {
        ToolContext {
            env:                 Arc::new(LocalSandbox::new(PathBuf::from("."))),
            cancel:              CancellationToken::new(),
            tool_env_provider:   None,
            session_id:          None,
            root_session_id:     None,
            tool_call_id:        None,
            agent_event_emitter: None,
        }
    }

    fn current_run_id() -> RunId {
        run_id("01KRBZW5C00000000000000001")
    }

    fn child_run_id() -> RunId {
        run_id("01KRBZW5C00000000000000002")
    }

    fn run_id(raw: &str) -> RunId {
        raw.parse().expect("test run id should parse")
    }

    fn run(run_id: RunId, parent_id: Option<RunId>, children_count: u64) -> Run {
        run_with_status(run_id, parent_id, children_count, RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        })
    }

    fn run_with_status(
        run_id: RunId,
        parent_id: Option<RunId>,
        children_count: u64,
        status: RunStatus,
    ) -> Run {
        Run {
            id: run_id,
            parent_id,
            children_count,
            title: "Test run".to_string(),
            goal: "Test run".to_string(),
            workflow: WorkflowRef {
                slug:       Some("simple".to_string()),
                name:       Some("Simple".to_string()),
                graph_name: None,
                node_count: 0,
                edge_count: 0,
            },
            automation: None,
            repository: None,
            created_by: test_support::test_principal(),
            origin: RunOrigin::default(),
            labels: HashMap::new(),
            lifecycle: RunLifecycle {
                status,
                approval: None,
                pending_control: None,
                queue_position: None,
                error: None,
                archived: false,
                archived_at: None,
            },
            sandbox: None,
            models: Vec::new(),
            source_directory: None,
            timestamps: RunTimestamps {
                created_at:    chrono::Utc.with_ymd_and_hms(2026, 5, 21, 12, 0, 0).unwrap(),
                started_at:    None,
                last_event_at: None,
                completed_at:  None,
            },
            timing: None,
            billing: None,
            size: fabro_types::RunSize::default(),
            ask_fabro: fabro_types::AskFabro::default(),
            diff: None,
            pull_request: None,
            current_question: None,
            superseded_by: None,
            retried_from: None,
            links: RunLinks { web: None },
        }
    }

    struct MockRunToolBackend {
        child_id:            RunId,
        created_parent_ids:  Mutex<Vec<Option<RunId>>>,
        started_run_ids:     Mutex<Vec<RunId>>,
        approved_run_ids:    Mutex<Vec<RunId>>,
        denied_run_ids:      Mutex<Vec<RunId>>,
        pair_status_run_ids: Mutex<Vec<RunId>>,
    }

    #[async_trait]
    impl FabroToolBackend for MockRunToolBackend {
        async fn create_run_from_spec(
            &self,
            _spec: &fabro_tool::ValidatedCreateRunSpec,
            _cwd: &Path,
            _user_settings_path: &Path,
            parent_id: Option<RunId>,
        ) -> anyhow::Result<RunId> {
            self.created_parent_ids.lock().unwrap().push(parent_id);
            Ok(self.child_id)
        }

        async fn resolve_run(&self, selector: &str) -> anyhow::Result<Run> {
            let run_id = selector.parse::<RunId>()?;
            Ok(run(run_id, None, 0))
        }

        async fn retrieve_run(&self, run_id: &RunId) -> anyhow::Result<Run> {
            assert_eq!(*run_id, self.child_id);
            Ok(run(self.child_id, Some(current_run_id()), 0))
        }

        async fn start_run(&self, run_id: &RunId, resume: bool) -> anyhow::Result<Run> {
            assert_eq!(*run_id, self.child_id);
            assert!(!resume);
            self.started_run_ids.lock().unwrap().push(*run_id);
            Ok(run_with_status(
                self.child_id,
                Some(current_run_id()),
                0,
                RunStatus::Pending {
                    reason: fabro_types::PendingReason::ApprovalRequired,
                },
            ))
        }

        async fn approve_run(&self, run_id: &RunId) -> anyhow::Result<Run> {
            self.approved_run_ids.lock().unwrap().push(*run_id);
            Ok(run_with_status(
                *run_id,
                Some(current_run_id()),
                0,
                RunStatus::Runnable,
            ))
        }

        async fn deny_run(&self, run_id: &RunId, _reason: Option<String>) -> anyhow::Result<Run> {
            self.denied_run_ids.lock().unwrap().push(*run_id);
            Ok(run_with_status(
                *run_id,
                Some(current_run_id()),
                0,
                RunStatus::Failed {
                    reason: FailureReason::ApprovalDenied,
                },
            ))
        }

        async fn cancel_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn interrupt_run(&self, _run_id: &RunId) -> anyhow::Result<()> {
            unreachable!()
        }

        async fn steer_run(
            &self,
            _run_id: &RunId,
            _text: String,
            _interrupt: bool,
        ) -> anyhow::Result<()> {
            unreachable!()
        }

        async fn archive_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn unarchive_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn list_store_runs(&self) -> anyhow::Result<Vec<Run>> {
            unreachable!()
        }

        async fn list_store_runs_by_parent(&self, _parent_id: RunId) -> anyhow::Result<Vec<Run>> {
            unreachable!()
        }

        async fn link_run_parent(
            &self,
            _child_id: &RunId,
            _parent_id: &RunId,
        ) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn unlink_run_parent(&self, _child_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn get_run_state(&self, _run_id: &RunId) -> anyhow::Result<RunProjection> {
            unreachable!()
        }

        async fn list_run_events(
            &self,
            _run_id: &RunId,
            _after: Option<u32>,
            _limit: Option<usize>,
        ) -> anyhow::Result<Vec<EventEnvelope>> {
            Ok(Vec::new())
        }

        async fn list_run_events_until(
            &self,
            _run_id: &RunId,
            _after: Option<u32>,
            _limit: usize,
        ) -> anyhow::Result<Vec<EventEnvelope>> {
            Ok(Vec::new())
        }

        async fn list_run_questions(
            &self,
            _run_id: &RunId,
        ) -> anyhow::Result<Vec<types::ApiQuestion>> {
            unreachable!()
        }

        async fn submit_run_answer(
            &self,
            _run_id: &RunId,
            _question_id: &str,
            _body: types::SubmitAnswerRequest,
        ) -> anyhow::Result<()> {
            unreachable!()
        }

        async fn get_run_pair_status(
            &self,
            run_id: &RunId,
        ) -> anyhow::Result<RunPairStatusResponse> {
            self.pair_status_run_ids.lock().unwrap().push(*run_id);
            Ok(RunPairStatusResponse {
                run_id:       *run_id,
                current_pair: None,
                targets:      Vec::new(),
            })
        }
    }

    fn new_file_tracking() -> FileTracking {
        FileTracking {
            pending: HashMap::new(),
            touched: HashSet::new(),
            last:    None,
        }
    }

    #[test]
    fn track_file_event_records_top_level_write() {
        let mut state = new_file_tracking();

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/tmp/foo.rs".to_string()),
        );

        track_file_event(
            &AgentEvent::ToolCallStarted {
                tool_name:    "write_file".to_string(),
                tool_call_id: "tc1".to_string(),
                arguments:    serde_json::Value::Object(args),
            },
            &mut state,
        );
        assert_eq!(state.pending.get("tc1").unwrap(), "/tmp/foo.rs");

        track_file_event(
            &AgentEvent::ToolCallCompleted {
                tool_call_id: "tc1".to_string(),
                tool_name:    "write_file".to_string(),
                is_error:     false,
                output:       serde_json::Value::String("ok".to_string()),
            },
            &mut state,
        );
        assert!(state.touched.contains("/tmp/foo.rs"));
        assert_eq!(state.last.as_deref(), Some("/tmp/foo.rs"));
    }

    #[test]
    fn track_file_event_tracks_edit_file() {
        let mut state = new_file_tracking();

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/src/lib.rs".to_string()),
        );

        track_file_event(
            &AgentEvent::ToolCallStarted {
                tool_name:    "edit_file".to_string(),
                tool_call_id: "tc-sub".to_string(),
                arguments:    serde_json::Value::Object(args),
            },
            &mut state,
        );
        assert_eq!(state.pending.get("tc-sub").unwrap(), "/src/lib.rs");

        track_file_event(
            &AgentEvent::ToolCallCompleted {
                tool_call_id: "tc-sub".to_string(),
                tool_name:    "edit_file".to_string(),
                is_error:     false,
                output:       serde_json::Value::String("ok".to_string()),
            },
            &mut state,
        );
        assert!(state.touched.contains("/src/lib.rs"));
        assert_eq!(state.last.as_deref(), Some("/src/lib.rs"));
    }

    #[test]
    fn track_file_event_error_removes_pending() {
        let mut state = new_file_tracking();

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/err.rs".to_string()),
        );

        track_file_event(
            &AgentEvent::ToolCallStarted {
                tool_name:    "edit_file".to_string(),
                tool_call_id: "tc-err".to_string(),
                arguments:    serde_json::Value::Object(args),
            },
            &mut state,
        );

        track_file_event(
            &AgentEvent::ToolCallCompleted {
                tool_call_id: "tc-err".to_string(),
                tool_name:    "edit_file".to_string(),
                is_error:     true,
                output:       serde_json::Value::String("failed".to_string()),
            },
            &mut state,
        );
        assert!(state.pending.is_empty());
        assert!(!state.touched.contains("/err.rs"));
    }

    #[test]
    fn build_profile_can_register_subagent_tools() {
        let mut profile = build_profile(
            "claude-opus-4-6",
            ProviderId::anthropic(),
            AgentProfileKind::Anthropic,
            Arc::new(Catalog::from_builtin().unwrap()),
        );
        let manager = Arc::new(TokioMutex::new(SubAgentManager::new(1)));
        let factory: SessionFactory = Arc::new(|| {
            panic!("factory should not be called in this test");
        });
        profile.register_subagent_tools(manager, factory, 0);

        let names = profile.tool_registry().names();
        assert!(names.contains(&"spawn_agent".to_string()));
        assert!(names.contains(&"send_input".to_string()));
        assert!(names.contains(&"wait".to_string()));
        assert!(names.contains(&"close_agent".to_string()));
    }

    #[test]
    fn api_backend_resolves_custom_catalog_provider_profile() {
        let settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.acme]
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[models.acme-llama]
provider = "acme"
display_name = "Acme Llama"
family = "llama"
training = "2026-01"
default = true

[models.acme-llama.limits]
context_window = 131072
max_output = 8192

[models.acme-llama.features]
tools = true
vision = false
reasoning = false
"#,
        )
        .unwrap();
        let catalog = Arc::new(Catalog::from_builtin_with_overrides(&settings).unwrap());
        let backend = AgentApiBackend::new_with_catalog(
            "acme-llama".to_string(),
            ProviderId::from("acme"),
            Vec::new(),
            Arc::new(EnvCredentialSource::new()),
            SteeringHub::for_tests(),
            catalog,
        );

        let provider = backend
            .resolve_provider_context("acme-llama", None)
            .unwrap();

        assert_eq!(provider.provider_id, ProviderId::from("acme"));
        assert_eq!(provider.profile_kind, AgentProfileKind::OpenAi);
    }

    #[test]
    fn api_backend_resolves_model_agent_profile_override() {
        let settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.acme]
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[models.acme-claude]
provider = "acme"
display_name = "Acme Claude"
family = "claude"
training = "2026-01"
default = true
agent_profile = "anthropic"
aliases = ["ac"]

[models.acme-claude.limits]
context_window = 131072
max_output = 8192

[models.acme-claude.features]
tools = true
vision = false
reasoning = false
"#,
        )
        .unwrap();
        let catalog = Arc::new(Catalog::from_builtin_with_overrides(&settings).unwrap());
        let backend = AgentApiBackend::new_with_catalog(
            "acme-claude".to_string(),
            ProviderId::from("acme"),
            Vec::new(),
            Arc::new(EnvCredentialSource::new()),
            SteeringHub::for_tests(),
            catalog,
        );

        let provider = backend.resolve_provider_context("ac", None).unwrap();

        assert_eq!(provider.provider_id, ProviderId::from("acme"));
        assert_eq!(provider.profile_kind, AgentProfileKind::Anthropic);
    }

    #[test]
    fn run_model_controls_apply_when_node_omits_controls() {
        let backend = AgentApiBackend::new_from_env(
            "gpt-5.4".to_string(),
            ProviderId::openai(),
            Vec::new(),
            SteeringHub::for_tests(),
        )
        .with_run_model_controls(fabro_types::settings::run::RunModelControls {
            reasoning_effort: Some("low".to_string()),
            speed:            Some("fast".to_string()),
        });
        let node = Node::new("work");

        let controls = backend.resolve_effective_request_controls(&node).unwrap();

        assert_eq!(controls.reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(controls.speed, Some(Speed::Fast));
    }

    #[test]
    fn node_controls_override_run_model_controls() {
        let backend = AgentApiBackend::new_from_env(
            "gpt-5.4".to_string(),
            ProviderId::openai(),
            Vec::new(),
            SteeringHub::for_tests(),
        )
        .with_run_model_controls(fabro_types::settings::run::RunModelControls {
            reasoning_effort: Some("low".to_string()),
            speed:            Some("fast".to_string()),
        });
        let mut node = Node::new("work");
        node.attrs.insert(
            "reasoning_effort".to_string(),
            fabro_graphviz::graph::AttrValue::String("high".to_string()),
        );
        node.attrs.insert(
            "speed".to_string(),
            fabro_graphviz::graph::AttrValue::String("standard".to_string()),
        );

        let controls = backend.resolve_effective_request_controls(&node).unwrap();

        assert_eq!(controls.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(controls.speed, Some(Speed::Standard));
    }

    #[test]
    fn omitted_reasoning_effort_stays_unset() {
        let backend = AgentApiBackend::new_from_env(
            "gpt-5.4".to_string(),
            ProviderId::openai(),
            Vec::new(),
            SteeringHub::for_tests(),
        );
        let node = Node::new("work");

        let controls = backend.resolve_effective_request_controls(&node).unwrap();

        assert_eq!(controls.reasoning_effort, None);
    }

    #[tokio::test]
    async fn api_backend_uses_source_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault
            .set(
                "ANTHROPIC_API_KEY",
                "anthropic-key",
                SecretType::Token,
                None,
            )
            .unwrap();
        let backend = AgentApiBackend::new(
            "claude-opus-4-6".to_string(),
            ProviderId::anthropic(),
            Vec::new(),
            Arc::new(VaultCredentialSource::with_env_lookup(
                Arc::new(AsyncRwLock::new(vault)),
                |_| None,
            )),
            SteeringHub::for_tests(),
        );

        let client = Client::from_source(backend.source.as_ref(), Arc::clone(&backend.catalog))
            .await
            .unwrap();

        assert_eq!(client.provider_names(), vec!["anthropic"]);
    }

    #[tokio::test]
    async fn one_shot_repairs_custom_output_schema_with_previous_assistant_message() {
        let server = MockServer::start();
        let first = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""type":"json_schema""#)
                .body_excludes(r#""role":"assistant""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(chat_completion_response("not json", 10, 1));
        });
        let repair = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""type":"json_schema""#)
                .body_includes(r#""role":"assistant""#)
                .body_includes("not json")
                .body_includes("output_schema");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(chat_completion_response(r#"{"passed":true}"#, 11, 2));
        });
        let backend = mock_api_backend(&server);
        let mut node = Node::new("audit");
        node.attrs
            .insert("output_schema".to_string(), custom_output_schema_attr());
        node.attrs
            .insert("output_retries".to_string(), AttrValue::Integer(1));
        let context = Context::new();
        let stage_scope = StageScope::for_handler(&context, &node.id);
        let emitter = Arc::new(Emitter::new(fabro_types::RunId::new()));
        let workspace = tempfile::tempdir().unwrap();
        let sandbox: Arc<dyn fabro_agent::Sandbox> =
            Arc::new(LocalSandbox::new(workspace.path().to_path_buf()));

        let result = backend
            .one_shot(OneShotRequest {
                node:          &node,
                prompt:        "Audit the result",
                system_prompt: None,
                emitter:       &emitter,
                stage_scope:   &stage_scope,
                sandbox:       &sandbox,
                cancel_token:  CancellationToken::new(),
            })
            .await
            .unwrap();

        first.assert_calls(1);
        repair.assert_calls(1);
        let CodergenResult::Text { text, usage, .. } = result else {
            panic!("one_shot should return text");
        };
        assert_eq!(text, r#"{"passed":true}"#);
        let usage = usage.expect("usage should be aggregated");
        assert_eq!(usage.tokens().input_tokens, 21);
        assert_eq!(usage.tokens().output_tokens, 3);
    }

    #[tokio::test]
    async fn agent_run_repairs_custom_output_schema_in_same_session() {
        let server = MockServer::start();
        let first = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""stream":true"#)
                .body_excludes(r#""role":"assistant""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream("not json", 20, 3));
        });
        let repair = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""stream":true"#)
                .body_includes(r#""role":"assistant""#)
                .body_includes("not json")
                .body_includes("output_schema");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream(r#"{"passed":true}"#, 21, 4));
        });
        let backend = mock_api_backend(&server);
        let mut node = Node::new("audit");
        node.attrs
            .insert("output_schema".to_string(), custom_output_schema_attr());
        node.attrs
            .insert("output_retries".to_string(), AttrValue::Integer(1));
        let context = Context::new();
        let emitter = Arc::new(Emitter::new(fabro_types::RunId::new()));
        let workspace = tempfile::tempdir().unwrap();
        let sandbox: Arc<dyn fabro_agent::Sandbox> =
            Arc::new(LocalSandbox::new(workspace.path().to_path_buf()));

        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "Audit the result",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await
            .unwrap();

        first.assert_calls(1);
        repair.assert_calls(1);
        let CodergenResult::Text { text, usage, .. } = result else {
            panic!("run should return text");
        };
        assert_eq!(text, r#"{"passed":true}"#);
        let usage = usage.expect("usage should be aggregated");
        assert_eq!(usage.tokens().input_tokens, 41);
        assert_eq!(usage.tokens().output_tokens, 7);
    }

    #[tokio::test]
    async fn api_backend_shutdown_closes_cached_sessions_once() {
        let backend = AgentApiBackend::new_from_env(
            "gpt-5.4".to_string(),
            ProviderId::openai(),
            Vec::new(),
            SteeringHub::for_tests(),
        );
        let emitter = Arc::new(Emitter::new(fabro_types::RunId::new()));
        let event_names = Arc::new(Mutex::new(Vec::new()));
        let event_names_for_listener = Arc::clone(&event_names);
        emitter.on_event(move |event| {
            event_names_for_listener
                .lock()
                .unwrap()
                .push(event.event_name().to_string());
        });

        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            Arc::new(ShutdownTestProvider) as Arc<dyn ProviderAdapter>,
        );
        let client = Client::new(providers, Some("openai".to_string()), Vec::new());
        let session = Session::new(
            client,
            Arc::new(ShutdownTestProfile::new()),
            Arc::new(fabro_agent::LocalSandbox::new(
                tempfile::tempdir().unwrap().path().to_path_buf(),
            )),
            SessionOptions::default(),
            None,
        );
        begin_session_lifecycle(&session, &emitter, None);
        backend
            .sessions
            .lock()
            .unwrap()
            .insert("thread-1".to_string(), session);

        backend.shutdown(&emitter).await;
        backend.shutdown(&emitter).await;

        assert_eq!(event_names.lock().unwrap().as_slice(), [
            "agent.session.started",
            "agent.session.ended"
        ]);
        assert!(backend.sessions.lock().unwrap().is_empty());
    }

    // --- Bridge guard tests ---

    fn failover_eligible_llm_error() -> LlmError {
        LlmError::Network {
            message: "boom".into(),
            source:  None,
        }
    }

    fn non_failover_llm_error() -> LlmError {
        LlmError::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail {
                message:     "bad key".into(),
                provider:    "openai".into(),
                status_code: Some(401),
                error_code:  None,
                retry_after: None,
                raw:         None,
            }),
        }
    }

    #[tokio::test]
    async fn spawn_bridge_task_sets_cancelled_and_cancels_session_token() {
        let run_token = CancellationToken::new();
        let interrupt_reason = Arc::new(Mutex::new(None));
        let session_token = CancellationToken::new();

        let handle = spawn_bridge_task(
            run_token.clone(),
            Arc::clone(&interrupt_reason),
            session_token.clone(),
        );

        assert!(!session_token.is_cancelled());
        assert!(interrupt_reason.lock().unwrap().is_none());

        run_token.cancel();
        handle.await.unwrap();

        assert!(session_token.is_cancelled());
        assert_eq!(
            *interrupt_reason.lock().unwrap(),
            Some(fabro_agent::InterruptReason::Cancelled)
        );
    }

    #[tokio::test]
    async fn spawn_bridge_task_preserves_existing_interrupt_reason() {
        let run_token = CancellationToken::new();
        let interrupt_reason = Arc::new(Mutex::new(Some(
            fabro_agent::InterruptReason::WallClockTimeout,
        )));
        let session_token = CancellationToken::new();

        let handle = spawn_bridge_task(
            run_token.clone(),
            Arc::clone(&interrupt_reason),
            session_token.clone(),
        );
        run_token.cancel();
        handle.await.unwrap();

        // Existing reason wins; the bridge does not overwrite a wall-clock
        // timeout already recorded by the session.
        assert_eq!(
            *interrupt_reason.lock().unwrap(),
            Some(fabro_agent::InterruptReason::WallClockTimeout)
        );
        assert!(session_token.is_cancelled());
    }

    #[tokio::test]
    async fn bridge_guard_drop_aborts_pending_task() {
        let run_token = CancellationToken::new();
        let interrupt_reason = Arc::new(Mutex::new(None));
        let session_token = CancellationToken::new();

        {
            let mut guard = SessionCancelBridgeGuard::new();
            guard.handle = Some(spawn_bridge_task(
                run_token.clone(),
                Arc::clone(&interrupt_reason),
                session_token.clone(),
            ));
            // guard dropped here
        }

        // Trigger the run token after the guard has been dropped. The aborted
        // task must not write to interrupt_reason or cancel session_token.
        run_token.cancel();
        // Yield enough times for any errant task to run.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        assert!(interrupt_reason.lock().unwrap().is_none());
        assert!(!session_token.is_cancelled());
    }

    #[tokio::test]
    async fn bridge_guard_replace_aborts_prior_task() {
        // First (prior) bridge wiring.
        let prior_run_token = CancellationToken::new();
        let prior_interrupt_reason = Arc::new(Mutex::new(None));
        let prior_session_token = CancellationToken::new();

        // Second (replacement) bridge wiring.
        let new_run_token = CancellationToken::new();
        let new_interrupt_reason = Arc::new(Mutex::new(None));
        let new_session_token = CancellationToken::new();

        let mut guard = SessionCancelBridgeGuard::new();
        guard.handle = Some(spawn_bridge_task(
            prior_run_token.clone(),
            Arc::clone(&prior_interrupt_reason),
            prior_session_token.clone(),
        ));

        // Replace with a new task pointing at different handles.
        guard.handle = {
            // Manually mirror `replace` semantics: abort then install.
            if let Some(h) = guard.handle.take() {
                h.abort();
            }
            Some(spawn_bridge_task(
                new_run_token.clone(),
                Arc::clone(&new_interrupt_reason),
                new_session_token.clone(),
            ))
        };

        // Cancelling the prior run token must not affect anything because the
        // prior task was aborted by `replace`.
        prior_run_token.cancel();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(prior_interrupt_reason.lock().unwrap().is_none());
        assert!(!prior_session_token.is_cancelled());

        // The replacement task must still be alive and react to its own token.
        new_run_token.cancel();
        guard.handle.take().unwrap().await.unwrap();
        assert_eq!(
            *new_interrupt_reason.lock().unwrap(),
            Some(fabro_agent::InterruptReason::Cancelled)
        );
        assert!(new_session_token.is_cancelled());
    }

    // --- classify_agent_error tests ---

    #[test]
    fn classify_interrupted_cancelled_is_cancelled() {
        let err = fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::Cancelled);
        assert!(matches!(
            classify_agent_error(err, true),
            AgentApiErrorDisposition::Cancelled
        ));
    }

    #[test]
    fn classify_interrupted_wall_clock_is_terminal_precondition() {
        let err = fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::WallClockTimeout);
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Precondition(msg)) => {
                assert!(msg.contains("wall-clock"));
            }
            _ => panic!("expected Terminal(Error::Precondition) for WallClockTimeout"),
        }
    }

    #[test]
    fn classify_failover_eligible_llm_returns_failover_when_allowed() {
        let err = fabro_agent::Error::Llm(failover_eligible_llm_error());
        assert!(matches!(
            classify_agent_error(err, true),
            AgentApiErrorDisposition::FailoverEligible(_)
        ));
    }

    #[test]
    fn classify_failover_eligible_llm_returns_terminal_when_not_allowed() {
        let err = fabro_agent::Error::Llm(failover_eligible_llm_error());
        match classify_agent_error(err, false) {
            AgentApiErrorDisposition::Terminal(Error::Llm(_)) => {}
            _ => panic!("expected Terminal(Error::Llm) when failover disallowed"),
        }
    }

    #[test]
    fn classify_non_failover_eligible_llm_is_terminal_llm() {
        let err = fabro_agent::Error::Llm(non_failover_llm_error());
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Llm(_)) => {}
            _ => panic!("expected Terminal(Error::Llm) for non-failover-eligible LLM error"),
        }
    }

    #[test]
    fn classify_session_closed_is_terminal_precondition() {
        let err = fabro_agent::Error::SessionClosed;
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Precondition(message)) => {
                assert!(message.contains("Agent session failed"));
            }
            _ => panic!("expected Terminal(Error::Precondition) for SessionClosed"),
        }
    }

    #[test]
    fn classify_invalid_state_is_terminal_precondition() {
        let err = fabro_agent::Error::InvalidState("oops".into());
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Precondition(message)) => {
                assert!(message.contains("Agent session failed"));
            }
            _ => panic!("expected Terminal(Error::Precondition) for InvalidState"),
        }
    }

    #[test]
    fn classify_tool_execution_is_terminal_precondition() {
        let err = fabro_agent::Error::ToolExecution("tool blew up".into());
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Precondition(message)) => {
                assert!(message.contains("Agent session failed"));
            }
            _ => panic!("expected Terminal(Error::Precondition) for ToolExecution"),
        }
    }
}
