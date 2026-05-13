use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fabro_agent::subagent::{SessionFactory, SubAgentManager};
use fabro_agent::{
    AgentEvent, AgentProfile, AnthropicProfile, CompletionCoordinator, GeminiProfile,
    OpenAiProfile, Sandbox, Session, SessionControlHandle, SessionOptions, StaticEnvProvider,
    ToolEnvProvider, Turn,
};
use fabro_auth::{CredentialSource, EnvCredentialSource};
use fabro_graphviz::graph::{AttrValue, Node};
use fabro_llm::client::Client;
use fabro_llm::types::{Message, ReasoningEffort, Request, Speed, TokenCounts};
use fabro_mcp::config::McpServerSettings;
use fabro_model::catalog::LlmCatalogSettings;
use fabro_model::{
    AgentProfileKind, Catalog, FallbackTarget, ModelRef, Provider, ProviderId, adapter,
};
use fabro_types::settings::run::RunModelControls;
use fabro_types::{SessionCapability, StageId};
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::super::agent::{CodergenBackend, CodergenResult, CodergenRunRequest, OneShotRequest};
use super::activation_lease::{ActivationLease, ActivationLeaseOptions};
use crate::context::WorkflowContext;
use crate::context::keys::Fidelity;
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope};
use crate::outcome::billed_model_usage_from_llm;
use crate::steering_hub::SteeringHub;

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

#[derive(Clone)]
struct ProviderContext {
    provider:     Provider,
    provider_id:  ProviderId,
    profile_kind: AgentProfileKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EffectiveRequestControls {
    pub(super) reasoning_effort: Option<ReasoningEffort>,
    pub(super) speed:            Option<Speed>,
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
    provider: Provider,
    provider_id: ProviderId,
    profile_kind: AgentProfileKind,
    catalog: Arc<Catalog>,
) -> Box<dyn AgentProfile> {
    match profile_kind {
        AgentProfileKind::OpenAi => Box::new(
            OpenAiProfile::new(model)
                .with_provider(provider)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
        AgentProfileKind::Gemini => Box::new(
            GeminiProfile::new(model)
                .with_provider(provider)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
        AgentProfileKind::Anthropic => Box::new(
            AnthropicProfile::new(model)
                .with_provider(provider)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
    }
}

fn default_profile_kind(provider: Provider) -> AgentProfileKind {
    match provider {
        Provider::Anthropic => AgentProfileKind::Anthropic,
        Provider::Gemini => AgentProfileKind::Gemini,
        Provider::OpenAi
        | Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception
        | Provider::OpenAiCompatible => AgentProfileKind::OpenAi,
    }
}

pub(super) fn effective_request_controls(
    catalog: &Catalog,
    run_model_controls: &RunModelControls,
    model: &str,
    node: &Node,
) -> Result<EffectiveRequestControls, Error> {
    let reasoning_effort = match control_attr(node, "reasoning_effort")
        .or(run_model_controls.reasoning_effort.as_deref())
    {
        Some(value) => Some(parse_reasoning_effort(node, value)?),
        None => legacy_reasoning_effort_default(catalog, model),
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

fn legacy_reasoning_effort_default(catalog: &Catalog, model: &str) -> Option<ReasoningEffort> {
    match catalog.model_settings(model) {
        Some(settings)
            if settings
                .controls
                .reasoning_effort
                .contains(&ReasoningEffort::High) =>
        {
            Some(ReasoningEffort::High)
        }
        Some(_) => None,
        None => Some(ReasoningEffort::High),
    }
}

fn profile_provider_for_catalog_provider(
    provider_id: &ProviderId,
    profile_kind: AgentProfileKind,
    adapter: &str,
) -> Provider {
    Provider::from_id(provider_id).unwrap_or(match (profile_kind, adapter) {
        (AgentProfileKind::Anthropic, _) => Provider::Anthropic,
        (AgentProfileKind::Gemini, _) => Provider::Gemini,
        (AgentProfileKind::OpenAi, "openai_compatible") => Provider::OpenAiCompatible,
        (AgentProfileKind::OpenAi, _) => Provider::OpenAi,
    })
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
            track_file_event(&event.event, &mut file_tracking.lock().unwrap());

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
    provider:           Provider,
    provider_id:        ProviderId,
    profile_kind:       AgentProfileKind,
    fallback_chain:     Vec<FallbackTarget>,
    sessions:           Mutex<HashMap<String, Session>>,
    tool_env:           Option<Arc<dyn ToolEnvProvider>>,
    mcp_servers:        Vec<McpServerSettings>,
    run_model_controls: RunModelControls,
    source:             Arc<dyn CredentialSource>,
    steering_hub:       Arc<SteeringHub>,
    catalog:            Arc<Catalog>,
}

impl AgentApiBackend {
    #[must_use]
    pub fn new(
        model: String,
        provider: Provider,
        fallback_chain: Vec<FallbackTarget>,
        source: Arc<dyn CredentialSource>,
        steering_hub: Arc<SteeringHub>,
    ) -> Self {
        let catalog = Arc::new(
            Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default())
                .expect("default catalog should build"),
        );
        Self::new_with_catalog(
            model,
            provider,
            provider.id(),
            default_profile_kind(provider),
            fallback_chain,
            source,
            steering_hub,
            catalog,
        )
    }

    #[must_use]
    pub fn new_with_catalog(
        model: String,
        provider: Provider,
        provider_id: ProviderId,
        profile_kind: AgentProfileKind,
        fallback_chain: Vec<FallbackTarget>,
        source: Arc<dyn CredentialSource>,
        steering_hub: Arc<SteeringHub>,
        catalog: Arc<Catalog>,
    ) -> Self {
        Self {
            model,
            provider,
            provider_id,
            profile_kind,
            fallback_chain,
            sessions: Mutex::new(HashMap::new()),
            tool_env: None,
            mcp_servers: Vec::new(),
            run_model_controls: RunModelControls::default(),
            source,
            steering_hub,
            catalog,
        }
    }

    #[must_use]
    pub fn new_from_env(
        model: String,
        provider: Provider,
        fallback_chain: Vec<FallbackTarget>,
        steering_hub: Arc<SteeringHub>,
    ) -> Self {
        Self::new(
            model,
            provider,
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
    pub fn with_run_model_controls(mut self, controls: RunModelControls) -> Self {
        self.run_model_controls = controls;
        self
    }

    fn effective_request_controls(
        &self,
        model: &str,
        node: &Node,
    ) -> Result<EffectiveRequestControls, Error> {
        effective_request_controls(self.catalog.as_ref(), &self.run_model_controls, model, node)
    }

    fn resolve_provider_context(
        &self,
        model: &str,
        provider_attr: Option<&str>,
    ) -> Result<ProviderContext, Error> {
        let provider_id = if let Some(provider) = provider_attr {
            let requested = ProviderId::from(provider);
            self.catalog
                .provider(&requested)
                .ok_or_else(|| {
                    Error::Precondition(format!("Provider \"{provider}\" is not configured"))
                })?
                .id
                .clone()
        } else if let Some(model) = self.catalog.get(model) {
            model.provider.clone()
        } else {
            self.provider_id.clone()
        };
        let Some(provider) = self.catalog.provider(&provider_id) else {
            return Ok(ProviderContext {
                provider:     self.provider,
                provider_id:  self.provider_id.clone(),
                profile_kind: self.profile_kind,
            });
        };
        let profile_kind = adapter::get(&provider.adapter)
            .map(|metadata| metadata.default_profile)
            .ok_or_else(|| {
                Error::Precondition(format!(
                    "Provider \"{provider_id}\" uses unknown adapter \"{}\"",
                    provider.adapter,
                ))
            })?;
        Ok(ProviderContext {
            provider: profile_provider_for_catalog_provider(
                &provider.id,
                profile_kind,
                &provider.adapter,
            ),
            provider_id: provider.id.clone(),
            profile_kind,
        })
    }

    async fn create_session(
        &self,
        node: &Node,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<Session, Error> {
        let model = node.model().unwrap_or(&self.model);
        let provider = self.resolve_provider_context(model, node.provider())?;
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
    ) -> Result<Session, Error> {
        let controls =
            effective_request_controls(catalog.as_ref(), run_model_controls, model, node)?;
        let client = Client::from_source(source, Arc::clone(&catalog))
            .await
            .map_err(|e| Error::handler_with_source("Failed to create LLM client", e))?;

        let mut profile = build_profile(
            model,
            provider.provider,
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
        let factory: SessionFactory = Arc::new(move || {
            let child_profile: Arc<dyn AgentProfile> = Arc::from(build_profile(
                &factory_model,
                factory_provider.provider,
                factory_provider.provider_id.clone(),
                factory_provider.profile_kind,
                Arc::clone(&factory_catalog),
            ));
            let mut session = Session::new(
                factory_client.clone(),
                child_profile,
                Arc::clone(&factory_env),
                SessionOptions {
                    reasoning_effort: controls.reasoning_effort,
                    speed: controls.speed,
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
        let handle = session.control_handle();
        let lease = ActivationLease::activate(
            ActivationLeaseOptions {
                stage_id:     stage_id.clone(),
                session_id:   session.id().to_string(),
                thread_id:    thread_id.map(str::to_string),
                provider:     Some(session.provider_id().to_string()),
                model:        Some(session.model().to_string()),
                capabilities: vec![SessionCapability::Steer],
                hub:          Arc::clone(&self.steering_hub),
                emitter:      Arc::clone(emitter),
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
            .unwrap()
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
}

#[async_trait]
impl CodergenBackend for AgentApiBackend {
    async fn shutdown(&self, emitter: &Arc<Emitter>) {
        self.shutdown_cached_sessions(emitter);
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
        let controls = self.effective_request_controls(model, node)?;

        let max_tokens = node
            .max_tokens()
            .or_else(|| self.catalog.get(model).and_then(|m| m.limits.max_output));

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(Message::system(sys));
        }
        messages.push(Message::user(prompt));

        let request = Request {
            model: model.to_string(),
            messages,
            provider: Some(provider_id),
            reasoning_effort: controls.reasoning_effort,
            speed: controls.speed,
            tools: None,
            tool_choice: None,
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens,
            stop_sequences: None,
            metadata: None,
            provider_options: None,
        };

        // Build per-request fallback chain: if the node overrides the provider,
        // no failover is available; otherwise use the backend's.
        let fallback_chain: &[FallbackTarget] = if node.provider().is_some() {
            &[]
        } else {
            &self.fallback_chain
        };

        let result = client.complete(&request).await;

        let default_provider = self.provider_id.to_string();

        let (response, actual_model, actual_provider, actual_speed) = match result {
            Ok(resp) => (
                resp,
                request.model.clone(),
                request
                    .provider
                    .clone()
                    .unwrap_or_else(|| default_provider.clone()),
                controls.speed,
            ),
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
                            .and_then(|m| m.limits.max_output)
                    });
                    let fallback_controls = self.effective_request_controls(&target.model, node)?;

                    let fallback_request = Request {
                        model: target.model.clone(),
                        provider: Some(target.provider.clone()),
                        max_tokens,
                        reasoning_effort: fallback_controls.reasoning_effort,
                        speed: fallback_controls.speed,
                        ..request.clone()
                    };

                    match client.complete(&fallback_request).await {
                        Ok(resp) => {
                            found = Some((
                                resp,
                                target.model.clone(),
                                target.provider.clone(),
                                fallback_controls.speed,
                            ));
                            break;
                        }
                        Err(err) if err.failover_eligible() => {
                            last_err = err;
                        }
                        Err(err) => return Err(Error::Llm(err)),
                    }
                }

                match found {
                    Some(triple) => triple,
                    None => return Err(Error::Llm(last_err)),
                }
            }
            Err(sdk_err) => return Err(Error::Llm(sdk_err)),
        };

        let stage_usage = billed_model_usage_from_llm(
            self.catalog.as_ref(),
            &ModelRef {
                provider: ProviderId::from(actual_provider),
                model_id: actual_model,
                speed:    actual_speed,
            },
            &response.usage,
        );

        Ok(CodergenResult::Text {
            text:              response.text(),
            usage:             Some(stage_usage),
            files_touched:     Vec::new(),
            last_file_touched: None,
        })
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
            let existing = self.sessions.lock().unwrap().remove(key);
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

        // Record turn count before processing so we only aggregate new usage.
        let mut turns_before = session.history().turns().len();

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
                session.process_input(prompt).await
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
                        turns_before = session.history().turns().len();

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
                        match session.process_input(prompt).await {
                            Ok(()) => {
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

        // Aggregate token usage only from new turns (prevents double-counting on
        // reuse).
        let mut total_usage = TokenCounts::default();
        for turn in &session.history().turns()[turns_before..] {
            if let Turn::Assistant { usage, .. } = turn {
                total_usage += *usage.clone();
            }
        }

        let billing_controls = self.effective_request_controls(session.model(), node)?;
        let stage_usage = billed_model_usage_from_llm(
            self.catalog.as_ref(),
            &ModelRef {
                provider: session.provider_id(),
                model_id: session.model().to_string(),
                speed:    billing_controls.speed,
            },
            &total_usage,
        );

        // Extract last assistant response from the session history.
        let response = session
            .history()
            .turns()
            .iter()
            .rev()
            .find_map(|turn| {
                if let Turn::Assistant { content, .. } = turn {
                    if !content.is_empty() {
                        return Some(content.clone());
                    }
                }
                None
            })
            .unwrap_or_default();

        // Collect files_touched from the shared tracking state.
        let (files_touched, last_file_touched) = {
            let s = file_tracking.lock().unwrap();
            let mut v: Vec<String> = s.touched.iter().cloned().collect();
            v.sort();
            (v, s.last.clone())
        };

        if let Some(lease) = lease.take() {
            lease.release();
        }

        // Cache session back for reuse on success. Detach the bridge first so
        // the cached session is not left wired to this run's cancel token.
        if let Some(key) = reuse_key {
            bridge.abort();
            self.sessions.lock().unwrap().insert(key, session);
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
        })
    }
}

/// Coordinator that lets the agent loop ask the workflow layer whether to
/// keep iterating after a no-tool natural completion. Implements the
/// "close-the-door" pattern: detach only if the queue is empty, otherwise
/// report `true` so the loop drains.
struct SteeringCompletionCoordinator {
    handle: SessionControlHandle,
    lease:  Mutex<Option<Arc<ActivationLease>>>,
}

impl CompletionCoordinator for SteeringCompletionCoordinator {
    fn on_natural_completion(&self) -> bool {
        let mut lease = self.lease.lock().expect("activation lease lock poisoned");
        let Some(active_lease) = lease.as_ref() else {
            return false;
        };
        if active_lease.release_if_no_pending_control_work(&self.handle) {
            lease.take();
            false
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use fabro_agent::subagent::SessionFactory;
    use fabro_agent::{AgentProfile, ToolRegistry};
    use fabro_auth::{AuthCredential, AuthDetails, EnvCredentialSource, VaultCredentialSource};
    use fabro_llm::provider::{ProviderAdapter, StreamEventStream};
    use fabro_llm::{Error as LlmError, ProviderErrorDetail, ProviderErrorKind};
    use fabro_vault::{SecretType, Vault};
    use futures::stream;
    use tokio::sync::RwLock as AsyncRwLock;

    use super::*;

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
        fn provider(&self) -> Provider {
            Provider::OpenAi
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

    #[test]
    fn agent_backend_stores_config() {
        let backend = AgentApiBackend::new_from_env(
            "claude-opus-4-6".to_string(),
            Provider::OpenAi,
            Vec::new(),
            SteeringHub::for_tests(),
        );
        assert_eq!(backend.model, "claude-opus-4-6");
        assert_eq!(backend.provider, Provider::OpenAi);
    }

    #[test]
    fn agent_backend_initializes_empty_sessions() {
        let backend = AgentApiBackend::new_from_env(
            "claude-opus-4-6".to_string(),
            Provider::Anthropic,
            Vec::new(),
            SteeringHub::for_tests(),
        );
        assert!(backend.sessions.lock().unwrap().is_empty());
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
            Provider::Anthropic,
            Provider::Anthropic.id(),
            AgentProfileKind::Anthropic,
            Arc::new(Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default()).unwrap()),
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
[providers.venice]
adapter = "openai_compatible"
base_url = "https://api.venice.ai/api/v1"
credentials = ["env:VENICE_API_KEY"]

[models.venice-llama]
provider = "venice"
display_name = "Venice Llama"
family = "llama"
training = "2026-01"
default = true

[models.venice-llama.limits]
context_window = 131072
max_output = 8192

[models.venice-llama.features]
tools = true
vision = false
reasoning = false
effort = false
"#,
        )
        .unwrap();
        let catalog = Arc::new(Catalog::from_builtin_with_overrides(&settings).unwrap());
        let backend = AgentApiBackend::new_with_catalog(
            "venice-llama".to_string(),
            Provider::OpenAiCompatible,
            ProviderId::from("venice"),
            AgentProfileKind::OpenAi,
            Vec::new(),
            Arc::new(EnvCredentialSource::new()),
            SteeringHub::for_tests(),
            catalog,
        );

        let provider = backend
            .resolve_provider_context("venice-llama", None)
            .unwrap();

        assert_eq!(provider.provider_id, ProviderId::from("venice"));
        assert_eq!(provider.profile_kind, AgentProfileKind::OpenAi);
        assert_eq!(provider.provider, Provider::OpenAiCompatible);
    }

    #[test]
    fn run_model_controls_apply_when_node_omits_controls() {
        let backend = AgentApiBackend::new_from_env(
            "gpt-5.4".to_string(),
            Provider::OpenAi,
            Vec::new(),
            SteeringHub::for_tests(),
        )
        .with_run_model_controls(fabro_types::settings::run::RunModelControls {
            reasoning_effort: Some("low".to_string()),
            speed:            Some("fast".to_string()),
        });
        let node = Node::new("work");

        let controls = backend
            .effective_request_controls("gpt-5.4", &node)
            .unwrap();

        assert_eq!(controls.reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(controls.speed, Some(Speed::Fast));
    }

    #[test]
    fn node_controls_override_run_model_controls() {
        let backend = AgentApiBackend::new_from_env(
            "gpt-5.4".to_string(),
            Provider::OpenAi,
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

        let controls = backend
            .effective_request_controls("gpt-5.4", &node)
            .unwrap();

        assert_eq!(controls.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(controls.speed, Some(Speed::Standard));
    }

    #[test]
    fn known_model_without_effort_omits_legacy_high_default() {
        let backend = AgentApiBackend::new_from_env(
            "kimi-k2.5".to_string(),
            Provider::Kimi,
            Vec::new(),
            SteeringHub::for_tests(),
        );
        let node = Node::new("work");

        let controls = backend
            .effective_request_controls("kimi-k2.5", &node)
            .unwrap();

        assert_eq!(controls.reasoning_effort, None);
    }

    #[tokio::test]
    async fn api_backend_uses_source_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault
            .set(
                "anthropic",
                &serde_json::to_string(&AuthCredential {
                    provider: Provider::Anthropic.id(),
                    details:  AuthDetails::ApiKey {
                        key: "anthropic-key".to_string(),
                    },
                })
                .unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();
        let backend = AgentApiBackend::new(
            "claude-opus-4-6".to_string(),
            Provider::Anthropic,
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
    async fn api_backend_shutdown_closes_cached_sessions_once() {
        let backend = AgentApiBackend::new_from_env(
            "gpt-5.4".to_string(),
            Provider::OpenAi,
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
