use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use fabro_api::types::{
    CreateRunSessionRequest, PaginatedSessionEventList, PaginationMeta, SubmitTurnRequest,
};
use fabro_llm::lithos_catalog::Catalog;
use fabro_llm::{FabroClient, ModelSelectionError, selection};
use fabro_sandbox::SecretRedactor;
use fabro_sandbox::reconnect::reconnect_for_run;
use fabro_store::{ProjectedRunSession, project_run_session, project_run_sessions};
use fabro_tool::fabro_client::ClientBackend;
use fabro_types::session_event::{
    SessionAssistantDeltaProps, SessionAssistantMessageProps, SessionCreatedProps,
    SessionToolCallCompletedProps, SessionToolCallStartedProps, SessionTurnFailedCode,
    SessionTurnFailedProps, SessionTurnInterruptedProps, SessionTurnStartedProps,
    SessionTurnSucceededProps, SessionUserMessageProps,
};
use fabro_types::settings::ModelRef as SettingsModelRef;
use fabro_types::{RunId, SessionDetail, SessionEvent, SessionEventBody, SessionId, TurnId};
use fabro_workflow::run_tools::register_named_fabro_run_tools;
use fabro_workflow::services::FabroRunToolServices;
use lithos_llm::catalog::ProviderId;
use pebble_coding_agent::environment::Environment;
use pebble_coding_agent::events::{CodingAgentEvent, CodingEvent, ToolSummary};
use pebble_coding_agent::extensions::{
    EnvContext, SystemPromptContext, SystemPromptDecision, SystemPromptTransform,
};
use pebble_coding_agent::tools::{
    PermissionMiddleware, ToolPermission, ToolPermissionPolicy, canonical_tool_name,
};
use pebble_coding_agent::{CodingAgent, CodingAgentOptions, Error as AgentError, ResumeMode};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use super::super::session_runtime::{InterruptTurnError, SessionTurnLease, StartTurnError};
use super::super::{AppState, PaginationParams, paginate_items, parse_run_id_path};
use crate::error::ApiError;
use crate::principal_middleware::RequiredUser;
use crate::worker_token::issue_worker_token;

const SESSION_SSE_BUFFER_CAPACITY: usize = 1024;

const ASK_FABRO_SYSTEM_PROMPT: &str = include_str!("prompts/ask_fabro.md.j2");

const ASK_FABRO_RUN_TOOL_NAMES: &[&str] = &[
    fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME,
    fabro_tool::FABRO_RUN_GET_TOOL_NAME,
];

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
        .route("/sessions/{id}/events", get(list_session_events))
        .route("/sessions/{id}/attach", get(attach_session_events))
        .route(
            "/sessions/{id}/turns",
            post(submit_turn).fallback(session_method_not_found),
        )
        .route(
            "/sessions/{id}/turns/{turnId}/interrupt",
            post(interrupt_turn),
        )
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum RunSessionListOrder {
    #[default]
    UpdatedDesc,
    CreatedDesc,
}

#[derive(serde::Deserialize)]
struct ListRunSessionsParams {
    #[serde(flatten)]
    pagination: PaginationParams,
    #[serde(default)]
    order:      RunSessionListOrder,
}

async fn list_run_sessions(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Query(params): Query<ListRunSessionsParams>,
) -> Response {
    let run_id = match parse_run_id_path(&run_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Err(response) = ensure_run(&state, run_id).await {
        return response;
    }
    match state.stores.session_events.list_for_run(run_id).await {
        Ok(events) => {
            let mut sessions = project_run_sessions(&events);
            match params.order {
                RunSessionListOrder::UpdatedDesc => sessions.sort_by(|left, right| {
                    right
                        .updated_at
                        .cmp(&left.updated_at)
                        .then_with(|| right.created_at.cmp(&left.created_at))
                        .then_with(|| right.id.cmp(&left.id))
                }),
                RunSessionListOrder::CreatedDesc => sessions.sort_by(|left, right| {
                    right
                        .created_at
                        .cmp(&left.created_at)
                        .then_with(|| right.id.cmp(&left.id))
                }),
            }
            let (data, has_more) = paginate_items(sessions, &params.pagination);
            Json(serde_json::json!({
                "data": data,
                "meta": { "has_more": has_more }
            }))
            .into_response()
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
    let run_id = match parse_run_id_path(&run_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Err(response) = ensure_run(&state, run_id).await {
        return response;
    }
    let llm_result = match state.resolve_llm_client().await {
        Ok(result) => result,
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to resolve LLM providers: {err}"),
            )
            .into_response();
        }
    };
    let eligible = llm_result.provider_ids().into_iter().collect();
    let (provider, model) = match canonical_session_model(
        state.catalog().as_ref(),
        &eligible,
        request.model.as_deref(),
        request.provider.as_ref(),
    ) {
        Ok(selection) => selection,
        Err(err) => return err.into_response(),
    };

    let session_id = SessionId::new();
    let now = Utc::now();
    let event = match append_run_session_event(
        &state,
        run_id,
        session_id,
        SessionEventBody::Created(SessionCreatedProps {
            title:    request.title,
            model:    Some(model),
            provider: Some(provider),
        }),
        now,
    )
    .await
    {
        Ok(event) => event,
        Err(err) => return store_error(&err).into_response(),
    };

    match project_run_session(session_id, &[event]) {
        Some(session) => (StatusCode::CREATED, Json(session.record)).into_response(),
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
    let (_, session) = match load_session(&state, session_id).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    Json(SessionDetail::new(session.record, session.last_seq)).into_response()
}

async fn session_method_not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// Query parameters for `/sessions/{id}/events`: the first sequence number
/// to include and the page size.
#[derive(serde::Deserialize)]
struct SessionEventListParams {
    #[serde(default)]
    since_seq: Option<u32>,
    #[serde(default)]
    limit:     Option<usize>,
}

impl SessionEventListParams {
    fn since_seq(&self) -> u32 {
        self.since_seq.unwrap_or(1).max(1)
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 1000)
    }
}

async fn list_session_events(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SessionEventListParams>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    if let Err(response) = load_session_owner(&state, session_id).await {
        return response;
    }
    let limit = params.limit();
    match state
        .stores
        .session_events
        .list_from(session_id, params.since_seq(), limit.saturating_add(1))
        .await
    {
        Ok(mut data) => {
            let has_more = data.len() > limit;
            data.truncate(limit);
            Json(PaginatedSessionEventList {
                data,
                meta: PaginationMeta {
                    has_more,
                    total: None,
                },
            })
            .into_response()
        }
        Err(err) => store_error(&err).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct AttachSessionParams {
    #[serde(default)]
    since_seq: Option<u32>,
}

async fn attach_session_events(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<AttachSessionParams>,
) -> Response {
    const ATTACH_REPLAY_BATCH_LIMIT: usize = 256;

    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    if let Err(response) = load_session_owner(&state, session_id).await {
        return response;
    }
    let start_seq = match params.since_seq {
        Some(seq) => seq.max(1),
        None => match state.stores.session_events.last_seq(session_id).await {
            Ok(last_seq) => last_seq.map_or(1, |seq| seq.saturating_add(1)),
            Err(err) => return store_error(&err).into_response(),
        },
    };
    let shutdown = state.shutdown_token();
    let (sender, receiver) = mpsc::channel(SESSION_SSE_BUFFER_CAPACITY);
    tokio::spawn(async move {
        let events = &state.stores.session_events;
        let mut next_seq = start_seq;
        // Subscribed before the replay, so an event committed between the
        // replay's last page and the live loop is not missed: the live loop
        // skips what the replay already sent by sequence number.
        let mut live = events.subscribe();
        'replay: loop {
            loop {
                let Ok(batch) = events
                    .list_from(session_id, next_seq, ATTACH_REPLAY_BATCH_LIMIT + 1)
                    .await
                else {
                    return;
                };
                let has_more = batch.len() > ATTACH_REPLAY_BATCH_LIMIT;
                for event in batch.into_iter().take(ATTACH_REPLAY_BATCH_LIMIT) {
                    next_seq = event.seq.saturating_add(1);
                    if !send_attach_sse_event(&sender, &shutdown, session_sse_event(&event)).await {
                        return;
                    }
                }
                if !has_more {
                    break;
                }
            }

            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return,
                    () = sender.closed() => return,
                    next = live.recv() => match next {
                        Ok(event) => {
                            if event.session_id != session_id || event.seq < next_seq {
                                continue;
                            }
                            next_seq = event.seq.saturating_add(1);
                            if !send_attach_sse_event(
                                &sender,
                                &shutdown,
                                session_sse_event(&event),
                            )
                            .await
                            {
                                return;
                            }
                        }
                        // The subscriber fell behind the store's buffer; the
                        // table holds what it missed.
                        Err(RecvError::Lagged(_)) => continue 'replay,
                        Err(RecvError::Closed) => return,
                    }
                }
            }
        }
    });

    Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::default())
        .into_response()
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
    let (run_id, session) = match load_session(&state, session_id).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    let input = request.input;

    let turn_id = match request.turn_id {
        Some(turn_id) => turn_id,
        None => TurnId::new(),
    };
    let turn_lease = match state.session_runtimes().reserve_turn(session_id, turn_id) {
        Ok(lease) => lease,
        Err(StartTurnError::ActiveTurn { turn_id }) => {
            let mut response = ApiError::with_code(
                StatusCode::CONFLICT,
                "Session already has an active turn.",
                "session_active_turn",
            )
            .into_response();
            if let Ok(value) = HeaderValue::from_str(&turn_id.to_string()) {
                response
                    .headers_mut()
                    .insert("x-fabro-active-turn-id", value);
            }
            return response;
        }
    };

    let (sender, receiver) = mpsc::channel(SESSION_SSE_BUFFER_CAPACITY);
    let now = Utc::now();
    for body in [
        SessionEventBody::TurnStarted(SessionTurnStartedProps {
            turn_id,
            input: input.clone(),
        }),
        SessionEventBody::UserMessage(SessionUserMessageProps {
            turn_id,
            text: input.clone(),
        }),
    ] {
        match append_and_send_event(&state, &sender, run_id, session_id, body, now).await {
            Ok(()) => {}
            Err(err) => {
                drop(turn_lease);
                return store_error(&err).into_response();
            }
        }
    }

    tokio::spawn(run_streaming_turn(
        state, run_id, session, turn_id, input, sender, turn_lease,
    ));
    let mut response = Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::default())
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&turn_id.to_string()) {
        response.headers_mut().insert("x-fabro-turn-id", value);
    }
    response
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
    let (run_id, _) = match load_session(&state, session_id).await {
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
        &state,
        run_id,
        session_id,
        SessionEventBody::TurnInterrupted(SessionTurnInterruptedProps {
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
    session: ProjectedRunSession,
    turn_id: TurnId,
    input: String,
    sender: SessionSseSender,
    turn_lease: SessionTurnLease,
) {
    let session_id = session.record.id;
    if turn_lease.interrupt_requested() {
        let _ = append_and_send_event(
            &state,
            &sender,
            run_id,
            session_id,
            SessionEventBody::TurnInterrupted(SessionTurnInterruptedProps {
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
        let mut agent_slot = runtime_entry.lock_agent().await;
        if agent_slot.is_none() {
            match build_agent(&state, run_id, &session).await {
                Ok(agent) => {
                    *agent_slot = Some(agent);
                }
                Err(err) => {
                    error!(error = ?err, session_id = %session_id, turn_id = %turn_id, "Failed to build run-backed session runtime");
                    let _ = append_and_send_event(
                        &state,
                        &sender,
                        run_id,
                        session_id,
                        turn_failed_body(
                            turn_id,
                            err.to_string(),
                            None,
                            err.code(),
                            err.retryable(),
                        ),
                        Utc::now(),
                    )
                    .await;
                    return;
                }
            }
        }
        let agent = agent_slot
            .as_mut()
            .expect("session runtime slot should be loaded");
        let cancel_token = CancellationToken::new();
        turn_lease.attach_cancel_token(&cancel_token);
        let model_input = match state.load_run_projection(&run_id).await {
            Ok(projection) => {
                let snapshot = build_ask_fabro_run_snapshot(&projection, run_id);
                build_ask_fabro_turn_input(&input, &snapshot)
            }
            Err(err) => {
                warn!(
                    error = err.detail(),
                    session_id = %session_id,
                    turn_id = %turn_id,
                    "Failed to build Ask Fabro run snapshot"
                );
                let snapshot = format!(
                    "Run ID: {run_id}\nRun snapshot unavailable: failed to load current run projection."
                );
                build_ask_fabro_turn_input(&input, &snapshot)
            }
        };
        let mut output = None;
        let result = Box::pin(drive_agent(
            &state,
            agent,
            run_id,
            session_id,
            turn_id,
            &model_input,
            &cancel_token,
            &sender,
            &mut output,
        ))
        .await;
        // The record is taken after the prompt's event barrier, so it holds
        // the whole turn. Persisting it after every turn is what makes the
        // session resumable by another process.
        if !matches!(result, Ok(Err(pebble_coding_agent::Error::SessionClosed))) {
            if let Err(err) = state
                .stores
                .session_records
                .put(session_id, run_id, &agent.to_record(), Utc::now())
                .await
            {
                error!(error = %err, session_id = %session_id, "Failed to persist Ask Fabro session record");
            }
        }
        TurnExecutionOutcome { result, output }
    };

    match outcome.result {
        Ok(Ok(())) => {
            let _ = append_and_send_event(
                &state,
                &sender,
                run_id,
                session_id,
                SessionEventBody::TurnSucceeded(SessionTurnSucceededProps {
                    turn_id,
                    output: outcome.output,
                }),
                Utc::now(),
            )
            .await;
        }
        Ok(Err(err)) => {
            turn_lease.entry().clear_agent().await;
            let body = if matches!(err, AgentError::Interrupted(_)) {
                SessionEventBody::TurnInterrupted(SessionTurnInterruptedProps {
                    turn_id,
                    error: Some(err.to_string()),
                })
            } else {
                let code = agent_failure_code(&err);
                turn_failed_body(turn_id, err.to_string(), outcome.output, code, false)
            };
            let _ =
                append_and_send_event(&state, &sender, run_id, session_id, body, Utc::now()).await;
        }
        Err(err) => {
            turn_lease.entry().clear_agent().await;
            let _ = append_and_send_event(
                &state,
                &sender,
                run_id,
                session_id,
                turn_failed_body(
                    turn_id,
                    err.to_string(),
                    outcome.output,
                    SessionTurnFailedCode::AgentError,
                    false,
                ),
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

#[derive(Debug, thiserror::Error)]
enum AskFabroBuildError {
    #[error("{0}")]
    LlmUnconfigured(String),
    #[error("{0}")]
    ModelUnavailable(String),
    #[error("run has no sandbox available for Ask Fabro")]
    NoSandbox,
    #[error("run sandbox is unavailable for Ask Fabro: {0}")]
    SandboxUnavailable(#[source] anyhow::Error),
    #[error("failed to create Ask Fabro agent session: {0}")]
    Agent(#[source] anyhow::Error),
}

impl AskFabroBuildError {
    fn code(&self) -> SessionTurnFailedCode {
        match self {
            Self::NoSandbox => SessionTurnFailedCode::NoSandbox,
            Self::SandboxUnavailable(_) => SessionTurnFailedCode::SandboxUnavailable,
            Self::LlmUnconfigured(_) => SessionTurnFailedCode::LlmUnconfigured,
            Self::ModelUnavailable(_) => SessionTurnFailedCode::ModelUnavailable,
            Self::Agent(_) => SessionTurnFailedCode::AgentError,
        }
    }

    fn retryable(&self) -> bool {
        matches!(self, Self::SandboxUnavailable(_))
    }
}

/// The Ask Fabro agent for `session`: resumed from its stored record when a
/// turn has been persisted, built fresh otherwise.
async fn build_agent(
    state: &AppState,
    run_id: RunId,
    session: &ProjectedRunSession,
) -> Result<CodingAgent, AskFabroBuildError> {
    let catalog = state.catalog();
    let llm_result = state.resolve_llm_client().await.map_err(|err| {
        AskFabroBuildError::LlmUnconfigured(format!("LLM credentials are not configured: {err}"))
    })?;
    for (provider, issue) in &llm_result.auth_issues {
        warn!(provider = %provider, error = %issue, "LLM provider unavailable due to auth issue");
    }
    for issue in &llm_result.build_issues {
        warn!(provider = %issue.provider, error = %issue.cause, "LLM provider unavailable due to build issue");
    }
    let (provider_id, model) = selected_session_model(&catalog, &llm_result, session)?;
    if !llm_result.has_provider(&provider_id) {
        let message = format!("LLM credentials not configured for provider '{provider_id}'");
        return if session.record.model.is_some() {
            Err(AskFabroBuildError::ModelUnavailable(message))
        } else {
            Err(AskFabroBuildError::LlmUnconfigured(message))
        };
    }

    let projection = state
        .load_run_projection(&run_id)
        .await
        .map_err(|err| AskFabroBuildError::Agent(anyhow::anyhow!("{}", err.detail())))?;
    let sandbox_record = projection
        .sandbox
        .as_ref()
        .ok_or(AskFabroBuildError::NoSandbox)?;
    let sandbox_instance = sandbox_record.instance().ok_or_else(|| {
        AskFabroBuildError::SandboxUnavailable(anyhow::anyhow!("run sandbox was not created"))
    })?;
    let access = state
        .provider_access()
        .await
        .map_err(|err| AskFabroBuildError::Agent(anyhow::Error::new(err)))?;
    let sandbox = reconnect_for_run(sandbox_instance, &access, Some(run_id), None)
        .await
        .map_err(AskFabroBuildError::SandboxUnavailable)?;
    sandbox
        .activate()
        .await
        .map_err(|err| AskFabroBuildError::SandboxUnavailable(anyhow::Error::new(err)))?;
    let environment: Arc<dyn Environment> = Arc::new(sandbox);

    // Give the Ask Fabro agent access to read-only run-inspection tools scoped
    // to its owning run. The session reaches the local HTTP API via a same-run
    // worker token; the scoped backend rejects accidental cross-run tool calls
    // and the server's auth middleware remains a backstop for direct HTTP.
    let worker_token = issue_worker_token(state.worker_token_keys(), &run_id)
        .map_err(|_| AskFabroBuildError::Agent(anyhow::anyhow!("failed to sign worker token")))?;
    let target = state
        .self_server_target()
        .map_err(AskFabroBuildError::Agent)?;
    let api_client = fabro_client::Client::builder()
        .target(target)
        .credential(fabro_client::Credential::Worker(worker_token))
        .connect()
        .await
        .map_err(AskFabroBuildError::Agent)?;
    let backend = ClientBackend::new(Arc::new(api_client)).with_run_scope(run_id);
    let services = FabroRunToolServices {
        backend:        Arc::new(backend),
        current_run_id: run_id,
    };
    let run_tools = register_named_fabro_run_tools(&services, ASK_FABRO_RUN_TOOL_NAMES);
    let selector = format!("{provider_id}/{model}");

    // A resumed session continues its stored conversation on the model it
    // recorded; a record whose events outran it (a crash between the event
    // append and the record write) is moved past the session's last
    // sequence so the stream never reuses a number.
    let stored = state
        .stores
        .session_records
        .get(session.record.id)
        .await
        .map_err(|err| AskFabroBuildError::Agent(anyhow::Error::new(err)))?;
    let builder = match stored {
        Some(stored) => {
            let mut record = stored.record;
            if let Ok(Some(last_seq)) = state
                .stores
                .session_events
                .last_seq(session.record.id)
                .await
            {
                record.resume_after(u64::from(last_seq));
            }
            CodingAgent::resume(
                llm_result.client,
                environment,
                record,
                ResumeMode::RecordedModel,
            )
        }
        None => CodingAgent::builder(llm_result.client, environment)
            .model(selector)
            .options(
                CodingAgentOptions::default()
                    // A short-lived analyst has no project memory or skills of
                    // its own; the prompt says what it may do.
                    .with_context_compaction(true),
            ),
    };
    builder
        .tools(run_tools)
        // The read-only policy hides and refuses every other tool, so the
        // agent gets exactly the read tools and the two run tools.
        .tool_middleware(Arc::new(PermissionMiddleware::new(Arc::new(
            AskFabroToolPolicy,
        ))))
        .system_prompt_transform(Arc::new(AskFabroPrompt))
        .redactor(Arc::new(SecretRedactor))
        .build()
        .await
        .map_err(|err| AskFabroBuildError::Agent(anyhow::Error::new(err)))
}

fn selected_session_model(
    catalog: &Catalog,
    llm_result: &FabroClient,
    session: &ProjectedRunSession,
) -> Result<(ProviderId, String), AskFabroBuildError> {
    let eligible = llm_result
        .provider_ids()
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let record = &session.record;
    let selected = selection::resolve_selection(
        catalog,
        record.model.as_deref(),
        record.provider.as_ref(),
        &eligible,
    )
    .map_err(|error| {
        // A missing default with no provider pin means no LLM is
        // configured at all; every other failure is about the requested
        // model/provider.
        if record.provider.is_none() && matches!(error, ModelSelectionError::NoDefaultModel { .. })
        {
            AskFabroBuildError::LlmUnconfigured(error.to_string())
        } else {
            AskFabroBuildError::ModelUnavailable(error.to_string())
        }
    })?;
    Ok((selected.provider, selected.model))
}

fn canonical_session_model(
    catalog: &Catalog,
    eligible: &std::collections::HashSet<ProviderId>,
    requested: Option<&str>,
    explicit_provider: Option<&ProviderId>,
) -> Result<(ProviderId, String), ApiError> {
    let explicit_provider = explicit_provider
        .map(|provider| {
            enabled_provider_id(catalog, provider.as_str()).ok_or_else(|| {
                session_selection_error(&ModelSelectionError::UnknownProvider {
                    provider: provider.to_string(),
                })
            })
        })
        .transpose()?;
    let Some(requested) = requested else {
        let selected =
            selection::resolve_selection(catalog, None, explicit_provider.as_ref(), eligible)
                .map_err(|error| session_selection_error(&error))?;
        return Ok((selected.provider, selected.model));
    };
    let requested = requested.trim();
    if requested.is_empty() {
        return Err(ApiError::bad_request("Session model must not be empty."));
    }
    // An aggregator's wire id (`openai/gpt-5.6-sol` on OpenRouter) is matched
    // whole on a pinned provider before its prefix is read as a provider.
    if let Some(explicit) = explicit_provider.as_ref().filter(|p| eligible.contains(*p)) {
        if let Some(entry) = catalog
            .enabled_provider(explicit.as_str())
            .and_then(|provider| provider.offering(requested))
        {
            return Ok((explicit.clone(), entry.model.id().to_string()));
        }
    }
    let model_ref = requested
        .parse::<SettingsModelRef>()
        .map_err(|err| ApiError::bad_request(err.to_string()))?
        .qualify(catalog);
    let (qualified_provider, selector) = match model_ref {
        SettingsModelRef::Qualified { provider, selector } => {
            let provider = enabled_provider_id(catalog, &provider).ok_or_else(|| {
                session_selection_error(&ModelSelectionError::UnknownProvider { provider })
            })?;
            // When the prefixed provider is not ready, the whole string may
            // still be an eligible aggregator's wire id for the same model.
            if explicit_provider.is_none() && !eligible.contains(&provider) {
                if let Some(found) = api_model_on_eligible(catalog, requested, eligible) {
                    return Ok(found);
                }
            }
            if let Some(explicit) = explicit_provider.as_ref() {
                if explicit != &provider {
                    return Err(ApiError::bad_request(format!(
                        "Session provider pin '{explicit}' conflicts with model reference provider \
                         '{provider}'."
                    )));
                }
            }
            (Some(provider), selector)
        }
        SettingsModelRef::Bare(selector) => {
            if explicit_provider.is_none() && catalog.enabled_provider(&selector).is_some() {
                let detail = if catalog.is_model_selector(&selector) {
                    format!(
                        "Session model reference '{selector}' is ambiguous between a provider and \
                         a model selector; supply `provider` or use `provider:model`."
                    )
                } else {
                    format!(
                        "Session model reference '{selector}' names a provider; include a model ID."
                    )
                };
                return Err(ApiError::bad_request(detail));
            }
            (None, selector)
        }
    };
    let provider = qualified_provider.as_ref().or(explicit_provider.as_ref());
    let selected = selection::resolve_selection(catalog, Some(&selector), provider, eligible)
        .map_err(|error| session_selection_error(&error))?;
    Ok((selected.provider, selected.model))
}

/// The highest-priority eligible provider offering `api_model` as a wire id.
fn api_model_on_eligible(
    catalog: &Catalog,
    api_model: &str,
    eligible: &std::collections::HashSet<ProviderId>,
) -> Option<(ProviderId, String)> {
    catalog
        .enabled_providers()
        .into_iter()
        .filter(|provider| eligible.contains(provider.id()))
        .find_map(|provider| {
            provider
                .offerings()
                .find(|model| model.model.api_model() == api_model)
                .map(|model| (provider.id().clone(), model.model.id().to_string()))
        })
}

/// The catalog id of an enabled provider named by id or alias.
fn enabled_provider_id(catalog: &Catalog, selector: &str) -> Option<ProviderId> {
    catalog
        .enabled_provider(selector)
        .map(|provider| provider.id().clone())
}

fn session_selection_error(error: &ModelSelectionError) -> ApiError {
    ApiError::bad_request(error.to_string())
}

/// Ask Fabro reads. Every write, shell, web, and run-control tool is hidden
/// from the model and refused if called anyway.
struct AskFabroToolPolicy;

impl ToolPermissionPolicy for AskFabroToolPolicy {
    fn permission(
        &self,
        _session: &pebble_coding_agent::SessionScope,
        tool: &pebble_agent::ToolDescriptor,
    ) -> ToolPermission {
        if ask_fabro_allows_tool(tool.id().as_str()) {
            ToolPermission::Allow
        } else {
            ToolPermission::Deny {
                reason: "denied by tool access policy: Ask Fabro is read-only".to_string(),
            }
        }
    }
}

/// Whether Ask Fabro may call `tool_name`, resolved through the canonical
/// name so a profile with its own vocabulary (the Kimi profile uses
/// `Read`/`Grep`/`Glob`) is not denied its whole tool set.
fn ask_fabro_allows_tool(tool_name: &str) -> bool {
    match canonical_tool_name(tool_name) {
        "read_file" | "grep" | "glob" => true,
        name => ASK_FABRO_RUN_TOOL_NAMES.contains(&name),
    }
}

/// The Ask Fabro system prompt: the analyst contract plus the environment
/// block and the tools the policy lets through.
struct AskFabroPrompt;

impl SystemPromptTransform for AskFabroPrompt {
    fn transform(&self, context: SystemPromptContext<'_>) -> SystemPromptDecision {
        SystemPromptDecision::Replace(build_ask_fabro_system_prompt(
            context.environment(),
            context.tools(),
        ))
    }
}

fn render_ask_fabro_tool_guidance(tools: &[ToolSummary]) -> String {
    let mut tools: Vec<&ToolSummary> = tools
        .iter()
        .filter(|tool| ask_fabro_allows_tool(&tool.name))
        .collect();
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools
        .into_iter()
        .map(|tool| format!("- `{}`: {}", tool.name, tool.description))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_ask_fabro_env_block(environment: &EnvContext) -> String {
    let mut lines = vec![
        "<environment>".to_string(),
        format!("Working directory: {}", environment.working_directory),
        format!("Is git repository: {}", environment.is_git_repo),
    ];
    if let Some(branch) = &environment.git_branch {
        lines.push(format!("Git branch: {branch}"));
    }
    lines.push(format!("Platform: {}", environment.platform));
    lines.push(format!("OS version: {}", environment.os_version));
    if !environment.current_date.is_empty() {
        lines.push(format!("Today's date: {}", environment.current_date));
    }
    if !environment.model.is_empty() {
        lines.push(format!("Model: {}", environment.model));
    }
    lines.push("</environment>".to_string());
    lines.join("\n")
}

fn build_ask_fabro_system_prompt(environment: &EnvContext, tools: &[ToolSummary]) -> String {
    // `tool_guidance` is passed as a template variable rather than interpolated
    // into the template text: it carries tool names and descriptions that can
    // come from MCP servers, and MiniJinja does not re-render substituted
    // values, so arbitrary `{{ ... }}` in a tool description stays inert.
    let inputs = HashMap::from([
        (
            "env_block".to_string(),
            toml::Value::String(render_ask_fabro_env_block(environment)),
        ),
        (
            "tool_guidance".to_string(),
            toml::Value::String(render_ask_fabro_tool_guidance(tools)),
        ),
    ]);
    let ctx = fabro_template::TemplateContext::new().with_inputs(inputs);
    fabro_template::render_named("ask_fabro.md.j2", ASK_FABRO_SYSTEM_PROMPT, &ctx)
        .unwrap_or_else(|err| panic!("embedded Ask Fabro prompt failed to render: {err}"))
}

fn build_ask_fabro_run_snapshot(projection: &fabro_types::RunProjection, run_id: RunId) -> String {
    let mut lines = Vec::new();
    let graph = &projection.spec().graph;
    lines.push(format!("Run ID: {run_id}"));
    lines.push(format!("Goal: {}", graph.goal()));
    lines.push(format!("Status: {}", projection.status()));

    let total_non_meta = graph
        .nodes
        .values()
        .filter(|node| !is_ask_fabro_meta_node(node))
        .count();
    let completed_non_meta = projection
        .iter_stages()
        .filter(|(stage_id, stage)| {
            graph
                .nodes
                .get(stage_id.node_id())
                .is_none_or(|node| !is_ask_fabro_meta_node(node))
                && stage.effective_state().is_terminal()
        })
        .count();
    lines.push(format!(
        "Progress: {completed_non_meta} of {total_non_meta} non-meta stages completed"
    ));

    let recent_stages = projection
        .iter_stages()
        .filter(|(stage_id, _)| {
            graph
                .nodes
                .get(stage_id.node_id())
                .is_none_or(|node| !is_ask_fabro_meta_node(node))
        })
        .collect::<Vec<_>>();
    let recent_stages = recent_stages
        .iter()
        .rev()
        .take(5)
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    if !recent_stages.is_empty() {
        lines.push(String::new());
        lines.push("Recent stages:".to_string());
        for (stage_id, stage) in recent_stages {
            lines.push(format!("- {}", ask_fabro_stage_summary(stage_id, stage)));
        }
    }

    if let Some((_, record)) = projection.pending_interviews().iter().next() {
        lines.push(String::new());
        lines.push("Pending human input:".to_string());
        let question_id = if record.question.id.trim().is_empty() {
            "question"
        } else {
            record.question.id.as_str()
        };
        lines.push(format!("- {question_id}: awaiting response"));
    }

    lines.push(String::new());
    lines.push("Use this snapshot as orientation only. For exact, current, or disputed details, inspect run events with `fabro_run_events`.".to_string());
    lines.join("\n")
}

fn is_ask_fabro_meta_node(node: &fabro_types::Node) -> bool {
    matches!(node.handler_type(), Some("start" | "exit"))
}

fn ask_fabro_stage_summary(
    stage_id: &fabro_types::StageId,
    stage: &fabro_types::StageProjection,
) -> String {
    let mut line = format!("{}: {}", stage_id.node_id(), stage.effective_state());
    if let Some(handler) = stage
        .handler
        .as_ref()
        .map(ToString::to_string)
        .filter(|handler| !handler.is_empty())
    {
        let _ = write!(line, ", {handler}");
    }
    if let Some(model) = &stage.model {
        let _ = write!(line, ", model {}", model.model_id);
    }
    if let Some(completion) = &stage.completion {
        if let Some(reason) = completion.failure_reason.as_deref() {
            let _ = write!(line, ", reason: {reason}");
        }
    }
    line
}

fn build_ask_fabro_turn_input(input: &str, snapshot: &str) -> String {
    format!(
        "\
Use the following run snapshot as orientation for this turn. Treat it as possibly stale. For exact or current details, inspect run events with `fabro_run_events`.

<run_snapshot>
{snapshot}
</run_snapshot>

User question:
{input}"
    )
}

async fn drive_agent(
    state: &AppState,
    agent: &mut CodingAgent,
    run_id: RunId,
    session_id: SessionId,
    turn_id: TurnId,
    input: &str,
    cancel_token: &CancellationToken,
    sender: &SessionSseSender,
    output: &mut Option<String>,
) -> anyhow::Result<Result<(), AgentError>> {
    let mut receiver = agent.subscribe();
    let prompt = agent.prompt_with_cancellation(input, cancel_token);
    tokio::pin!(prompt);

    loop {
        tokio::select! {
            report = &mut prompt => {
                while let Ok(event) = receiver.try_recv() {
                    record_turn_output(output, &event);
                    Box::pin(persist_agent_event(
                        state, run_id, session_id, turn_id, event, sender,
                    ))
                    .await?;
                }
                return Ok(report.result.map(|_| ()));
            }
            event = receiver.recv() => {
                match event {
                    Ok(event) => {
                        record_turn_output(output, &event);
                        Box::pin(persist_agent_event(
                            state, run_id, session_id, turn_id, event, sender,
                        ))
                        .await?;
                    }
                    Err(RecvError::Lagged(_) | RecvError::Closed) => {}
                }
            }
        }
    }
}

fn record_turn_output(output: &mut Option<String>, event: &CodingAgentEvent) {
    if let CodingEvent::AssistantMessage { text, .. } = &event.event {
        *output = Some(text.clone());
    }
}

fn turn_failed_body(
    turn_id: TurnId,
    error: String,
    output: Option<String>,
    code: SessionTurnFailedCode,
    retryable: bool,
) -> SessionEventBody {
    SessionEventBody::TurnFailed(SessionTurnFailedProps {
        turn_id,
        error,
        output,
        code,
        retryable,
    })
}

fn agent_failure_code(err: &AgentError) -> SessionTurnFailedCode {
    match err {
        AgentError::ToolExecution(message)
            if message.contains("denied") || message.contains("not allowed") =>
        {
            SessionTurnFailedCode::ToolDenied
        }
        _ => SessionTurnFailedCode::AgentError,
    }
}

async fn persist_agent_event(
    state: &AppState,
    run_id: RunId,
    session_id: SessionId,
    turn_id: TurnId,
    event: CodingAgentEvent,
    sender: &SessionSseSender,
) -> anyhow::Result<()> {
    let ts = event.timestamp.into();
    let Some(body) = agent_event_payload(turn_id, event.event) else {
        return Ok(());
    };
    append_and_send_event(state, sender, run_id, session_id, body, ts)
        .await
        .map_err(Into::into)
}

fn agent_event_payload(event_turn_id: TurnId, event: CodingEvent) -> Option<SessionEventBody> {
    match event {
        CodingEvent::AssistantMessage {
            text, model, usage, ..
        } => Some(SessionEventBody::AssistantMessage(
            SessionAssistantMessageProps {
                turn_id: event_turn_id,
                text,
                model: Some(model),
                usage: serde_json::to_value(usage).unwrap_or(Value::Null),
            },
        )),
        CodingEvent::TextDelta { delta } => Some(SessionEventBody::AssistantDelta(
            SessionAssistantDeltaProps {
                turn_id: event_turn_id,
                delta,
            },
        )),
        CodingEvent::ToolCallStarted {
            tool_name,
            tool_call_id,
            arguments,
        } => Some(SessionEventBody::ToolCallStarted(
            SessionToolCallStartedProps {
                turn_id: event_turn_id,
                tool_name,
                tool_call_id,
                arguments,
            },
        )),
        CodingEvent::ToolCallCompleted {
            tool_name,
            tool_call_id,
            output,
            is_error,
            output_bytes_observed,
            output_bytes_retained,
            output_bytes_omitted,
            ..
        } => Some(SessionEventBody::ToolCallCompleted(
            SessionToolCallCompletedProps {
                turn_id: event_turn_id,
                tool_name,
                tool_call_id,
                output,
                is_error,
                output_bytes_observed: Some(
                    u64::try_from(output_bytes_observed).unwrap_or(u64::MAX),
                ),
                output_bytes_retained: Some(
                    u64::try_from(output_bytes_retained).unwrap_or(u64::MAX),
                ),
                output_bytes_omitted: Some(u64::try_from(output_bytes_omitted).unwrap_or(u64::MAX)),
            },
        )),
        _ => None,
    }
}

async fn append_and_send_event(
    state: &AppState,
    sender: &SessionSseSender,
    run_id: RunId,
    session_id: SessionId,
    body: SessionEventBody,
    ts: DateTime<Utc>,
) -> fabro_store::Result<()> {
    let event = append_run_session_event(state, run_id, session_id, body, ts).await?;
    send_sse_event(sender, &event).await;
    Ok(())
}

async fn append_run_session_event(
    state: &AppState,
    run_id: RunId,
    session_id: SessionId,
    body: SessionEventBody,
    ts: DateTime<Utc>,
) -> fabro_store::Result<SessionEvent> {
    state
        .stores
        .session_events
        .append(run_id, session_id, body, ts)
        .await
}

async fn send_sse_event(sender: &SessionSseSender, event: &SessionEvent) -> bool {
    sender.send(Ok(session_sse_event(event))).await.is_ok()
}

fn session_sse_event(event: &SessionEvent) -> Event {
    let data = serde_json::to_string(event).unwrap_or_else(|_| "null".to_string());
    Event::default()
        .id(event.seq.to_string())
        .event(event.event_name())
        .data(data)
}

async fn send_attach_sse_event(
    sender: &SessionSseSender,
    shutdown: &CancellationToken,
    event: Event,
) -> bool {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => false,
        () = sender.closed() => false,
        result = sender.send(Ok(event)) => result.is_ok(),
    }
}

/// The session as its events describe it, with the run that owns it.
async fn load_session(
    state: &AppState,
    session_id: SessionId,
) -> Result<(RunId, ProjectedRunSession), Response> {
    let run_id = load_session_owner(state, session_id).await?;
    let events = state
        .stores
        .session_events
        .list_from(session_id, 1, usize::MAX)
        .await
        .map_err(|err| store_error(&err).into_response())?;
    match project_run_session(session_id, &events) {
        Some(session) => Ok((run_id, session)),
        None => Err(ApiError::not_found("Session not found.").into_response()),
    }
}

/// The run that owns `session_id`, from the session's creation event.
async fn load_session_owner(state: &AppState, session_id: SessionId) -> Result<RunId, Response> {
    match state.stores.session_events.owner(session_id).await {
        Ok(Some(run_id)) => Ok(run_id),
        Ok(None) => Err(ApiError::not_found("Session not found.").into_response()),
        Err(err) => Err(store_error(&err).into_response()),
    }
}

async fn ensure_run(state: &AppState, run_id: RunId) -> Result<(), Response> {
    match state.stores.run_summaries.contains(&run_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(ApiError::not_found("Run not found.").into_response()),
        Err(err) => Err(store_error(&err).into_response()),
    }
}

fn store_error(err: &fabro_store::Error) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_types::{PetriAdmission, test_support};
    use pebble_coding_agent::events::{ToolCategory, ToolSource};

    use super::*;

    fn tool_summary(name: &str) -> ToolSummary {
        ToolSummary {
            name:        name.to_string(),
            description: format!("{name} test tool"),
            source:      ToolSource::Native,
            category:    ToolCategory::Other,
            invoked:     false,
        }
    }

    fn ask_fabro_test_tools() -> Vec<ToolSummary> {
        [
            "read_file",
            "grep",
            "glob",
            "write_file",
            "edit_file",
            "shell",
            "web_search",
            "web_fetch",
            fabro_tool::FABRO_RUN_CREATE_TOOL_NAME,
            fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME,
            fabro_tool::FABRO_RUN_GET_TOOL_NAME,
            fabro_tool::FABRO_RUN_INTERACT_TOOL_NAME,
            fabro_tool::FABRO_RUN_PAIR_TOOL_NAME,
        ]
        .into_iter()
        .map(tool_summary)
        .collect()
    }

    /// OpenAI and OpenRouter both offer `gpt-5.6-sol` under the `gpt-56-sol`
    /// alias; both ship enabled, and the overlay makes it the default on
    /// each.
    fn portable_session_catalog() -> Catalog {
        fabro_llm::test_support::test_catalog_with_overlay(
            r#"
[providers.openai]
default_model = "gpt-5.6-sol"

[providers.openrouter]
default_model = "gpt-5.6-sol"
enabled = true

"#,
        )
    }

    #[test]
    fn canonical_session_model_uses_readiness_priority_and_explicit_pins() {
        let catalog = portable_session_catalog();
        let openai = lithos_llm::catalog::builtin::openai();
        let openrouter = ProviderId::new("openrouter");

        assert_eq!(
            canonical_session_model(
                &catalog,
                &std::collections::HashSet::from([openai.clone()]),
                Some("gpt-56-sol"),
                None,
            )
            .unwrap(),
            (openai.clone(), "gpt-5.6-sol".to_string())
        );
        assert_eq!(
            canonical_session_model(
                &catalog,
                &std::collections::HashSet::from([openrouter.clone()]),
                Some("gpt-56-sol"),
                None,
            )
            .unwrap(),
            (openrouter.clone(), "gpt-5.6-sol".to_string())
        );
        let both = std::collections::HashSet::from([openai.clone(), openrouter.clone()]);
        assert_eq!(
            canonical_session_model(&catalog, &both, Some("gpt-56-sol"), None).unwrap(),
            (openai, "gpt-5.6-sol".to_string())
        );
        assert_eq!(
            canonical_session_model(&catalog, &both, Some("gpt-56-sol"), Some(&openrouter),)
                .unwrap(),
            (openrouter.clone(), "gpt-5.6-sol".to_string())
        );
        assert_eq!(
            canonical_session_model(&catalog, &both, Some("openrouter:gpt-56-sol"), None,).unwrap(),
            (openrouter.clone(), "gpt-5.6-sol".to_string())
        );
        assert_eq!(
            canonical_session_model(&catalog, &both, Some("openrouter:openai/gpt-5.6-sol"), None,)
                .unwrap(),
            (openrouter, "gpt-5.6-sol".to_string())
        );
    }

    #[test]
    fn canonical_session_model_preserves_unknown_passthrough_on_selected_provider() {
        let catalog = portable_session_catalog();
        let openai = lithos_llm::catalog::builtin::openai();
        let openrouter = ProviderId::new("openrouter");
        let both = std::collections::HashSet::from([openai.clone(), openrouter.clone()]);

        assert_eq!(
            canonical_session_model(&catalog, &both, Some("future-model"), Some(&openrouter),)
                .unwrap(),
            (openrouter, "future-model".to_string())
        );
        assert_eq!(
            canonical_session_model(&catalog, &both, Some("future-model"), None).unwrap(),
            (openai, "future-model".to_string())
        );
    }

    /// A colon in a model ID does not make it provider-qualified. Ollama
    /// `name:tag` values and Bedrock ARNs must still reach the provider.
    #[test]
    fn canonical_session_model_passes_through_colon_bearing_model_ids() {
        let catalog = portable_session_catalog();
        let openai = lithos_llm::catalog::builtin::openai();
        let openrouter = ProviderId::new("openrouter");
        let both = std::collections::HashSet::from([openai.clone(), openrouter.clone()]);

        assert_eq!(
            canonical_session_model(
                &catalog,
                &both,
                Some("future-model:latest"),
                Some(&openrouter),
            )
            .unwrap(),
            (openrouter, "future-model:latest".to_string())
        );
        assert_eq!(
            canonical_session_model(&catalog, &both, Some("future-model:latest"), None).unwrap(),
            (openai, "future-model:latest".to_string())
        );
    }

    #[test]
    fn canonical_session_model_rejects_an_unavailable_explicit_provider() {
        let catalog = portable_session_catalog();
        let error = canonical_session_model(
            &catalog,
            &std::collections::HashSet::from([lithos_llm::catalog::builtin::openai()]),
            Some("gpt-56-sol"),
            Some(&ProviderId::new("openrouter")),
        )
        .unwrap_err();

        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn canonical_session_model_normalizes_legacy_builtin_selector_before_qualification() {
        let catalog = portable_session_catalog();
        let openai = lithos_llm::catalog::builtin::openai();
        let openrouter = ProviderId::new("openrouter");
        let both = std::collections::HashSet::from([openai.clone(), openrouter.clone()]);

        assert_eq!(
            canonical_session_model(&catalog, &both, Some("openai/gpt-5.6-sol"), None,).unwrap(),
            (openai, "gpt-5.6-sol".to_string())
        );
        assert_eq!(
            canonical_session_model(
                &catalog,
                &both,
                Some("openai/gpt-5.6-sol"),
                Some(&openrouter),
            )
            .unwrap(),
            (openrouter.clone(), "gpt-5.6-sol".to_string())
        );
        assert_eq!(
            canonical_session_model(
                &catalog,
                &std::collections::HashSet::from([openrouter.clone()]),
                Some("openai/gpt-5.6-sol"),
                None,
            )
            .unwrap(),
            (openrouter, "gpt-5.6-sol".to_string())
        );
    }

    #[test]
    fn canonical_session_model_treats_colon_qualified_model_as_a_pin() {
        let catalog = portable_session_catalog();
        let openrouter = ProviderId::new("openrouter");

        assert_eq!(
            canonical_session_model(
                &catalog,
                &catalog.enabled_provider_ids().into_iter().collect(),
                Some("openrouter:gpt-56-sol"),
                None,
            )
            .unwrap(),
            (openrouter, "gpt-5.6-sol".to_string())
        );
    }

    #[test]
    fn canonical_session_model_rejects_conflicting_non_legacy_provider_pins() {
        let catalog = portable_session_catalog();
        let error = canonical_session_model(
            &catalog,
            &catalog.enabled_provider_ids().into_iter().collect(),
            Some("openrouter:gpt-56-sol"),
            Some(&lithos_llm::catalog::builtin::openai()),
        )
        .unwrap_err();

        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert!(
            error
                .into_response_entry()
                .detail
                .contains("conflicts with model reference provider")
        );
    }

    #[test]
    fn agent_event_payload_maps_text_delta_to_session_assistant_delta() {
        let turn_id = TurnId::new();
        let body = agent_event_payload(turn_id, CodingEvent::TextDelta {
            delta: "Hello".to_string(),
        });

        match body {
            Some(SessionEventBody::AssistantDelta(props)) => {
                assert_eq!(props.turn_id, turn_id);
                assert_eq!(props.delta, "Hello");
            }
            other => panic!("expected assistant delta event, got {other:?}"),
        }
    }

    #[test]
    fn agent_event_payload_drops_reasoning_delta() {
        let turn_id = TurnId::new();
        let body = agent_event_payload(turn_id, CodingEvent::ReasoningDelta {
            delta: "The user just said hello.".to_string(),
        });

        assert!(body.is_none(), "reasoning delta should not be visible");
    }

    #[test]
    fn ask_fabro_tool_policy_allows_only_expected_tools() {
        for tool_name in [
            "read_file",
            "grep",
            "glob",
            fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME,
            fabro_tool::FABRO_RUN_GET_TOOL_NAME,
        ] {
            assert!(ask_fabro_allows_tool(tool_name), "{tool_name}");
        }
        // A profile vocabulary alias resolves to its canonical tool.
        assert!(ask_fabro_allows_tool("Read"));

        for tool_name in [
            "write_file",
            "edit_file",
            "shell",
            "web_search",
            "web_fetch",
            fabro_tool::FABRO_RUN_CREATE_TOOL_NAME,
            fabro_tool::FABRO_RUN_INTERACT_TOOL_NAME,
            fabro_tool::FABRO_RUN_PAIR_TOOL_NAME,
        ] {
            assert!(!ask_fabro_allows_tool(tool_name), "{tool_name}");
        }
    }

    #[test]
    fn ask_fabro_tool_policy_denies_with_a_reason_the_model_can_read() {
        let scope = pebble_coding_agent::SessionScope::root(pebble_coding_agent::SessionId::new(
            "ses_test",
        ));
        let descriptor = |name: &str| {
            pebble_agent::ToolDescriptor::new(
                pebble_agent::ToolId::try_new(name).expect("tool id"),
                lithos_llm::types::ToolDefinition::function(
                    name.to_string(),
                    format!("{name} test tool"),
                    serde_json::json!({"type": "object"}),
                ),
            )
        };

        assert_eq!(
            AskFabroToolPolicy.permission(&scope, &descriptor("read_file")),
            ToolPermission::Allow
        );
        match AskFabroToolPolicy.permission(&scope, &descriptor("shell")) {
            ToolPermission::Deny { reason } => {
                assert!(reason.contains("denied by tool access policy"), "{reason}");
            }
            other => panic!("shell should be denied, got {other:?}"),
        }
    }

    #[test]
    fn ask_fabro_prompt_lists_effective_tools_without_denied_tools() {
        let prompt = build_ask_fabro_system_prompt(
            &EnvContext {
                working_directory: "/workspace".to_string(),
                ..EnvContext::default()
            },
            &ask_fabro_test_tools(),
        );

        for tool_name in [
            "read_file",
            "grep",
            "glob",
            fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME,
            fabro_tool::FABRO_RUN_GET_TOOL_NAME,
        ] {
            assert!(
                prompt.contains(&format!("`{tool_name}`")),
                "prompt should list {tool_name}"
            );
        }

        for hidden_tool in [
            "write_file",
            "edit_file",
            "shell",
            "web_search",
            "web_fetch",
            fabro_tool::FABRO_RUN_CREATE_TOOL_NAME,
            fabro_tool::FABRO_RUN_INTERACT_TOOL_NAME,
        ] {
            assert!(
                !prompt.contains(hidden_tool),
                "prompt should not mention hidden tool {hidden_tool}"
            );
        }
        assert!(prompt.contains("Working directory: /workspace"));
        assert!(prompt.contains("read-only"));
        assert!(prompt.contains("run-scoped"));
        assert!(prompt.contains("interactive read-only"));
        assert!(prompt.contains("Use the provided run snapshot for orientation"));
        assert!(prompt.contains("Use `fabro_run_events` for current status"));
        assert!(prompt.contains("Use workspace file tools only when the question asks"));
    }

    #[test]
    fn ask_fabro_prompt_keeps_tool_descriptions_inert() {
        let mut tool = tool_summary("read_file");
        tool.description = "{{ inputs.env_block }}".to_string();

        let prompt = build_ask_fabro_system_prompt(&EnvContext::default(), &[tool]);

        assert!(prompt.contains("- `read_file`: {{ inputs.env_block }}"));
        assert_eq!(prompt.matches("<environment>").count(), 1);
    }

    #[test]
    fn ask_fabro_run_snapshot_summarizes_goal_progress_and_recent_stages() {
        let run_id = RunId::new();
        let now = Utc::now();
        let mut graph = fabro_types::Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            fabro_types::AttrValue::String("Ship the feature".to_string()),
        );
        for node_id in ["start", "plan", "code", "test", "review", "deploy", "exit"] {
            let mut node = fabro_types::Node::new(node_id);
            let shape = match node_id {
                "start" => "Mdiamond",
                "exit" => "Msquare",
                "test" => "parallelogram",
                _ => "box",
            };
            node.attrs.insert(
                "shape".to_string(),
                fabro_types::AttrValue::String(shape.to_string()),
            );
            graph.nodes.insert(node_id.to_string(), node);
        }
        let spec = fabro_types::RunSpec {
            run_id,
            settings: fabro_types::WorkflowSettings::default(),
            graph,
            graph_source: None,
            workflow_slug: None,
            workflow_version_id: None,
            target: None,
            automation: None,
            source_directory: None,
            labels: HashMap::default(),
            provenance: test_support::test_run_provenance(),
            definition_blob: None,
            spec_blob: None,
            git: None,
            fork_source_ref: None,
            admission: PetriAdmission::default(),
        };
        let mut projection = fabro_types::RunProjection::new(String::new(), spec, now);
        for (index, node_id) in ["start", "plan", "code", "test", "review", "deploy"]
            .iter()
            .enumerate()
        {
            let handler = projection
                .spec
                .graph
                .nodes
                .get(*node_id)
                .and_then(fabro_types::Node::handler_type)
                .and_then(|handler| handler.parse().ok());
            let stage = projection.stage_entry(
                node_id,
                1,
                std::num::NonZeroU32::new(u32::try_from(index + 1).unwrap()).unwrap(),
            );
            stage.handler = handler;
            stage.state = if *node_id == "deploy" {
                fabro_types::StageState::Running
            } else {
                fabro_types::StageState::Succeeded
            };
            if *node_id == "test" {
                stage.completion = Some(fabro_types::StageCompletion {
                    outcome:        fabro_types::StageOutcome::Failed {
                        retry_requested: false,
                    },
                    notes:          None,
                    failure_reason: Some("tests failed".to_string()),
                    timestamp:      now,
                });
                stage.state = fabro_types::StageState::Failed;
            }
        }

        let snapshot = build_ask_fabro_run_snapshot(&projection, run_id);

        assert!(snapshot.contains(&format!("Run ID: {run_id}")));
        assert!(snapshot.contains("Goal: Ship the feature"));
        assert!(snapshot.contains("Progress: 4 of 5 non-meta stages completed"));
        assert!(!snapshot.contains("start"));
        assert!(snapshot.contains("- plan: succeeded, agent"));
        assert!(snapshot.contains("- test: failed, command, reason: tests failed"));
        assert!(snapshot.contains("- deploy: running, agent"));
        assert!(snapshot.contains("Use this snapshot as orientation only."));
    }

    #[test]
    fn ask_fabro_turn_input_wraps_snapshot_without_losing_user_question() {
        let input =
            build_ask_fabro_turn_input("Why did it fail?", "Run ID: run_123\nGoal: Fix tests");

        assert!(input.contains("<run_snapshot>"));
        assert!(input.contains("Run ID: run_123"));
        assert!(input.contains("Treat it as possibly stale"));
        assert!(input.ends_with("User question:\nWhy did it fail?"));
    }
}

/// Ask Fabro across turns and processes: a second turn resumes the stored
/// pebble record, and a record whose cursor fell behind the run's event log
/// (a crash between the two writes) is moved past the log before it answers.
#[cfg(test)]
mod resume_tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use fabro_config::daemon::ServerDaemon;
    use fabro_config::{RunEnvironmentLayer, RunLayer, Storage};
    use fabro_static::EnvVars;
    use fabro_test::{TwinScenario, TwinScenarios, twin_openai};
    use fabro_types::{RunId, SessionId};
    use pebble_coding_agent::state::SESSION_RECORD_FORMAT_VERSION;
    use tower::ServiceExt;

    use crate::server::{AppState, spawn_scheduler};
    use crate::test_support::{
        TestAppStateBuilder, build_test_router, default_test_server_settings,
        llm_overlay_with_provider_base_url,
    };

    const MODEL: &str = "gpt-5.4-mini";
    const DOT: &str = r#"digraph Test {
    graph [goal="Test"]
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    start -> exit
}"#;

    fn api(path: &str) -> String {
        format!("/api/v1{path}")
    }

    async fn json_response(
        app: &axum::Router,
        request: Request<Body>,
        expected: StatusCode,
    ) -> serde_json::Value {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            status,
            expected,
            "unexpected status, body {}",
            String::from_utf8_lossy(&bytes)
        );
        if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("response should be JSON")
        }
    }

    fn post_json(path: &str, body: &serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(api(path))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// A server whose `openai` provider is the twin under `namespace`, whose
    /// runs execute in place, and whose own address Ask Fabro can resolve.
    fn twin_backed_state(base_url: String, namespace: &str) -> Arc<AppState> {
        let api_key = namespace.to_string();
        let state = TestAppStateBuilder::new()
            .runtime_settings(default_test_server_settings(), RunLayer {
                environment: Some(RunEnvironmentLayer {
                    id: Some("local".to_string()),
                    ..RunEnvironmentLayer::default()
                }),
                ..RunLayer::default()
            })
            .max_concurrent_runs(2)
            // A registry factory runs the dry run in this process, so no
            // worker executable is needed.
            .in_process_execution()
            .llm_overlay(llm_overlay_with_provider_base_url("openai", base_url))
            .vault_entries([(EnvVars::OPENAI_API_KEY, namespace.to_string())])
            .env_lookup(move |name| (name == EnvVars::OPENAI_API_KEY).then(|| api_key.clone()))
            .build();
        let runtime_directory = Storage::new(state.server_storage_dir()).runtime_directory();
        ServerDaemon::new(
            std::process::id(),
            fabro_config::bind::Bind::Tcp("127.0.0.1:32277".parse().unwrap()),
            runtime_directory.log_path(),
        )
        .write(&runtime_directory)
        .expect("test server record should be written");
        state
    }

    /// A completed local dry run, so the session has a sandbox to reconnect.
    async fn completed_run(app: &axum::Router, workspace: &std::path::Path) -> RunId {
        let path = fabro_types::WorkflowPath::new("workflow.fabro").unwrap();
        let version = fabro_types::WorkflowVersion::new(
            path.clone(),
            std::collections::BTreeMap::from([(path, DOT.to_string())]),
            std::collections::BTreeMap::new(),
        )
        .unwrap();
        let id = crate::test_support::test_register_workflow_version(app, &version, None).await;
        let intent = serde_json::json!({"workflow_version_id": id, "target": {"kind": "folder", "path": workspace}, "environment_id": "local", "args": {"dry_run": true}});
        let created = json_response(app, post_json("/runs", &intent), StatusCode::CREATED).await;
        let run_id = created["id"].as_str().unwrap().to_string();
        let start = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/start")))
            .body(Body::empty())
            .unwrap();
        json_response(app, start, StatusCode::OK).await;
        for _ in 0..500 {
            let get = Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}")))
                .body(Body::empty())
                .unwrap();
            let run = json_response(app, get, StatusCode::OK).await;
            match run["lifecycle"]["status"]["kind"].as_str() {
                Some("succeeded") => return run_id.parse().unwrap(),
                Some("failed") => panic!("the dry run failed: {run}"),
                _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
        panic!("run {run_id} did not complete");
    }

    /// Submits one turn and returns the streamed session events.
    async fn turn(
        app: &axum::Router,
        session_id: SessionId,
        input: &str,
    ) -> Vec<serde_json::Value> {
        let response = app
            .clone()
            .oneshot(post_json(
                &format!("/sessions/{session_id}/turns"),
                &serde_json::json!({ "input": input }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        let events: Vec<serde_json::Value> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        assert!(
            events
                .iter()
                .any(|event| event["event"] == "run.session.turn.succeeded"),
            "the turn should succeed: {events:#?}"
        );
        events
    }

    /// A record written by an older build is refused, not read: pebble checks
    /// the format version before it reads the route, and fabro turns that
    /// refusal into a turn failure naming both versions. Old runs get no
    /// migration, so this is what a session persisted before the record
    /// format moved sees on its next turn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stored_record_in_an_older_format_fails_the_turn_naming_both_versions() {
        let twin = twin_openai().await;
        let namespace = format!("{}::{}", module_path!(), line!());
        TwinScenarios::new(namespace.clone())
            .scenario(
                TwinScenario::responses(MODEL)
                    .input_contains("First question")
                    .text("First answer"),
            )
            .load(twin)
            .await;
        let state = twin_backed_state(twin.base_url.clone(), &namespace);
        spawn_scheduler(Arc::clone(&state));
        let app = build_test_router(Arc::clone(&state));
        let workspace = tempfile::tempdir().unwrap();
        let run_id = completed_run(&app, workspace.path()).await;

        let created = json_response(
            &app,
            post_json(
                &format!("/runs/{run_id}/sessions"),
                &serde_json::json!({ "title": "Ask Fabro", "model": MODEL }),
            ),
            StatusCode::CREATED,
        )
        .await;
        let session_id: SessionId = created["id"].as_str().unwrap().parse().unwrap();

        turn(&app, session_id, "First question").await;
        let stored = state
            .stores
            .session_records
            .get(session_id)
            .await
            .unwrap()
            .expect("the first turn persists the record");

        // The record as an older build wrote it: the previous format version.
        // The store parses it, and the version check refuses it on resume.
        let previous = SESSION_RECORD_FORMAT_VERSION - 1;
        let mut older = stored.record.clone();
        older.format_version = previous;
        state
            .stores
            .session_records
            .put(session_id, run_id, &older, chrono::Utc::now())
            .await
            .unwrap();
        state
            .session_runtimes()
            .load_or_create_runtime(session_id)
            .clear_agent()
            .await;

        let response = app
            .clone()
            .oneshot(post_json(
                &format!("/sessions/{session_id}/turns"),
                &serde_json::json!({ "input": "Second question" }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        let events: Vec<serde_json::Value> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        let failed = events
            .iter()
            .find(|event| event["event"] == "run.session.turn.failed")
            .unwrap_or_else(|| panic!("the resumed turn fails: {events:#?}"));
        assert_eq!(failed["properties"]["code"], "agent_error");
        assert_eq!(failed["properties"]["retryable"], false);
        let error = failed["properties"]["error"].as_str().unwrap();
        let expected = format!(
            "session record format version {previous} is not supported (this build requires {SESSION_RECORD_FORMAT_VERSION})"
        );
        assert!(
            error.contains(&expected),
            "the failure names both versions: {error}"
        );
        assert!(
            !events
                .iter()
                .any(|event| event["event"] == "run.session.turn.succeeded"),
            "a refused record runs no turn: {events:#?}"
        );

        // The stored record is untouched, so a build that reads its format
        // can still resume it.
        let after = state
            .stores
            .session_records
            .get(session_id)
            .await
            .unwrap()
            .expect("the refused record stays stored");
        assert_eq!(after.record.format_version, previous);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resumed_session_continues_its_conversation_past_the_event_log() {
        let twin = twin_openai().await;
        let namespace = format!("{}::{}", module_path!(), line!());
        TwinScenarios::new(namespace.clone())
            .scenario(
                TwinScenario::responses(MODEL)
                    .input_contains("First question")
                    .text("First answer"),
            )
            .scenario(
                TwinScenario::responses(MODEL)
                    .input_contains("Second question")
                    .text("Second answer"),
            )
            .load(twin)
            .await;
        let state = twin_backed_state(twin.base_url.clone(), &namespace);
        spawn_scheduler(Arc::clone(&state));
        let app = build_test_router(Arc::clone(&state));
        let workspace = tempfile::tempdir().unwrap();
        let run_id = completed_run(&app, workspace.path()).await;

        let created = json_response(
            &app,
            post_json(
                &format!("/runs/{run_id}/sessions"),
                &serde_json::json!({ "title": "Ask Fabro", "model": MODEL }),
            ),
            StatusCode::CREATED,
        )
        .await;
        let session_id: SessionId = created["id"].as_str().unwrap().parse().unwrap();

        turn(&app, session_id, "First question").await;
        let after_first = state
            .stores
            .session_records
            .get(session_id)
            .await
            .unwrap()
            .expect("the first turn persists the record");
        assert!(
            after_first.record.last_event_seq > 0,
            "the record carries the committed event cursor"
        );

        // The crash: the run's events were written, the record's cursor was
        // not. Drop the agent so the next turn resumes from the stale record
        // the way a new process would.
        let mut stale = after_first.record.clone();
        stale.last_event_seq = 0;
        state
            .stores
            .session_records
            .put(session_id, run_id, &stale, chrono::Utc::now())
            .await
            .unwrap();
        state
            .session_runtimes()
            .load_or_create_runtime(session_id)
            .clear_agent()
            .await;
        let log_head_before_resume = state
            .stores
            .session_events
            .last_seq(session_id)
            .await
            .unwrap()
            .expect("the session has events");

        turn(&app, session_id, "Second question").await;

        let after_second = state
            .stores
            .session_records
            .get(session_id)
            .await
            .unwrap()
            .expect("the second turn persists the record");
        assert!(
            after_second.record.last_event_seq > u64::from(log_head_before_resume),
            "the resumed session numbers past the log head {log_head_before_resume}, got {}",
            after_second.record.last_event_seq
        );
        assert!(
            after_second.record.last_event_seq > after_first.record.last_event_seq,
            "the cursor only moves forward"
        );
        assert_eq!(
            after_second.record.messages.len(),
            2 * after_first.record.messages.len(),
            "the record holds both turns"
        );

        // The run's title generator also calls the model; the turns are the
        // streamed requests. The twin logs the user side of the input, so the
        // resumed turn shows as carrying the first question ahead of the
        // second.
        let logs = twin.request_logs(&namespace).await;
        let turns: Vec<&str> = logs["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|request| request["stream"] == true)
            .map(|request| request["input_text"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(turns.len(), 2, "one model call per turn: {logs}");
        assert!(
            !turns[0].contains("Second question"),
            "the first turn knows nothing of the second, got {}",
            turns[0]
        );
        let first_at = turns[1]
            .find("User question: First question")
            .expect("the resumed turn replays the first question");
        let second_at = turns[1]
            .find("User question: Second question")
            .expect("the resumed turn ends with the second question");
        assert!(first_at < second_at, "got {}", turns[1]);
    }
}
