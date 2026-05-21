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
use fabro_agent::{
    AgentEvent, AgentProfile, AnthropicProfile, Error as AgentError, GeminiProfile, OpenAiProfile,
    Session, SessionEvent, SessionOptions, ToolApprovalAdapter, WebFetchSummarizer,
};
use fabro_llm::client::Client as LlmClient;
use fabro_model::{AgentProfileKind, Catalog, ModelHandle, ProviderId};
use fabro_sandbox::reconnect::reconnect_for_run;
use fabro_store::{
    EventPayload, ProjectedRunSession, RunDatabase, project_run_session, project_run_sessions,
};
use fabro_types::{EventEnvelope, RunId, SessionId, TurnId};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, warn};

use super::super::session_runtime::{InterruptTurnError, SessionTurnLease, StartTurnError};
use super::super::{AppState, ListResponse};
use crate::error::ApiError;
use crate::principal_middleware::RequiredUser;

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

#[derive(Debug, serde::Deserialize)]
struct CreateRunSessionRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct SubmitTurnRequest {
    input: String,
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
    let run_store = match open_run(&state, run_id).await {
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

    let session_id = SessionId::new();
    let now = Utc::now();
    let event = match append_run_session_event(
        &run_store,
        run_id,
        session_id,
        "run.session.created",
        json!({
            "title": request.title,
            "model": request.model,
        }),
        now,
    )
    .await
    {
        Ok(event) => event,
        Err(err) => return store_error(&err).into_response(),
    };
    if let Err(err) = state
        .store_ref()
        .put_session_run_index(&session_id, &run_id)
        .await
    {
        return store_error(&err).into_response();
    }

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
    let (_, _, session) = match load_session(&state, session_id).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    Json(session.record).into_response()
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
    for (event_name, properties) in [
        (
            "run.session.turn.started",
            json!({ "turn_id": turn_id, "input": request.input }),
        ),
        (
            "run.session.user_message",
            json!({ "turn_id": turn_id, "text": request.input }),
        ),
    ] {
        match append_and_send_event(
            &run_store, &sender, run_id, session_id, event_name, properties, now,
        )
        .await
        {
            Ok(()) => {}
            Err(err) => {
                drop(turn_lease);
                return store_error(&err).into_response();
            }
        }
    }

    tokio::spawn(run_streaming_turn(
        state,
        run_id,
        run_store,
        session,
        turn_id,
        request.input,
        sender,
        turn_lease,
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
        "run.session.turn.interrupted",
        json!({ "turn_id": turn_id, "error": "Interrupted." }),
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
            "run.session.turn.interrupted",
            json!({ "turn_id": turn_id, "error": "Interrupted." }),
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
                        "run.session.turn.failed",
                        json!({ "turn_id": turn_id, "error": err.to_string() }),
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
        let result = drive_agent_session(
            &run_store,
            session,
            run_id,
            session_id,
            turn_id,
            &input,
            initialize,
            &sender,
            &mut output,
        )
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
                "run.session.turn.succeeded",
                json!({ "turn_id": turn_id, "output": outcome.output }),
                Utc::now(),
            )
            .await;
        }
        Ok(Err(err)) => {
            turn_lease.entry().clear_session().await;
            let event_name = if matches!(err, AgentError::Interrupted(_)) {
                "run.session.turn.interrupted"
            } else {
                "run.session.turn.failed"
            };
            let _ = append_and_send_event(
                &run_store,
                &sender,
                run_id,
                session_id,
                event_name,
                json!({ "turn_id": turn_id, "error": err.to_string(), "output": outcome.output }),
                Utc::now(),
            )
            .await;
        }
        Err(err) => {
            turn_lease.entry().clear_session().await;
            let _ = append_and_send_event(
                &run_store,
                &sender,
                run_id,
                session_id,
                "run.session.turn.failed",
                json!({ "turn_id": turn_id, "error": err.to_string(), "output": outcome.output }),
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
    let requested_provider_id = ProviderId::anthropic();
    let (provider_id, profile_kind) = {
        let provider = catalog.provider(&requested_provider_id).ok_or_else(|| {
            anyhow::anyhow!("provider '{requested_provider_id}' is not configured")
        })?;
        (provider.id.clone(), provider.agent_profile)
    };
    let model = session
        .record
        .model
        .clone()
        .or_else(|| {
            catalog
                .default_for_provider(&provider_id)
                .map(|model| model.id.clone())
        })
        .ok_or_else(|| anyhow::anyhow!("provider '{provider_id}' has no default model"))?;
    let llm_result = state.resolve_llm_client().await?;
    for (provider, issue) in &llm_result.auth_issues {
        warn!(provider = %provider, error = %issue, "LLM provider unavailable due to auth issue");
    }
    for issue in &llm_result.registration_issues {
        warn!(provider = %issue.provider, error = %issue.error, "LLM provider unavailable due to registration issue");
    }
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
        if is_ask_fabro_auto_approved(tool_category(tool_name)) {
            Ok(())
        } else {
            Err(format!(
                "{tool_name} tool denied by Ask Fabro read-only policy"
            ))
        }
    })
}

fn tool_category(name: &str) -> &'static str {
    match name {
        "read_file" | "read_many_files" | "grep" | "glob" | "list_dir" => "read",
        "write_file" | "edit_file" | "apply_patch" => "write",
        "spawn_agent" | "send_input" | "wait" | "close_agent" => "subagent",
        _ => "shell",
    }
}

fn is_ask_fabro_auto_approved(category: &str) -> bool {
    matches!(category, "read" | "subagent")
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
    let Some((event_name, properties)) = agent_event_payload(turn_id, event.event) else {
        return Ok(());
    };
    append_and_send_event(
        run_store, sender, run_id, session_id, event_name, properties, ts,
    )
    .await
    .map_err(Into::into)
}

fn agent_event_payload(event_turn_id: TurnId, event: AgentEvent) -> Option<(&'static str, Value)> {
    match event {
        AgentEvent::AssistantMessage {
            text, model, usage, ..
        } => Some((
            "run.session.assistant_message",
            json!({ "turn_id": event_turn_id, "text": text, "model": model, "usage": usage }),
        )),
        AgentEvent::TextDelta { delta } | AgentEvent::ReasoningDelta { delta } => Some((
            "run.session.assistant_delta",
            json!({ "turn_id": event_turn_id, "delta": delta }),
        )),
        AgentEvent::ToolCallStarted {
            tool_name,
            tool_call_id,
            arguments,
        } => Some((
            "run.session.tool_call.started",
            json!({
                "turn_id": event_turn_id,
                "tool_name": tool_name,
                "tool_call_id": tool_call_id,
                "arguments": arguments
            }),
        )),
        AgentEvent::ToolCallCompleted {
            tool_name,
            tool_call_id,
            output,
            is_error,
        } => Some((
            "run.session.tool_call.completed",
            json!({
                "turn_id": event_turn_id,
                "tool_name": tool_name,
                "tool_call_id": tool_call_id,
                "output": output,
                "is_error": is_error
            }),
        )),
        _ => None,
    }
}

async fn append_and_send_event(
    run_store: &RunDatabase,
    sender: &SessionSseSender,
    run_id: RunId,
    session_id: SessionId,
    event_name: &str,
    properties: Value,
    ts: DateTime<Utc>,
) -> fabro_store::Result<()> {
    let event =
        append_run_session_event(run_store, run_id, session_id, event_name, properties, ts).await?;
    send_sse_event(sender, &event).await;
    Ok(())
}

async fn append_run_session_event(
    run_store: &RunDatabase,
    run_id: RunId,
    session_id: SessionId,
    event_name: &str,
    properties: Value,
    ts: DateTime<Utc>,
) -> fabro_store::Result<EventEnvelope> {
    let payload = EventPayload::new(
        json!({
            "id": format!("evt_{}", ulid::Ulid::new()),
            "ts": ts,
            "run_id": run_id,
            "session_id": session_id,
            "event": event_name,
            "properties": properties,
        }),
        &run_id,
    )?;
    let seq = run_store.append_event(&payload).await?;
    run_store
        .get_event(seq)
        .await?
        .ok_or_else(|| fabro_store::Error::Other(format!("missing appended run event {seq}")))
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

async fn open_run(state: &AppState, run_id: RunId) -> Result<RunDatabase, Response> {
    state.store_ref().open_run(&run_id).await.map_err(|err| {
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
