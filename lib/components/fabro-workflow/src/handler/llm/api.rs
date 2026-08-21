use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fabro_agent::subagent::{SessionFactory, SubAgentSupervisor};
use fabro_agent::tool_registry::{RegisteredTool, ToolContext, ToolRegistry, ToolSource};
use fabro_agent::{
    AgentEvent, AgentProfile, AgentProfileBuilder, CompletionCoordinator, Message as AgentMessage,
    Sandbox, Session, SessionOptions, SessionShutdownReason, StaticEnvProvider, ToolEnvProvider,
    ToolSecrets, WebFetchSummarizer, canonical_tool_name, register_question_tools,
};
use fabro_auth::CredentialSource;
use fabro_graphviz::graph::{AttrValue, Node};
use fabro_llm::client::Client;
use fabro_llm::types::{
    Message, ReasoningEffort, Request, Response, ResponseFormat, Speed, TokenCounts,
    ToolDefinition as LlmToolDefinition,
};
use fabro_mcp::config::McpServerSettings;
#[cfg(test)]
use fabro_model::catalog::LlmCatalogSettings;
use fabro_model::{
    AgentProfileKind, Catalog, FallbackTarget, ModelHandle, ModelRef, ProviderId, RunBudget,
    UsdMicros,
};
use fabro_types::settings::run::RunModelControls;
use fabro_types::{FailoverProps, PermissionLevel, RunId, SessionCapability, StageId, StageTiming};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
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
use crate::event::{Emitter, Event, RunNoticeCode, RunNoticeLevel, StageScope};
use crate::model_fallback::{ModelFallbackNotice, ModelFallbackPolicy};
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

fn classify_agent_error(
    err: fabro_agent::Error,
    allow_failover: bool,
    budget: Option<&RunBudget>,
) -> AgentApiErrorDisposition {
    match err {
        fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::Cancelled) => {
            AgentApiErrorDisposition::Cancelled
        }
        fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::WallClockTimeout) => {
            AgentApiErrorDisposition::Terminal(Error::Precondition(
                "Agent session hit its wall-clock timeout".to_string(),
            ))
        }
        fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::BudgetExhausted) => {
            let message = budget.and_then(RunBudget::exceeded).map_or_else(
                || "run budget exhausted".to_string(),
                |usage| usage.exceeded_message(),
            );
            AgentApiErrorDisposition::Terminal(Error::BudgetExhausted(message))
        }
        fabro_agent::Error::Llm(err) if allow_failover && err.failover_eligible() => {
            AgentApiErrorDisposition::FailoverEligible(err)
        }
        fabro_agent::Error::Llm(err) => AgentApiErrorDisposition::Terminal(Error::Llm(err)),
        other @ (fabro_agent::Error::SessionClosed
        | fabro_agent::Error::Compaction(_)
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

async fn discard_session(
    session: &mut Session,
    lease: &mut Option<Arc<ActivationLease>>,
    event_forwarder: &mut EventForwarder,
    emitter: &Arc<Emitter>,
) {
    if let Some(lease) = lease.take() {
        lease.release();
    }
    let session_id = session.id().to_string();
    let reason = if session.cancel_token().is_cancelled() {
        SessionShutdownReason::Cancelled
    } else {
        SessionShutdownReason::Error
    };
    session.shutdown(reason).await;
    event_forwarder.wait_for_session_end().await;
    // The agent-layer SessionEnded event is deliberately filtered by the
    // bridge. This workflow-level event owns the durable session lifecycle,
    // even when process_input already performed internal shutdown.
    emitter.emit(&Event::AgentSessionEnded {
        session_id,
        parent_session_id: None,
    });
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
        } if matches!(canonical_tool_name(tool_name), "write_file" | "edit_file") => {
            if let Some(path) = arguments
                .get("file_path")
                .or_else(|| arguments.get("path"))
                .and_then(|v| v.as_str())
            {
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
///
/// The returned handle exposes a per-input barrier. A successful
/// `process_input_with_runtime` emits `ProcessingEnd` after all events for
/// that input, so waiting for the barrier keeps terminal stage events from
/// overtaking queued agent events.
struct EventForwarder {
    processing_end_rx: mpsc::UnboundedReceiver<()>,
    session_end_rx:    mpsc::UnboundedReceiver<()>,
    task:              JoinHandle<()>,
}

impl EventForwarder {
    async fn wait_for_processing_end(&mut self) {
        if self.processing_end_rx.recv().await.is_none() {
            tracing::warn!("Agent event forwarder stopped before processing input events");
        }
    }

    async fn wait_for_session_end(&mut self) {
        if self.session_end_rx.recv().await.is_none() {
            tracing::warn!("Agent event forwarder stopped before session shutdown events");
        }
    }

    fn abort(&self) {
        self.task.abort();
    }
}

impl Drop for EventForwarder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn spawn_event_forwarder(
    session: &Session,
    node_id: String,
    scope: StageScope,
    emitter: Arc<Emitter>,
    file_tracking: Arc<Mutex<FileTracking>>,
    run_budget: Option<Arc<RunBudget>>,
) -> EventForwarder {
    let mut rx = session.subscribe();
    let root_session_id = session.id().to_string();
    let (processing_end_tx, processing_end_rx) = mpsc::unbounded_channel();
    let (session_end_tx, session_end_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let is_root_processing_end = event.session_id == root_session_id
                && event.parent_session_id.is_none()
                && matches!(&event.event, AgentEvent::ProcessingEnd);
            let is_root_session_end = event.session_id == root_session_id
                && event.parent_session_id.is_none()
                && matches!(&event.event, AgentEvent::SessionEnded);

            // Reset watchdog on every event, including streaming deltas
            emitter.touch();

            // The session charges the budget as each response's usage
            // arrives; this task is the closest site with a run emitter, so
            // it drains the one-shot warning notices.
            if matches!(&event.event, AgentEvent::AssistantMessage { .. }) {
                if let Some(budget) = &run_budget {
                    for warning in budget.take_pending_warnings() {
                        emitter.notice_scoped(
                            RunNoticeLevel::Warn,
                            RunNoticeCode::BudgetNearlyExhausted,
                            warning.warning_message(),
                            &scope,
                        );
                    }
                }
            }

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

            if is_root_processing_end {
                let _ = processing_end_tx.send(());
            }
            if is_root_session_end {
                let _ = session_end_tx.send(());
            }
        }
    });

    EventForwarder {
        processing_end_rx,
        session_end_rx,
        task,
    }
}

/// LLM backend that delegates to an `agent` Session per invocation.
///
/// For `full` fidelity nodes sharing a thread key, sessions are cached
/// and reused so the LLM sees the full conversation history.
pub struct AgentApiBackend {
    model:                String,
    provider_id:          ProviderId,
    fallbacks:            ModelFallbackPolicy,
    sessions:             Mutex<HashMap<String, CachedAgentSession>>,
    /// Messages of fallback-plan notices already emitted for this run, so the
    /// same configuration warning is not repeated on every LLM call.
    emitted_plan_notices: Mutex<HashSet<String>>,
    /// Shared run budget: charged by agent sessions and the one-shot prompt
    /// path; `None` means unlimited.
    run_budget:           Option<Arc<RunBudget>>,
    tool_env:             Option<Arc<dyn ToolEnvProvider>>,
    mcp_servers:          Vec<McpServerSettings>,
    tool_secrets:         ToolSecrets,
    run_model_controls:   RunModelControls,
    source:               Arc<dyn CredentialSource>,
    steering_hub:         Arc<SteeringHub>,
    catalog:              Arc<Catalog>,
    fabro_run_tools:      Option<FabroRunToolServices>,
}

struct CachedAgentSession {
    session:       Session,
    fallback_plan: FallbackPlan,
}

#[derive(Clone, Debug)]
struct LlmRoute {
    target:   FallbackTarget,
    controls: EffectiveRequestControls,
}

#[derive(Clone, Debug)]
struct FallbackPlan {
    original:  LlmRoute,
    remaining: Vec<LlmRoute>,
    /// 0 addresses the original route; N addresses `remaining[N - 1]`.
    position:  usize,
}

impl FallbackPlan {
    fn current(&self) -> &LlmRoute {
        self.route_at(self.position)
    }

    /// The route that was active before the most recent [`Self::advance`].
    fn previous(&self) -> &LlmRoute {
        self.route_at(self.position.saturating_sub(1))
    }

    fn route_at(&self, position: usize) -> &LlmRoute {
        position
            .checked_sub(1)
            .map_or(&self.original, |index| &self.remaining[index])
    }

    fn attempt(&self) -> u32 {
        u32::try_from(self.position).unwrap_or(u32::MAX)
    }

    #[must_use]
    fn has_next(&self) -> bool {
        self.position < self.remaining.len()
    }

    /// Move to the next fallback route. Returns false when the plan is
    /// exhausted.
    fn advance(&mut self) -> bool {
        if self.has_next() {
            self.position += 1;
            true
        } else {
            false
        }
    }
}

/// Request controls resolved for one fallback target.
enum FallbackControls {
    /// The target can serve the request with these controls.
    Usable(EffectiveRequestControls),
    /// The target advertises reasoning levels, but none is near the requested
    /// effort.
    NoNearbyReasoningLevel(ReasoningEffort),
}

struct OneShotCompletion {
    response: Response,
    model:    ModelRef,
}

/// One agent invocation's live session, cancel bridge, activation lease,
/// event forwarding, and accounting state.
///
/// Failover discards and replaces the session while the accumulated usage,
/// cost, and timing keep counting across routes.
struct LiveAgentInvocation {
    session:            Session,
    bridge:             SessionCancelBridgeGuard,
    lease:              Option<Arc<ActivationLease>>,
    event_forwarder:    EventForwarder,
    file_tracking:      Arc<Mutex<FileTracking>>,
    run_budget:         Option<Arc<RunBudget>>,
    total_usage:        TokenCounts,
    total_cost:         Option<UsdMicros>,
    inference_duration: Duration,
    tool_duration:      Duration,
}

impl LiveAgentInvocation {
    /// Tear down the current session: detach the cancel bridge, release the
    /// lease, shut the session down, and stop event forwarding.
    async fn abort_and_discard(&mut self, emitter: &Arc<Emitter>) {
        self.bridge.abort();
        discard_session(
            &mut self.session,
            &mut self.lease,
            &mut self.event_forwarder,
            emitter,
        )
        .await;
        self.event_forwarder.abort();
    }

    /// Tear down the session for a failed agent call and classify the error.
    /// Terminal and cancelled errors come back as `Err` for the caller to
    /// propagate; a failover-eligible error comes back as `Ok` so the caller
    /// can continue the fallback plan.
    async fn discard_for_error(
        &mut self,
        error: fabro_agent::Error,
        allow_failover: bool,
        emitter: &Arc<Emitter>,
    ) -> Result<fabro_llm::Error, Error> {
        let disposition = classify_agent_error(error, allow_failover, self.run_budget.as_deref());
        self.abort_and_discard(emitter).await;
        match disposition {
            AgentApiErrorDisposition::Cancelled => Err(Error::Cancelled),
            AgentApiErrorDisposition::Terminal(error) => Err(error),
            AgentApiErrorDisposition::FailoverEligible(error) => Ok(error),
        }
    }

    fn record_input_timing(&mut self) {
        let timing = self.session.last_input_timing();
        self.inference_duration = self.inference_duration.saturating_add(timing.inference);
        self.tool_duration = self.tool_duration.saturating_add(timing.tool);
    }

    async fn record_input_usage(&mut self) {
        self.event_forwarder.wait_for_processing_end().await;
        self.total_usage += self.session.last_input_usage();
        UsdMicros::accumulate(&mut self.total_cost, self.session.last_input_cost());
    }
}

impl AgentApiBackend {
    #[must_use]
    pub fn new(
        model: String,
        provider_id: impl Into<ProviderId>,
        fallbacks: ModelFallbackPolicy,
        source: Arc<dyn CredentialSource>,
        steering_hub: Arc<SteeringHub>,
    ) -> Self {
        let catalog = Arc::new(Catalog::from_builtin().expect("default catalog should build"));
        Self::new_with_catalog(
            model,
            provider_id.into(),
            fallbacks,
            source,
            steering_hub,
            catalog,
        )
    }

    #[must_use]
    pub fn new_with_catalog(
        model: String,
        provider_id: ProviderId,
        fallbacks: ModelFallbackPolicy,
        source: Arc<dyn CredentialSource>,
        steering_hub: Arc<SteeringHub>,
        catalog: Arc<Catalog>,
    ) -> Self {
        Self {
            model,
            provider_id,
            fallbacks,
            sessions: Mutex::new(HashMap::new()),
            emitted_plan_notices: Mutex::new(HashSet::new()),
            run_budget: None,
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

    #[must_use]
    pub fn with_run_budget(mut self, budget: Option<Arc<RunBudget>>) -> Self {
        self.run_budget = budget;
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

    fn fallback_controls_for_target(
        &self,
        target: &FallbackTarget,
        requested: EffectiveRequestControls,
    ) -> FallbackControls {
        let Some(requested_effort) = requested.reasoning_effort else {
            return FallbackControls::Usable(requested);
        };
        let Some(offering) = self
            .catalog
            .get_on_provider(&target.provider, target.model.as_str())
        else {
            // A catalog-unknown passthrough target has no advertised controls.
            // Preserve the request and let the provider validate it.
            return FallbackControls::Usable(requested);
        };
        let effective_effort = self.catalog.settings_for(offering).and_then(|settings| {
            requested_effort.closest_supported(&settings.controls.reasoning_effort)
        });
        match effective_effort {
            Some(effort) => FallbackControls::Usable(EffectiveRequestControls {
                reasoning_effort: Some(effort),
                speed:            requested.speed,
            }),
            None => FallbackControls::NoNearbyReasoningLevel(requested_effort),
        }
    }

    fn fallback_plan(
        &self,
        model: &str,
        provider: &ProviderId,
        requested_controls: EffectiveRequestControls,
    ) -> (FallbackPlan, Vec<ModelFallbackNotice>) {
        let primary_model = self.catalog.canonical_model_id(provider, model);
        let original = LlmRoute {
            target:   FallbackTarget::new(provider, &primary_model),
            controls: requested_controls,
        };
        let Some(configured) = self.fallbacks.chain_for_canonical(&primary_model) else {
            return (
                FallbackPlan {
                    original,
                    remaining: Vec::new(),
                    position: 0,
                },
                Vec::new(),
            );
        };

        let mut remaining = Vec::new();
        let mut notices = Vec::new();
        for target in configured {
            // The resolver already de-duplicated the chain; only the primary
            // target, which the resolver cannot know, needs filtering here.
            if *target == original.target {
                continue;
            }

            let controls = match self.fallback_controls_for_target(target, requested_controls) {
                FallbackControls::Usable(controls) => controls,
                FallbackControls::NoNearbyReasoningLevel(requested_effort) => {
                    notices.push(ModelFallbackNotice::NoNearbyReasoningLevel {
                        requested_model: original.target.model.to_string(),
                        target: target.clone(),
                        requested_effort,
                    });
                    continue;
                }
            };
            remaining.push(LlmRoute {
                target: target.clone(),
                controls,
            });
        }

        if !configured.is_empty() && remaining.is_empty() {
            notices.push(ModelFallbackNotice::ChainEmpty {
                requested_model: original.target.model.to_string(),
            });
        }

        (
            FallbackPlan {
                original,
                remaining,
                position: 0,
            },
            notices,
        )
    }

    fn emit_fallback_plan_notices(
        &self,
        notices: &[ModelFallbackNotice],
        emitter: &Emitter,
        stage_scope: &StageScope,
    ) {
        let mut emitted = self
            .emitted_plan_notices
            .lock()
            .expect("notices mutex is never poisoned: no code panics while holding this lock");
        for notice in notices {
            let message = notice.message();
            if emitted.insert(message.clone()) {
                emitter.notice_scoped(notice.level(), notice.code(), message, stage_scope);
            }
        }
    }

    async fn create_session_with_plan(
        &self,
        node: &Node,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<(CachedAgentSession, Vec<ModelFallbackNotice>), Error> {
        let model = node.model().unwrap_or(&self.model);
        let provider = routing::resolve_node_provider_context(
            self.catalog.as_ref(),
            &self.provider_id,
            &self.model,
            node,
        )?;
        let controls = self.resolve_effective_request_controls(node)?;
        let (fallback_plan, notices) = self.fallback_plan(model, &provider.provider_id, controls);
        let route = fallback_plan.current();
        let route_provider = self.resolve_provider_context(
            route.target.model.as_str(),
            Some(route.target.provider.as_str()),
        )?;
        let session = Self::create_session_for(
            route.target.model.as_str(),
            route_provider,
            route.controls,
            node,
            sandbox,
            self.source.as_ref(),
            Arc::clone(&self.catalog),
            self.tool_env.as_ref(),
            tool_hooks,
            self.mcp_servers.clone(),
            self.tool_secrets.clone(),
            self.fabro_run_tools.clone(),
            self.run_budget.clone(),
        )
        .await?;
        Ok((
            CachedAgentSession {
                session,
                fallback_plan,
            },
            notices,
        ))
    }

    async fn create_session_for(
        model: &str,
        provider: ProviderContext,
        controls: EffectiveRequestControls,
        node: &Node,
        sandbox: &Arc<dyn Sandbox>,
        source: &dyn CredentialSource,
        catalog: Arc<Catalog>,
        tool_env: Option<&Arc<dyn ToolEnvProvider>>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
        mcp_servers: Vec<McpServerSettings>,
        tool_secrets: ToolSecrets,
        fabro_run_tools: Option<FabroRunToolServices>,
        run_budget: Option<Arc<RunBudget>>,
    ) -> Result<Session, Error> {
        let client = Client::from_source(source, Arc::clone(&catalog))
            .await
            .map_err(|e| Error::handler_with_source("Failed to create LLM client", e))?;

        let profile_builder = AgentProfileBuilder::new(
            provider.profile_kind,
            provider.provider_id.clone(),
            model,
            Arc::clone(&catalog),
        )
        .with_tool_secrets(tool_secrets);
        let profile_builder = if provider.profile_kind == AgentProfileKind::Claude5 {
            profile_builder.with_web_fetch_summarizer(Some(WebFetchSummarizer {
                client:     client.clone(),
                model_id:   ModelHandle::ByName {
                    provider: provider.provider_id.clone(),
                    model:    model.to_string(),
                },
                run_budget: run_budget.clone(),
            }))
        } else {
            profile_builder
        };
        let mut profile = profile_builder.build();

        let config = SessionOptions {
            max_tokens: node.max_tokens(),
            reasoning_effort: controls.reasoning_effort,
            speed: controls.speed,
            tool_hooks,
            mcp_servers,
            run_budget: run_budget.clone(),
            // Workflow agents run with no `tool_access_policy`, which exposes
            // the entire tool registry (read, write, shell, subagent, MCP) and
            // skips approval gating. Report that truthfully so the UI doesn't
            // render "Unknown" for every workflow stage. Override per-stage if
            // a future workflow attribute narrows the scope.
            permission_level: Some(PermissionLevel::Full),
            ..SessionOptions::default()
        };

        let supervisor = SubAgentSupervisor::new(config.max_subagent_depth);
        let supervisor_for_session = supervisor.clone();

        // Build factory that creates child sessions WITHOUT subagent tools.
        // Child sessions inherit the parent's tool hooks: blocking
        // pre_tool_use hooks are the only policy boundary workflow agents
        // have, so a subagent's tool calls must pass through them too.
        let factory_client = client.clone();
        let factory_profile_builder = profile_builder;
        let factory_env = Arc::clone(sandbox);
        let factory_tool_env = tool_env.cloned();
        let factory_fabro_run_tools = fabro_run_tools.clone();
        let factory_permission_level = config.permission_level;
        let factory_tool_hooks = config.tool_hooks.clone();
        let factory_run_budget = run_budget;
        let factory: SessionFactory = Arc::new(move || {
            let mut child_profile = factory_profile_builder.build();
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
                    tool_hooks: factory_tool_hooks.clone(),
                    permission_level: factory_permission_level,
                    // Subagents burn real spend: charge them against the
                    // same run budget as the parent session.
                    run_budget: factory_run_budget.clone(),
                    ..SessionOptions::default()
                },
                None,
            );
            if let Some(provider) = &factory_tool_env {
                session.set_tool_env_provider(Arc::clone(provider));
            }
            session
        });

        profile.register_subagent_tools(supervisor.clone(), factory, 0);
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
            Some(supervisor_for_session),
        );
        if let Some(provider) = tool_env {
            session.set_tool_env_provider(Arc::clone(provider));
        }

        // Wire subagent event callback to parent session's emitter
        supervisor.set_event_callback(session.sub_agent_event_callback());

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

    /// Continue the fallback plan after a failover-eligible agent error.
    ///
    /// The caller must already have torn down the failed session (see
    /// [`LiveAgentInvocation::discard_for_error`]); this method only builds
    /// and drives replacement sessions.
    async fn failover_agent_session(
        &self,
        fallback_plan: &mut FallbackPlan,
        initial_error: fabro_llm::Error,
        request: &CodergenRunRequest<'_>,
        input: &str,
        stage_scope: &StageScope,
        stage_id: &StageId,
        live: &mut LiveAgentInvocation,
    ) -> Result<(), Error> {
        let emitter = request.emitter;
        let mut last_error = Error::Llm(initial_error);

        while fallback_plan.advance() {
            Self::emit_failover(
                request.node,
                emitter,
                stage_scope,
                fallback_plan,
                &last_error.to_string(),
            );
            let route = fallback_plan.current().clone();

            let target_provider = match self.resolve_provider_context(
                route.target.model.as_str(),
                Some(route.target.provider.as_str()),
            ) {
                Ok(provider) => provider,
                Err(error) => {
                    last_error = error;
                    continue;
                }
            };

            if request.cancel_token.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let new_session = Self::create_session_for(
                route.target.model.as_str(),
                target_provider,
                route.controls,
                request.node,
                request.sandbox,
                self.source.as_ref(),
                Arc::clone(&self.catalog),
                self.tool_env.as_ref(),
                request.tool_hooks.clone(),
                self.mcp_servers.clone(),
                self.tool_secrets.clone(),
                self.fabro_run_tools.clone(),
                self.run_budget.clone(),
            )
            .await;
            if request.cancel_token.is_cancelled() {
                return Err(Error::Cancelled);
            }
            live.session = match new_session {
                Ok(session) => session,
                Err(error) => {
                    last_error = error;
                    continue;
                }
            };
            live.bridge
                .replace(request.cancel_token.clone(), &live.session);
            live.event_forwarder = spawn_event_forwarder(
                &live.session,
                request.node.id.clone(),
                stage_scope.clone(),
                Arc::clone(emitter),
                Arc::clone(&live.file_tracking),
                self.run_budget.clone(),
            );

            begin_session_lifecycle(&live.session, emitter, None);
            if let Err(error) = live.session.initialize().await {
                let allow_failover = fallback_plan.has_next();
                last_error = Error::Llm(
                    live.discard_for_error(error, allow_failover, emitter)
                        .await?,
                );
                continue;
            }

            match self.attach_session_to_hub(
                &mut live.session,
                stage_id,
                request.thread_id,
                emitter,
            ) {
                Ok(active_lease) => live.lease = Some(active_lease),
                Err(error) => {
                    live.abort_and_discard(emitter).await;
                    return Err(error);
                }
            }
            emit_agent_tools_available(&live.session, &request.node.id, stage_id, emitter);

            let process_result = live
                .session
                .process_input_with_runtime(input, request.agent_tool_runtime.clone())
                .await;
            live.record_input_timing();
            match process_result {
                Ok(()) => {
                    live.record_input_usage().await;
                    return Ok(());
                }
                Err(error) => {
                    let allow_failover = fallback_plan.has_next();
                    last_error = Error::Llm(
                        live.discard_for_error(error, allow_failover, emitter)
                            .await?,
                    );
                }
            }
        }

        Err(last_error)
    }

    async fn shutdown_cached_sessions(&self, emitter: &Arc<Emitter>) {
        let sessions: Vec<CachedAgentSession> = self
            .sessions
            .lock()
            .expect("sessions mutex is never poisoned: no code panics while holding this lock")
            .drain()
            .map(|(_, s)| s)
            .collect();
        for cached in sessions {
            let mut session = cached.session;
            let session_id = session.id().to_string();
            if session.shutdown(SessionShutdownReason::Completed).await {
                emitter.emit(&Event::AgentSessionEnded {
                    session_id,
                    parent_session_id: None,
                });
            }
        }
    }

    /// Emit `agent.failover` for the plan's most recent
    /// [`FallbackPlan::advance`].
    ///
    /// `from` is the previously attempted candidate, which may have failed
    /// during activation without ever serving traffic; `error` says why it
    /// was abandoned. Consecutive events therefore chain — one event's `to`
    /// is the next event's `from` — recording every candidate the plan tried.
    fn emit_failover(
        node: &Node,
        emitter: &Emitter,
        stage_scope: &StageScope,
        plan: &FallbackPlan,
        error: &str,
    ) {
        let from = plan.previous();
        let to = plan.current();
        emitter.emit_scoped(
            &Event::Failover {
                stage: node.id.clone(),
                props: FailoverProps {
                    original_provider: Some(plan.original.target.provider.to_string()),
                    original_model: Some(plan.original.target.model.to_string()),
                    attempt: Some(plan.attempt()),
                    from_provider: from.target.provider.to_string(),
                    from_model: from.target.model.to_string(),
                    to_provider: to.target.provider.to_string(),
                    to_model: to.target.model.to_string(),
                    requested_reasoning_effort: plan.original.controls.reasoning_effort,
                    effective_reasoning_effort: to.controls.reasoning_effort,
                    error: error.to_string(),
                },
            },
            stage_scope,
        );
    }

    fn route_max_tokens(&self, node: &Node, route: &LlmRoute) -> Option<i64> {
        node.max_tokens().or_else(|| {
            self.catalog
                .get_on_provider(&route.target.provider, route.target.model.as_str())
                .and_then(|model| model.limits.max_output)
        })
    }

    /// Build a one-shot completion request addressed to `route`.
    fn route_request(
        &self,
        node: &Node,
        route: &LlmRoute,
        messages: Vec<Message>,
        response_format: Option<ResponseFormat>,
    ) -> Request {
        Request {
            model: route.target.model.to_string(),
            messages,
            provider: Some(route.target.provider.to_string()),
            tools: None,
            tool_choice: None,
            response_format,
            temperature: None,
            top_p: None,
            max_tokens: self.route_max_tokens(node, route),
            stop_sequences: None,
            reasoning_effort: route.controls.reasoning_effort,
            speed: route.controls.speed,
            metadata: None,
            provider_options: None,
        }
    }

    async fn complete_one_shot_request(
        &self,
        client: &Client,
        node: &Node,
        emitter: &Arc<Emitter>,
        stage_scope: &StageScope,
        mut request: Request,
        plan: &mut FallbackPlan,
    ) -> Result<OneShotCompletion, Error> {
        loop {
            match client.complete(&request).await {
                Ok(response) => {
                    let route = plan.current();
                    return Ok(OneShotCompletion {
                        response,
                        model: ModelRef {
                            provider: route.target.provider.clone(),
                            model_id: route.target.model.clone(),
                            speed:    route.controls.speed,
                        },
                    });
                }
                Err(error) if error.failover_eligible() && plan.has_next() => {
                    let error_message = error.to_string();
                    plan.advance();
                    Self::emit_failover(node, emitter, stage_scope, plan, &error_message);
                    request = self.route_request(
                        node,
                        plan.current(),
                        request.messages,
                        request.response_format,
                    );
                }
                Err(error) => return Err(Error::Llm(error)),
            }
        }
    }
}

#[async_trait]
impl CodergenBackend for AgentApiBackend {
    async fn shutdown(&self, emitter: &Arc<Emitter>) {
        self.shutdown_cached_sessions(emitter).await;
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
        let controls = self.resolve_effective_request_controls(node)?;
        let (mut fallback_plan, notices) =
            self.fallback_plan(model, &provider.provider_id, controls);
        self.emit_fallback_plan_notices(&notices, emitter, stage_scope);

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(Message::system(sys));
        }
        messages.push(Message::user(prompt));

        let output_schema = structured_output::parse_node_output_schema(node)?;
        let response_format = output_schema
            .as_ref()
            .map(structured_output::prompt_response_format);
        let mut repair_attempts = 0_i64;
        let mut previous_validation_error = None;
        let mut total_usage = TokenCounts::default();
        let mut total_cost = None;
        let mut inference_duration = Duration::ZERO;

        loop {
            if let Some(budget) = &self.run_budget {
                if let Some(exceeded) = budget.exceeded() {
                    return Err(Error::BudgetExhausted(exceeded.exceeded_message()));
                }
            }

            let request = self.route_request(
                node,
                fallback_plan.current(),
                messages.clone(),
                response_format.clone(),
            );

            let inference_start = Instant::now();
            let completion_result = self
                .complete_one_shot_request(
                    &client,
                    node,
                    emitter,
                    stage_scope,
                    request,
                    &mut fallback_plan,
                )
                .await;
            inference_duration = inference_duration.saturating_add(inference_start.elapsed());
            let completion = completion_result?;
            total_usage += completion.response.usage.clone();
            UsdMicros::accumulate(
                &mut total_cost,
                completion.response.cost_usd.map(UsdMicros::from_usd),
            );
            if let Some(budget) = &self.run_budget {
                let exceeded = budget.charge(
                    completion.response.usage.total_tokens(),
                    completion.response.cost_usd.map(UsdMicros::from_usd),
                );
                for warning in budget.take_pending_warnings() {
                    emitter.notice_scoped(
                        RunNoticeLevel::Warn,
                        RunNoticeCode::BudgetNearlyExhausted,
                        warning.warning_message(),
                        stage_scope,
                    );
                }
                if let Some(exceeded) = exceeded {
                    return Err(Error::BudgetExhausted(exceeded.exceeded_message()));
                }
            }
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
                let repair_message =
                    error.repair_message(schema, previous_validation_error.as_ref());
                previous_validation_error = Some(error);
                messages.push(Message::assistant(response_text));
                messages.push(Message::user(repair_message));
                repair_attempts += 1;
                continue;
            }

            let stage_usage = billed_model_usage_from_llm(
                self.catalog.as_ref(),
                &completion.model,
                &total_usage,
            )?
            .with_reported_cost(total_cost);

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
        let emitter = request.emitter;
        let output_schema = structured_output::parse_node_output_schema(node)?;

        let fidelity = request.context.fidelity();
        let reuse_key = if fidelity == Fidelity::Full {
            request.thread_id.map(String::from)
        } else {
            None
        };

        // Take a cached session if reusing, otherwise create a new one. Cancel
        // checks bracket `Client::from_source(...)` so cancellation arriving
        // during credential refresh is not lost.
        if request.cancel_token.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let cached_session = reuse_key.as_ref().and_then(|key| {
            self.sessions
                .lock()
                .expect("sessions mutex is never poisoned: no code panics while holding this lock")
                .remove(key)
        });
        let is_reused = cached_session.is_some();
        let (cached, fallback_notices) = if let Some(cached) = cached_session {
            (cached, Vec::new())
        } else {
            let created = self
                .create_session_with_plan(node, request.sandbox, request.tool_hooks.clone())
                .await;
            if request.cancel_token.is_cancelled() {
                return Err(Error::Cancelled);
            }
            created?
        };
        let CachedAgentSession {
            session,
            mut fallback_plan,
        } = cached;
        if request.cancel_token.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut bridge = SessionCancelBridgeGuard::new();
        bridge.replace(request.cancel_token.clone(), &session);

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
        let stage_scope = StageScope::for_handler(request.context, &node.id);
        self.emit_fallback_plan_notices(&fallback_notices, emitter, &stage_scope);

        // Subscribe to session events: forward to pipeline emitter + track files.
        let event_forwarder = spawn_event_forwarder(
            &session,
            node.id.clone(),
            stage_scope.clone(),
            Arc::clone(emitter),
            Arc::clone(&file_tracking),
            self.run_budget.clone(),
        );

        // Activate with the steering hub after initialization so HTTP
        // `POST /runs/{id}/steer` calls reach this session. The activation
        // lease is shared with the natural-completion coordinator and is
        // released on every exit path.
        let stage_id = stage_scope.stage_id();
        let mut live = LiveAgentInvocation {
            session,
            bridge,
            lease: None,
            event_forwarder,
            file_tracking,
            run_budget: self.run_budget.clone(),
            total_usage: TokenCounts::default(),
            total_cost: None,
            inference_duration: Duration::ZERO,
            tool_duration: Duration::ZERO,
        };

        let allow_failover_primary = fallback_plan.has_next();
        let init_result = if is_reused {
            Ok(())
        } else {
            begin_session_lifecycle(&live.session, emitter, None);
            live.session.initialize().await
        };

        // If initialize failed with a failover-eligible error, treat as a
        // process_input failover trigger; otherwise run process_input.
        let result = match init_result {
            Ok(()) => {
                match self.attach_session_to_hub(
                    &mut live.session,
                    &stage_id,
                    request.thread_id,
                    emitter,
                ) {
                    Ok(active_lease) => live.lease = Some(active_lease),
                    Err(err) => {
                        live.abort_and_discard(emitter).await;
                        return Err(err);
                    }
                }
                // Reused steerable sessions already emitted their effective
                // tool list on first activation; the registry, access policy,
                // and exposure mode are immutable for the session's lifetime,
                // so re-emitting on every subsequent prompt is wasted work.
                if !is_reused {
                    emit_agent_tools_available(&live.session, &node.id, &stage_id, emitter);
                }
                let process_result = live
                    .session
                    .process_input_with_runtime(request.prompt, request.agent_tool_runtime.clone())
                    .await;
                live.record_input_timing();
                if process_result.is_ok() {
                    live.record_input_usage().await;
                }
                process_result
            }
            Err(err) => Err(err),
        };

        // On a provider-local failure, continue the fixed fallback plan that
        // belongs to the originally requested model.
        let result: Result<(), Error> = match result {
            Ok(()) => Ok(()),
            Err(err) => {
                let sdk_err = live
                    .discard_for_error(err, allow_failover_primary, emitter)
                    .await?;
                self.failover_agent_session(
                    &mut fallback_plan,
                    sdk_err,
                    &request,
                    request.prompt,
                    &stage_scope,
                    &stage_id,
                    &mut live,
                )
                .await
            }
        };

        // On error, discard the session (don't cache failed state). The
        // bridge's `Drop` will abort the spawned task on early return.
        if let Err(err) = result {
            live.abort_and_discard(emitter).await;
            return Err(err);
        }

        let mut response = last_assistant_response(&live.session);
        if let Some(schema) = &output_schema {
            let mut repair_attempts = 0_i64;
            let mut previous_validation_error = None;
            loop {
                let last_file_touched = last_touched_file(&live.file_tracking);
                match validate_agent_output_sources(
                    schema,
                    &response,
                    request.sandbox,
                    last_file_touched.as_deref(),
                )
                .await
                {
                    Ok(_) => break,
                    Err(error) => {
                        if repair_attempts >= node.output_retries() {
                            live.abort_and_discard(emitter).await;
                            return Err(Error::OutputSchemaValidation(
                                structured_output::exhausted_failure_reason(node.output_retries()),
                            ));
                        }
                        let repair_message =
                            error.repair_message(schema, previous_validation_error.as_ref());
                        let repair_result = live
                            .session
                            .process_input_with_runtime(
                                &repair_message,
                                fabro_agent::AgentToolRuntime::default(),
                            )
                            .await;
                        live.record_input_timing();
                        match repair_result {
                            Ok(()) => {
                                // Only once the model has actually seen the
                                // repair can a later identical failure mean it
                                // ignored the correction. Failover rebuilds the
                                // session from the original prompt instead.
                                previous_validation_error = Some(error);
                                live.record_input_usage().await;
                                repair_attempts += 1;
                                response = last_assistant_response(&live.session);
                            }
                            Err(err) => {
                                let allow_failover = fallback_plan.has_next();
                                let sdk_err =
                                    live.discard_for_error(err, allow_failover, emitter).await?;
                                self.failover_agent_session(
                                    &mut fallback_plan,
                                    sdk_err,
                                    &request,
                                    request.prompt,
                                    &stage_scope,
                                    &stage_id,
                                    &mut live,
                                )
                                .await?;
                                response = last_assistant_response(&live.session);
                            }
                        }
                    }
                }
            }
        }

        let stage_usage = billed_model_usage_from_llm(
            self.catalog.as_ref(),
            &ModelRef {
                provider: live.session.provider_id(),
                model_id: live.session.model().into(),
                speed:    live.session.speed(),
            },
            &live.total_usage,
        )?
        .with_reported_cost(live.total_cost);

        if let Some(lease) = live.lease.take() {
            lease.release();
        }

        // Cache session back for reuse on success. Detach the bridge first so
        // the cached session is not left wired to this run's cancel token.
        live.bridge.abort();
        let LiveAgentInvocation {
            session,
            event_forwarder,
            file_tracking,
            inference_duration,
            tool_duration,
            ..
        } = live;
        if let Some(key) = reuse_key {
            drop(event_forwarder);
            self.sessions
                .lock()
                .expect("sessions mutex is never poisoned: no code panics while holding this lock")
                .insert(key, CachedAgentSession {
                    session,
                    fallback_plan,
                });
        } else {
            let mut session = session;
            let mut event_forwarder = event_forwarder;
            let session_id = session.id().to_string();
            session.shutdown(SessionShutdownReason::Completed).await;
            event_forwarder.wait_for_session_end().await;
            emitter.emit(&Event::AgentSessionEnded {
                session_id,
                parent_session_id: None,
            });
            drop(event_forwarder);
        }

        // Snapshot after non-cached shutdown so final child events are included.
        let (files_touched, last_file_touched) = file_tracking_snapshot(&file_tracking);

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
    use fabro_auth::{VaultCredentialSource, test_support as auth_test_support};
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

    struct RefusalTestProvider;

    #[async_trait]
    impl ProviderAdapter for RefusalTestProvider {
        fn name(&self) -> &str {
            "anthropic"
        }

        async fn complete(
            &self,
            _request: &Request,
        ) -> Result<fabro_llm::types::Response, LlmError> {
            Err(refusal_llm_error())
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
            Ok(Box::pin(stream::empty()))
        }
    }

    struct TextTestProvider {
        provider: &'static str,
        text:     &'static str,
    }

    #[async_trait]
    impl ProviderAdapter for TextTestProvider {
        fn name(&self) -> &str {
            self.provider
        }

        async fn complete(
            &self,
            request: &Request,
        ) -> Result<fabro_llm::types::Response, LlmError> {
            Ok(fabro_llm::types::Response {
                id:            "msg_fallback".to_string(),
                model:         request.model.clone(),
                provider:      self.provider.to_string(),
                message:       Message::assistant(self.text),
                finish_reason: fabro_llm::types::FinishReason::Stop,
                usage:         TokenCounts {
                    input_tokens: 3,
                    output_tokens: 2,
                    ..TokenCounts::default()
                },
                raw:           None,
                warnings:      vec![],
                rate_limit:    None,
                cost_usd:      None,
                cost_source:   None,
            })
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

    fn enabled_fallback_catalog() -> Arc<Catalog> {
        let settings: LlmCatalogSettings = toml::from_str(
            r"
[providers.modal]
enabled = true

[providers.openrouter]
enabled = true
",
        )
        .expect("fallback catalog overrides should parse");
        Arc::new(Catalog::from_builtin_with_overrides(&settings).unwrap())
    }

    fn mock_api_backend(server: &MockServer) -> AgentApiBackend {
        let source = auth_test_support::env_credential_source(|name| {
            if name == "MOCK_API_KEY" {
                Some("sk-test".to_string())
            } else {
                None
            }
        });
        AgentApiBackend::new_with_catalog(
            "mock-model".to_string(),
            ProviderId::from("mock"),
            ModelFallbackPolicy::default(),
            source,
            SteeringHub::for_tests(),
            mock_llm_catalog(server),
        )
    }

    fn fallback_api_backend(server: &MockServer) -> AgentApiBackend {
        let settings: LlmCatalogSettings = toml::from_str(&format!(
            r#"
[providers.primary]
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "{}/primary"

[providers.primary.auth]
credentials = ["env:PRIMARY_API_KEY"]

[providers.primary.models.test-model]
display_name = "Primary Test Model"
family = "test"
default = true

[providers.primary.models.test-model.limits]
context_window = 8192
max_output = 1024

[providers.primary.models.test-model.features]
tools = true
vision = false
reasoning = false

[providers.fallback]
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "{}/fallback"

[providers.fallback.auth]
credentials = ["env:FALLBACK_API_KEY"]

[providers.fallback.models.test-model]
display_name = "Fallback Test Model"
family = "test"
default = true

[providers.fallback.models.test-model.limits]
context_window = 8192
max_output = 1024

[providers.fallback.models.test-model.features]
tools = true
vision = false
reasoning = false
"#,
            server.base_url(),
            server.base_url(),
        ))
        .expect("fallback catalog should parse");
        let catalog = Arc::new(
            Catalog::from_builtin_with_overrides(&settings).expect("catalog should build"),
        );
        let source = auth_test_support::env_credential_source(|name| match name {
            "PRIMARY_API_KEY" | "FALLBACK_API_KEY" => Some("sk-test".to_string()),
            _ => None,
        });
        let policy = ModelFallbackPolicy::new(std::collections::BTreeMap::from([(
            "test-model".to_string(),
            vec![FallbackTarget::new("fallback", "test-model")],
        )]));
        AgentApiBackend::new_with_catalog(
            "test-model".to_string(),
            ProviderId::new("primary"),
            policy,
            source,
            SteeringHub::for_tests(),
            catalog,
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

    fn chat_completion_tool_call_stream(
        tool_name: &str,
        tool_call_id: &str,
        arguments: &str,
    ) -> String {
        let tool_call_chunk = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "model": "mock-model",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": tool_call_id,
                        "type": "function",
                        "function": {
                            "name": tool_name,
                            "arguments": arguments
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let finish_chunk = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "model": "mock-model",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        });
        format!("data: {tool_call_chunk}\n\ndata: {finish_chunk}\n\ndata: [DONE]\n\n")
    }

    fn custom_output_schema_attr() -> AttrValue {
        AttrValue::String(
            r#"{"type":"object","required":["passed"],"properties":{"passed":{"type":"boolean"}}}"#
                .to_string(),
        )
    }

    fn nested_output_schema_attr() -> AttrValue {
        AttrValue::String(
            r#"{"type":"object","required":["findings"],"properties":{"findings":{"type":"array","items":{"type":"object","required":["rationale"],"properties":{"rationale":{"type":"string"}}}}}}"#
                .to_string(),
        )
    }

    #[test]
    fn agent_backend_stores_config() {
        let backend = AgentApiBackend::new(
            "claude-opus-4-6".to_string(),
            ProviderId::openai(),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
        );
        assert_eq!(backend.model, "claude-opus-4-6");
        assert_eq!(backend.provider_id, ProviderId::openai());
    }

    #[test]
    fn agent_backend_initializes_empty_sessions() {
        let backend = AgentApiBackend::new(
            "claude-opus-4-6".to_string(),
            ProviderId::anthropic(),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
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
    fn track_file_event_tracks_kimi_write_alias() {
        let mut state = new_file_tracking();
        track_file_event(
            &AgentEvent::ToolCallStarted {
                tool_name:    "Write".to_string(),
                tool_call_id: "tc-kimi".to_string(),
                arguments:    serde_json::json!({
                    "path": "/src/kimi.rs",
                    "content": "new"
                }),
            },
            &mut state,
        );
        track_file_event(
            &AgentEvent::ToolCallCompleted {
                tool_call_id: "tc-kimi".to_string(),
                tool_name:    "Write".to_string(),
                is_error:     false,
                output:       serde_json::Value::String("ok".to_string()),
            },
            &mut state,
        );

        assert!(state.touched.contains("/src/kimi.rs"));
        assert_eq!(state.last.as_deref(), Some("/src/kimi.rs"));
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
        let mut profile = AgentProfileBuilder::new(
            AgentProfileKind::Anthropic,
            ProviderId::anthropic(),
            "claude-opus-4-6",
            Arc::new(Catalog::from_builtin().unwrap()),
        )
        .build();
        let supervisor = SubAgentSupervisor::new(1);
        let factory: SessionFactory = Arc::new(|| {
            panic!("factory should not be called in this test");
        });
        profile.register_subagent_tools(supervisor, factory, 0);

        let names = profile.tool_registry().names();
        assert!(names.contains(&"spawn_agent".to_string()));
        assert!(names.contains(&"send_input".to_string()));
        assert!(names.contains(&"wait".to_string()));
        assert!(names.contains(&"close_agent".to_string()));
    }

    /// Records every `pre_tool_use` call it sees and lets them all proceed.
    struct RecordingHooks(Arc<Mutex<Vec<String>>>);

    #[async_trait]
    impl fabro_agent::ToolHookCallback for RecordingHooks {
        async fn pre_tool_use(
            &self,
            tool_name: &str,
            _tool_input: &serde_json::Value,
        ) -> fabro_agent::ToolHookDecision {
            self.0.lock().unwrap().push(tool_name.to_string());
            fabro_agent::ToolHookDecision::Proceed
        }

        async fn post_tool_use(&self, _tool_name: &str, _tool_call_id: &str, _output: &str) {}

        async fn post_tool_use_failure(&self, _tool_name: &str, _tool_call_id: &str, _error: &str) {
        }
    }

    /// Blocking `pre_tool_use` hooks are the only policy boundary a workflow
    /// agent has: workflow sessions run at `PermissionLevel::Full` with the
    /// whole tool registry exposed. A child session created for `spawn_agent`
    /// therefore has to run under the same `tool_hooks` as its parent —
    /// otherwise any agent that can spawn a subagent gets an unguarded
    /// read-write-shell escape from every hook-enforced policy.
    #[tokio::test]
    async fn subagent_tool_calls_pass_through_session_tool_hooks() {
        let server = MockServer::start();
        // Parent turn 1: spawn a subagent.
        let parent_spawn = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("PARENT_PROMPT_MARKER")
                .body_excludes(r#""role":"tool""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_tool_call_stream(
                    "spawn_agent",
                    "call_spawn_helper",
                    r#"{"task":"CHILD_TASK_MARKER: read data.txt and report its contents"}"#,
                ));
        });
        // Parent turn 2: the spawn result is back; finish the parent turn
        // while the child keeps running in the background.
        let parent_final = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("call_spawn_helper");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream("parent done", 10, 1));
        });
        // Child turn 1: the child session uses a tool.
        let child_read = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("CHILD_TASK_MARKER")
                .body_excludes("PARENT_PROMPT_MARKER")
                .body_excludes(r#""role":"tool""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_tool_call_stream(
                    "read_file",
                    "call_child_read",
                    r#"{"file_path":"data.txt"}"#,
                ));
        });
        // Child turn 2: the tool result is back; the child completes.
        let child_final = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("call_child_read");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream("child done", 10, 1));
        });

        let hook_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let hooks: Arc<dyn fabro_agent::ToolHookCallback> =
            Arc::new(RecordingHooks(Arc::clone(&hook_calls)));

        let backend = mock_api_backend(&server);
        let node = Node::new("researcher");
        let workspace = tempfile::tempdir().unwrap();
        tokio::fs::write(workspace.path().join("data.txt"), "hello\n")
            .await
            .unwrap();
        let sandbox: Arc<dyn fabro_agent::Sandbox> =
            Arc::new(LocalSandbox::new(workspace.path().to_path_buf()));

        let mut session = backend
            .create_session_with_plan(&node, &sandbox, Some(hooks))
            .await
            .unwrap()
            .0
            .session;
        session
            .process_input("PARENT_PROMPT_MARKER: spawn a helper subagent")
            .await
            .unwrap();

        // The child runs on background tasks owned by the still-alive parent
        // session; wait until its final turn has been served.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while child_final.calls() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the spawned subagent never completed its turns against the mock provider"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        parent_spawn.assert_calls(1);
        parent_final.assert_calls(1);
        child_read.assert_calls(1);
        child_final.assert_calls(1);

        let recorded = hook_calls.lock().unwrap().clone();
        assert!(
            recorded.iter().any(|name| name == "spawn_agent"),
            "the parent's own tool calls should reach the hooks; hooks saw: {recorded:?}"
        );
        assert!(
            recorded.iter().any(|name| name == "read_file"),
            "the child subagent's tool calls must pass through the same tool hooks as the \
             parent's, but the hooks never saw the child's read_file; hooks saw: {recorded:?}"
        );
    }

    #[test]
    fn api_backend_provider_pin_wins_over_priority_selection() {
        let settings: LlmCatalogSettings = toml::from_str(
            r"
[providers.openrouter]
enabled = true
",
        )
        .unwrap();
        let backend = AgentApiBackend::new_with_catalog(
            "gpt-5.4".to_string(),
            ProviderId::from("openrouter"),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
            Arc::new(Catalog::from_builtin_with_overrides(&settings).unwrap()),
        );

        let provider = backend.resolve_provider_context("gpt-5.4", None).unwrap();

        assert_eq!(provider.provider_id, ProviderId::from("openrouter"));
    }

    #[test]
    fn api_backend_node_provider_attr_overrides_backend_pin() {
        let backend = AgentApiBackend::new_with_catalog(
            "gpt-5.4".to_string(),
            ProviderId::from("openrouter"),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
            Arc::new(Catalog::from_builtin().unwrap()),
        );

        let provider = backend
            .resolve_provider_context("gpt-5.4", Some("openai"))
            .unwrap();

        assert_eq!(provider.provider_id, ProviderId::openai());
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
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
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
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
            catalog,
        );

        let provider = backend.resolve_provider_context("ac", None).unwrap();

        assert_eq!(provider.provider_id, ProviderId::from("acme"));
        assert_eq!(provider.profile_kind, AgentProfileKind::Anthropic);
    }

    #[test]
    fn api_backend_selects_claude5_profile_for_sonnet5() {
        let backend = AgentApiBackend::new_with_catalog(
            "claude-sonnet-5".to_string(),
            ProviderId::anthropic(),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
            Arc::new(Catalog::from_builtin().unwrap()),
        );

        let provider = backend
            .resolve_provider_context("claude-sonnet-5", None)
            .unwrap();

        assert_eq!(provider.provider_id, ProviderId::anthropic());
        assert_eq!(provider.profile_kind, AgentProfileKind::Claude5);
    }

    #[test]
    fn api_backend_preserves_default_provider_for_legacy_model_identifier() {
        let settings: LlmCatalogSettings = toml::from_str(
            r"
[providers.openrouter]
enabled = true
",
        )
        .unwrap();
        let catalog = Arc::new(Catalog::from_builtin_with_overrides(&settings).unwrap());
        let backend = AgentApiBackend::new_with_catalog(
            "openai/gpt-5.4".to_string(),
            ProviderId::from("openrouter"),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
            catalog,
        );

        let provider = backend
            .resolve_provider_context("openai/gpt-5.4", None)
            .unwrap();

        assert_eq!(provider.provider_id, ProviderId::from("openrouter"));
        assert_eq!(provider.profile_kind, AgentProfileKind::OpenAi);
    }

    #[test]
    fn run_model_controls_apply_when_node_omits_controls() {
        let backend = AgentApiBackend::new(
            "gpt-5.4".to_string(),
            ProviderId::openai(),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
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
        let backend = AgentApiBackend::new(
            "gpt-5.4".to_string(),
            ProviderId::openai(),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
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
        let backend = AgentApiBackend::new(
            "gpt-5.4".to_string(),
            ProviderId::openai(),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
        );
        let node = Node::new("work");

        let controls = backend.resolve_effective_request_controls(&node).unwrap();

        assert_eq!(controls.reasoning_effort, None);
    }

    #[test]
    fn fallback_plan_maps_reasoning_to_each_target_and_rounds_ties_up() {
        let policy = ModelFallbackPolicy::new(std::collections::BTreeMap::from([(
            "kimi-k3".to_string(),
            vec![
                FallbackTarget::new("moonshot", "kimi-k3"),
                FallbackTarget::new("openrouter", "kimi-k3"),
                FallbackTarget::new("anthropic", "claude-opus-5"),
            ],
        )]));
        let backend = AgentApiBackend::new_with_catalog(
            "kimi-k3".to_string(),
            ProviderId::new("modal"),
            policy,
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
            enabled_fallback_catalog(),
        );

        let (plan, notices) = backend.fallback_plan(
            "kimi-k3",
            &ProviderId::new("modal"),
            EffectiveRequestControls {
                reasoning_effort: Some(ReasoningEffort::Medium),
                speed:            None,
            },
        );

        assert!(notices.is_empty());
        assert_eq!(
            plan.remaining
                .iter()
                .map(|route| route.controls.reasoning_effort)
                .collect::<Vec<_>>(),
            vec![
                Some(ReasoningEffort::High),
                Some(ReasoningEffort::High),
                Some(ReasoningEffort::Medium),
            ]
        );
    }

    #[test]
    fn advancing_a_fallback_plan_never_activates_the_target_models_chain() {
        let policy = ModelFallbackPolicy::new(std::collections::BTreeMap::from([
            ("claude-fable-5".to_string(), vec![
                FallbackTarget::new("openai", "gpt-5.6-sol"),
                FallbackTarget::new("anthropic", "claude-opus-5"),
            ]),
            ("gpt-5.6-sol".to_string(), vec![FallbackTarget::new(
                "anthropic",
                "claude-sonnet-5",
            )]),
        ]));
        let backend = AgentApiBackend::new_with_catalog(
            "claude-fable-5".to_string(),
            ProviderId::anthropic(),
            policy,
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
            enabled_fallback_catalog(),
        );
        let (mut plan, notices) = backend.fallback_plan(
            "claude-fable-5",
            &ProviderId::anthropic(),
            EffectiveRequestControls::default(),
        );

        assert!(notices.is_empty());
        assert!(plan.advance(), "Sol should be first");
        assert_eq!(
            plan.current().target,
            FallbackTarget::new("openai", "gpt-5.6-sol")
        );
        assert_eq!(plan.attempt(), 1);
        assert!(plan.advance(), "Opus should be second");
        assert_eq!(
            plan.current().target,
            FallbackTarget::new("anthropic", "claude-opus-5")
        );
        assert_eq!(plan.attempt(), 2);
        assert!(!plan.has_next());
        assert!(!plan.advance());
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
            ModelFallbackPolicy::default(),
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
    async fn one_shot_falls_back_after_refusal_error() {
        let configured_targets = vec![FallbackTarget::new("openai", "gpt-5.5")];
        let fallback_policy = ModelFallbackPolicy::new(std::collections::BTreeMap::from([(
            "claude-fable-5".to_string(),
            configured_targets,
        )]));
        let backend = AgentApiBackend::new(
            "claude-fable-5".to_string(),
            ProviderId::anthropic(),
            fallback_policy,
            auth_test_support::vault_only_credential_source(),
            SteeringHub::for_tests(),
        );
        let mut providers = HashMap::new();
        providers.insert(
            "anthropic".to_string(),
            Arc::new(RefusalTestProvider) as Arc<dyn ProviderAdapter>,
        );
        providers.insert(
            "openai".to_string(),
            Arc::new(TextTestProvider {
                provider: "openai",
                text:     "fallback ok",
            }) as Arc<dyn ProviderAdapter>,
        );
        let client = Client::new(providers, Some("anthropic".to_string()), Vec::new());
        let node = Node::new("ask");
        let context = Context::new();
        let stage_scope = StageScope::for_handler(&context, &node.id);
        let emitter = Arc::new(Emitter::new(fabro_types::RunId::new()));
        let emitted_failover = Arc::new(Mutex::new(None));
        let emitted_failover_for_listener = Arc::clone(&emitted_failover);
        emitter.on_event(move |event| {
            if let fabro_types::EventBody::Failover(props) = &event.body {
                *emitted_failover_for_listener.lock().unwrap() = Some(props.clone());
            }
        });
        let request = Request {
            model:            "claude-fable-5".to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         Some("anthropic".to_string()),
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       Some(128),
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        };
        let (mut fallback_plan, notices) = backend.fallback_plan(
            "claude-fable-5",
            &ProviderId::anthropic(),
            EffectiveRequestControls::default(),
        );
        assert!(notices.is_empty());

        let completion = backend
            .complete_one_shot_request(
                &client,
                &node,
                &emitter,
                &stage_scope,
                request,
                &mut fallback_plan,
            )
            .await
            .unwrap();

        assert_eq!(completion.response.text(), "fallback ok");
        assert_eq!(completion.model.provider, ProviderId::openai());
        assert_eq!(completion.model.model_id, "gpt-5.5");
        let failover = emitted_failover
            .lock()
            .unwrap()
            .clone()
            .expect("agent.failover should be emitted");
        assert_eq!(failover.original_provider.as_deref(), Some("anthropic"));
        assert_eq!(failover.original_model.as_deref(), Some("claude-fable-5"));
        assert_eq!(failover.attempt, Some(1));
        assert_eq!(failover.from_provider, "anthropic");
        assert_eq!(failover.from_model, "claude-fable-5");
        assert_eq!(failover.to_provider, "openai");
        assert_eq!(failover.to_model, "gpt-5.5");
        assert_eq!(failover.requested_reasoning_effort, None);
        assert_eq!(failover.effective_reasoning_effort, None);
        assert!(failover.error.contains("refused"));
    }

    #[tokio::test]
    async fn explicit_provider_one_shot_stays_on_fallback_during_output_repair() {
        let server = MockServer::start();
        let primary_failure = server.mock(|when, then| {
            when.method(POST).path("/primary/chat/completions");
            then.status(401)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "error": {
                        "message": "primary credential expired",
                        "type": "authentication_error"
                    }
                }));
        });
        let fallback_response = server.mock(|when, then| {
            when.method(POST)
                .path("/fallback/chat/completions")
                .body_excludes(r#""role":"assistant""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(chat_completion_response("not json", 10, 1));
        });
        let fallback_repair = server.mock(|when, then| {
            when.method(POST)
                .path("/fallback/chat/completions")
                .body_includes(r#""role":"assistant""#)
                .body_includes("not json");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(chat_completion_response(r#"{"passed":true}"#, 11, 2));
        });
        let backend = fallback_api_backend(&server);
        let mut node = Node::new("audit");
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("primary".to_string()),
        );
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

        primary_failure.assert_calls(1);
        fallback_response.assert_calls(1);
        fallback_repair.assert_calls(1);
        let CodergenResult::Text { text, .. } = result else {
            panic!("one_shot should return text");
        };
        assert_eq!(text, r#"{"passed":true}"#);
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
                .json_body({
                    let mut response = chat_completion_response("not json", 10, 1);
                    response["usage"]["cost"] = serde_json::json!(0.04);
                    response
                });
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
                .json_body({
                    let mut response = chat_completion_response(r#"{"passed":true}"#, 11, 2);
                    response["usage"]["cost"] = serde_json::json!(0.06);
                    response
                });
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
        assert_eq!(usage.total_usd_micros, Some(100_000));
    }

    fn token_budget(max_tokens: u64) -> Arc<RunBudget> {
        Arc::new(RunBudget::new(None, Some(max_tokens)))
    }

    fn collect_run_events(emitter: &Emitter) -> Arc<Mutex<Vec<fabro_types::RunEvent>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        emitter.on_event({
            let seen = Arc::clone(&seen);
            move |event| seen.lock().unwrap().push(event.clone())
        });
        seen
    }

    fn budget_notice_messages(events: &[fabro_types::RunEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match &event.body {
                fabro_types::EventBody::RunNotice(props)
                    if props.code == "budget_nearly_exhausted" =>
                {
                    Some(props.message.clone())
                }
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn one_shot_fails_when_budget_exceeded_by_response() {
        let server = MockServer::start();
        let completion = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(chat_completion_response("done", 40, 10));
        });
        let backend = mock_api_backend(&server).with_run_budget(Some(token_budget(30)));
        let node = Node::new("audit");
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
            .await;
        let Err(error) = result else {
            panic!("expected the budget to fail the call")
        };

        completion.assert_calls(1);
        assert!(
            matches!(error, Error::BudgetExhausted(_)),
            "expected budget exhausted, got {error:?}"
        );
        assert!(
            error.to_string().contains("run.limits.max_tokens"),
            "message should name the limit: {error}"
        );
    }

    #[tokio::test]
    async fn one_shot_skips_the_request_when_budget_is_already_exhausted() {
        let server = MockServer::start();
        let completion = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(chat_completion_response("done", 1, 1));
        });
        let budget = token_budget(10);
        assert!(budget.charge(50, None).is_some());
        let backend = mock_api_backend(&server).with_run_budget(Some(Arc::clone(&budget)));
        let node = Node::new("audit");
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
            .await;
        let Err(error) = result else {
            panic!("expected the budget to fail the call")
        };

        completion.assert_calls(0);
        assert!(
            matches!(error, Error::BudgetExhausted(_)),
            "expected budget exhausted, got {error:?}"
        );
    }

    #[tokio::test]
    async fn one_shot_emits_the_eighty_percent_budget_notice_once() {
        let server = MockServer::start();
        let completion = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(chat_completion_response("done", 75, 10));
        });
        // 85 of 100 tokens: past the 80% warning threshold, under the limit.
        let backend = mock_api_backend(&server).with_run_budget(Some(token_budget(100)));
        let node = Node::new("audit");
        let context = Context::new();
        let stage_scope = StageScope::for_handler(&context, &node.id);
        let emitter = Arc::new(Emitter::new(fabro_types::RunId::new()));
        let events = collect_run_events(&emitter);
        let workspace = tempfile::tempdir().unwrap();
        let sandbox: Arc<dyn fabro_agent::Sandbox> =
            Arc::new(LocalSandbox::new(workspace.path().to_path_buf()));

        backend
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

        completion.assert_calls(1);
        let notices = budget_notice_messages(&events.lock().unwrap());
        assert_eq!(notices.len(), 1, "exactly one warning notice: {notices:?}");
        assert!(
            notices[0].contains("80% of run.limits.max_tokens"),
            "got {notices:?}"
        );
    }

    #[tokio::test]
    async fn agent_run_fails_fast_when_budget_is_already_exhausted() {
        let server = MockServer::start();
        let completion = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream("done", 1, 1));
        });
        let budget = token_budget(10);
        assert!(budget.charge(50, None).is_some());
        let backend = mock_api_backend(&server).with_run_budget(Some(budget));
        let node = Node::new("code");
        let context = Context::new();
        let emitter = Arc::new(Emitter::new(fabro_types::RunId::new()));
        let workspace = tempfile::tempdir().unwrap();
        let sandbox: Arc<dyn fabro_agent::Sandbox> =
            Arc::new(LocalSandbox::new(workspace.path().to_path_buf()));

        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "Implement the feature",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await;
        let Err(error) = result else {
            panic!("expected the budget to fail the call")
        };

        completion.assert_calls(0);
        assert!(
            matches!(error, Error::BudgetExhausted(_)),
            "expected budget exhausted, got {error:?}"
        );
    }

    #[tokio::test]
    async fn agent_run_halts_mid_session_when_a_response_trips_the_budget() {
        let server = MockServer::start();
        // First turn: a tool call whose usage blows the budget. The session
        // must interrupt without sending the follow-up turn.
        let usage_chunk = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "model": "mock-model",
            "choices": [],
            "usage": { "prompt_tokens": 90, "completion_tokens": 20 }
        });
        let first_body = chat_completion_tool_call_stream(
            "read_file",
            "call_budget",
            r#"{"file_path":"data.txt"}"#,
        )
        .replace(
            "data: [DONE]",
            &format!("data: {usage_chunk}\n\ndata: [DONE]"),
        );
        let first = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_excludes(r#""role":"tool""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(first_body);
        });
        let followup = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""role":"tool""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream("done", 1, 1));
        });
        let backend = mock_api_backend(&server).with_run_budget(Some(token_budget(100)));
        let node = Node::new("code");
        let context = Context::new();
        let emitter = Arc::new(Emitter::new(fabro_types::RunId::new()));
        let workspace = tempfile::tempdir().unwrap();
        tokio::fs::write(workspace.path().join("data.txt"), "hello\n")
            .await
            .unwrap();
        let sandbox: Arc<dyn fabro_agent::Sandbox> =
            Arc::new(LocalSandbox::new(workspace.path().to_path_buf()));

        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "Implement the feature",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await;
        let Err(error) = result else {
            panic!("expected the budget to fail the call")
        };

        first.assert_calls(1);
        followup.assert_calls(0);
        assert!(
            matches!(error, Error::BudgetExhausted(_)),
            "expected budget exhausted, got {error:?}"
        );
        assert!(
            error.to_string().contains("run.limits.max_tokens"),
            "message should name the limit: {error}"
        );
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
                .body_includes("output_schema")
                .body_includes(r#"\"required\":[\"passed\"]"#);
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
    async fn agent_run_identifies_a_schema_error_repeated_during_repair() {
        let server = MockServer::start();
        let first = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""stream":true"#)
                .body_excludes(r#""role":"assistant""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream(r#"{"findings":[{}]}"#, 20, 3));
        });
        let first_repair = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("JSON Pointer `/findings/0/rationale`")
                .body_excludes("unchanged from your previous repair");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream(r#"{"findings":[{}]}"#, 21, 4));
        });
        let second_repair = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("JSON Pointer `/findings/0/rationale`")
                .body_includes("unchanged from your previous repair");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream(
                    r#"{"findings":[{"rationale":"done"}]}"#,
                    22,
                    5,
                ));
        });
        let backend = mock_api_backend(&server);
        let mut node = Node::new("audit");
        node.attrs
            .insert("output_schema".to_string(), nested_output_schema_attr());
        node.attrs
            .insert("output_retries".to_string(), AttrValue::Integer(2));
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
        first_repair.assert_calls(1);
        second_repair.assert_calls(1);
        let CodergenResult::Text { text, .. } = result else {
            panic!("run should return text");
        };
        assert_eq!(text, r#"{"findings":[{"rationale":"done"}]}"#);
    }

    #[tokio::test]
    async fn agent_output_repair_continues_on_the_original_models_fallback_plan() {
        let server = MockServer::start();
        let primary_response = server.mock(|when, then| {
            when.method(POST)
                .path("/primary/chat/completions")
                .body_includes(r#""stream":true"#)
                .body_excludes(r#""role":"assistant""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream("not json", 20, 3));
        });
        let failed_repair = server.mock(|when, then| {
            when.method(POST)
                .path("/primary/chat/completions")
                .body_includes(r#""role":"assistant""#)
                .body_includes("not json");
            then.status(401)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "error": {
                        "message": "primary credential expired",
                        "type": "authentication_error"
                    }
                }));
        });
        let fallback_response = server.mock(|when, then| {
            when.method(POST)
                .path("/fallback/chat/completions")
                .body_includes(r#""stream":true"#)
                .body_excludes(r#""role":"assistant""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream(r#"{"passed":true}"#, 22, 4));
        });
        let backend = fallback_api_backend(&server);
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

        primary_response.assert_calls(1);
        failed_repair.assert_calls(1);
        fallback_response.assert_calls(1);
        let CodergenResult::Text { text, .. } = result else {
            panic!("run should return text");
        };
        assert_eq!(text, r#"{"passed":true}"#);
    }

    #[tokio::test]
    async fn agent_run_web_search_uses_configured_brave_search_key() {
        let server = MockServer::start();
        let tool_call = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""stream":true"#)
                .body_excludes(r#""role":"tool""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_tool_call_stream(
                    "web_search",
                    "call_web_search",
                    r#"{"query":"fabro"}"#,
                ));
        });
        let completion = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("call_web_search");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(chat_completion_stream("Done", 10, 1));
        });
        let backend = mock_api_backend(&server).with_tool_secrets(ToolSecrets {
            // An invalid header value makes a correctly configured executor
            // fail locally before any request can leave the test process.
            brave_search_api_key: Some("\n".to_string()),
        });
        let node = Node::new("search");
        let context = Context::new();
        let emitter = Arc::new(Emitter::new(fabro_types::RunId::new()));
        let web_search_results = Arc::new(Mutex::new(Vec::new()));
        let web_search_results_for_listener = Arc::clone(&web_search_results);
        emitter.on_event(move |event| {
            if let fabro_types::EventBody::AgentToolCompleted(props) = &event.body {
                if props.tool_name != "web_search" {
                    return;
                }
                web_search_results_for_listener
                    .lock()
                    .unwrap()
                    .push((props.output.clone(), props.is_error));
            }
        });
        let workspace = tempfile::tempdir().unwrap();
        let sandbox: Arc<dyn fabro_agent::Sandbox> =
            Arc::new(LocalSandbox::new(workspace.path().to_path_buf()));

        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "Search the web",
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

        tool_call.assert_calls(1);
        completion.assert_calls(1);
        let web_search_results = web_search_results.lock().unwrap();
        assert_eq!(web_search_results.len(), 1);
        let (output, is_error) = &web_search_results[0];
        assert!(*is_error);
        let output = output
            .as_str()
            .expect("web_search error output should be a string");
        assert!(
            output.starts_with("HTTP request failed:"),
            "configured web_search should use its API key; got: {output}"
        );
        let CodergenResult::Text { text, .. } = result else {
            panic!("run should return text");
        };
        assert_eq!(text, "Done");
    }

    #[tokio::test]
    async fn api_backend_shutdown_closes_cached_sessions_once() {
        let backend = AgentApiBackend::new(
            "gpt-5.4".to_string(),
            ProviderId::openai(),
            ModelFallbackPolicy::default(),
            auth_test_support::vault_only_credential_source(),
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
        let (fallback_plan, notices) = backend.fallback_plan(
            "gpt-5.4",
            &ProviderId::openai(),
            EffectiveRequestControls::default(),
        );
        assert!(notices.is_empty());
        begin_session_lifecycle(&session, &emitter, None);
        backend
            .sessions
            .lock()
            .unwrap()
            .insert("thread-1".to_string(), CachedAgentSession {
                session,
                fallback_plan,
            });

        backend.shutdown(&emitter).await;
        backend.shutdown(&emitter).await;

        assert_eq!(event_names.lock().unwrap().as_slice(), [
            "agent.session.started",
            "agent.session.ended"
        ]);
        assert!(backend.sessions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn session_end_barrier_preserves_child_close_ordering() {
        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            Arc::new(ShutdownTestProvider) as Arc<dyn ProviderAdapter>,
        );
        let client = Client::new(providers, Some("openai".to_string()), Vec::new());
        let mut session = Session::new(
            client,
            Arc::new(ShutdownTestProfile::new()),
            Arc::new(LocalSandbox::new(
                tempfile::tempdir().unwrap().path().to_path_buf(),
            )),
            SessionOptions::default(),
            None,
        );
        let emitter = Arc::new(Emitter::new(RunId::new()));
        let event_names = Arc::new(Mutex::new(Vec::new()));
        let event_names_for_listener = Arc::clone(&event_names);
        emitter.on_event(move |event| {
            event_names_for_listener
                .lock()
                .unwrap()
                .push(event.event_name().to_string());
        });
        let context = Context::new();
        let scope = StageScope::for_handler(&context, "code");
        let file_tracking = Arc::new(Mutex::new(FileTracking {
            pending: HashMap::new(),
            touched: HashSet::new(),
            last:    None,
        }));
        let mut forwarder = spawn_event_forwarder(
            &session,
            "code".to_string(),
            scope,
            Arc::clone(&emitter),
            file_tracking,
            None,
        );

        session.sub_agent_event_callback()(
            fabro_agent::subagent::SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentClosed {
                agent_id:   "child-1".to_string(),
                depth:      1,
                generation: 1,
            }),
        );
        let session_id = session.id().to_string();
        session.shutdown(SessionShutdownReason::Completed).await;
        forwarder.wait_for_session_end().await;
        emitter.emit(&Event::AgentSessionEnded {
            session_id,
            parent_session_id: None,
        });

        assert_eq!(event_names.lock().unwrap().as_slice(), [
            "agent.sub.closed",
            "agent.session.ended"
        ]);
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
            kind:   ProviderErrorKind::InvalidRequest,
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

    fn refusal_llm_error() -> LlmError {
        LlmError::Provider {
            kind:   ProviderErrorKind::ContentFilter,
            detail: Box::new(ProviderErrorDetail {
                message:     "claude-fable-5 refused the request".into(),
                provider:    "anthropic".into(),
                status_code: None,
                error_code:  Some("refusal".into()),
                retry_after: None,
                raw:         Some(serde_json::json!({
                    "stop_reason": "refusal",
                    "stop_details": {
                        "type": "refusal",
                        "category": "cyber",
                        "explanation": "This request was declined."
                    }
                })),
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
            classify_agent_error(err, true, None),
            AgentApiErrorDisposition::Cancelled
        ));
    }

    #[test]
    fn classify_interrupted_wall_clock_is_terminal_precondition() {
        let err = fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::WallClockTimeout);
        match classify_agent_error(err, true, None) {
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
            classify_agent_error(err, true, None),
            AgentApiErrorDisposition::FailoverEligible(_)
        ));
    }

    #[test]
    fn classify_failover_eligible_llm_returns_terminal_when_not_allowed() {
        let err = fabro_agent::Error::Llm(failover_eligible_llm_error());
        match classify_agent_error(err, false, None) {
            AgentApiErrorDisposition::Terminal(Error::Llm(_)) => {}
            _ => panic!("expected Terminal(Error::Llm) when failover disallowed"),
        }
    }

    #[test]
    fn classify_non_failover_eligible_llm_is_terminal_llm() {
        let err = fabro_agent::Error::Llm(non_failover_llm_error());
        match classify_agent_error(err, true, None) {
            AgentApiErrorDisposition::Terminal(Error::Llm(_)) => {}
            _ => panic!("expected Terminal(Error::Llm) for non-failover-eligible LLM error"),
        }
    }

    #[test]
    fn classify_refusal_llm_returns_failover_when_allowed() {
        let err = fabro_agent::Error::Llm(refusal_llm_error());
        assert!(matches!(
            classify_agent_error(err, true, None),
            AgentApiErrorDisposition::FailoverEligible(_)
        ));
    }

    #[test]
    fn classify_refusal_llm_returns_terminal_when_not_allowed() {
        let err = fabro_agent::Error::Llm(refusal_llm_error());
        match classify_agent_error(err, false, None) {
            AgentApiErrorDisposition::Terminal(Error::Llm(llm_err)) => {
                assert!(llm_err.to_string().contains("claude-fable-5 refused"));
            }
            _ => panic!("expected Terminal(Error::Llm) when refusal failover is disallowed"),
        }
    }

    #[test]
    fn classify_session_closed_is_terminal_precondition() {
        let err = fabro_agent::Error::SessionClosed;
        match classify_agent_error(err, true, None) {
            AgentApiErrorDisposition::Terminal(Error::Precondition(message)) => {
                assert!(message.contains("Agent session failed"));
            }
            _ => panic!("expected Terminal(Error::Precondition) for SessionClosed"),
        }
    }

    #[test]
    fn classify_invalid_state_is_terminal_precondition() {
        let err = fabro_agent::Error::InvalidState("oops".into());
        match classify_agent_error(err, true, None) {
            AgentApiErrorDisposition::Terminal(Error::Precondition(message)) => {
                assert!(message.contains("Agent session failed"));
            }
            _ => panic!("expected Terminal(Error::Precondition) for InvalidState"),
        }
    }

    #[test]
    fn classify_tool_execution_is_terminal_precondition() {
        let err = fabro_agent::Error::ToolExecution("tool blew up".into());
        match classify_agent_error(err, true, None) {
            AgentApiErrorDisposition::Terminal(Error::Precondition(message)) => {
                assert!(message.contains("Agent session failed"));
            }
            _ => panic!("expected Terminal(Error::Precondition) for ToolExecution"),
        }
    }
}
