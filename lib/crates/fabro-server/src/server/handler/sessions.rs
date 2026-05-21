use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use fabro_agent::config::ToolApprovalFn;
use fabro_agent::tool_permissions::is_tool_auto_approved;
use fabro_agent::{
    AgentEvent, AgentProfile, AnthropicProfile, Error as AgentError, GeminiProfile, OpenAiProfile,
    Session, SessionEvent, SessionOptions, ToolApprovalAdapter, WebFetchSummarizer,
};
use fabro_api::types::{CreateRunSessionRequest, SubmitTurnRequest};
use fabro_llm::client::Client as LlmClient;
use fabro_model::{AgentProfileKind, Catalog, ModelHandle, ProviderId};
use fabro_sandbox::reconnect::reconnect_for_run;
use fabro_store::{
    EventPayload, ProjectedRunSession, RunDatabase, project_run_session, project_run_sessions,
};
use fabro_types::run_event::{
    RunSessionAssistantDeltaProps, RunSessionAssistantMessageProps, RunSessionCreatedProps,
    RunSessionToolCallCompletedProps, RunSessionToolCallStartedProps, RunSessionTurnFailedProps,
    RunSessionTurnInterruptedProps, RunSessionTurnStartedProps, RunSessionTurnSucceededProps,
    RunSessionUserMessageProps,
};
use fabro_types::settings::{ModelRef as SettingsModelRef, ModelRegistry, ResolvedModelRef};
use fabro_types::{
    EventBody, EventEnvelope, PermissionLevel, RunEvent, RunId, SessionId, SessionRecord, TurnId,
};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, warn};

use super::super::session_runtime::{InterruptTurnError, SessionTurnLease, StartTurnError};
use super::super::{AppState, ListResponse};
use crate::error::ApiError;
use crate::principal_middleware::RequiredUser;
use crate::server_secrets::LlmClientResult;

const SESSION_SSE_BUFFER_CAPACITY: usize = 1024;

type SessionSseSender = mpsc::Sender<Result<Event, Infallible>>;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/runs/{run_id}/sessions",
            get(list_run_sessions).post(create_run_session),
        )
        .route(
            "/sessions/{id}",
            get(get_session).fallback(session_method_not_found),
        )
        .route(
            "/sessions/{id}/turns",
            post(submit_turn).fallback(session_method_not_found),
        )
        .route(
            "/sessions/{id}/turns/{turnId}/interrupt",
            post(interrupt_turn),
        )
}

async fn list_run_sessions(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Response {
    let run_id = match parse_run_id(&run_id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let run_store = match open_run_reader(&state, run_id).await {
        Ok(store) => store,
        Err(response) => return response,
    };
    match run_store.list_events().await {
        Ok(events) => {
            Json(ListResponse::new(project_run_sessions(run_id, &events))).into_response()
        }
        Err(err) => store_error(&err).into_response(),
    }
}

async fn create_run_session(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Json(request): Json<CreateRunSessionRequest>,
) -> Response {
    let run_id = match parse_run_id(&run_id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let run_store = match open_run(&state, run_id).await {
        Ok(store) => store,
        Err(response) => return response,
    };
    let model = match canonical_session_model(state.catalog().as_ref(), request.model.as_deref()) {
        Ok(model) => model,
        Err(err) => return err.into_response(),
    };

    let session_id = SessionId::new();
    let now = Utc::now();
    if let Err(err) = state
        .store_ref()
        .put_session_run_index(&session_id, &run_id)
        .await
    {
        return store_error(&err).into_response();
    }

    let event = match append_run_session_event(
        &run_store,
        run_id,
        session_id,
        EventBody::RunSessionCreated(RunSessionCreatedProps {
            title: request.title,
            model,
        }),
        now,
    )
    .await
    {
        Ok(event) => event,
        Err(err) => return store_error(&err).into_response(),
    };

    let events = vec![event];
    match project_run_session(run_id, session_id, &events) {
        Some(record) => (StatusCode::CREATED, Json(record)).into_response(),
        None => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Session event projection failed.",
        )
        .into_response(),
    }
}

async fn get_session(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let (_, session) = match load_session_read(&state, session_id).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    Json(session).into_response()
}

async fn session_method_not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

async fn submit_turn(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<SubmitTurnRequest>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let (run_id, run_store, session) = match load_session(&state, session_id).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    let input = request.input;

    let turn_id = TurnId::new();
    let turn_lease = match state.session_runtimes().reserve_turn(session_id, turn_id) {
        Ok(lease) => lease,
        Err(StartTurnError::ActiveTurn) => {
            return ApiError::new(StatusCode::CONFLICT, "Session already has an active turn.")
                .into_response();
        }
    };

    let (sender, receiver) = mpsc::channel(SESSION_SSE_BUFFER_CAPACITY);
    let now = Utc::now();
    for body in [
        EventBody::RunSessionTurnStarted(RunSessionTurnStartedProps {
            turn_id,
            input: input.clone(),
        }),
        EventBody::RunSessionUserMessage(RunSessionUserMessageProps {
            turn_id,
            text: input.clone(),
        }),
    ] {
        match append_and_send_event(&run_store, &sender, run_id, session_id, body, now).await {
            Ok(()) => {}
            Err(err) => {
                drop(turn_lease);
                return store_error(&err).into_response();
            }
        }
    }

    tokio::spawn(run_streaming_turn(
        state, run_id, run_store, session, turn_id, input, sender, turn_lease,
    ));
    Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn interrupt_turn(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((id, turn_id)): Path<(String, String)>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let turn_id = match parse_turn_id(&turn_id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let (run_id, run_store, _) = match load_session(&state, session_id).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    let pending_interrupt = match state
        .session_runtimes()
        .request_interrupt(session_id, turn_id)
    {
        Ok(pending_interrupt) => pending_interrupt,
        Err(InterruptTurnError::NotActive) => {
            return ApiError::new(StatusCode::CONFLICT, "Turn is not active for this session.")
                .into_response();
        }
    };
    match append_run_session_event(
        &run_store,
        run_id,
        session_id,
        EventBody::RunSessionTurnInterrupted(RunSessionTurnInterruptedProps {
            turn_id,
            error: Some("Interrupted.".to_string()),
        }),
        Utc::now(),
    )
    .await
    {
        Ok(event) => {
            pending_interrupt.cancel();
            (StatusCode::ACCEPTED, Json(event)).into_response()
        }
        Err(err) => {
            drop(pending_interrupt);
            store_error(&err).into_response()
        }
    }
}

async fn run_streaming_turn(
    state: Arc<AppState>,
    run_id: RunId,
    run_store: RunDatabase,
    session: ProjectedRunSession,
    turn_id: TurnId,
    input: String,
    sender: SessionSseSender,
    turn_lease: SessionTurnLease,
) {
    let session_id = session.record.id;
    if turn_lease.interrupt_requested() {
        let _ = append_and_send_event(
            &run_store,
            &sender,
            run_id,
            session_id,
            EventBody::RunSessionTurnInterrupted(RunSessionTurnInterruptedProps {
                turn_id,
                error: Some("Interrupted.".to_string()),
            }),
            Utc::now(),
        )
        .await;
        return;
    }

    let outcome = {
        let runtime_entry = turn_lease.entry();
        let mut session_slot = runtime_entry.lock_session().await;
        if session_slot.is_none() {
            match build_agent_session(&state, run_id, &session).await {
                Ok(agent_session) => {
                    *session_slot = Some(agent_session);
                }
                Err(err) => {
                    error!(error = ?err, session_id = %session_id, turn_id = %turn_id, "Failed to build run-backed session runtime");
                    let _ = append_and_send_event(
                        &run_store,
                        &sender,
                        run_id,
                        session_id,
                        EventBody::RunSessionTurnFailed(RunSessionTurnFailedProps {
                            turn_id,
                            error: err.to_string(),
                            output: None,
                        }),
                        Utc::now(),
                    )
                    .await;
                    return;
                }
            }
        }
        let session = session_slot
            .as_mut()
            .expect("session runtime slot should be loaded");
        let cancel_token = session.cancel_token();
        turn_lease.attach_cancel_token(&cancel_token);
        let initialize = !runtime_entry.is_initialized();
        let mut output = None;
        let result = Box::pin(drive_agent_session(
            &run_store,
            session,
            run_id,
            session_id,
            turn_id,
            &input,
            initialize,
            &sender,
            &mut output,
        ))
        .await;
        if initialize && matches!(result, Ok(Ok(()))) {
            runtime_entry.mark_initialized();
        }
        TurnExecutionOutcome { result, output }
    };

    match outcome.result {
        Ok(Ok(())) => {
            let _ = append_and_send_event(
                &run_store,
                &sender,
                run_id,
                session_id,
                EventBody::RunSessionTurnSucceeded(RunSessionTurnSucceededProps {
                    turn_id,
                    output: outcome.output,
                }),
                Utc::now(),
            )
            .await;
        }
        Ok(Err(err)) => {
            turn_lease.entry().clear_session().await;
            let body = if matches!(err, AgentError::Interrupted(_)) {
                EventBody::RunSessionTurnInterrupted(RunSessionTurnInterruptedProps {
                    turn_id,
                    error: Some(err.to_string()),
                })
            } else {
                EventBody::RunSessionTurnFailed(RunSessionTurnFailedProps {
                    turn_id,
                    error: err.to_string(),
                    output: outcome.output,
                })
            };
            let _ =
                append_and_send_event(&run_store, &sender, run_id, session_id, body, Utc::now())
                    .await;
        }
        Err(err) => {
            turn_lease.entry().clear_session().await;
            let _ = append_and_send_event(
                &run_store,
                &sender,
                run_id,
                session_id,
                EventBody::RunSessionTurnFailed(RunSessionTurnFailedProps {
                    turn_id,
                    error: err.to_string(),
                    output: outcome.output,
                }),
                Utc::now(),
            )
            .await;
        }
    }
}

struct TurnExecutionOutcome {
    result: anyhow::Result<Result<(), AgentError>>,
    output: Option<String>,
}

async fn build_agent_session(
    state: &AppState,
    run_id: RunId,
    session: &ProjectedRunSession,
) -> anyhow::Result<Session> {
    let catalog = state.catalog();
    let llm_result = state.resolve_llm_client().await?;
    for (provider, issue) in &llm_result.auth_issues {
        warn!(provider = %provider, error = %issue, "LLM provider unavailable due to auth issue");
    }
    for issue in &llm_result.registration_issues {
        warn!(provider = %issue.provider, error = %issue.error, "LLM provider unavailable due to registration issue");
    }
    let (provider_id, model, profile_kind) =
        selected_session_model(&catalog, &llm_result, session)?;
    if !llm_result.client.has_provider(provider_id.as_str()) {
        anyhow::bail!("LLM credentials not configured for provider '{provider_id}'");
    }

    let run_store = state.store_ref().open_run_reader(&run_id).await?;
    let projection = run_store.state().await?;
    let sandbox_record = projection
        .sandbox
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("run has no sandbox available for Ask Fabro"))?;
    let sandbox = reconnect_for_run(
        sandbox_record,
        state.vault_or_env("DAYTONA_API_KEY"),
        Some(run_id),
    )
    .await?;
    let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::from(sandbox);
    let profile = build_profile(
        provider_id,
        profile_kind,
        &model,
        &llm_result.client,
        Arc::clone(&catalog),
    );
    let config = SessionOptions {
        tool_hooks: Some(Arc::new(ToolApprovalAdapter(
            build_ask_fabro_tool_approval(),
        ))),
        ..SessionOptions::default()
    };

    Session::from_record(
        &session.record,
        &session.runtime_context,
        llm_result.client,
        profile,
        sandbox,
        config,
        None,
    )
    .map_err(Into::into)
}

fn selected_session_model(
    catalog: &Catalog,
    llm_result: &LlmClientResult,
    session: &ProjectedRunSession,
) -> anyhow::Result<(ProviderId, String, AgentProfileKind)> {
    let configured_provider_ids = llm_result.provider_ids();
    let selected = match session.record.model.as_deref() {
        Some(model_id) => catalog
            .get(model_id)
            .ok_or_else(|| anyhow::anyhow!("session model '{model_id}' is not in the catalog"))?,
        None => catalog.default_for_configured_ids(&configured_provider_ids),
    };
    let provider_id = selected.provider.clone();
    let model = selected.id.clone();
    let profile_kind = catalog
        .effective_agent_profile(&provider_id, Some(&model))
        .ok_or_else(|| anyhow::anyhow!("provider '{provider_id}' is not configured"))?;
    Ok((provider_id, model, profile_kind))
}

fn canonical_session_model(
    catalog: &Catalog,
    requested: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    let requested = requested.trim();
    if requested.is_empty() {
        return Err(ApiError::bad_request("Session model must not be empty."));
    }
    let model_ref = requested
        .parse::<SettingsModelRef>()
        .map_err(|err| ApiError::bad_request(err.to_string()))?;
    let registry = CatalogModelRegistry { catalog };
    match model_ref
        .resolve(&registry)
        .map_err(|err| ApiError::bad_request(err.to_string()))?
    {
        ResolvedModelRef::Provider(provider) => Err(ApiError::bad_request(format!(
            "Session model reference '{provider}' names a provider; include a model ID."
        ))),
        ResolvedModelRef::Model {
            provider: Some(provider),
            model,
        } => resolve_provider_qualified_session_model(catalog, &provider, &model).map(Some),
        ResolvedModelRef::Model {
            provider: None,
            model,
        } => catalog
            .get(&model)
            .map(|model| Some(model.id.clone()))
            .ok_or_else(|| ApiError::bad_request(format!("Unknown session model '{model}'."))),
    }
}

fn resolve_provider_qualified_session_model(
    catalog: &Catalog,
    provider_ref: &str,
    model_ref: &str,
) -> Result<String, ApiError> {
    let provider_id = ProviderId::new(provider_ref);
    let provider = catalog.provider(&provider_id).ok_or_else(|| {
        ApiError::bad_request(format!("Unknown session model provider '{provider_ref}'."))
    })?;
    let model = catalog
        .get(model_ref)
        .ok_or_else(|| ApiError::bad_request(format!("Unknown session model '{model_ref}'.")))?;
    if model.provider != provider.id {
        return Err(ApiError::bad_request(format!(
            "Session model '{model_ref}' belongs to provider '{}', not '{}'.",
            model.provider, provider.id
        )));
    }
    Ok(model.id.clone())
}

struct CatalogModelRegistry<'a> {
    catalog: &'a Catalog,
}

impl ModelRegistry for CatalogModelRegistry<'_> {
    fn is_provider(&self, token: &str) -> bool {
        self.catalog.provider(&ProviderId::new(token)).is_some()
    }

    fn is_model(&self, token: &str) -> bool {
        self.catalog.get(token).is_some()
    }

    fn provider_of(&self, token: &str) -> Option<String> {
        self.catalog
            .get(token)
            .map(|model| model.provider.to_string())
    }
}

fn build_profile(
    provider_id: ProviderId,
    profile_kind: AgentProfileKind,
    model: &str,
    llm_client: &LlmClient,
    catalog: Arc<Catalog>,
) -> Arc<dyn AgentProfile> {
    let summarizer = Some(WebFetchSummarizer {
        client:   llm_client.clone(),
        model_id: summarizer_model_id(&provider_id, profile_kind, &catalog, model),
    });
    let profile: Box<dyn AgentProfile> = match profile_kind {
        AgentProfileKind::OpenAi => Box::new(
            OpenAiProfile::with_summarizer(model, summarizer)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
        AgentProfileKind::Gemini => Box::new(
            GeminiProfile::with_summarizer(model, summarizer)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
        AgentProfileKind::Anthropic => Box::new(
            AnthropicProfile::with_summarizer(model, summarizer)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
    };
    Arc::from(profile)
}

fn summarizer_model_id(
    provider_id: &ProviderId,
    profile_kind: AgentProfileKind,
    catalog: &Catalog,
    selected_model: &str,
) -> ModelHandle {
    ModelHandle::ByName {
        provider: provider_id.clone(),
        model:    catalog
            .default_for_provider(provider_id)
            .map_or_else(
                || match profile_kind {
                    AgentProfileKind::Anthropic => "claude-haiku-4-5",
                    AgentProfileKind::OpenAi => selected_model,
                    AgentProfileKind::Gemini => "gemini-2.0-flash",
                },
                |model| model.id.as_str(),
            )
            .to_string(),
    }
}

fn build_ask_fabro_tool_approval() -> ToolApprovalFn {
    Arc::new(move |tool_name: &str, _args: &Value| {
        if is_tool_auto_approved(PermissionLevel::ReadOnly, tool_name) {
            Ok(())
        } else {
            Err(format!(
                "{tool_name} tool denied by Ask Fabro read-only policy"
            ))
        }
    })
}

async fn drive_agent_session(
    run_store: &RunDatabase,
    session: &mut Session,
    run_id: RunId,
    session_id: SessionId,
    turn_id: TurnId,
    input: &str,
    initialize: bool,
    sender: &SessionSseSender,
    output: &mut Option<String>,
) -> anyhow::Result<Result<(), AgentError>> {
    let mut receiver = session.subscribe();
    let process = async {
        if initialize {
            session.initialize().await?;
        }
        session.process_input(input).await
    };
    tokio::pin!(process);

    loop {
        tokio::select! {
            result = &mut process => {
                while let Ok(event) = receiver.try_recv() {
                    record_turn_output(output, &event);
                    persist_agent_event(run_store, run_id, session_id, turn_id, event, sender).await?;
                }
                return Ok(result);
            }
            event = receiver.recv() => {
                match event {
                    Ok(event) => {
                        record_turn_output(output, &event);
                        persist_agent_event(run_store, run_id, session_id, turn_id, event, sender).await?;
                    }
                    Err(RecvError::Lagged(_) | RecvError::Closed) => {}
                }
            }
        }
    }
}

fn record_turn_output(output: &mut Option<String>, event: &SessionEvent) {
    if let AgentEvent::AssistantMessage { text, .. } = &event.event {
        *output = Some(text.clone());
    }
}

async fn persist_agent_event(
    run_store: &RunDatabase,
    run_id: RunId,
    session_id: SessionId,
    turn_id: TurnId,
    event: SessionEvent,
    sender: &SessionSseSender,
) -> anyhow::Result<()> {
    let ts = event.timestamp.into();
    let Some(body) = agent_event_payload(turn_id, event.event) else {
        return Ok(());
    };
    append_and_send_event(run_store, sender, run_id, session_id, body, ts)
        .await
        .map_err(Into::into)
}

fn agent_event_payload(event_turn_id: TurnId, event: AgentEvent) -> Option<EventBody> {
    match event {
        AgentEvent::AssistantMessage {
            text, model, usage, ..
        } => Some(EventBody::RunSessionAssistantMessage(
            RunSessionAssistantMessageProps {
                turn_id: event_turn_id,
                text,
                model: Some(model.model_id),
                usage: serde_json::to_value(usage).unwrap_or(Value::Null),
            },
        )),
        AgentEvent::TextDelta { delta } | AgentEvent::ReasoningDelta { delta } => Some(
            EventBody::RunSessionAssistantDelta(RunSessionAssistantDeltaProps {
                turn_id: event_turn_id,
                delta,
            }),
        ),
        AgentEvent::ToolCallStarted {
            tool_name,
            tool_call_id,
            arguments,
        } => Some(EventBody::RunSessionToolCallStarted(
            RunSessionToolCallStartedProps {
                turn_id: event_turn_id,
                tool_name,
                tool_call_id,
                arguments,
            },
        )),
        AgentEvent::ToolCallCompleted {
            tool_name,
            tool_call_id,
            output,
            is_error,
        } => Some(EventBody::RunSessionToolCallCompleted(
            RunSessionToolCallCompletedProps {
                turn_id: event_turn_id,
                tool_name,
                tool_call_id,
                output,
                is_error,
            },
        )),
        _ => None,
    }
}

async fn append_and_send_event(
    run_store: &RunDatabase,
    sender: &SessionSseSender,
    run_id: RunId,
    session_id: SessionId,
    body: EventBody,
    ts: DateTime<Utc>,
) -> fabro_store::Result<()> {
    let event = append_run_session_event(run_store, run_id, session_id, body, ts).await?;
    send_sse_event(sender, &event).await;
    Ok(())
}

async fn append_run_session_event(
    run_store: &RunDatabase,
    run_id: RunId,
    session_id: SessionId,
    body: EventBody,
    ts: DateTime<Utc>,
) -> fabro_store::Result<EventEnvelope> {
    let event = RunEvent {
        id: format!("evt_{}", ulid::Ulid::new()),
        ts,
        run_id,
        node_id: None,
        node_label: None,
        stage_id: None,
        parallel_group_id: None,
        parallel_branch_id: None,
        session_id: Some(session_id.to_string()),
        parent_session_id: None,
        tool_call_id: None,
        actor: None,
        body,
    };
    let payload = EventPayload::new(event.to_value()?, &run_id)?;
    run_store.append_event_envelope(&payload).await
}

async fn send_sse_event(sender: &SessionSseSender, event: &EventEnvelope) -> bool {
    let Ok(data) = serde_json::to_string(event) else {
        return true;
    };
    sender
        .send(Ok(Event::default()
            .id(event.seq.to_string())
            .event(event.event.event_name())
            .data(data)))
        .await
        .is_ok()
}

async fn load_session(
    state: &AppState,
    session_id: SessionId,
) -> Result<(RunId, RunDatabase, ProjectedRunSession), Response> {
    let run_id = match state.store_ref().get_session_run_id(&session_id).await {
        Ok(Some(run_id)) => run_id,
        Ok(None) => return Err(ApiError::not_found("Session not found.").into_response()),
        Err(err) => return Err(store_error(&err).into_response()),
    };
    let run_store = open_run(state, run_id).await?;
    let events = match run_store.list_events().await {
        Ok(events) => events,
        Err(err) => return Err(store_error(&err).into_response()),
    };
    match fabro_store::project_run_session_with_context(run_id, session_id, &events) {
        Some(session) => Ok((run_id, run_store, session)),
        None => Err(ApiError::not_found("Session not found.").into_response()),
    }
}

async fn load_session_read(
    state: &AppState,
    session_id: SessionId,
) -> Result<(RunId, SessionRecord), Response> {
    let run_id = match state.store_ref().get_session_run_id(&session_id).await {
        Ok(Some(run_id)) => run_id,
        Ok(None) => return Err(ApiError::not_found("Session not found.").into_response()),
        Err(err) => return Err(store_error(&err).into_response()),
    };
    let run_store = open_run_reader(state, run_id).await?;
    let events = match run_store.list_events().await {
        Ok(events) => events,
        Err(err) => return Err(store_error(&err).into_response()),
    };
    match project_run_session(run_id, session_id, &events) {
        Some(session) => Ok((run_id, session)),
        None => Err(ApiError::not_found("Session not found.").into_response()),
    }
}

async fn open_run(state: &AppState, run_id: RunId) -> Result<RunDatabase, Response> {
    state.store_ref().open_run(&run_id).await.map_err(|err| {
        if matches!(err, fabro_store::Error::RunNotFound(_)) {
            ApiError::not_found("Run not found.").into_response()
        } else {
            store_error(&err).into_response()
        }
    })
}

async fn open_run_reader(state: &AppState, run_id: RunId) -> Result<RunDatabase, Response> {
    state
        .store_ref()
        .open_run_reader(&run_id)
        .await
        .map_err(|err| {
            if matches!(err, fabro_store::Error::RunNotFound(_)) {
                ApiError::not_found("Run not found.").into_response()
            } else {
                store_error(&err).into_response()
            }
        })
}

fn store_error(err: &fabro_store::Error) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn parse_run_id(value: &str) -> Result<RunId, ApiError> {
    value
        .parse()
        .map_err(|err| ApiError::bad_request(format!("Invalid run ID: {err}")))
}

fn parse_session_id(value: &str) -> Result<SessionId, ApiError> {
    value
        .parse()
        .map_err(|err| ApiError::bad_request(format!("Invalid session ID: {err}")))
}

fn parse_turn_id(value: &str) -> Result<TurnId, ApiError> {
    value
        .parse()
        .map_err(|err| ApiError::bad_request(format!("Invalid turn ID: {err}")))
}
