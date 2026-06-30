use std::sync::Arc;

use axum::extract::ws::{
    CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade, close_code,
};
use fabro_interview::{
    WORKER_CONTROL_INVALID_CURSOR_REASON, WORKER_CONTROL_PONG_TIMEOUT_REASON,
    WORKER_CONTROL_WS_LIVENESS_TIMEOUT, WORKER_CONTROL_WS_PING_INTERVAL,
    WorkerControlDeliveryFrame,
};
use futures_util::{SinkExt, StreamExt};
use tokio::time::{self, Instant, MissedTickBehavior};

use super::super::{
    ApiError, AppState, IntoResponse, Query, RequireWorkerRunScoped, Response, Router, State,
    StatusCode, get,
};
use crate::worker_control::{WorkerControlBusError, WorkerControlCursor, WorkerControlReceiver};

#[derive(Debug, serde::Deserialize)]
struct WorkerControlStreamQuery {
    after: Option<String>,
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/runs/{id}/worker/control-stream",
        get(worker_control_stream),
    )
}

async fn worker_control_stream(
    RequireWorkerRunScoped(id): RequireWorkerRunScoped,
    State(state): State<Arc<AppState>>,
    Query(query): Query<WorkerControlStreamQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let cached = match state.stores.runs.get_cached_run(&id).await {
        Ok(Some(cached)) => cached,
        Ok(None) => return ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if cached.projection.archived_at.is_some() {
        return ApiError::new(StatusCode::CONFLICT, "Run is archived.").into_response();
    }
    if cached.projection.status.is_terminal() {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Run is terminal and cannot accept worker control streams.",
        )
        .into_response();
    }

    let cursor = match WorkerControlCursor::from_after_query(query.after.as_deref()) {
        Ok(cursor) => cursor,
        Err(err @ WorkerControlBusError::InvalidCursor { .. }) => {
            return ApiError::new(StatusCode::GONE, err.to_string()).into_response();
        }
        Err(err) => return worker_control_bus_error_response(&err),
    };
    let receiver = match state.worker_control_bus.subscribe(id, cursor).await {
        Ok(receiver) => receiver,
        Err(err @ WorkerControlBusError::InvalidCursor { .. }) => {
            return ApiError::new(StatusCode::GONE, err.to_string()).into_response();
        }
        Err(err) => return worker_control_bus_error_response(&err),
    };

    ws.on_upgrade(move |socket| worker_control_websocket(socket, receiver))
}

fn worker_control_bus_error_response(err: &WorkerControlBusError) -> Response {
    let status = match err {
        WorkerControlBusError::Closed | WorkerControlBusError::Unavailable => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        WorkerControlBusError::PublishTimeout => StatusCode::SERVICE_UNAVAILABLE,
        WorkerControlBusError::InvalidCursor { .. } => StatusCode::GONE,
    };
    ApiError::new(status, err.to_string()).into_response()
}

async fn worker_control_websocket(socket: WebSocket, mut receiver: WorkerControlReceiver) {
    let (mut sender, mut receiver_ws) = socket.split();
    let mut ping_interval = time::interval(WORKER_CONTROL_WS_PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_liveness = Instant::now();
    let liveness_timeout = time::sleep_until(last_liveness + WORKER_CONTROL_WS_LIVENESS_TIMEOUT);
    tokio::pin!(liveness_timeout);

    loop {
        liveness_timeout
            .as_mut()
            .reset(last_liveness + WORKER_CONTROL_WS_LIVENESS_TIMEOUT);

        tokio::select! {
            delivery = receiver.recv() => {
                let Some(delivery) = delivery else {
                    let _ = sender.send(WsMessage::Close(None)).await;
                    return;
                };
                let delivery = match delivery {
                    Ok(delivery) => delivery,
                    Err(WorkerControlBusError::InvalidCursor { .. }) => {
                        let _ = sender.send(invalid_cursor_close_message()).await;
                        return;
                    }
                    Err(_) => {
                        let _ = sender.send(WsMessage::Close(None)).await;
                        return;
                    }
                };
                let frame = WorkerControlDeliveryFrame {
                    id: delivery.id.to_string(),
                    envelope: delivery.envelope,
                };
                let Ok(text) = serde_json::to_string(&frame) else {
                    let _ = sender.send(WsMessage::Close(None)).await;
                    return;
                };
                if sender.send(WsMessage::Text(text.into())).await.is_err() {
                    return;
                }
            }
            message = receiver_ws.next() => {
                let Some(message) = message else {
                    return;
                };
                match message {
                    Ok(WsMessage::Ping(payload)) => {
                        last_liveness = Instant::now();
                        if sender.send(WsMessage::Pong(payload)).await.is_err() {
                            return;
                        }
                    }
                    Ok(WsMessage::Pong(_) | WsMessage::Text(_) | WsMessage::Binary(_)) => {
                        last_liveness = Instant::now();
                    }
                    Ok(WsMessage::Close(_)) | Err(_) => return,
                }
            }
            _ = ping_interval.tick() => {
                if sender.send(WsMessage::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
            () = &mut liveness_timeout => {
                let _ = sender.send(WsMessage::Close(Some(CloseFrame {
                    code: close_code::AWAY,
                    reason: WORKER_CONTROL_PONG_TIMEOUT_REASON.into(),
                }))).await;
                return;
            }
        }
    }
}

fn invalid_cursor_close_message() -> WsMessage {
    WsMessage::Close(Some(CloseFrame {
        code:   close_code::POLICY,
        reason: WORKER_CONTROL_INVALID_CURSOR_REASON.into(),
    }))
}
