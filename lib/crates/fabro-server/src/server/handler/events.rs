use std::sync::Arc;

use super::super::{
    ApiError, AppState, AppendEventResponse, BroadcastStream, Event, EventBody, EventEnvelope,
    EventPayload, HashSet, IntoResponse, Json, KeepAlive, PaginatedEventList, PaginationMeta, Path,
    Query, RequireRunScoped, RequiredUser, Response, Router, RunEvent, RunId, RunStatus, Sse,
    State, StatusCode, StreamExt, UnboundedReceiverStream, broadcast, get, mpsc, parse_run_id_path,
    redact_jsonl_line, reject_if_archived, update_live_run_from_event,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/attach", get(attach_events))
        .route(
            "/runs/{id}/events",
            get(list_run_events).post(append_run_event),
        )
        .route("/runs/{id}/attach", get(attach_run_events))
}

#[derive(serde::Deserialize)]
struct EventListParams {
    #[serde(default)]
    since_seq: Option<u32>,
    #[serde(default)]
    limit:     Option<usize>,
}

impl EventListParams {
    fn since_seq(&self) -> u32 {
        self.since_seq.unwrap_or(1).max(1)
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 1000)
    }
}

#[derive(serde::Deserialize)]
struct AttachParams {
    #[serde(default)]
    since_seq: Option<u32>,
}

#[derive(serde::Deserialize)]
struct GlobalAttachParams {
    #[serde(default)]
    run_id: Option<String>,
}

async fn attach_events(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<GlobalAttachParams>,
) -> Response {
    let run_filter = match parse_global_run_filter(params.run_id.as_deref()) {
        Ok(filter) => filter,
        Err(err) => return ApiError::new(StatusCode::BAD_REQUEST, err).into_response(),
    };

    let stream =
        filtered_global_events(state.global_event_tx.subscribe(), run_filter).filter_map(|event| {
            sse_event_from_store(&event).map(Ok::<Event, std::convert::Infallible>)
        });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub(in crate::server) fn filtered_global_events(
    event_rx: broadcast::Receiver<EventEnvelope>,
    run_filter: Option<HashSet<RunId>>,
) -> impl tokio_stream::Stream<Item = EventEnvelope> {
    BroadcastStream::new(event_rx).filter_map(move |result| match result {
        Ok(event) if event_matches_run_filter(&event, run_filter.as_ref()) => Some(event),
        Ok(_) | Err(_) => None,
    })
}

fn parse_global_run_filter(raw: Option<&str>) -> Result<Option<HashSet<RunId>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };

    let mut run_ids = HashSet::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let run_id = part
            .parse::<RunId>()
            .map_err(|err| format!("invalid run_id '{part}': {err}"))?;
        run_ids.insert(run_id);
    }

    if run_ids.is_empty() {
        Ok(None)
    } else {
        Ok(Some(run_ids))
    }
}

fn event_matches_run_filter(event: &EventEnvelope, run_filter: Option<&HashSet<RunId>>) -> bool {
    let Some(run_filter) = run_filter else {
        return true;
    };
    run_filter.contains(&event.event.run_id)
}

fn sse_event_from_store(event: &EventEnvelope) -> Option<Event> {
    let data = serde_json::to_string(event).ok()?;
    let data = redact_jsonl_line(&data);
    Some(Event::default().data(data))
}

fn attach_event_is_terminal(event: &EventEnvelope) -> bool {
    matches!(
        &event.event.body,
        EventBody::RunCompleted(_) | EventBody::RunFailed(_)
    )
}

fn run_projection_is_active(state: &fabro_store::RunProjection) -> bool {
    state.status.is_some_and(RunStatus::is_active)
}

async fn append_run_event(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Json(value): Json<serde_json::Value>,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let event = match RunEvent::from_value(value.clone()) {
        Ok(event) => event,
        Err(err) => {
            return ApiError::bad_request(format!("Invalid run event: {err}")).into_response();
        }
    };
    if event.run_id != id {
        return ApiError::bad_request("Event run_id does not match path run ID.").into_response();
    }
    if let Some(denied) = denied_lifecycle_event_name(&event.body) {
        return ApiError::bad_request(format!(
            "{denied} is a lifecycle event; clients must call the corresponding operation endpoint instead of injecting it via append_run_event"
        ))
        .into_response();
    }
    let payload = match EventPayload::new(value, &id) {
        Ok(payload) => payload,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };

    match state.store.open_run(&id).await {
        Ok(run_store) => match run_store.append_event(&payload).await {
            Ok(seq) => {
                update_live_run_from_event(&state, id, &event);
                Json(AppendEventResponse {
                    seq: i64::from(seq),
                })
                .into_response()
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn list_run_events(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Query(params): Query<EventListParams>,
) -> Response {
    let since_seq = params.since_seq();
    let limit = params.limit();
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store
            .list_events_from_with_limit(since_seq, limit)
            .await
        {
            Ok(mut events) => {
                let has_more = events.len() > limit;
                events.truncate(limit);
                Json(PaginatedEventList {
                    data: events,
                    meta: PaginationMeta { has_more },
                })
                .into_response()
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn attach_run_events(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<AttachParams>,
) -> Response {
    const ATTACH_REPLAY_BATCH_LIMIT: usize = 256;

    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let Ok(run_store) = state.store.open_run_reader(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let start_seq = match params.since_seq {
        Some(seq) if seq >= 1 => seq,
        Some(_) => 1,
        None => match run_store.list_events().await {
            Ok(events) => events.last().map_or(1, |event| event.seq.saturating_add(1)),
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        },
    };
    let (sender, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut next_seq = start_seq;

        loop {
            let Ok(replay_batch) = run_store
                .list_events_from_with_limit(next_seq, ATTACH_REPLAY_BATCH_LIMIT)
                .await
            else {
                return;
            };
            let replay_has_more = replay_batch.len() > ATTACH_REPLAY_BATCH_LIMIT;

            for event in replay_batch.into_iter().take(ATTACH_REPLAY_BATCH_LIMIT) {
                next_seq = event.seq.saturating_add(1);
                let terminal = attach_event_is_terminal(&event);
                if let Some(sse_event) = sse_event_from_store(&event) {
                    if sender
                        .send(Ok::<Event, std::convert::Infallible>(sse_event))
                        .is_err()
                    {
                        return;
                    }
                }
                if terminal {
                    return;
                }
            }

            if replay_has_more {
                continue;
            }

            let Ok(state) = run_store.state().await else {
                return;
            };

            if run_projection_is_active(&state) {
                break;
            }

            let Ok(tail_batch) = run_store
                .list_events_from_with_limit(next_seq, ATTACH_REPLAY_BATCH_LIMIT)
                .await
            else {
                return;
            };
            let tail_has_more = tail_batch.len() > ATTACH_REPLAY_BATCH_LIMIT;

            for event in tail_batch.into_iter().take(ATTACH_REPLAY_BATCH_LIMIT) {
                next_seq = event.seq.saturating_add(1);
                let terminal = attach_event_is_terminal(&event);
                if let Some(sse_event) = sse_event_from_store(&event) {
                    if sender
                        .send(Ok::<Event, std::convert::Infallible>(sse_event))
                        .is_err()
                    {
                        return;
                    }
                }
                if terminal {
                    return;
                }
            }

            if tail_has_more {
                continue;
            }

            return;
        }

        let Ok(mut live_stream) = run_store.watch_events_from(next_seq) else {
            return;
        };

        while let Some(result) = live_stream.next().await {
            let Ok(event) = result else {
                return;
            };
            let terminal = attach_event_is_terminal(&event);
            if let Some(sse_event) = sse_event_from_store(&event) {
                if sender
                    .send(Ok::<Event, std::convert::Infallible>(sse_event))
                    .is_err()
                {
                    return;
                }
            }
            if terminal {
                return;
            }
        }
    });

    Sse::new(UnboundedReceiverStream::new(receiver))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Returns the wire event name if the given body has a dedicated operation
/// endpoint that clients must use instead of injecting via `append_run_event`.
/// These endpoints enforce authorization and status-transition preconditions
/// (e.g. "archive only from terminal") that a direct event append would
/// bypass. Other run-lifecycle events flow through this endpoint legitimately:
/// the worker subprocess emits state transitions during execution.
fn denied_lifecycle_event_name(body: &EventBody) -> Option<&'static str> {
    match body {
        EventBody::RunArchived(_) => Some("run.archived"),
        EventBody::RunUnarchived(_) => Some("run.unarchived"),
        EventBody::RunCancelRequested(_) => Some("run.cancel.requested"),
        EventBody::RunPauseRequested(_) => Some("run.pause.requested"),
        EventBody::RunUnpauseRequested(_) => Some("run.unpause.requested"),
        _ => None,
    }
}
