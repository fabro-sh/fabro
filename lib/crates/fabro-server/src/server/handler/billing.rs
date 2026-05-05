use std::sync::Arc;

use fabro_store::RunProjectionReducer;
use fabro_types::{EventBody, RunProjection, StageId};

use super::super::{
    ApiError, AppState, BillingByModel, BillingStageRef, EventEnvelope, HashMap, IntoResponse,
    Json, ListResponse, ModelReference, PaginationParams, Path, Query, RequiredUser, Response,
    Router, RunBilling, RunBillingStage, RunBillingTotals, RunId, StageState, State, StatusCode,
    get, parse_run_id_path, run_stage_from_stage_id,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/stages", get(list_run_stages))
        .route("/runs/{id}/billing", get(get_run_billing))
}

/// Map a `stage.*` lifecycle event body to the [`StageState`] it implies.
/// Returns `None` for any other variant.
fn stage_state_from_lifecycle(body: &EventBody) -> Option<StageState> {
    match body {
        EventBody::StageStarted(_) => Some(StageState::Running),
        EventBody::StageRetrying(_) => Some(StageState::Retrying),
        EventBody::StageFailed(props) => Some(if props.will_retry {
            StageState::Retrying
        } else {
            StageState::Failed
        }),
        EventBody::StageCompleted(props) => Some(StageState::from(props.status)),
        _ => None,
    }
}

/// Single-pass scan over `events` building the latest [`StageState`] for each
/// [`StageId`] from lifecycle events (started/retrying/completed/failed). Each
/// later lifecycle event overwrites earlier ones, leaving the latest as the
/// stored value — equivalent to "scan in reverse, take first match" but in O(E)
/// for the whole list rather than O(stages × events).
fn latest_stage_states(events: &[EventEnvelope]) -> HashMap<StageId, StageState> {
    let mut states = HashMap::new();
    for envelope in events {
        let Some(stage_id) = envelope.event.stage_id.as_ref() else {
            continue;
        };
        let Some(state) = stage_state_from_lifecycle(&envelope.event.body) else {
            continue;
        };
        states.insert(stage_id.clone(), state);
    }
    states
}

async fn list_run_stages(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(_pagination): Query<PaginationParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let events = match state.store.open_run_reader(&id).await {
        Ok(run_store) => run_store.list_events().await.unwrap_or_default(),
        Err(_) => return ApiError::not_found("Run not found.").into_response(),
    };

    let projection = match RunProjection::apply_events(&events) {
        Ok(projection) => projection,
        Err(err) => {
            tracing::warn!(
                run_id = %id,
                error = %err,
                "Failed to build run projection; returning empty stages list",
            );
            RunProjection::default()
        }
    };
    let stage_durations = fabro_workflow::extract_stage_durations_by_stage_id(&events);
    let lifecycle_states = latest_stage_states(&events);

    let mut stages = Vec::new();
    for (stage_id, stage_projection) in projection.iter_stages() {
        // Prefer the latest lifecycle event; fall back to the projection's
        // stored completion (e.g. for runs recovered from snapshot only).
        let status = lifecycle_states.get(stage_id).copied().unwrap_or_else(|| {
            stage_projection
                .completion
                .as_ref()
                .map_or(StageState::Pending, |c| StageState::from(c.outcome))
        });
        stages.push(run_stage_from_stage_id(
            stage_id,
            stage_id.node_id().to_string(),
            status,
            stage_durations.get(stage_id).map(|ms| *ms as f64 / 1000.0),
        ));
    }

    (StatusCode::OK, Json(ListResponse::new(stages))).into_response()
}

async fn get_run_billing(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<RunId>,
) -> Response {
    let run_store = match state.store.open_run_reader(&id).await {
        Ok(run_store) => run_store,
        Err(err) => {
            return ApiError::new(StatusCode::NOT_FOUND, err.to_string()).into_response();
        }
    };

    let projection = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let rollup = fabro_workflow::billing_rollup_from_projection(&projection);
    let by_model = rollup
        .by_model
        .iter()
        .map(|model| BillingByModel {
            billing: model.billing.clone(),
            model:   ModelReference {
                id: model.model_id.clone(),
            },
            stages:  model.stages,
        })
        .collect::<Vec<_>>();
    let stages = rollup
        .stages
        .iter()
        .map(|stage| RunBillingStage {
            billing:      stage.billing.clone(),
            model:        stage
                .model_id
                .as_ref()
                .map(|id| ModelReference { id: id.clone() }),
            runtime_secs: stage.duration_ms as f64 / 1000.0,
            stage:        BillingStageRef {
                id:   stage.node_id.clone(),
                name: stage.node_id.clone(),
            },
        })
        .collect::<Vec<_>>();

    let response = RunBilling {
        by_model,
        stages,
        totals: RunBillingTotals {
            cache_read_tokens:  rollup.totals.cache_read_tokens,
            cache_write_tokens: rollup.totals.cache_write_tokens,
            input_tokens:       rollup.totals.input_tokens,
            output_tokens:      rollup.totals.output_tokens,
            reasoning_tokens:   rollup.totals.reasoning_tokens,
            runtime_secs:       rollup.runtime_ms as f64 / 1000.0,
            total_tokens:       rollup.totals.total_tokens,
            total_usd_micros:   rollup.totals.total_usd_micros,
        },
    };

    (StatusCode::OK, Json(response)).into_response()
}
