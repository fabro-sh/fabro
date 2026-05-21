use std::sync::Arc;

use fabro_types::{
    RunEventDetailContent, RunEventDetailContentKind, RunEventDetailEnvelope,
    RunEventDetailResponse,
};
use fabro_workflow::event::build_redacted_event_payload;

use super::super::{
    ApiError, AppState, AppendEventResponse, BroadcastStream, Event, EventBody, EventEnvelope,
    EventPayload, HashSet, IntoResponse, Json, KeepAlive, PaginatedEventList, PaginationMeta, Path,
    Query, RequireRunScoped, RequireRunScopedOrRunTools, RequireRunStageScoped, RequiredUser,
    Response, Router, RunEvent, RunId, Sse, State, StatusCode, StreamExt, UnboundedReceiverStream,
    broadcast, get, mpsc, parse_run_id_path, parse_stage_id_path, redact_jsonl_line,
    reject_if_archived, update_live_run_from_event,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/attach", get(attach_events))
        .route(
            "/runs/{id}/events",
            get(list_run_events).post(append_run_event),
        )
        .route("/runs/{id}/events/{seq}", get(get_run_event_detail))
        .route(
            "/runs/{id}/stages/{stageId}/events",
            get(list_run_stage_events),
        )
        .route("/runs/{id}/attach", get(attach_run_events))
}

#[derive(serde::Deserialize)]
pub(crate) struct EventListParams {
    #[serde(default)]
    since_seq: Option<u32>,
    #[serde(default)]
    limit:     Option<usize>,
}

impl EventListParams {
    pub(crate) fn since_seq(&self) -> u32 {
        self.since_seq.unwrap_or(1).max(1)
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 1000)
    }
}

#[derive(serde::Deserialize)]
struct AttachParams {
    #[serde(default)]
    since_seq: Option<u32>,
}

#[derive(serde::Deserialize)]
struct EventDetailParams {
    #[serde(default)]
    max_content_length: Option<usize>,
}

impl EventDetailParams {
    fn max_content_length(&self) -> usize {
        self.max_content_length.unwrap_or(20_000).clamp(1, 200_000)
    }
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
    let stream =
        futures_util::StreamExt::take_until(stream, state.shutdown_token().cancelled_owned());

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
    state.status.is_active()
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
    RequireRunScopedOrRunTools(id, _actor): RequireRunScopedOrRunTools,
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

async fn list_run_stage_events(
    RequireRunStageScoped(id, stage_id): RequireRunStageScoped,
    State(state): State<Arc<AppState>>,
    Query(params): Query<EventListParams>,
) -> Response {
    let stage_id = match parse_stage_id_path(&stage_id) {
        Ok(stage_id) => stage_id,
        Err(response) => return response,
    };
    let since_seq = params.since_seq();
    let limit = params.limit();
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store
            .list_events_for_stage_from_with_limit(&stage_id, since_seq, limit)
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

async fn get_run_event_detail(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Path((_id, seq)): Path<(String, u32)>,
    Query(params): Query<EventDetailParams>,
) -> Response {
    let max_content_length = params.max_content_length();
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.get_event(seq).await {
            Ok(event) => {
                let Some(envelope) = event else {
                    return ApiError::with_code(
                        StatusCode::NOT_FOUND,
                        "Event not found.",
                        "event_not_found",
                    )
                    .into_response();
                };
                Json(detail_response(envelope, max_content_length)).into_response()
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

fn detail_response(envelope: EventEnvelope, max_content_length: usize) -> RunEventDetailResponse {
    let raw_properties = event_properties(&envelope.event);
    let redacted_properties = redacted_event_properties(&envelope.event);
    let redacted = raw_properties != redacted_properties;
    let mut properties = redacted_properties;
    let event_name = envelope.event.event_name().to_string();
    let mut content = None;
    let mut truncated = false;

    for (key, kind) in [
        ("text", RunEventDetailContentKind::Text),
        ("output", RunEventDetailContentKind::ToolOutput),
        ("arguments", RunEventDetailContentKind::ToolArguments),
        ("error", RunEventDetailContentKind::Error),
        ("details", RunEventDetailContentKind::Details),
    ] {
        if let Some(value) = properties.remove(key) {
            let raw = match value {
                serde_json::Value::String(value) => value,
                other => serde_json::to_string(&other).unwrap_or_else(|_| String::new()),
            };
            let (value, was_truncated) = truncate_content(raw, max_content_length);
            truncated = truncated || was_truncated;
            content = Some(RunEventDetailContent { kind, value });
            break;
        }
    }

    RunEventDetailResponse {
        event: RunEventDetailEnvelope {
            seq:          envelope.seq,
            id:           envelope.event.id,
            ts:           envelope.event.ts,
            run_id:       envelope.event.run_id,
            event:        event_name,
            actor:        envelope.event.actor,
            session_id:   envelope.event.session_id,
            node_id:      envelope.event.node_id,
            node_label:   envelope.event.node_label,
            stage_id:     envelope.event.stage_id,
            tool_call_id: envelope.event.tool_call_id,
        },
        properties,
        content,
        truncated,
        redacted,
        max_content_length,
    }
}

fn event_properties(event: &RunEvent) -> serde_json::Map<String, serde_json::Value> {
    event
        .properties()
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn redacted_event_properties(event: &RunEvent) -> serde_json::Map<String, serde_json::Value> {
    build_redacted_event_payload(event, &event.run_id)
        .ok()
        .and_then(|payload| {
            payload
                .as_value()
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .cloned()
        })
        .unwrap_or_else(|| event_properties(event))
}

fn truncate_content(value: String, max_content_length: usize) -> (String, bool) {
    if value.len() <= max_content_length {
        return (value, false);
    }
    let end = value.floor_char_boundary(max_content_length);
    (value[..end].to_string(), true)
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
    let shutdown = state.shutdown_token();
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

        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                next = live_stream.next() => {
                    let Some(result) = next else {
                        return;
                    };
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
fn denied_lifecycle_event_name(body: &EventBody) -> Option<&str> {
    match body {
        EventBody::RunArchived(_)
        | EventBody::RunUnarchived(_)
        | EventBody::RunTitleUpdated(_)
        | EventBody::RunCancelRequested(_)
        | EventBody::RunPauseRequested(_)
        | EventBody::RunUnpauseRequested(_)
        | EventBody::PullRequestLinked(_)
        | EventBody::PullRequestUnlinked(_) => Some(body.event_name()),
        _ => None,
    }
}

#[cfg(test)]
mod stage_events_tests {
    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use fabro_store::EventPayload;
    use fabro_types::{Graph, RunId, WorkflowSettings};
    use fabro_workflow::event as workflow_event;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tokio::time::timeout;
    use tower::ServiceExt;

    use crate::test_support::{build_test_router, test_app_state};

    fn req_get(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("stage events GET request should build")
    }

    fn make_event(run_id: &RunId, idx: u32, node_id: Option<&str>) -> EventPayload {
        make_event_with_stage_id(run_id, idx, node_id, None)
    }

    async fn append_run_created(run_store: &fabro_store::RunDatabase, run_id: &RunId) {
        workflow_event::append_event(run_store, run_id, &workflow_event::Event::RunCreated {
            run_id:           *run_id,
            title:            None,
            settings:         serde_json::to_value(WorkflowSettings::default()).unwrap(),
            graph:            serde_json::to_value(Graph::new("test")).unwrap(),
            workflow_source:  None,
            workflow_config:  None,
            labels:           std::collections::BTreeMap::new(),
            run_dir:          "/tmp/test".to_string(),
            source_directory: None,
            workflow_slug:    None,
            db_prefix:        None,
            provenance:       None,
            manifest_blob:    None,
            git:              None,
            fork_source_ref:  None,
            parent_id:        None,
            web_url:          None,
        })
        .await
        .expect("run.created should append");
    }

    fn make_event_with_stage_id(
        run_id: &RunId,
        idx: u32,
        node_id: Option<&str>,
        stage_id: Option<&str>,
    ) -> EventPayload {
        let mut value = json!({
            "id": format!("evt-{idx}"),
            "ts": "2026-04-09T12:00:00Z",
            "run_id": run_id.to_string(),
            "event": "stage.prompt",
            "properties": {
                "visit": 1,
                "text": format!("prompt {idx}"),
            },
        });
        if let Some(node) = node_id {
            value
                .as_object_mut()
                .unwrap()
                .insert("node_id".into(), json!(node));
        }
        if let Some(stage_id) = stage_id {
            value
                .as_object_mut()
                .unwrap()
                .insert("stage_id".into(), json!(stage_id));
        }
        EventPayload::new(value, run_id).expect("event payload should validate")
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should fit in memory");
        serde_json::from_slice(&bytes).expect("response body should be valid JSON")
    }

    fn assert_event_stream_response(response: &axum::response::Response) {
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("SSE response should set content-type")
            .to_str()
            .expect("content-type should be valid UTF-8");
        assert!(
            content_type.contains("text/event-stream"),
            "expected text/event-stream content-type, got {content_type:?}"
        );
    }

    async fn assert_sse_body_is_live(body: &mut Body) {
        let result = timeout(Duration::from_millis(100), body.frame()).await;
        assert!(
            result.is_err(),
            "SSE body should remain open before shutdown cancellation"
        );
    }

    async fn assert_sse_body_completes_after_shutdown(mut body: Body) {
        timeout(Duration::from_secs(1), async {
            while let Some(frame) = body.frame().await {
                frame.expect("SSE body frame should be readable");
            }
        })
        .await
        .expect("SSE body should complete promptly after shutdown cancellation");
    }

    #[tokio::test]
    async fn attach_events_ends_when_shutdown_fires() {
        let state = test_app_state();
        let app = build_test_router(state.clone());

        let response = app
            .oneshot(req_get("/api/v1/attach"))
            .await
            .expect("attach request should complete");
        assert_event_stream_response(&response);

        let mut body = response.into_body();
        assert_sse_body_is_live(&mut body).await;

        state.shutdown_token().cancel();

        assert_sse_body_completes_after_shutdown(body).await;
    }

    #[tokio::test]
    async fn attach_run_events_ends_when_shutdown_fires() {
        let state = test_app_state();
        let app = build_test_router(state.clone());
        let run_id = RunId::new();
        let run_store = state
            .store_ref()
            .create_run(&run_id)
            .await
            .expect("test run should be creatable");
        append_run_created(&run_store, &run_id).await;
        for event in [
            workflow_event::Event::RunSubmitted {
                definition_blob: None,
            },
            workflow_event::Event::RunStarting,
            workflow_event::Event::RunRunning,
        ] {
            workflow_event::append_event(&run_store, &run_id, &event)
                .await
                .expect("run lifecycle event should append");
        }

        let response = app
            .oneshot(req_get(&format!("/api/v1/runs/{run_id}/attach")))
            .await
            .expect("run attach request should complete");
        assert_event_stream_response(&response);

        let mut body = response.into_body();
        assert_sse_body_is_live(&mut body).await;

        state.shutdown_token().cancel();

        assert_sse_body_completes_after_shutdown(body).await;
    }

    async fn seed_run_with_mixed_events() -> (RunId, axum::Router) {
        let state = test_app_state();
        let app = build_test_router(state.clone());
        let run_id = RunId::new();
        let run_store = state
            .store_ref()
            .create_run(&run_id)
            .await
            .expect("test run should be creatable");
        append_run_created(&run_store, &run_id).await;

        // Seed 200 unrelated 'beta' events first so any node-blind
        // truncation would lose the sparse 'alpha' tail. Then 3 'alpha'
        // events past seq 100, plus a couple with no node_id at all.
        for idx in 1..=200_u32 {
            run_store
                .append_event(&make_event(&run_id, idx, Some("beta")))
                .await
                .expect("append should succeed");
        }
        run_store
            .append_event(&make_event(&run_id, 201, None))
            .await
            .expect("append should succeed");
        for idx in 202..=204_u32 {
            run_store
                .append_event(&make_event(&run_id, idx, Some("alpha")))
                .await
                .expect("append should succeed");
        }

        (run_id, app)
    }

    #[tokio::test]
    async fn returns_only_matching_node_events_in_seq_order() {
        let (run_id, app) = seed_run_with_mixed_events().await;
        let response = app
            .oneshot(req_get(&format!(
                "/api/v1/runs/{run_id}/stages/alpha@1/events"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response).await;
        let data = body["data"].as_array().expect("data is array");
        let seqs: Vec<u64> = data.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, vec![203, 204, 205]);
        assert_eq!(body["meta"]["has_more"], false);
    }

    #[tokio::test]
    async fn since_seq_filters_to_events_with_seq_at_least_k() {
        let (run_id, app) = seed_run_with_mixed_events().await;
        let response = app
            .oneshot(req_get(&format!(
                "/api/v1/runs/{run_id}/stages/alpha@1/events?since_seq=204"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response).await;
        let seqs: Vec<u64> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![204, 205]);
    }

    #[tokio::test]
    async fn limit_one_returns_first_envelope_with_has_more_true() {
        let (run_id, app) = seed_run_with_mixed_events().await;
        let response = app
            .oneshot(req_get(&format!(
                "/api/v1/runs/{run_id}/stages/alpha@1/events?limit=1"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response).await;
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["seq"].as_u64().unwrap(), 203);
        assert_eq!(body["meta"]["has_more"], true);
    }

    #[tokio::test]
    async fn unknown_stage_in_existing_run_returns_empty_list_with_no_more() {
        let (run_id, app) = seed_run_with_mixed_events().await;
        let response = app
            .oneshot(req_get(&format!(
                "/api/v1/runs/{run_id}/stages/unknown-stage@1/events"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response).await;
        assert_eq!(body["data"].as_array().unwrap().len(), 0);
        assert_eq!(body["meta"]["has_more"], false);
    }

    #[tokio::test]
    async fn missing_run_returns_404_with_run_not_found() {
        let app = build_test_router(test_app_state());
        // A syntactically valid RunId that the store has never seen, so
        // `parse_run_id_path` succeeds but `open_run_reader` fails — that
        // exercises the handler's not-found branch rather than the path
        // parser's 400 branch.
        let absent = RunId::new();
        let response = app
            .oneshot(req_get(&format!(
                "/api/v1/runs/{absent}/stages/alpha@1/events"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = body_json(response).await;
        let detail = body["errors"][0]["detail"]
            .as_str()
            .expect("error detail string");
        assert!(
            detail.contains("Run not found."),
            "unexpected error body: {body}"
        );
    }

    #[tokio::test]
    async fn unauthenticated_request_is_rejected() {
        let state = test_app_state();
        // Bypass `build_test_router`'s auto-injected bearer token by
        // building the raw router directly. The principal middleware sees
        // a missing Authorization header and the extractor enforces auth.
        let app = crate::server::build_router(state, crate::test_support::test_auth_mode());
        let run_id = RunId::new();

        let request = Request::builder()
            .method("GET")
            .uri(format!("/api/v1/runs/{run_id}/stages/alpha@1/events"))
            .header(header::ACCEPT, "application/json")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn returns_only_requested_visit_when_stage_id_is_present() {
        let state = test_app_state();
        let app = build_test_router(state.clone());
        let run_id = RunId::new();
        let run_store = state
            .store_ref()
            .create_run(&run_id)
            .await
            .expect("test run should be creatable");
        append_run_created(&run_store, &run_id).await;
        run_store
            .append_event(&make_event_with_stage_id(
                &run_id,
                1,
                Some("verify"),
                Some("verify@1"),
            ))
            .await
            .expect("append should succeed");
        run_store
            .append_event(&make_event_with_stage_id(
                &run_id,
                2,
                Some("verify"),
                Some("verify@2"),
            ))
            .await
            .expect("append should succeed");

        let response = app
            .oneshot(req_get(&format!(
                "/api/v1/runs/{run_id}/stages/verify@2/events"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response).await;
        let seqs: Vec<u64> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![3]);
    }
}
