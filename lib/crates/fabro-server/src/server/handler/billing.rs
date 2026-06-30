use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fabro_types::{RunProjection, StageHandler, StageProjection, StageState, StageTiming};

use super::super::{
    ApiError, AppState, BillingByModel, BillingStageRef, IntoResponse, Json, ListResponse,
    PaginationParams, Path, Query, RequiredUser, Response, Router, RunBilling, RunBillingStage,
    RunBillingTotals, RunId, State, StatusCode, get, parse_run_id_path, run_stage_from_stage_id,
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

    let cached = match state.stores.runs.get_cached_run(&id).await {
        Ok(Some(cached)) => cached,
        Ok(None) => return ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let projection = cached.projection;

    let now = Utc::now();
    let graph = projection.spec().graph();
    let stages = projection
        .iter_stages()
        .map(|(stage_id, stage)| {
            let handler = stage.handler.unwrap_or_else(|| {
                StageHandler::from_handler_type(
                    graph
                        .nodes
                        .get(stage_id.node_id())
                        .and_then(|n| n.handler_type()),
                )
            });
            run_stage_from_stage_id(
                stage_id,
                stage_id.node_id().to_string(),
                stage.effective_state(),
                stage.live_wall_time_ms(now),
                stage.started_at,
                handler,
                stage.provider_used.clone(),
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
    let cached = match state.stores.runs.get_cached_run(&id).await {
        Ok(Some(cached)) => cached,
        Ok(None) => return ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let projection = cached.projection;

    let catalog = state.catalog();
    let rollup = fabro_workflow::billing_rollup_from_projection(&projection, Some(&catalog));
    let by_model = rollup
        .by_model
        .iter()
        .map(|model| BillingByModel {
            billing: model.billing.clone(),
            model:   model.model.clone(),
            stages:  model.stages,
        })
        .collect::<Vec<_>>();

    let rollup_by_node = rollup
        .stages
        .iter()
        .map(|stage| (stage.node_id.as_str(), stage))
        .collect::<HashMap<_, _>>();
    let live_rows = live_billing_rows(&projection, Utc::now());
    let totals_timing = live_rows.iter().fold(StageTiming::default(), |acc, row| {
        acc.saturating_add(&row.timing)
    });
    let stages = live_rows
        .into_iter()
        .map(|row| {
            let rollup_stage = rollup_by_node.get(row.node_id.as_str());
            RunBillingStage {
                billing:    rollup_stage
                    .map(|stage| stage.billing.clone())
                    .unwrap_or_default(),
                model:      rollup_stage.and_then(|stage| stage.model.as_ref()).cloned(),
                timing:     row.timing,
                stage:      BillingStageRef {
                    id:   row.node_id.clone(),
                    name: row.node_id,
                },
                started_at: row.started_at,
                state:      row.state,
            }
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
            timing:             totals_timing.into(),
            total_tokens:       rollup.totals.total_tokens,
            total_usd_micros:   rollup.totals.total_usd_micros,
        },
    };

    (StatusCode::OK, Json(response)).into_response()
}

struct LiveBillingRow {
    node_id:      String,
    timing:       StageTiming,
    started_at:   Option<DateTime<Utc>>,
    state:        Option<StageState>,
    latest_visit: u32,
}

fn live_billing_rows(projection: &RunProjection, now: DateTime<Utc>) -> Vec<LiveBillingRow> {
    let mut row_indices = HashMap::<String, usize>::new();
    let mut rows = Vec::<LiveBillingRow>::new();

    for (stage_id, stage) in projection.iter_stages() {
        let node_id = stage_id.node_id();
        if is_boundary_stage(projection, node_id) || !stage_has_billing_row(stage) {
            continue;
        }

        let index = *row_indices.entry(node_id.to_string()).or_insert_with(|| {
            let index = rows.len();
            rows.push(LiveBillingRow {
                node_id:      node_id.to_string(),
                timing:       StageTiming::default(),
                started_at:   None,
                state:        None,
                latest_visit: 0,
            });
            index
        });
        let row = &mut rows[index];
        let stage_timing = billing_stage_timing(stage, now);
        row.timing = row.timing.saturating_add(&stage_timing);

        if stage_id.visit() >= row.latest_visit {
            row.latest_visit = stage_id.visit();
            row.started_at = stage.started_at;
            row.state = Some(stage.effective_state());
        }
    }

    rows
}

/// Per-visit timing for a stage. For terminal visits, the stored breakdown is
/// used directly. For in-flight visits, fall back to the live wall-clock since
/// `started_at` (no active breakdown yet — that is only finalized at terminal
/// event time in v1).
fn billing_stage_timing(stage: &StageProjection, now: DateTime<Utc>) -> StageTiming {
    if let Some(timing) = stage.timing {
        return timing;
    }
    if let Some(live_wall) = stage.live_wall_time_ms(now) {
        return StageTiming::wall_only(live_wall);
    }
    StageTiming::default()
}

fn stage_has_billing_row(stage: &StageProjection) -> bool {
    stage.completion.is_some()
        || stage.timing.is_some()
        || !stage.usage.is_zero()
        || stage.started_at.is_some()
}

fn is_boundary_stage(projection: &RunProjection, node_id: &str) -> bool {
    projection
        .spec()
        .graph()
        .nodes
        .get(node_id)
        .is_some_and(|node| matches!(node.handler_type(), Some("start" | "exit")))
}
