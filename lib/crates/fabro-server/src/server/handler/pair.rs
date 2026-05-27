use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use fabro_store::EventEnvelope;
use fabro_types::{
    EventBody, MAX_PAIR_MESSAGE_BYTES, PairId, PairMessageId, PairMessageRecord,
    PairMessageRequest, PairRecord, PairStartRequest, PairStatus, PairTarget,
    PairTranscriptAssistantMessage, PairTranscriptDetailRef, PairTranscriptEntry,
    PairTranscriptError, PairTranscriptMeta, PairTranscriptResponse, PairTranscriptSystemMessage,
    PairTranscriptToolCall, PairTranscriptToolStatus, PairTranscriptUserMessage,
    PairTranscriptWarning, RunId, StageId,
};
use fabro_workflow::run_status::RunStatus;
use tokio::time::timeout;
use tokio_stream::StreamExt;

use super::super::{AppState, PairTransportError, durable_run_status, reject_if_archived};
use super::events::EventListParams;
use crate::error::ApiError;
use crate::principal_middleware::RequireRunManagementTarget;

const PAIR_CONFIRM_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/runs/{id}/pair", get(get_pair_status).post(start_pair))
        .route("/runs/{id}/pair/{pair_id}", get(get_pair).delete(end_pair))
        .route(
            "/runs/{id}/pair/{pair_id}/messages",
            post(send_pair_message),
        )
        .route("/runs/{id}/pair/{pair_id}/transcript", get(get_transcript))
}

async fn get_pair_status(
    RequireRunManagementTarget(id, _actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    let targets = live_pair_targets(state.as_ref(), &id);
    let current_pair = match reconstruct_pairs(state.as_ref(), &id).await {
        Ok(pairs) => pairs
            .values()
            .filter(|pair| pair.status == PairStatus::Active)
            .max_by_key(|pair| pair.started_at)
            .cloned(),
        Err(response) => return response,
    };

    Json(fabro_types::RunPairStatusResponse {
        run_id: id,
        current_pair,
        targets,
    })
    .into_response()
}

async fn start_pair(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    Json(req): Json<PairStartRequest>,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    if let Some(active) = current_pair(state.as_ref(), &id).await {
        match active {
            Ok(_) => {
                return ApiError::with_code(
                    StatusCode::CONFLICT,
                    "Run already has an active pair.",
                    "already_paired",
                )
                .into_response();
            }
            Err(response) => return response,
        }
    }

    let (target, transport) = match pair_target_and_transport(state.as_ref(), &id, &req.stage_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(transport) = transport else {
        return worker_unavailable("Run has no live worker control channel.");
    };

    let pair_id = PairId::new();
    match transport.start_pair(id, pair_id, target, actor).await {
        Ok(()) => {
            match wait_for_pair_record(state.as_ref(), &id, pair_id, PairStatus::Active, None).await
            {
                Ok(record) => Json(record).into_response(),
                Err(response) => response,
            }
        }
        Err(err) => pair_transport_error_response(err),
    }
}

async fn get_pair(
    RequireRunManagementTarget(id, _actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    Path((_id, pair_id)): Path<(String, String)>,
) -> Response {
    let pair_id = match parse_pair_id(&pair_id) {
        Ok(pair_id) => pair_id,
        Err(response) => return response,
    };
    match pair_by_id(state.as_ref(), &id, pair_id).await {
        Ok(pair) => Json(pair).into_response(),
        Err(response) => response,
    }
}

async fn end_pair(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    Path((_id, pair_id)): Path<(String, String)>,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let pair_id = match parse_pair_id(&pair_id) {
        Ok(pair_id) => pair_id,
        Err(response) => return response,
    };
    let existing = match pair_window_by_id(state.as_ref(), &id, pair_id).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if existing.record.status != PairStatus::Active {
        return pair_conflict("Pair is not active.", "pair_not_active");
    }
    let transport = match live_transport_for_pair_command(state.as_ref(), &id) {
        Ok(transport) => transport,
        Err(response) => return response,
    };
    let Some(transport) = transport else {
        return worker_unavailable("Run has no live worker control channel.");
    };

    match transport.end_pair(pair_id, actor).await {
        Ok(()) => {
            match wait_for_pair_record(
                state.as_ref(),
                &id,
                pair_id,
                PairStatus::Ended,
                Some(&existing.record),
            )
            .await
            {
                Ok(record) => Json(record).into_response(),
                Err(response) => response,
            }
        }
        Err(err) => pair_transport_error_response(err),
    }
}

async fn send_pair_message(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    Path((_id, pair_id)): Path<(String, String)>,
    Json(req): Json<PairMessageRequest>,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let pair_id = match parse_pair_id(&pair_id) {
        Ok(pair_id) => pair_id,
        Err(response) => return response,
    };
    let text = req.text;
    let text = text.trim().to_string();
    if text.is_empty() {
        return ApiError::bad_request("Pair message text must not be empty.").into_response();
    }
    if text.len() > MAX_PAIR_MESSAGE_BYTES {
        return ApiError::bad_request(format!(
            "Pair message text must be at most {MAX_PAIR_MESSAGE_BYTES} bytes."
        ))
        .into_response();
    }
    let pair_window = match pair_window_by_id(state.as_ref(), &id, pair_id).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if pair_window.record.status != PairStatus::Active {
        return pair_conflict("Pair is not active.", "pair_not_active");
    }

    let transport = match live_transport_for_pair_command(state.as_ref(), &id) {
        Ok(transport) => transport,
        Err(response) => return response,
    };
    let Some(transport) = transport else {
        return worker_unavailable("Run has no live worker control channel.");
    };
    let message_id = PairMessageId::new();
    match transport
        .send_pair_message(pair_id, message_id, text, req.client_message_id, actor)
        .await
    {
        Ok(()) => {
            match wait_for_pair_message_record(state.as_ref(), &id, &pair_window, message_id).await
            {
                Ok(record) => (StatusCode::ACCEPTED, Json(record)).into_response(),
                Err(response) => response,
            }
        }
        Err(err) => pair_transport_error_response(err),
    }
}

async fn get_transcript(
    RequireRunManagementTarget(id, _actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    Path((_id, pair_id)): Path<(String, String)>,
    Query(params): Query<EventListParams>,
) -> Response {
    let pair_id = match parse_pair_id(&pair_id) {
        Ok(pair_id) => pair_id,
        Err(response) => return response,
    };
    let window = match pair_window_by_id(state.as_ref(), &id, pair_id).await {
        Ok(window) => window,
        Err(response) => return response,
    };
    let page = match transcript_page(
        state.as_ref(),
        &id,
        &window,
        params.since_seq(),
        params.limit(),
    )
    .await
    {
        Ok(page) => page,
        Err(response) => return response,
    };
    Json(PairTranscriptResponse {
        data: page.entries,
        meta: PairTranscriptMeta {
            next_since_seq: page.next_since_seq,
            has_more:       page.has_more,
        },
    })
    .into_response()
}

fn transcript_entry_from_event(
    pair: &PairRecord,
    envelope: &EventEnvelope,
) -> Option<PairTranscriptEntry> {
    match &envelope.event.body {
        EventBody::AgentPairUserMessage(props) if props.pair_id == pair.pair_id => Some(
            PairTranscriptEntry::UserMessage(PairTranscriptUserMessage {
                seq:               envelope.seq,
                event_id:          envelope.event.id.clone(),
                ts:                envelope.event.ts,
                pair_id:           props.pair_id,
                target:            pair.target.clone(),
                message_id:        props.message_id,
                client_message_id: props.client_message_id.clone(),
                text:              props.text.clone(),
            }),
        ),
        EventBody::AgentPairSystemMessage(props) if props.pair_id == pair.pair_id => Some(
            PairTranscriptEntry::SystemMessage(PairTranscriptSystemMessage {
                seq:                 envelope.seq,
                event_id:            envelope.event.id.clone(),
                ts:                  envelope.event.ts,
                pair_id:             props.pair_id,
                target:              pair.target.clone(),
                system_message_kind: props.kind,
                text:                props.text.clone(),
            }),
        ),
        EventBody::AgentMessage(props) if event_matches_pair_target(pair, &envelope.event) => Some(
            PairTranscriptEntry::AssistantMessage(PairTranscriptAssistantMessage {
                seq:             envelope.seq,
                event_id:        envelope.event.id.clone(),
                ts:              envelope.event.ts,
                pair_id:         pair.pair_id,
                target:          pair.target.clone(),
                text:            props.text.clone(),
                tool_call_count: props.tool_call_count,
            }),
        ),
        EventBody::AgentToolStarted(props) if event_matches_pair_target(pair, &envelope.event) => {
            Some(PairTranscriptEntry::ToolCall(PairTranscriptToolCall {
                seq:          envelope.seq,
                event_id:     envelope.event.id.clone(),
                ts:           envelope.event.ts,
                pair_id:      pair.pair_id,
                target:       pair.target.clone(),
                tool_call_id: props.tool_call_id.clone(),
                tool_name:    props.tool_name.clone(),
                status:       PairTranscriptToolStatus::Started,
                summary:      compact_summary(&props.tool_name, &props.arguments, false),
                is_error:     false,
                truncated:    true,
                detail_ref:   PairTranscriptDetailRef {
                    seq:          envelope.seq,
                    tool_call_id: Some(props.tool_call_id.clone()),
                },
            }))
        }
        EventBody::AgentToolCompleted(props)
            if event_matches_pair_target(pair, &envelope.event) =>
        {
            Some(PairTranscriptEntry::ToolCall(PairTranscriptToolCall {
                seq:          envelope.seq,
                event_id:     envelope.event.id.clone(),
                ts:           envelope.event.ts,
                pair_id:      pair.pair_id,
                target:       pair.target.clone(),
                tool_call_id: props.tool_call_id.clone(),
                tool_name:    props.tool_name.clone(),
                status:       PairTranscriptToolStatus::Completed,
                summary:      compact_summary(&props.tool_name, &props.output, props.is_error),
                is_error:     props.is_error,
                truncated:    true,
                detail_ref:   PairTranscriptDetailRef {
                    seq:          envelope.seq,
                    tool_call_id: Some(props.tool_call_id.clone()),
                },
            }))
        }
        EventBody::AgentError(props) if event_matches_pair_target(pair, &envelope.event) => {
            Some(PairTranscriptEntry::Error(PairTranscriptError {
                seq:        envelope.seq,
                event_id:   envelope.event.id.clone(),
                ts:         envelope.event.ts,
                pair_id:    pair.pair_id,
                target:     pair.target.clone(),
                message:    compact_value(&props.error, 240),
                detail_ref: PairTranscriptDetailRef {
                    seq:          envelope.seq,
                    tool_call_id: None,
                },
            }))
        }
        EventBody::AgentWarning(props) if event_matches_pair_target(pair, &envelope.event) => {
            Some(PairTranscriptEntry::Warning(PairTranscriptWarning {
                seq:          envelope.seq,
                event_id:     envelope.event.id.clone(),
                ts:           envelope.event.ts,
                pair_id:      pair.pair_id,
                target:       pair.target.clone(),
                warning_kind: props.kind.clone(),
                message:      props.message.clone(),
                detail_ref:   PairTranscriptDetailRef {
                    seq:          envelope.seq,
                    tool_call_id: None,
                },
            }))
        }
        _ => None,
    }
}

fn event_matches_pair_target(pair: &PairRecord, event: &fabro_types::RunEvent) -> bool {
    event.stage_id.as_ref() == Some(&pair.target.stage_id)
}

fn compact_summary(tool_name: &str, value: &serde_json::Value, is_error: bool) -> String {
    let status = if is_error { "error" } else { "ok" };
    format!("{tool_name} {status}: {}", compact_value(value, 180))
}

fn compact_value(value: &serde_json::Value, max_len: usize) -> String {
    match value {
        serde_json::Value::String(value) => compact_text(value, max_len),
        other => compact_text(&serde_json::to_string(other).unwrap_or_default(), max_len),
    }
}

fn compact_text(value: &str, max_len: usize) -> String {
    let mut rendered = String::with_capacity(max_len.min(value.len()).saturating_add(3));
    let mut truncated = false;
    for ch in value.chars() {
        let ch = if ch == '\n' || ch == '\r' { ' ' } else { ch };
        if rendered.len().saturating_add(ch.len_utf8()) > max_len {
            truncated = true;
            break;
        }
        rendered.push(ch);
    }
    if truncated {
        rendered.push_str("...");
    }
    rendered
}

fn live_pair_targets(state: &AppState, id: &RunId) -> Vec<PairTarget> {
    state
        .runs
        .lock()
        .expect("runs lock poisoned")
        .get(id)
        .map(|run| run.active_api_targets.values().cloned().collect())
        .unwrap_or_default()
}

#[allow(
    clippy::result_large_err,
    reason = "Pair request validation maps failures directly to HTTP responses."
)]
fn pair_target_and_transport(
    state: &AppState,
    id: &RunId,
    stage_id: &StageId,
) -> Result<(PairTarget, Option<super::super::RunAnswerTransport>), Response> {
    let runs = state.runs.lock().expect("runs lock poisoned");
    let Some(run) = runs.get(id) else {
        return Err(ApiError::not_found("Run not found.").into_response());
    };
    reject_unpairable_status(run.status)?;
    let Some(target) = run.active_api_targets.get(stage_id) else {
        return Err(pair_conflict(
            "Requested pair target is not active.",
            "pair_target_not_active",
        ));
    };
    Ok((target.clone(), run.answer_transport.clone()))
}

#[allow(
    clippy::result_large_err,
    reason = "Pair request validation maps failures directly to HTTP responses."
)]
fn live_transport_for_pair_command(
    state: &AppState,
    id: &RunId,
) -> Result<Option<super::super::RunAnswerTransport>, Response> {
    let runs = state.runs.lock().expect("runs lock poisoned");
    let Some(run) = runs.get(id) else {
        return Err(ApiError::not_found("Run not found.").into_response());
    };
    reject_unpairable_status(run.status)?;
    Ok(run.answer_transport.clone())
}

#[allow(
    clippy::result_large_err,
    reason = "Pair request validation maps failures directly to HTTP responses."
)]
fn reject_unpairable_status(status: RunStatus) -> Result<(), Response> {
    match status {
        RunStatus::Running => Ok(()),
        RunStatus::Blocked { .. } => Err(pair_conflict(
            "Run is blocked on a question; answer it before pairing.",
            "run_not_pairable",
        )),
        RunStatus::Submitted
        | RunStatus::Pending { .. }
        | RunStatus::Runnable
        | RunStatus::Starting
        | RunStatus::Paused { .. }
        | RunStatus::Failed { .. }
        | RunStatus::Succeeded { .. }
        | RunStatus::Removing
        | RunStatus::Dead => Err(pair_conflict(
            "Run is not currently pairable.",
            "run_not_pairable",
        )),
    }
}

async fn current_pair(state: &AppState, id: &RunId) -> Option<Result<PairRecord, Response>> {
    match reconstruct_pairs(state, id).await {
        Ok(pairs) => pairs
            .values()
            .find(|pair| pair.status == PairStatus::Active)
            .cloned()
            .map(Ok),
        Err(response) => Some(Err(response)),
    }
}

async fn pair_by_id(state: &AppState, id: &RunId, pair_id: PairId) -> Result<PairRecord, Response> {
    pair_window_by_id(state, id, pair_id)
        .await
        .map(|window| window.record)
}

async fn wait_for_pair_record(
    state: &AppState,
    id: &RunId,
    pair_id: PairId,
    status: PairStatus,
    existing: Option<&PairRecord>,
) -> Result<PairRecord, Response> {
    let run_store = open_pair_run_reader(state, id).await?;
    let mut events = run_store.watch_events_from(1).map_err(|err| {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
    })?;

    match timeout(PAIR_CONFIRM_TIMEOUT, async {
        while let Some(envelope) = events.next().await {
            let envelope = envelope.map_err(|err| {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            })?;
            match &envelope.event.body {
                EventBody::RunPairStarted(props)
                    if status == PairStatus::Active && props.pair_id == pair_id =>
                {
                    return Ok(PairRecord {
                        pair_id:        props.pair_id,
                        run_id:         *id,
                        status:         PairStatus::Active,
                        started_at:     envelope.event.ts,
                        ended_at:       None,
                        failure_reason: None,
                        target:         props.target.clone(),
                    });
                }
                EventBody::RunPairEnded(props)
                    if status == PairStatus::Ended && props.pair_id == pair_id =>
                {
                    let Some(existing) = existing else {
                        continue;
                    };
                    let mut record = existing.clone();
                    record.status = PairStatus::Ended;
                    record.ended_at = Some(envelope.event.ts);
                    return Ok(record);
                }
                EventBody::RunPairFailed(props)
                    if status == PairStatus::Failed && props.pair_id == pair_id =>
                {
                    let Some(existing) = existing else {
                        continue;
                    };
                    let mut record = existing.clone();
                    record.status = PairStatus::Failed;
                    record.ended_at = Some(envelope.event.ts);
                    record.failure_reason = Some(props.message.clone());
                    return Ok(record);
                }
                _ => {}
            }
        }
        Err(worker_unavailable(
            "Worker control channel cannot confirm pair command.",
        ))
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(worker_unavailable(
            "Worker control channel cannot confirm pair command.",
        )),
    }
}

async fn wait_for_pair_message_record(
    state: &AppState,
    id: &RunId,
    pair: &PairWindow,
    message_id: PairMessageId,
) -> Result<PairMessageRecord, Response> {
    let run_store = open_pair_run_reader(state, id).await?;
    let mut events = run_store.watch_events_from(pair.start_seq).map_err(|err| {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
    })?;

    match timeout(PAIR_CONFIRM_TIMEOUT, async {
        while let Some(envelope) = events.next().await {
            let envelope = envelope.map_err(|err| {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            })?;
            if let EventBody::AgentPairUserMessage(props) = &envelope.event.body {
                if props.pair_id == pair.record.pair_id && props.message_id == message_id {
                    return Ok(PairMessageRecord {
                        message_id:        props.message_id,
                        client_message_id: props.client_message_id.clone(),
                        pair_id:           props.pair_id,
                        run_id:            *id,
                        stage_id:          pair.record.target.stage_id.clone(),
                        text:              props.text.clone(),
                        accepted_at:       envelope.event.ts,
                    });
                }
            }
        }
        Err(worker_unavailable(
            "Worker control channel cannot confirm pair message.",
        ))
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(worker_unavailable(
            "Worker control channel cannot confirm pair message.",
        )),
    }
}

#[derive(Debug, Clone)]
struct PairWindow {
    record:    PairRecord,
    start_seq: u32,
    end_seq:   Option<u32>,
}

struct TranscriptPage {
    entries:        Vec<PairTranscriptEntry>,
    next_since_seq: u32,
    has_more:       bool,
}

async fn pair_window_by_id(
    state: &AppState,
    id: &RunId,
    pair_id: PairId,
) -> Result<PairWindow, Response> {
    let pairs = reconstruct_pair_windows(state, id).await?;
    pairs.get(&pair_id).cloned().ok_or_else(|| {
        ApiError::with_code(StatusCode::NOT_FOUND, "Pair not found.", "pair_not_found")
            .into_response()
    })
}

async fn reconstruct_pairs(
    state: &AppState,
    id: &RunId,
) -> Result<HashMap<PairId, PairRecord>, Response> {
    Ok(reconstruct_pair_windows(state, id)
        .await?
        .into_iter()
        .map(|(pair_id, window)| (pair_id, window.record))
        .collect())
}

async fn reconstruct_pair_windows(
    state: &AppState,
    id: &RunId,
) -> Result<HashMap<PairId, PairWindow>, Response> {
    let events = list_all_events(state, id).await?;
    let mut pairs = HashMap::new();
    for envelope in events {
        match &envelope.event.body {
            EventBody::RunPairStarted(props) => {
                pairs.insert(props.pair_id, PairWindow {
                    record:    PairRecord {
                        pair_id:        props.pair_id,
                        run_id:         *id,
                        status:         PairStatus::Active,
                        started_at:     envelope.event.ts,
                        ended_at:       None,
                        failure_reason: None,
                        target:         props.target.clone(),
                    },
                    start_seq: envelope.seq,
                    end_seq:   None,
                });
            }
            EventBody::RunPairEnded(props) => {
                if let Some(pair) = pairs.get_mut(&props.pair_id) {
                    pair.record.status = PairStatus::Ended;
                    pair.record.ended_at = Some(envelope.event.ts);
                    pair.end_seq = Some(envelope.seq);
                }
            }
            EventBody::RunPairFailed(props) => {
                if let Some(pair) = pairs.get_mut(&props.pair_id) {
                    pair.record.status = PairStatus::Failed;
                    pair.record.ended_at = Some(envelope.event.ts);
                    pair.record.failure_reason = Some(props.message.clone());
                    pair.end_seq = Some(envelope.seq);
                }
            }
            _ => {}
        }
    }
    Ok(pairs)
}

async fn open_pair_run_reader(
    state: &AppState,
    id: &RunId,
) -> Result<fabro_store::RunDatabase, Response> {
    match state.store.open_run_reader(id).await {
        Ok(run_store) => Ok(run_store),
        Err(_) => match durable_run_status(state, *id).await {
            Ok(Some(_)) => Err(worker_unavailable(
                "Worker control channel cannot confirm pair command.",
            )),
            Ok(None) => Err(ApiError::not_found("Run not found.").into_response()),
            Err(err) => Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            ),
        },
    }
}

async fn list_all_events(state: &AppState, id: &RunId) -> Result<Vec<EventEnvelope>, Response> {
    match state.store.open_run_reader(id).await {
        Ok(run_store) => run_store.list_events().await.map_err(|err| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }),
        Err(_) => match durable_run_status(state, *id).await {
            Ok(Some(_)) => Ok(Vec::new()),
            Ok(None) => Err(ApiError::not_found("Run not found.").into_response()),
            Err(err) => Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            ),
        },
    }
}

async fn transcript_page(
    state: &AppState,
    id: &RunId,
    window: &PairWindow,
    since_seq: u32,
    limit: usize,
) -> Result<TranscriptPage, Response> {
    match state.store.open_run_reader(id).await {
        Ok(run_store) => {
            let mut next_seq = since_seq.max(window.start_seq);
            let mut highest_scanned_seq = since_seq.saturating_sub(1);
            let mut entries = Vec::new();
            loop {
                let batch_limit = limit.max(256);
                let batch = run_store
                    .list_events_for_stage_from_with_limit(
                        &window.record.target.stage_id,
                        next_seq,
                        batch_limit,
                    )
                    .await
                    .map_err(|err| {
                        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                            .into_response()
                    })?;
                if batch.is_empty() {
                    break;
                }
                let batch_has_more = batch.len() > batch_limit;

                for envelope in batch {
                    next_seq = envelope.seq.saturating_add(1);
                    if envelope.seq < window.start_seq
                        || window.end_seq.is_some_and(|end| envelope.seq > end)
                    {
                        highest_scanned_seq = highest_scanned_seq.max(envelope.seq);
                        continue;
                    }
                    if let Some(entry) = transcript_entry_from_event(&window.record, &envelope) {
                        if entries.len() >= limit {
                            return Ok(TranscriptPage {
                                entries,
                                next_since_seq: highest_scanned_seq.saturating_add(1),
                                has_more: true,
                            });
                        }
                        entries.push(entry);
                    }
                    highest_scanned_seq = highest_scanned_seq.max(envelope.seq);
                }

                if !batch_has_more {
                    break;
                }
            }
            Ok(TranscriptPage {
                entries,
                next_since_seq: highest_scanned_seq.saturating_add(1),
                has_more: false,
            })
        }
        Err(_) => match durable_run_status(state, *id).await {
            Ok(Some(_)) => Ok(TranscriptPage {
                entries:        Vec::new(),
                next_since_seq: since_seq,
                has_more:       false,
            }),
            Ok(None) => Err(ApiError::not_found("Run not found.").into_response()),
            Err(err) => Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            ),
        },
    }
}

#[allow(
    clippy::result_large_err,
    reason = "Pair path parsing maps failures directly to HTTP responses."
)]
fn parse_pair_id(raw: &str) -> Result<PairId, Response> {
    raw.parse()
        .map_err(|_| ApiError::bad_request("Invalid pair_id.").into_response())
}

fn pair_transport_error_response(err: PairTransportError) -> Response {
    match err {
        PairTransportError::Closed | PairTransportError::Timeout => {
            worker_unavailable("Worker control channel cannot confirm pair command.")
        }
        PairTransportError::Control(fabro_workflow::PairControlError::AlreadyPaired) => {
            pair_conflict("Run already has an active pair.", "already_paired")
        }
        PairTransportError::Control(fabro_workflow::PairControlError::PairNotCurrent) => {
            pair_conflict("Pair is not the current active pair.", "pair_not_current")
        }
        PairTransportError::Control(fabro_workflow::PairControlError::PairNotActive) => {
            pair_conflict("Pair is not active.", "pair_not_active")
        }
        PairTransportError::Control(fabro_workflow::PairControlError::TargetNotActive) => {
            pair_conflict("Pair target is not active.", "pair_target_not_active")
        }
        PairTransportError::Control(fabro_workflow::PairControlError::MessageNotAccepted) => {
            pair_conflict(
                "Pair message was not accepted.",
                "pair_message_not_accepted",
            )
        }
    }
}

fn pair_conflict(message: &str, code: &str) -> Response {
    ApiError::with_code(StatusCode::CONFLICT, message, code).into_response()
}

fn worker_unavailable(message: &str) -> Response {
    ApiError::with_code(
        StatusCode::SERVICE_UNAVAILABLE,
        message,
        "worker_control_unavailable",
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::{TimeZone, Utc};
    use fabro_model::{ModelRef, ProviderId};
    use fabro_types::run_event::AgentMessageProps;
    use fabro_types::{
        BilledTokenCounts, EventEnvelope, Graph, PairMessageId, RunEvent, StageId,
        WorkflowSettings, fixtures,
    };
    use fabro_workflow::event as workflow_event;
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::{build_test_router, test_app_state};

    #[test]
    fn transcript_projection_matches_by_stage_id() {
        let pair = PairRecord {
            pair_id:        "01HZX6M29F1CD5YYMHT1F5D7WQ".parse().unwrap(),
            run_id:         fixtures::RUN_1,
            status:         PairStatus::Active,
            started_at:     Utc.with_ymd_and_hms(2026, 5, 18, 12, 0, 0).unwrap(),
            ended_at:       None,
            failure_reason: None,
            target:         PairTarget {
                stage_id:   StageId::new("code", 1),
                node_label: "Code".to_string(),
            },
        };

        let entry = transcript_entry_from_event(
            &pair,
            &envelope(
                7,
                Some("ses_01"),
                Some(StageId::new("code", 1)),
                EventBody::AgentMessage(AgentMessageProps {
                    text:            "I found the issue.".to_string(),
                    model:           ModelRef {
                        provider: ProviderId::new("openai"),
                        model_id: "gpt-5.4".to_string(),
                        speed:    None,
                    },
                    billing:         BilledTokenCounts::default(),
                    tool_call_count: 0,
                    visit:           1,
                    message:         None,
                    context_window:  None,
                }),
            ),
        )
        .unwrap();

        assert!(matches!(
            &entry,
            PairTranscriptEntry::AssistantMessage(PairTranscriptAssistantMessage {
                text,
                ..
            }) if text == "I found the issue."
        ));

        assert!(
            transcript_entry_from_event(
                &pair,
                &envelope(
                    8,
                    Some("ses_01"),
                    Some(StageId::new("other", 1)),
                    EventBody::AgentMessage(AgentMessageProps {
                        text:            "wrong stage".to_string(),
                        model:           ModelRef {
                            provider: ProviderId::new("openai"),
                            model_id: "gpt-5.4".to_string(),
                            speed:    None,
                        },
                        billing:         BilledTokenCounts::default(),
                        tool_call_count: 0,
                        visit:           1,
                        message:         None,
                        context_window:  None,
                    }),
                ),
            )
            .is_none()
        );

        let serialized = serde_json::to_value(&entry).unwrap();
        let serialized_text = serialized.to_string();
        assert!(!serialized_text.contains("agent_session_id"));
        assert!(!serialized_text.contains("provider"));
        assert!(!serialized_text.contains("\"model\""));
    }

    #[tokio::test]
    async fn transcript_cursor_does_not_skip_lookahead_entry() {
        let state = test_app_state();
        let app = build_test_router(state.clone());
        let run_id = RunId::new();
        let pair_id = PairId::new();
        let target = PairTarget {
            stage_id:   StageId::new("code", 1),
            node_label: "Code".to_string(),
        };
        let run_store = state
            .store_ref()
            .create_run(&run_id)
            .await
            .expect("test run should be creatable");
        append_run_created(&run_store, run_id).await;
        workflow_event::append_event(
            &run_store,
            &run_id,
            &workflow_event::Event::RunPairStarted {
                pair_id,
                target: target.clone(),
                actor: None,
            },
        )
        .await
        .expect("run.pair.started should append");
        for text in ["first", "second"] {
            workflow_event::append_event(
                &run_store,
                &run_id,
                &workflow_event::Event::AgentPairUserMessage {
                    node_id: target.stage_id.node_id().to_string(),
                    visit: target.stage_id.visit(),
                    session_id: "ses_01".to_string(),
                    pair_id,
                    message_id: PairMessageId::new(),
                    client_message_id: None,
                    text: text.to_string(),
                    actor: None,
                },
            )
            .await
            .expect("pair message should append");
        }

        let first_page = app
            .clone()
            .oneshot(get(&format!(
                "/api/v1/runs/{run_id}/pair/{pair_id}/transcript?limit=1"
            )))
            .await
            .expect("first transcript request should complete");
        let first_page =
            fabro_test::expect_axum_json(first_page, StatusCode::OK, "GET pair transcript page 1")
                .await;
        assert_eq!(transcript_texts(&first_page), vec!["first"]);
        assert_eq!(first_page["meta"]["has_more"], true);

        let next_since_seq = first_page["meta"]["next_since_seq"]
            .as_u64()
            .expect("next_since_seq should be a number");
        let second_page = app
            .oneshot(get(&format!(
                "/api/v1/runs/{run_id}/pair/{pair_id}/transcript?limit=1&since_seq={next_since_seq}"
            )))
            .await
            .expect("second transcript request should complete");
        let second_page =
            fabro_test::expect_axum_json(second_page, StatusCode::OK, "GET pair transcript page 2")
                .await;
        assert_eq!(transcript_texts(&second_page), vec!["second"]);
    }

    async fn append_run_created(run_store: &fabro_store::RunDatabase, run_id: RunId) {
        workflow_event::append_event(run_store, &run_id, &workflow_event::Event::RunCreated {
            run_id,
            title: None,
            settings: serde_json::to_value(WorkflowSettings::default()).unwrap(),
            graph: serde_json::to_value(Graph::new("test")).unwrap(),
            workflow_source: None,
            workflow_config: None,
            labels: std::collections::BTreeMap::new(),
            run_dir: "/tmp/test".to_string(),
            source_directory: None,
            workflow_slug: None,
            automation: None,
            db_prefix: None,
            provenance: None,
            manifest_blob: None,
            git: None,
            fork_source_ref: None,
            retried_from: None,
            parent_id: None,
            web_url: None,
        })
        .await
        .expect("run.created should append");
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("GET request should build")
    }

    fn transcript_texts(body: &serde_json::Value) -> Vec<&str> {
        body["data"]
            .as_array()
            .expect("data should be an array")
            .iter()
            .map(|entry| entry["text"].as_str().expect("entry should have text"))
            .collect()
    }

    fn envelope(
        seq: u32,
        session_id: Option<&str>,
        stage_id: Option<StageId>,
        body: EventBody,
    ) -> EventEnvelope {
        EventEnvelope {
            seq,
            event: RunEvent {
                id: format!("evt_{seq}"),
                ts: Utc.with_ymd_and_hms(2026, 5, 18, 12, 0, 0).unwrap(),
                run_id: fixtures::RUN_1,
                node_id: Some("code".to_string()),
                node_label: Some("Code".to_string()),
                stage_id,
                parallel_group_id: None,
                parallel_branch_id: None,
                session_id: session_id.map(str::to_string),
                parent_session_id: None,
                tool_call_id: None,
                actor: None,
                body,
            },
        }
    }
}
