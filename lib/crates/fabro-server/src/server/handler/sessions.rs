use std::convert::Infallible;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use fabro_agent::config::{ToolAccess, ToolAccessPolicy, ToolExposureMode};
use fabro_agent::profiles::assemble_system_prompt;
use fabro_agent::tool_registry::ToolRegistry;
use fabro_agent::{
    AgentEvent, AgentProfile, AnthropicProfile, Error as AgentError, GeminiProfile, OpenAiProfile,
    Session, SessionEvent, SessionOptions, ToolSecrets, WebFetchSummarizer,
};
use fabro_api::types::{
    CreateRunSessionRequest, PaginatedEventList, PaginationMeta, SubmitTurnRequest,
};
use fabro_llm::client::Client as LlmClient;
use fabro_llm::types::ToolDefinition;
use fabro_model::{AgentProfileKind, Catalog, ModelHandle, ProviderId};
use fabro_sandbox::reconnect::reconnect_for_run;
use fabro_static::EnvVars;
use fabro_store::{
    EventPayload, ProjectedRunSession, RunDatabase, project_run_session, project_run_sessions,
};
use fabro_tool::fabro_client::ClientBackend;
use fabro_types::run_event::{
    RunSessionAssistantDeltaProps, RunSessionAssistantMessageProps, RunSessionCreatedProps,
    RunSessionToolCallCompletedProps, RunSessionToolCallStartedProps, RunSessionTurnFailedCode,
    RunSessionTurnFailedProps, RunSessionTurnInterruptedProps, RunSessionTurnStartedProps,
    RunSessionTurnSucceededProps, RunSessionUserMessageProps,
};
use fabro_types::settings::{ModelRef as SettingsModelRef, ModelRegistry, ResolvedModelRef};
use fabro_types::{EventBody, EventEnvelope, RunEvent, RunId, SessionDetail, SessionId, TurnId};
use fabro_workflow::handler::llm::api::register_named_fabro_run_tools;
use fabro_workflow::services::FabroRunToolServices;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use super::super::session_runtime::{InterruptTurnError, SessionTurnLease, StartTurnError};
use super::super::{
    AppState, EventListParams, PaginationParams, paginate_items, parse_run_id_path,
};
use crate::error::ApiError;
use crate::principal_middleware::RequiredUser;
use crate::server_secrets::LlmClientResult;
use crate::worker_token::issue_worker_token;

const SESSION_SSE_BUFFER_CAPACITY: usize = 1024;

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
    let run_store = match open_run_reader(&state, run_id).await {
        Ok(store) => store,
        Err(response) => return response,
    };
    match run_store.list_events().await {
        Ok(events) => {
            let mut sessions = project_run_sessions(run_id, &events);
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
    Json(SessionDetail::new(
        session.record,
        session.runtime_context,
        session.last_seq,
    ))
    .into_response()
}

async fn session_method_not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

async fn list_session_events(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<EventListParams>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let (_, run_store) = match load_session_run_reader(&state, session_id).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    match run_store
        .list_events_for_session_from_with_limit(session_id, params.since_seq(), params.limit())
        .await
    {
        Ok(mut data) => {
            let limit = params.limit();
            let has_more = data.len() > limit;
            data.truncate(limit);
            Json(PaginatedEventList {
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
    let (_, run_store) = match load_session_run_reader(&state, session_id).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    let start_seq = match params.since_seq {
        Some(seq) => seq.max(1),
        None => match run_store.list_events().await {
            Ok(events) => events.last().map_or(1, |event| event.seq.saturating_add(1)),
            Err(err) => return store_error(&err).into_response(),
        },
    };
    let shutdown = state.shutdown_token();
    let (sender, receiver) = mpsc::channel(SESSION_SSE_BUFFER_CAPACITY);
    tokio::spawn(async move {
        let mut next_seq = start_seq;

        loop {
            let Ok(replay_batch) = run_store
                .list_events_for_session_from_with_limit(
                    session_id,
                    next_seq,
                    ATTACH_REPLAY_BATCH_LIMIT,
                )
                .await
            else {
                return;
            };
            let replay_has_more = replay_batch.len() > ATTACH_REPLAY_BATCH_LIMIT;

            for event in replay_batch.into_iter().take(ATTACH_REPLAY_BATCH_LIMIT) {
                next_seq = event.seq.saturating_add(1);
                if let Some(sse_event) = session_sse_event(&event) {
                    if !send_attach_sse_event(&sender, &shutdown, sse_event).await {
                        return;
                    }
                }
            }

            if replay_has_more {
                continue;
            }
            break;
        }

        let Ok(mut live_stream) = run_store.watch_events_from(next_seq) else {
            return;
        };
        let session_id_string = session_id.to_string();
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                () = sender.closed() => break,
                next = live_stream.next() => {
                    let Some(result) = next else {
                        return;
                    };
                    let Ok(event) = result else {
                        return;
                    };
                    if event_matches_session(&event, &session_id_string) {
                        if let Some(sse_event) = session_sse_event(&event) {
                            if !send_attach_sse_event(&sender, &shutdown, sse_event).await {
                                return;
                            }
                        }
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
    let (run_id, run_store, session) = match load_session(&state, session_id).await {
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
        let session = session_slot
            .as_mut()
            .expect("session runtime slot should be loaded");
        let cancel_token = session.cancel_token();
        turn_lease.attach_cancel_token(&cancel_token);
        let initialize = !runtime_entry.is_initialized();
        let model_input = match run_store.state().await {
            Ok(projection) => {
                let snapshot = build_ask_fabro_run_snapshot(&projection, run_id);
                build_ask_fabro_turn_input(&input, &snapshot)
            }
            Err(err) => {
                warn!(
                    error = %err,
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
        let result = Box::pin(drive_agent_session(
            &run_store,
            session,
            run_id,
            session_id,
            turn_id,
            &model_input,
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
                let code = agent_failure_code(&err);
                turn_failed_body(turn_id, err.to_string(), outcome.output, code, false)
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
                turn_failed_body(
                    turn_id,
                    err.to_string(),
                    outcome.output,
                    RunSessionTurnFailedCode::AgentError,
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
    fn code(&self) -> RunSessionTurnFailedCode {
        match self {
            Self::NoSandbox => RunSessionTurnFailedCode::NoSandbox,
            Self::SandboxUnavailable(_) => RunSessionTurnFailedCode::SandboxUnavailable,
            Self::LlmUnconfigured(_) => RunSessionTurnFailedCode::LlmUnconfigured,
            Self::ModelUnavailable(_) => RunSessionTurnFailedCode::ModelUnavailable,
            Self::Agent(_) => RunSessionTurnFailedCode::AgentError,
        }
    }

    fn retryable(&self) -> bool {
        matches!(self, Self::SandboxUnavailable(_))
    }
}

async fn build_agent_session(
    state: &AppState,
    run_id: RunId,
    session: &ProjectedRunSession,
) -> Result<Session, AskFabroBuildError> {
    let catalog = state.catalog();
    let llm_result = state.resolve_llm_client().await.map_err(|err| {
        AskFabroBuildError::LlmUnconfigured(format!("LLM credentials are not configured: {err}"))
    })?;
    for (provider, issue) in &llm_result.auth_issues {
        warn!(provider = %provider, error = %issue, "LLM provider unavailable due to auth issue");
    }
    for issue in &llm_result.registration_issues {
        warn!(provider = %issue.provider, error = %issue.error, "LLM provider unavailable due to registration issue");
    }
    let (provider_id, model, profile_kind) =
        selected_session_model(&catalog, &llm_result, session)?;
    if !llm_result.client.has_provider(provider_id.as_str()) {
        let message = format!("LLM credentials not configured for provider '{provider_id}'");
        return if session.record.model.is_some() {
            Err(AskFabroBuildError::ModelUnavailable(message))
        } else {
            Err(AskFabroBuildError::LlmUnconfigured(message))
        };
    }

    let run_store = state
        .store_ref()
        .open_run_reader(&run_id)
        .await
        .map_err(|err| AskFabroBuildError::Agent(anyhow::Error::new(err)))?;
    let projection = run_store
        .state()
        .await
        .map_err(|err| AskFabroBuildError::Agent(anyhow::Error::new(err)))?;
    let sandbox_record = projection
        .sandbox
        .as_ref()
        .ok_or(AskFabroBuildError::NoSandbox)?;
    let sandbox_instance = sandbox_record.instance().ok_or_else(|| {
        AskFabroBuildError::SandboxUnavailable(anyhow::anyhow!("run sandbox was not created"))
    })?;
    let sandbox = reconnect_for_run(
        sandbox_instance,
        state.vault_secret(EnvVars::DAYTONA_API_KEY),
        Some(run_id),
    )
    .await
    .map_err(AskFabroBuildError::SandboxUnavailable)?;
    let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::from(sandbox);
    let mut profile = build_profile(
        provider_id,
        profile_kind,
        &model,
        &llm_result.client,
        Arc::clone(&catalog),
    );

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
        backend:            Arc::new(backend),
        current_run_id:     run_id,
        base_cwd:           PathBuf::new(),
        user_settings_path: PathBuf::new(),
    };
    register_named_fabro_run_tools(
        profile.tool_registry_mut(),
        &services,
        ASK_FABRO_RUN_TOOL_NAMES,
    );
    let ask_fabro_policy = build_ask_fabro_tool_access_policy();
    let profile: Arc<dyn AgentProfile> =
        Arc::new(AskFabroProfile::new(profile, Arc::clone(&ask_fabro_policy)));

    let config = SessionOptions {
        tool_access_policy: Some(ask_fabro_policy),
        tool_exposure_mode: ToolExposureMode::AutoApprovedOnly,
        tool_secrets: ToolSecrets {
            brave_search_api_key: state.vault_secret(EnvVars::BRAVE_SEARCH_API_KEY),
        },
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
    .map_err(|err| AskFabroBuildError::Agent(anyhow::Error::new(err)))
}

fn selected_session_model(
    catalog: &Catalog,
    llm_result: &LlmClientResult,
    session: &ProjectedRunSession,
) -> Result<(ProviderId, String, AgentProfileKind), AskFabroBuildError> {
    let configured_provider_ids = llm_result.provider_ids();
    let selected = match session.record.model.as_deref() {
        Some(model_id) => catalog.get(model_id).ok_or_else(|| {
            AskFabroBuildError::ModelUnavailable(format!(
                "session model '{model_id}' is not in the catalog"
            ))
        })?,
        None => catalog.default_for_configured_ids(&configured_provider_ids),
    };
    let provider_id = selected.provider.clone();
    let model = selected.id.clone();
    let profile_kind = catalog
        .effective_agent_profile(&provider_id, Some(&model))
        .ok_or_else(|| {
            AskFabroBuildError::ModelUnavailable(format!(
                "provider '{provider_id}' is not configured"
            ))
        })?;
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
) -> Box<dyn AgentProfile> {
    let summarizer = Some(WebFetchSummarizer {
        client:   llm_client.clone(),
        model_id: summarizer_model_id(&provider_id, profile_kind, &catalog, model),
    });
    match profile_kind {
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
    }
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

struct AskFabroToolAccessPolicy;

impl ToolAccessPolicy for AskFabroToolAccessPolicy {
    fn access_for_tool(&self, tool_name: &str) -> ToolAccess {
        match tool_name {
            "read_file" | "grep" | "glob" => ToolAccess::Allowed,
            name if ASK_FABRO_RUN_TOOL_NAMES.contains(&name) => ToolAccess::Allowed,
            _ => ToolAccess::Denied,
        }
    }
}

fn build_ask_fabro_tool_access_policy() -> Arc<dyn ToolAccessPolicy> {
    Arc::new(AskFabroToolAccessPolicy)
}

fn ask_fabro_effective_tool_definitions(
    registry: &ToolRegistry,
    policy: &dyn ToolAccessPolicy,
) -> Vec<ToolDefinition> {
    registry.definitions_for_policy(Some(policy), ToolExposureMode::AutoApprovedOnly)
}

fn render_ask_fabro_tool_guidance(
    registry: &ToolRegistry,
    policy: &dyn ToolAccessPolicy,
) -> String {
    let mut definitions = ask_fabro_effective_tool_definitions(registry, policy);
    definitions.sort_by(|left, right| left.name.cmp(&right.name));

    definitions
        .into_iter()
        .map(|tool| format!("- `{}`: {}", tool.name, tool.description))
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_ask_fabro_system_prompt(
    env: &dyn fabro_agent::Sandbox,
    env_context: &fabro_agent::EnvContext,
    _memory: &[String],
    user_instructions: Option<&str>,
    _skills: &[fabro_agent::Skill],
    registry: &ToolRegistry,
    policy: &dyn ToolAccessPolicy,
) -> String {
    let tool_guidance = render_ask_fabro_tool_guidance(registry, policy);
    let core_prompt = format!(
        "\
You are Ask Fabro, an interactive read-only, run-scoped analyst.

Answer questions about the current Fabro run, its event history, and its workspace. Stay scoped to this run. Do not modify the run or workspace, and do not take control actions.

Use the provided run snapshot for orientation. Treat it as possibly stale. Use `fabro_run_events` for current status, exact timestamps, failures, tool calls, stage outputs, and event-backed claims. Use workspace file tools only when the question asks about files, code, artifacts, or implementation details.

When answering:
- Be concise by default.
- Cite the source of important facts in plain language, such as \"from run events\" or \"from workspace file <path>\".
- If evidence is incomplete, say what you could not inspect.

{{env_block}}

# Tool Access

You can only call these tools:
{tool_guidance}

Do not claim access to tools that are not listed. Treat tool failures as real failures, not as permission discovery. If the available tools are insufficient, say what cannot be inspected."
    );

    assemble_system_prompt(&core_prompt, env, env_context, &[], user_instructions, &[])
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

struct AskFabroProfile {
    inner:  Box<dyn AgentProfile>,
    policy: Arc<dyn ToolAccessPolicy>,
}

impl AskFabroProfile {
    fn new(inner: Box<dyn AgentProfile>, policy: Arc<dyn ToolAccessPolicy>) -> Self {
        Self { inner, policy }
    }
}

impl AgentProfile for AskFabroProfile {
    fn profile_kind(&self) -> AgentProfileKind {
        self.inner.profile_kind()
    }

    fn provider_id(&self) -> ProviderId {
        self.inner.provider_id()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn catalog(&self) -> Option<&Catalog> {
        self.inner.catalog()
    }

    fn tool_registry(&self) -> &ToolRegistry {
        self.inner.tool_registry()
    }

    fn tool_registry_mut(&mut self) -> &mut ToolRegistry {
        self.inner.tool_registry_mut()
    }

    fn build_system_prompt(
        &self,
        env: &dyn fabro_agent::Sandbox,
        env_context: &fabro_agent::EnvContext,
        memory: &[String],
        user_instructions: Option<&str>,
        skills: &[fabro_agent::Skill],
    ) -> String {
        build_ask_fabro_system_prompt(
            env,
            env_context,
            memory,
            user_instructions,
            skills,
            self.tool_registry(),
            self.policy.as_ref(),
        )
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        ask_fabro_effective_tool_definitions(self.tool_registry(), self.policy.as_ref())
    }
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

fn turn_failed_body(
    turn_id: TurnId,
    error: String,
    output: Option<String>,
    code: RunSessionTurnFailedCode,
    retryable: bool,
) -> EventBody {
    EventBody::RunSessionTurnFailed(RunSessionTurnFailedProps {
        turn_id,
        error,
        output,
        code,
        retryable,
    })
}

fn agent_failure_code(err: &AgentError) -> RunSessionTurnFailedCode {
    match err {
        AgentError::ToolExecution(message)
            if message.contains("denied") || message.contains("not allowed") =>
        {
            RunSessionTurnFailedCode::ToolDenied
        }
        _ => RunSessionTurnFailedCode::AgentError,
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
        AgentEvent::TextDelta { delta } => Some(EventBody::RunSessionAssistantDelta(
            RunSessionAssistantDeltaProps {
                turn_id: event_turn_id,
                delta,
            },
        )),
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

fn session_sse_event(event: &EventEnvelope) -> Option<Event> {
    let data = serde_json::to_string(event).ok()?;
    Some(
        Event::default()
            .id(event.seq.to_string())
            .event(event.event.event_name())
            .data(data),
    )
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

fn event_matches_session(event: &EventEnvelope, session_id: &str) -> bool {
    event
        .event
        .session_id
        .as_deref()
        .is_some_and(|id| id == session_id)
        && event.event.body.is_run_session_event()
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
) -> Result<(RunId, ProjectedRunSession), Response> {
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
    match fabro_store::project_run_session_with_context(run_id, session_id, &events) {
        Some(session) => Ok((run_id, session)),
        None => Err(ApiError::not_found("Session not found.").into_response()),
    }
}

async fn load_session_run_reader(
    state: &AppState,
    session_id: SessionId,
) -> Result<(RunId, RunDatabase), Response> {
    let run_id = match state.store_ref().get_session_run_id(&session_id).await {
        Ok(Some(run_id)) => run_id,
        Ok(None) => return Err(ApiError::not_found("Session not found.").into_response()),
        Err(err) => return Err(store_error(&err).into_response()),
    };
    let run_store = open_run_reader(state, run_id).await?;
    let events = match run_store
        .list_events_for_session_from_with_limit(session_id, 1, 0)
        .await
    {
        Ok(events) => events,
        Err(err) => return Err(store_error(&err).into_response()),
    };
    if events.is_empty() {
        return Err(ApiError::not_found("Session not found.").into_response());
    }
    Ok((run_id, run_store))
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fabro_agent::config::ToolAccess;
    use fabro_agent::tool_registry::{RegisteredTool, ToolContext, ToolRegistry, ToolSource};
    use fabro_llm::types::{ToolCall, ToolDefinition};

    use super::*;

    fn stub_tool(name: &str) -> RegisteredTool {
        RegisteredTool {
            definition: ToolDefinition {
                name:        name.to_string(),
                description: format!("{name} test tool"),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(|_args, _ctx: ToolContext| {
                Box::pin(async { Ok("ok".to_string()) })
            }),
            source:     ToolSource::Native,
        }
    }

    fn ask_fabro_test_registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        for name in [
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
        ] {
            registry.register(stub_tool(name));
        }
        registry
    }

    #[test]
    fn agent_event_payload_maps_text_delta_to_session_assistant_delta() {
        let turn_id = TurnId::new();
        let body = agent_event_payload(turn_id, AgentEvent::TextDelta {
            delta: "Hello".to_string(),
        });

        match body {
            Some(EventBody::RunSessionAssistantDelta(props)) => {
                assert_eq!(props.turn_id, turn_id);
                assert_eq!(props.delta, "Hello");
            }
            other => panic!("expected assistant delta event, got {other:?}"),
        }
    }

    #[test]
    fn agent_event_payload_drops_reasoning_delta() {
        let turn_id = TurnId::new();
        let body = agent_event_payload(turn_id, AgentEvent::ReasoningDelta {
            delta: "The user just said hello.".to_string(),
        });

        assert!(body.is_none(), "reasoning delta should not be visible");
    }

    #[test]
    fn ask_fabro_tool_policy_allows_only_expected_tools() {
        let policy = build_ask_fabro_tool_access_policy();
        for tool_name in [
            "read_file",
            "grep",
            "glob",
            fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME,
            fabro_tool::FABRO_RUN_GET_TOOL_NAME,
        ] {
            assert_eq!(policy.access_for_tool(tool_name), ToolAccess::Allowed);
        }

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
            assert_eq!(policy.access_for_tool(tool_name), ToolAccess::Denied);
        }
    }

    #[test]
    fn ask_fabro_effective_tools_are_limited_to_policy_allow_list() {
        let registry = ask_fabro_test_registry();
        let policy = build_ask_fabro_tool_access_policy();

        let mut names: Vec<_> = ask_fabro_effective_tool_definitions(&registry, policy.as_ref())
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        names.sort();

        assert_eq!(names, vec![
            "fabro_run_events",
            "fabro_run_get",
            "glob",
            "grep",
            "read_file",
        ]);
    }

    #[test]
    fn ask_fabro_prompt_lists_effective_tools_without_denied_tools() {
        let registry = ask_fabro_test_registry();
        let policy = build_ask_fabro_tool_access_policy();

        let prompt = build_ask_fabro_system_prompt(
            &fabro_agent::LocalSandbox::new(std::env::current_dir().unwrap()),
            &fabro_agent::EnvContext::default(),
            &[],
            None,
            &[],
            &registry,
            policy.as_ref(),
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
        assert!(prompt.contains("read-only"));
        assert!(prompt.contains("run-scoped"));
        assert!(prompt.contains("interactive read-only"));
        assert!(prompt.contains("Use the provided run snapshot for orientation"));
        assert!(prompt.contains("Use `fabro_run_events` for current status"));
        assert!(prompt.contains("Use workspace file tools only when the question asks"));
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
            automation: None,
            source_directory: None,
            labels: HashMap::default(),
            provenance: None,
            manifest_blob: None,
            definition_blob: None,
            git: None,
            fork_source_ref: None,
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

    #[tokio::test]
    async fn ask_fabro_blocks_denied_tools_at_execution_time() {
        let denied_tools = [
            "write_file",
            "edit_file",
            "shell",
            "web_search",
            "web_fetch",
        ];
        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        for tool_name in denied_tools {
            let executions = Arc::clone(&executions);
            registry.register(RegisteredTool {
                definition: ToolDefinition {
                    name:        tool_name.to_string(),
                    description: format!("{tool_name} test tool"),
                    parameters:  serde_json::json!({"type": "object"}),
                },
                executor:   Arc::new(move |_args, _ctx: ToolContext| {
                    let executions = Arc::clone(&executions);
                    Box::pin(async move {
                        executions.fetch_add(1, Ordering::SeqCst);
                        Ok("executed".to_string())
                    })
                }),
                source:     ToolSource::Native,
            });
        }
        let config = SessionOptions {
            tool_access_policy: Some(build_ask_fabro_tool_access_policy()),
            tool_exposure_mode: ToolExposureMode::AutoApprovedOnly,
            ..SessionOptions::default()
        };
        let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ));

        for tool_name in denied_tools {
            let result = fabro_agent::tool_execution::execute_and_emit_one_tool(
                &ToolCall::new("call_1", tool_name, serde_json::json!({})),
                &registry,
                Arc::clone(&sandbox),
                None,
                tokio_util::sync::CancellationToken::new(),
                &config,
                &fabro_agent::Emitter::new(),
                "test-session",
                "test-session",
                None,
            )
            .await;

            assert!(result.is_error, "{tool_name} should be blocked");
            assert!(
                result
                    .content
                    .as_str()
                    .unwrap_or_default()
                    .contains("denied by tool access policy")
            );
        }
        assert_eq!(executions.load(Ordering::SeqCst), 0);
    }
}
