//! A run's stream: Petri's events and Fabro's platform records, one
//! `stream_seq` each, as the projector commits them. `GET /runs/{id}/events`
//! pages it by `after`, `GET /runs/{id}/attach` follows it live, and
//! `GET /attach` follows every run's stream at once.

use std::sync::Arc;
use std::time::Duration;

use fabro_api::types::PaginatedRunStreamList;
use fabro_petri::petri::EVENT_CONTRACT_VERSION;
use fabro_redact::redact_json_value;
use fabro_types::RunStreamItem;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{self, Instant};

use super::super::{
    ApiError, AppState, BroadcastStream, Event, HashSet, IntoResponse, Json, KeepAlive,
    PaginationMeta, Path, Query, RequireRunManagementTarget, RequiredUser, Response, Router, RunId,
    Sse, State, StatusCode, StreamExt, UnboundedReceiverStream, broadcast, get, mpsc,
    parse_run_id_path, redact_jsonl_line,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/attach", get(attach_events))
        .route("/runs/{id}/events", get(list_run_events))
        .route("/runs/{id}/attach", get(attach_run_events))
}

/// Query parameters for `/runs/{id}/events`: the stream cursor and a page
/// size.
#[derive(serde::Deserialize)]
struct RunEventListParams {
    #[serde(default)]
    limit: Option<usize>,
    /// The run stream cursor: the last `stream_seq` seen.
    #[serde(default)]
    after: Option<u64>,
}

impl RunEventListParams {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 1000)
    }
}

#[derive(serde::Deserialize)]
struct AttachParams {
    /// The run stream cursor: the last `stream_seq` seen.
    #[serde(default)]
    after: Option<u64>,
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
        filtered_global_events(state.global_event_tx.subscribe(), run_filter).filter_map(|item| {
            sse_event_from_stream_item(&item).map(Ok::<Event, std::convert::Infallible>)
        });
    let stream =
        futures_util::StreamExt::take_until(stream, state.shutdown_token().cancelled_owned());

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub(in crate::server) fn filtered_global_events(
    event_rx: broadcast::Receiver<RunStreamItem>,
    run_filter: Option<HashSet<RunId>>,
) -> impl tokio_stream::Stream<Item = RunStreamItem> {
    BroadcastStream::new(event_rx).filter_map(move |result| match result {
        Ok(item) if item_matches_run_filter(&item, run_filter.as_ref()) => Some(item),
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

fn item_matches_run_filter(item: &RunStreamItem, run_filter: Option<&HashSet<RunId>>) -> bool {
    let Some(run_filter) = run_filter else {
        return true;
    };
    run_filter.contains(&item.run_id)
}

fn run_projection_is_active(state: &fabro_store::RunProjection) -> bool {
    state.status.is_active()
}

async fn list_run_events(
    RequireRunManagementTarget(id, _actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    Query(params): Query<RunEventListParams>,
) -> Response {
    let limit = params.limit();
    if let Err(response) = ensure_run_exists(&state, &id).await {
        return response;
    }
    list_run_stream(&state, id, params.after.unwrap_or(0), limit).await
}

/// The canonical 404 when there is no such run.
async fn ensure_run_exists(state: &AppState, id: &RunId) -> Result<(), Response> {
    state
        .load_run_projection(id)
        .await
        .map(|_| ())
        .map_err(IntoResponse::into_response)
}

/// One page of a Petri run's stream past `after`.
async fn list_run_stream(state: &AppState, id: RunId, after: u64, limit: usize) -> Response {
    match state
        .petri_projector
        .stream_after(id, after, limit.saturating_add(1))
        .await
    {
        Ok(mut items) => {
            let has_more = items.len() > limit;
            items.truncate(limit);
            // The same items the attached stream serves, redacted the same
            // way, so a client that pages the listing after a stream sees
            // what the stream showed.
            for item in &mut items {
                item.item = redact_json_value(std::mem::take(&mut item.item));
            }
            Json(PaginatedRunStreamList {
                data:                   items,
                meta:                   PaginationMeta {
                    has_more,
                    total: None,
                },
                event_contract_version: EVENT_CONTRACT_VERSION,
            })
            .into_response()
        }
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

fn sse_event_from_stream_item(item: &RunStreamItem) -> Option<Event> {
    let data = serde_json::to_string(item).ok()?;
    let data = redact_jsonl_line(&data);
    Some(Event::default().data(data))
}

/// How many stream items one read takes while attached.
const STREAM_ATTACH_BATCH_LIMIT: usize = 256;

/// How long an attached reader waits for a commit signal before it re-reads
/// its cursor anyway: a signal is a wake-up, never the source of facts.
const STREAM_ATTACH_POLL: Duration = Duration::from_secs(1);

/// How long an attached reader keeps following a run whose projection is
/// already terminal, waiting for the platform record of the terminal
/// lifecycle transition that ends the stream; after that it ends anyway.
const STREAM_ATTACH_TERMINAL_GRACE: Duration = Duration::from_secs(15);

/// Whether the item ends an attached stream: the platform record of the
/// run's terminal lifecycle transition, which Fabro writes after the engine
/// recorded the run's finish. The analog of the legacy stream's
/// `run.completed` and `run.failed`.
fn stream_item_is_terminal(item: &RunStreamItem) -> bool {
    if item.kind != fabro_types::RunStreamItemKind::Platform {
        return false;
    }
    let record = &item.item["record"];
    record["kind"].as_str() == Some("run.lifecycle")
        && matches!(
            record["transition"].as_str(),
            Some("succeeded" | "failed" | "dead")
        )
}

/// The live stream of a Petri run from `after` (the last `stream_seq` the
/// client saw; `None` starts at the next unseen item), as server-sent
/// events. Every committed item past the cursor is sent once, in order,
/// and the stream ends once the run is no longer active and every
/// committed item is out.
async fn attach_run_stream(state: Arc<AppState>, id: RunId, after: Option<u64>) -> Response {
    let cursor = match after {
        Some(after) => after,
        None => match state.petri_projector.stream_head(id).await {
            Ok(head) => head.unwrap_or(0),
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        },
    };
    let (sender, receiver) = mpsc::unbounded_channel();
    let shutdown = state.shutdown_token();
    tokio::spawn(async move {
        // Subscribed before the first read, so a pass that commits between
        // the read and the wait is not missed.
        let mut committed = state.petri_projector.subscribe();
        let mut cursor = cursor;
        // Set once the projection is terminal: the stream then ends at the
        // terminal lifecycle record, or when the grace runs out.
        let mut terminal_deadline: Option<Instant> = None;
        loop {
            // Drain everything committed past the cursor.
            let mut drained = false;
            while !drained {
                let Ok(items) = state
                    .petri_projector
                    .stream_after(id, cursor, STREAM_ATTACH_BATCH_LIMIT)
                    .await
                else {
                    return;
                };
                drained = items.len() < STREAM_ATTACH_BATCH_LIMIT;
                for item in items {
                    cursor = item.stream_seq;
                    let terminal = stream_item_is_terminal(&item);
                    if let Some(sse_event) = sse_event_from_stream_item(&item) {
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
            }

            // The run's status is read after the drain, so an item
            // committed with the finish is already out. Once terminal, the
            // stream keeps following for the terminal lifecycle record,
            // which Fabro writes after the engine's finish, for a bounded
            // time.
            if terminal_deadline.is_none() {
                let active = match state.stores.runs.load_run_projection(&id).await {
                    Ok(Some(projection)) => run_projection_is_active(&projection),
                    Ok(None) | Err(_) => false,
                };
                if !active {
                    terminal_deadline = Some(Instant::now() + STREAM_ATTACH_TERMINAL_GRACE);
                }
            }
            if terminal_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return;
            }

            // Wait for the projector to commit more of this run, or poll.
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return,
                    signal = committed.recv() => match signal {
                        Ok(run_id) if run_id == id => break,
                        Ok(_) => {}
                        Err(RecvError::Lagged(_)) => break,
                        Err(RecvError::Closed) => return,
                    },
                    () = time::sleep(STREAM_ATTACH_POLL) => break,
                }
            }
        }
    });

    Sse::new(UnboundedReceiverStream::new(receiver))
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn attach_run_events(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<AttachParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match ensure_run_exists(&state, &id).await {
        Ok(()) => attach_run_stream(state, id, params.after).await,
        Err(response) => response,
    }
}
