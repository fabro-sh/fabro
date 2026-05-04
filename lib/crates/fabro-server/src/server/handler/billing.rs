use std::sync::Arc;

use chrono::Utc;
use fabro_types::{StageId, StageProjection};

use super::super::{
    ApiError, AppState, BilledTokenCounts, BillingByModel, BillingStageRef, HashMap, IntoResponse,
    Json, ListResponse, ModelBillingTotals, ModelReference, PaginationParams, Path, Query,
    RequiredUser, Response, Router, RunBilling, RunBillingStage, RunBillingTotals, RunId, RunStage,
    State, StatusCode, accumulate_model_billing, get, parse_run_id_path,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/stages", get(list_run_stages))
        .route("/runs/{id}/billing", get(get_run_billing))
}

/// One row per `node_id`, latest visit wins.
///
/// Mirrors the aggregation rule used in `fabro_workflow::pipeline::finalize`:
/// the displayed row uses the latest visit's data, but the row's sort key is
/// the minimum `first_event_seq` across all visits of that node — i.e. the
/// node's first appearance in the event log. This produces the same A, B
/// order for an A → B → A loop that finalize produces.
struct DedupedStage<'a> {
    node_id:              String,
    stage:                &'a StageProjection,
    sort_key_first_event: u32,
}

fn dedupe_by_node_id<'a>(
    stages: impl IntoIterator<Item = (&'a StageId, &'a StageProjection)>,
) -> Vec<DedupedStage<'a>> {
    let mut by_node: HashMap<&'a str, (u32, u32, &'a StageProjection)> = HashMap::new();
    for (stage_id, stage) in stages {
        let node_id = stage_id.node_id();
        let visit = stage_id.visit();
        let first_event = stage.first_event_seq.get();
        by_node
            .entry(node_id)
            .and_modify(|entry| {
                if first_event < entry.0 {
                    entry.0 = first_event;
                }
                if visit >= entry.1 {
                    entry.1 = visit;
                    entry.2 = stage;
                }
            })
            .or_insert((first_event, visit, stage));
    }

    let mut deduped: Vec<DedupedStage<'a>> = by_node
        .into_iter()
        .map(|(node_id, (first_event, _visit, stage))| DedupedStage {
            node_id: node_id.to_string(),
            stage,
            sort_key_first_event: first_event,
        })
        .collect();
    deduped.sort_by(|a, b| {
        a.sort_key_first_event
            .cmp(&b.sort_key_first_event)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    deduped
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
    let stages: Vec<RunStage> = dedupe_by_node_id(projection.iter_stages())
        .into_iter()
        .map(|entry| {
            let DedupedStage { node_id, stage, .. } = entry;
            RunStage {
                id:            node_id.clone(),
                name:          node_id.clone(),
                status:        stage.effective_state(),
                duration_secs: stage.runtime_secs(now),
                dot_id:        Some(node_id),
                started_at:    stage.started_at,
            }
        })
        .collect();

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

    let now = Utc::now();

    let mut by_model_totals = HashMap::<String, ModelBillingTotals>::new();
    let mut runtime_secs = 0.0_f64;
    let mut stages = Vec::new();

    for entry in dedupe_by_node_id(projection.iter_stages()) {
        let DedupedStage { node_id, stage, .. } = entry;

        let row_runtime = stage.runtime_secs(now).unwrap_or(0.0);
        runtime_secs += row_runtime;

        let (billing, model) = if let Some(usage) = stage.usage.as_ref() {
            let billing = BilledTokenCounts::from_billed_usage(std::slice::from_ref(usage));
            let model_id = usage.model_id();
            let model_totals = match by_model_totals.get_mut(model_id) {
                Some(totals) => totals,
                None => by_model_totals.entry(model_id.to_string()).or_default(),
            };
            accumulate_model_billing(model_totals, usage);
            (
                billing,
                Some(ModelReference {
                    id: model_id.to_string(),
                }),
            )
        } else {
            (BilledTokenCounts::default(), None)
        };

        stages.push(RunBillingStage {
            billing,
            model,
            runtime_secs: row_runtime,
            stage: BillingStageRef {
                id:   node_id.clone(),
                name: node_id,
            },
            started_at: stage.started_at,
            state: Some(stage.effective_state()),
        });
    }

    // Grand totals are the sum of the per-model totals we already accumulated.
    let mut totals = BilledTokenCounts::default();
    for model_totals in by_model_totals.values() {
        totals.input_tokens += model_totals.billing.input_tokens;
        totals.output_tokens += model_totals.billing.output_tokens;
        totals.reasoning_tokens += model_totals.billing.reasoning_tokens;
        totals.cache_read_tokens += model_totals.billing.cache_read_tokens;
        totals.cache_write_tokens += model_totals.billing.cache_write_tokens;
        totals.total_tokens += model_totals.billing.total_tokens;
        if let Some(value) = model_totals.billing.total_usd_micros {
            *totals.total_usd_micros.get_or_insert(0) += value;
        }
    }

    let by_model = by_model_totals
        .into_iter()
        .map(|(model, totals)| BillingByModel {
            billing: totals.billing,
            model:   ModelReference { id: model },
            stages:  totals.stages,
        })
        .collect::<Vec<_>>();

    let response = RunBilling {
        by_model,
        stages,
        totals: RunBillingTotals {
            cache_read_tokens: totals.cache_read_tokens,
            cache_write_tokens: totals.cache_write_tokens,
            input_tokens: totals.input_tokens,
            output_tokens: totals.output_tokens,
            reasoning_tokens: totals.reasoning_tokens,
            runtime_secs,
            total_tokens: totals.total_tokens,
            total_usd_micros: totals.total_usd_micros,
        },
    };

    (StatusCode::OK, Json(response)).into_response()
}
