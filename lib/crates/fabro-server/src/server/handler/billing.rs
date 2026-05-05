use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fabro_types::{RunProjection, StageProjection, StageState};

use super::super::{
    ApiError, AppState, BillingByModel, BillingStageRef, IntoResponse, Json, ListResponse,
    ModelReference, PaginationParams, Path, Query, RequiredUser, Response, Router, RunBilling,
    RunBillingStage, RunBillingTotals, RunId, State, StatusCode, get, parse_run_id_path,
    run_stage_from_stage_id,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/stages", get(list_run_stages))
        .route("/runs/{id}/billing", get(get_run_billing))
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

    let Ok(run_store) = state.store.open_run_reader(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let projection = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let now = Utc::now();
    let stages = projection
        .iter_stages()
        .map(|(stage_id, stage)| {
            run_stage_from_stage_id(
                stage_id,
                stage_id.node_id().to_string(),
                stage.effective_state(),
                stage.runtime_secs(now),
                stage.started_at,
            )
        })
        .collect::<Vec<_>>();

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

    let rollup_by_node = rollup
        .stages
        .iter()
        .map(|stage| (stage.node_id.as_str(), stage))
        .collect::<HashMap<_, _>>();
    let live_rows = live_billing_rows(&projection, Utc::now());
    let runtime_secs = live_rows.iter().map(|row| row.runtime_secs).sum::<f64>();
    let stages = live_rows
        .into_iter()
        .map(|row| {
            let rollup_stage = rollup_by_node.get(row.node_id.as_str());
            RunBillingStage {
                billing:      rollup_stage
                    .map(|stage| stage.billing.clone())
                    .unwrap_or_default(),
                model:        rollup_stage
                    .and_then(|stage| stage.model_id.as_ref())
                    .map(|id| ModelReference { id: id.clone() }),
                runtime_secs: row.runtime_secs,
                stage:        BillingStageRef {
                    id:   row.node_id.clone(),
                    name: row.node_id,
                },
                started_at:   row.started_at,
                state:        row.state,
            }
        })
        .collect::<Vec<_>>();

    let response = RunBilling {
        by_model,
        stages,
        totals: RunBillingTotals {
            cache_read_tokens: rollup.totals.cache_read_tokens,
            cache_write_tokens: rollup.totals.cache_write_tokens,
            input_tokens: rollup.totals.input_tokens,
            output_tokens: rollup.totals.output_tokens,
            reasoning_tokens: rollup.totals.reasoning_tokens,
            runtime_secs,
            total_tokens: rollup.totals.total_tokens,
            total_usd_micros: rollup.totals.total_usd_micros,
        },
    };

    (StatusCode::OK, Json(response)).into_response()
}

struct LiveBillingRow {
    node_id:      String,
    runtime_secs: f64,
    started_at:   Option<DateTime<Utc>>,
    state:        Option<StageState>,
    latest_visit: u32,
}

fn live_billing_rows(projection: &RunProjection, now: DateTime<Utc>) -> Vec<LiveBillingRow> {
    let mut row_indices = HashMap::<String, usize>::new();
    let mut rows = Vec::<LiveBillingRow>::new();

    for (stage_id, stage) in projection.iter_stages() {
        let node_id = stage_id.node_id();
        if is_exit_stage(projection, node_id) || !stage_has_billing_row(stage) {
            continue;
        }

        let index = *row_indices.entry(node_id.to_string()).or_insert_with(|| {
            let index = rows.len();
            rows.push(LiveBillingRow {
                node_id:      node_id.to_string(),
                runtime_secs: 0.0,
                started_at:   None,
                state:        None,
                latest_visit: 0,
            });
            index
        });
        let row = &mut rows[index];
        row.runtime_secs += billing_runtime_secs(stage, now).unwrap_or(0.0);

        if stage_id.visit() >= row.latest_visit {
            row.latest_visit = stage_id.visit();
            row.started_at = stage.started_at;
            row.state = Some(stage.effective_state());
        }
    }

    rows
}

fn billing_runtime_secs(stage: &StageProjection, now: DateTime<Utc>) -> Option<f64> {
    stage
        .duration_ms
        .map(|ms| ms as f64 / 1000.0)
        .or_else(|| stage.runtime_secs(now))
}

fn stage_has_billing_row(stage: &StageProjection) -> bool {
    stage.completion.is_some()
        || stage.duration_ms.is_some()
        || stage.usage.is_some()
        || stage.started_at.is_some()
        || stage.state.is_some()
}

fn is_exit_stage(projection: &RunProjection, node_id: &str) -> bool {
    projection
        .spec()
        .and_then(|spec| spec.graph().nodes.get(node_id))
        .is_some_and(|node| node.handler_type() == Some("exit"))
}
