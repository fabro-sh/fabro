use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use fabro_agent::config::ToolApprovalFn;
use fabro_agent::{
    AgentEvent, AgentProfile, AnthropicProfile, Error as AgentError, GeminiProfile, LocalSandbox,
    OpenAiProfile, ReadBeforeWriteSandbox, Session, SessionEvent, SessionOptions,
    ToolApprovalAdapter, WebFetchSummarizer,
};
use fabro_llm::client::Client as LlmClient;
use fabro_model::{AgentProfileKind, Catalog, ModelHandle, ProviderId};
use fabro_types::{
    SessionEventEnvelope, SessionId, SessionMessage, SessionRecord, SessionStatus, TurnId,
    TurnRecord, TurnStatus,
};
use serde_json::json;
use tokio::fs;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{error, warn};

use super::super::{ActiveSessionTurnGuard, AppState, ListResponse};
use crate::error::ApiError;
use crate::principal_middleware::RequiredUser;

type SessionSseSender = mpsc::UnboundedSender<Result<Event, Infallible>>;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sessions", get(list_sessions).post(create_session))
        .route(
            "/sessions/{id}",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        .route("/sessions/{id}/turns", get(list_turns).post(submit_turn))
        .route("/sessions/{id}/turns/{turnId}", get(get_turn))
        .route(
            "/sessions/{id}/turns/{turnId}/interrupt",
            post(interrupt_turn),
        )
        .route("/sessions/{id}/events", get(list_events))
        .route("/sessions/{id}/tools", get(list_tools))
}

#[derive(Debug, serde::Deserialize)]
struct CreateSessionRequest {
    #[serde(default)]
    title:       Option<String>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    provider:    Option<String>,
    #[serde(default)]
    model:       Option<String>,
    #[serde(default)]
    permissions: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct UpdateSessionRequest {
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct SubmitTurnRequest {
    input: String,
}

#[derive(Debug, serde::Deserialize)]
struct SubmitTurnQuery {
    #[serde(default = "default_stream")]
    stream: bool,
}

fn default_stream() -> bool {
    true
}

#[derive(Debug, serde::Deserialize)]
struct EventQuery {
    #[serde(default)]
    since_seq: Option<u32>,
}

async fn list_sessions(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    match state.session_store().list_sessions().await {
        Ok(sessions) => Json(ListResponse::new(sessions)).into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn create_session(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateSessionRequest>,
) -> Response {
    let now = Utc::now();
    let session_id = SessionId::new();
    let mut record = SessionRecord::new(session_id, now);
    record.title = request.title;
    record.working_dir = request.working_dir;
    record.provider = request.provider;
    record.model = request.model;
    record.permissions = request.permissions;

    match state.session_store().create_session(record).await {
        Ok(record) => {
            let _ = state
                .session_store()
                .append_event(SessionEventEnvelope::new(
                    session_id,
                    None,
                    "session.created",
                    json!({ "title": record.title }),
                    now,
                ))
                .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
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
    match state.session_store().get_session(session_id).await {
        Ok(Some(record)) => Json(record).into_response(),
        Ok(None) => ApiError::not_found("Session not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn update_session(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateSessionRequest>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let mut record = match state.session_store().get_session(session_id).await {
        Ok(Some(record)) => record,
        Ok(None) => return ApiError::not_found("Session not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Some(title) = request.title {
        record.title = Some(title);
        record.updated_at = Utc::now();
    }
    match state.session_store().update_session(record).await {
        Ok(record) => Json(record).into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn delete_session(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    match state.session_store().delete_session(session_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn submit_turn(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<SubmitTurnQuery>,
    Json(request): Json<SubmitTurnRequest>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Some(session_record) = (match state.session_store().get_session(session_id).await {
        Ok(session) => session,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }) else {
        return ApiError::not_found("Session not found.").into_response();
    };
    let active_guard = if query.stream {
        match state.try_lock_session_turn(session_id) {
            Some(guard) => Some(guard),
            None => {
                return ApiError::new(StatusCode::CONFLICT, "Session already has an active turn.")
                    .into_response();
            }
        }
    } else {
        None
    };

    let turn_id = TurnId::new();
    let now = Utc::now();
    let turn = TurnRecord {
        id: turn_id,
        session_id,
        input: request.input.clone(),
        status: TurnStatus::Queued,
        messages: vec![SessionMessage::user(request.input.clone(), now)],
        error: None,
        created_at: now,
        updated_at: now,
        completed_at: None,
    };
    if let Err(err) = state.session_store().append_turn(turn).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    match state
        .session_store()
        .append_event(SessionEventEnvelope::new(
            session_id,
            Some(turn_id),
            "turn.queued",
            json!({ "turn_id": turn_id }),
            now,
        ))
        .await
    {
        Ok(event) if query.stream => {
            let (sender, receiver) = mpsc::unbounded_channel();
            send_sse_event(&sender, &event);
            tokio::spawn(run_streaming_turn(
                state,
                session_record,
                turn_id,
                request.input,
                sender,
                active_guard.expect("streaming turn should hold active guard"),
            ));
            Sse::new(UnboundedReceiverStream::new(receiver))
                .keep_alive(KeepAlive::default())
                .into_response()
        }
        Ok(_) => (StatusCode::ACCEPTED, Json(json!({ "turn_id": turn_id }))).into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn list_turns(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    match state.session_store().get_session(session_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::not_found("Session not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }
    match state.session_store().list_turns(session_id).await {
        Ok(turns) => Json(ListResponse::new(turns)).into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn get_turn(
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
    match state.session_store().get_turn(session_id, turn_id).await {
        Ok(Some(turn)) => Json(turn).into_response(),
        Ok(None) => ApiError::not_found("Turn not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
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
    match state.session_store().get_turn(session_id, turn_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::not_found("Turn not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }
    let now = Utc::now();
    match state
        .session_store()
        .append_event(SessionEventEnvelope::new(
            session_id,
            Some(turn_id),
            "turn.interrupt_requested",
            json!({ "turn_id": turn_id }),
            now,
        ))
        .await
    {
        Ok(event) => (StatusCode::ACCEPTED, Json(event)).into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn list_events(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<EventQuery>,
    headers: HeaderMap,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    match state.session_store().get_session(session_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::not_found("Session not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }
    let since_seq = query
        .since_seq
        .or_else(|| {
            headers
                .get("last-event-id")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u32>().ok())
                .map(|seq| seq.saturating_add(1))
        })
        .unwrap_or(1);
    match state
        .session_store()
        .list_events(session_id, Some(since_seq))
        .await
    {
        Ok(events) => Json(ListResponse::new(events)).into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn list_tools(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let session_id = match parse_session_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    match state.session_store().get_session(session_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::not_found("Session not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }
    Json(ListResponse::new(Vec::<serde_json::Value>::new())).into_response()
}

async fn run_streaming_turn(
    state: Arc<AppState>,
    mut record: SessionRecord,
    turn_id: TurnId,
    input: String,
    sender: SessionSseSender,
    _guard: ActiveSessionTurnGuard,
) {
    if let Err(err) = mark_turn_running(&state, &mut record, turn_id, &sender).await {
        error!(error = ?err, session_id = %record.id, turn_id = %turn_id, "Failed to mark session turn running");
        mark_turn_failed(&state, &mut record, turn_id, err.to_string(), &sender).await;
        return;
    }

    let mut session = match build_agent_session(&state, &record).await {
        Ok(session) => session,
        Err(err) => {
            error!(error = ?err, session_id = %record.id, turn_id = %turn_id, "Failed to build session runtime");
            mark_turn_failed(&state, &mut record, turn_id, err.to_string(), &sender).await;
            return;
        }
    };

    let result =
        drive_agent_session(&state, &mut session, record.id, turn_id, &input, &sender).await;
    let mut final_record = session.to_record(record);
    let messages = session.history().to_session_messages();
    match result {
        Ok(Ok(())) => {
            final_record.status = SessionStatus::Idle;
            if let Err(err) = mark_turn_finished(
                &state,
                final_record,
                turn_id,
                TurnStatus::Succeeded,
                messages,
                None,
                &sender,
            )
            .await
            {
                error!(error = ?err, turn_id = %turn_id, "Failed to persist successful session turn");
            }
        }
        Ok(Err(err)) => {
            let status = if matches!(err, AgentError::Interrupted(_)) {
                TurnStatus::Interrupted
            } else {
                TurnStatus::Failed
            };
            final_record.status = if status == TurnStatus::Interrupted {
                SessionStatus::Idle
            } else {
                SessionStatus::Failed
            };
            if let Err(update_err) = mark_turn_finished(
                &state,
                final_record,
                turn_id,
                status,
                messages,
                Some(err.to_string()),
                &sender,
            )
            .await
            {
                error!(error = ?update_err, turn_id = %turn_id, "Failed to persist failed session turn");
            }
        }
        Err(err) => {
            final_record.status = SessionStatus::Failed;
            if let Err(update_err) = mark_turn_finished(
                &state,
                final_record,
                turn_id,
                TurnStatus::Failed,
                messages,
                Some(err.to_string()),
                &sender,
            )
            .await
            {
                error!(error = ?update_err, turn_id = %turn_id, "Failed to persist errored session turn");
            }
        }
    }
}

async fn mark_turn_running(
    state: &AppState,
    record: &mut SessionRecord,
    turn_id: TurnId,
    sender: &SessionSseSender,
) -> anyhow::Result<()> {
    let now = Utc::now();
    record.status = SessionStatus::Running;
    record.updated_at = now;
    state.session_store().update_session(record.clone()).await?;

    if let Some(mut turn) = state.session_store().get_turn(record.id, turn_id).await? {
        turn.status = TurnStatus::Running;
        turn.updated_at = now;
        state.session_store().update_turn(turn).await?;
    }

    append_and_send_event(
        state,
        sender,
        SessionEventEnvelope::new(record.id, Some(turn_id), "turn.running", json!({}), now),
    )
    .await?;
    Ok(())
}

async fn mark_turn_failed(
    state: &AppState,
    record: &mut SessionRecord,
    turn_id: TurnId,
    error: String,
    sender: &SessionSseSender,
) {
    record.status = SessionStatus::Failed;
    let _ = mark_turn_finished(
        state,
        record.clone(),
        turn_id,
        TurnStatus::Failed,
        Vec::new(),
        Some(error),
        sender,
    )
    .await;
}

async fn mark_turn_finished(
    state: &AppState,
    mut record: SessionRecord,
    turn_id: TurnId,
    status: TurnStatus,
    messages: Vec<SessionMessage>,
    error: Option<String>,
    sender: &SessionSseSender,
) -> anyhow::Result<()> {
    let now = Utc::now();
    record.updated_at = now;
    state.session_store().update_session(record.clone()).await?;

    if let Some(mut turn) = state.session_store().get_turn(record.id, turn_id).await? {
        turn.status = status;
        if !messages.is_empty() {
            turn.messages = messages;
        }
        turn.error = error.clone();
        turn.updated_at = now;
        turn.completed_at = Some(now);
        state.session_store().update_turn(turn).await?;
    }

    let event_name = match status {
        TurnStatus::Succeeded => "turn.succeeded",
        TurnStatus::Interrupted => "turn.interrupted",
        TurnStatus::Failed => "turn.failed",
        TurnStatus::Queued => "turn.queued",
        TurnStatus::Running => "turn.running",
    };
    let mut properties = json!({ "turn_id": turn_id });
    if let Some(error) = error {
        properties["error"] = serde_json::Value::String(error);
    }
    append_and_send_event(
        state,
        sender,
        SessionEventEnvelope::new(record.id, Some(turn_id), event_name, properties, now),
    )
    .await?;
    Ok(())
}

async fn build_agent_session(state: &AppState, record: &SessionRecord) -> anyhow::Result<Session> {
    let catalog = state.catalog();
    let requested_provider_id = record
        .provider
        .as_deref()
        .map_or_else(ProviderId::anthropic, ProviderId::from);
    let (provider_id, profile_kind) = {
        let provider = catalog.provider(&requested_provider_id).ok_or_else(|| {
            anyhow::anyhow!("provider '{requested_provider_id}' is not configured")
        })?;
        (
            provider.id.clone(),
            provider.adapter.metadata().default_profile,
        )
    };
    let model = record
        .model
        .clone()
        .or_else(|| {
            catalog
                .default_for_provider(&provider_id)
                .map(|model| model.id.clone())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "provider '{provider_id}' has no default model in the catalog; pass --model explicitly"
            )
        })?;
    let llm_result = state.resolve_llm_client().await?;
    for (provider, issue) in &llm_result.auth_issues {
        warn!(provider = %provider, error = %issue, "LLM provider unavailable due to auth issue");
    }
    if !llm_result.client.has_provider(provider_id.as_str()) {
        anyhow::bail!("LLM credentials not configured for provider '{provider_id}'");
    }

    let working_dir = resolve_working_dir(record).await?;
    let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(ReadBeforeWriteSandbox::new(Arc::new(
        LocalSandbox::new(working_dir.clone()),
    )));
    let profile = build_profile(
        provider_id,
        profile_kind,
        &model,
        &llm_result.client,
        Arc::clone(&catalog),
    );
    let config = SessionOptions {
        git_root: Some(working_dir.to_string_lossy().into_owned()),
        tool_hooks: Some(Arc::new(ToolApprovalAdapter(build_tool_approval(
            record.permissions.as_deref(),
        )))),
        ..SessionOptions::default()
    };

    Session::from_record(record, llm_result.client, profile, sandbox, config, None)
        .map_err(Into::into)
}

async fn resolve_working_dir(record: &SessionRecord) -> anyhow::Result<PathBuf> {
    let working_dir = match &record.working_dir {
        Some(working_dir) => PathBuf::from(working_dir),
        None => std::env::current_dir()?,
    };
    if !working_dir.is_absolute() {
        anyhow::bail!("working_dir must be an absolute path for v1 sessions");
    }
    let metadata = fs::metadata(&working_dir).await.map_err(|err| {
        anyhow::anyhow!(
            "working_dir is only supported for local same-machine server targets; server cannot access {}: {err}",
            working_dir.display()
        )
    })?;
    if !metadata.is_dir() {
        anyhow::bail!(
            "working_dir is only supported for local same-machine server targets; {} is not a directory",
            working_dir.display()
        );
    }
    Ok(working_dir)
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

fn build_tool_approval(raw: Option<&str>) -> ToolApprovalFn {
    let level = match raw.unwrap_or("read-write") {
        "read-only" => PermissionLevel::ReadOnly,
        "full" => PermissionLevel::Full,
        _ => PermissionLevel::ReadWrite,
    };
    Arc::new(move |tool_name: &str, _args: &serde_json::Value| {
        if is_auto_approved(level, tool_category(tool_name)) {
            Ok(())
        } else {
            Err(format!(
                "{tool_name} tool denied at current permission level"
            ))
        }
    })
}

#[derive(Clone, Copy)]
enum PermissionLevel {
    ReadOnly,
    ReadWrite,
    Full,
}

fn tool_category(name: &str) -> &'static str {
    match name {
        "read_file" | "read_many_files" | "grep" | "glob" | "list_dir" => "read",
        "write_file" | "edit_file" | "apply_patch" => "write",
        "spawn_agent" | "send_input" | "wait" | "close_agent" => "subagent",
        _ => "shell",
    }
}

fn is_auto_approved(level: PermissionLevel, category: &str) -> bool {
    matches!(
        (level, category),
        (_, "read" | "subagent")
            | (PermissionLevel::ReadWrite | PermissionLevel::Full, "write")
            | (PermissionLevel::Full, "shell")
    )
}

async fn drive_agent_session(
    state: &AppState,
    session: &mut Session,
    session_id: SessionId,
    turn_id: TurnId,
    input: &str,
    sender: &SessionSseSender,
) -> anyhow::Result<Result<(), AgentError>> {
    let mut receiver = session.subscribe();
    let process = async {
        session.initialize().await?;
        session.process_input(input).await
    };
    tokio::pin!(process);

    loop {
        tokio::select! {
            result = &mut process => {
                while let Ok(event) = receiver.try_recv() {
                    persist_agent_event(state, session_id, turn_id, event, sender).await?;
                }
                return Ok(result);
            }
            event = receiver.recv() => {
                match event {
                    Ok(event) => persist_agent_event(state, session_id, turn_id, event, sender).await?,
                    Err(RecvError::Lagged(_) | RecvError::Closed) => {}
                }
            }
        }
    }
}

async fn persist_agent_event(
    state: &AppState,
    session_id: SessionId,
    turn_id: TurnId,
    event: SessionEvent,
    sender: &SessionSseSender,
) -> anyhow::Result<()> {
    append_and_send_event(
        state,
        sender,
        agent_event_envelope(session_id, turn_id, event),
    )
    .await
}

async fn append_and_send_event(
    state: &AppState,
    sender: &SessionSseSender,
    event: SessionEventEnvelope,
) -> anyhow::Result<()> {
    let event = state.session_store().append_event(event).await?;
    send_sse_event(sender, &event);
    Ok(())
}

fn send_sse_event(sender: &SessionSseSender, event: &SessionEventEnvelope) {
    if let Ok(data) = serde_json::to_string(event) {
        let _ = sender.send(Ok(Event::default()
            .id(event.seq.to_string())
            .event(event.event.clone())
            .data(data)));
    }
}

fn agent_event_envelope(
    session_id: SessionId,
    turn_id: TurnId,
    event: SessionEvent,
) -> SessionEventEnvelope {
    let ts = event.timestamp.into();
    let event_name = agent_event_name(&event.event);
    let properties = agent_event_properties(event);
    SessionEventEnvelope::new(session_id, Some(turn_id), event_name, properties, ts)
}

fn agent_event_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::SessionStarted { .. } => "session.started",
        AgentEvent::SessionEnded => "session.ended",
        AgentEvent::ProcessingEnd => "turn.processing_end",
        AgentEvent::UserInput { .. } => "turn.user_input",
        AgentEvent::AssistantTextStart => "turn.assistant_text_start",
        AgentEvent::AssistantOutputReplace { .. } => "turn.assistant_output_replace",
        AgentEvent::AssistantMessage { .. } => "turn.assistant_message",
        AgentEvent::TextDelta { .. } => "turn.text_delta",
        AgentEvent::ReasoningDelta { .. } => "turn.reasoning_delta",
        AgentEvent::ToolCallStarted { .. } => "turn.tool_call_started",
        AgentEvent::ToolCallOutputDelta { .. } => "turn.tool_call_output_delta",
        AgentEvent::ToolCallCompleted { .. } => "turn.tool_call_completed",
        AgentEvent::Error { .. } => "turn.error",
        AgentEvent::Warning { .. } => "turn.warning",
        AgentEvent::LoopDetected => "turn.loop_detected",
        AgentEvent::TurnLimitReached { .. } => "turn.limit_reached",
        AgentEvent::SkillExpanded { .. } => "turn.skill_expanded",
        AgentEvent::SteeringInjected { .. } => "turn.steering_injected",
        AgentEvent::CompactionStarted { .. } => "turn.compaction_started",
        AgentEvent::CompactionCompleted { .. } => "turn.compaction_completed",
        AgentEvent::LlmRetry { .. } => "turn.llm_retry",
        AgentEvent::SubAgentSpawned { .. } => "turn.subagent_spawned",
        AgentEvent::SubAgentCompleted { .. } => "turn.subagent_completed",
        AgentEvent::SubAgentFailed { .. } => "turn.subagent_failed",
        AgentEvent::SubAgentClosed { .. } => "turn.subagent_closed",
        AgentEvent::McpServerReady { .. } => "session.mcp_server_ready",
        AgentEvent::McpServerFailed { .. } => "session.mcp_server_failed",
    }
}

fn agent_event_properties(event: SessionEvent) -> serde_json::Value {
    match event.event {
        AgentEvent::UserInput { text } => json!({ "text": text }),
        AgentEvent::AssistantOutputReplace { text, reasoning } => {
            json!({ "text": text, "reasoning": reasoning })
        }
        AgentEvent::AssistantMessage {
            text,
            model,
            usage,
            tool_call_count,
        } => json!({
            "text": text,
            "model": model,
            "usage": usage,
            "tool_call_count": tool_call_count
        }),
        AgentEvent::TextDelta { delta }
        | AgentEvent::ReasoningDelta { delta }
        | AgentEvent::ToolCallOutputDelta { delta } => json!({ "delta": delta }),
        AgentEvent::ToolCallStarted {
            tool_name,
            tool_call_id,
            arguments,
        } => json!({
            "tool_name": tool_name,
            "tool_call_id": tool_call_id,
            "arguments": arguments
        }),
        AgentEvent::ToolCallCompleted {
            tool_name,
            tool_call_id,
            output,
            is_error,
        } => json!({
            "tool_name": tool_name,
            "tool_call_id": tool_call_id,
            "output": output,
            "is_error": is_error
        }),
        AgentEvent::Error { error } => json!({ "error": error.to_string() }),
        AgentEvent::Warning {
            kind,
            message,
            details,
        } => json!({ "kind": kind, "message": message, "details": details }),
        AgentEvent::TurnLimitReached { max_turns } => json!({ "max_turns": max_turns }),
        AgentEvent::SkillExpanded { skill_name } => json!({ "skill_name": skill_name }),
        AgentEvent::SteeringInjected { text, actor } => json!({ "text": text, "actor": actor }),
        AgentEvent::CompactionStarted {
            estimated_tokens,
            context_window_size,
        } => json!({
            "estimated_tokens": estimated_tokens,
            "context_window_size": context_window_size
        }),
        AgentEvent::CompactionCompleted {
            original_turn_count,
            preserved_turn_count,
            summary_token_estimate,
            tracked_file_count,
        } => json!({
            "original_turn_count": original_turn_count,
            "preserved_turn_count": preserved_turn_count,
            "summary_token_estimate": summary_token_estimate,
            "tracked_file_count": tracked_file_count
        }),
        AgentEvent::LlmRetry {
            provider,
            model,
            attempt,
            delay_secs,
            error,
        } => json!({
            "provider": provider,
            "model": model,
            "attempt": attempt,
            "delay_secs": delay_secs,
            "error": error.to_string()
        }),
        AgentEvent::SubAgentSpawned {
            agent_id,
            depth,
            task,
        } => json!({ "agent_id": agent_id, "depth": depth, "task": task }),
        AgentEvent::SubAgentCompleted {
            agent_id,
            depth,
            success,
            turns_used,
        } => json!({
            "agent_id": agent_id,
            "depth": depth,
            "success": success,
            "turns_used": turns_used
        }),
        AgentEvent::SubAgentFailed {
            agent_id,
            depth,
            error,
        } => json!({ "agent_id": agent_id, "depth": depth, "error": error.to_string() }),
        AgentEvent::SubAgentClosed { agent_id, depth } => {
            json!({ "agent_id": agent_id, "depth": depth })
        }
        AgentEvent::McpServerReady {
            server_name,
            tool_count,
        } => json!({ "server_name": server_name, "tool_count": tool_count }),
        AgentEvent::McpServerFailed { server_name, error } => {
            json!({ "server_name": server_name, "error": error })
        }
        AgentEvent::SessionStarted { provider, model } => {
            json!({ "provider": provider, "model": model })
        }
        AgentEvent::SessionEnded
        | AgentEvent::ProcessingEnd
        | AgentEvent::AssistantTextStart
        | AgentEvent::LoopDetected => {
            json!({})
        }
    }
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
