use std::num::NonZeroU32;
use std::sync::Arc;

use fabro_store::RunProjectionReducer;
use fabro_types::{EventBody, RunProjection, StageId, StageProjection};

use super::super::{
    ApiError, AppState, BilledTokenCounts, BillingByModel, BillingStageRef, EventEnvelope, HashMap,
    IntoResponse, Json, ListResponse, ModelBillingTotals, ModelReference, PaginationParams, Path,
    Query, RequiredUser, Response, Router, RunBilling, RunBillingStage, RunBillingTotals, RunId,
    RunStage, StageState, State, StatusCode, accumulate_model_billing, get, parse_run_id_path,
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

    let mut entries: Vec<(&StageId, &StageProjection)> = projection.iter_stages().collect();
    entries.sort_by_key(|(_, stage)| stage.first_event_seq);

    let mut stages = Vec::with_capacity(entries.len());
    for (stage_id, stage_projection) in entries {
        let node_id = stage_id.node_id().to_string();
        let Some(visit) = NonZeroU32::new(stage_id.visit()) else {
            tracing::warn!(
                run_id = %id,
                stage_id = %stage_id,
                "Skipping stage with non-positive visit",
            );
            continue;
        };
        // Prefer the latest lifecycle event; fall back to the projection's
        // stored completion (e.g. for runs recovered from snapshot only).
        let status = lifecycle_states.get(stage_id).copied().unwrap_or_else(|| {
            stage_projection
                .completion
                .as_ref()
                .map_or(StageState::Pending, |c| StageState::from(c.outcome))
        });
        stages.push(RunStage {
            id: stage_id.to_string(),
            name: node_id.clone(),
            status,
            duration_secs: stage_durations.get(stage_id).map(|ms| *ms as f64 / 1000.0),
            node_id,
            visit,
        });
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

    let checkpoint = match run_store.state().await {
        Ok(state) => state.checkpoint,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let Some(checkpoint) = checkpoint else {
        let empty = RunBilling {
            by_model: Vec::new(),
            stages:   Vec::new(),
            totals:   RunBillingTotals {
                cache_read_tokens:  0,
                cache_write_tokens: 0,
                input_tokens:       0,
                output_tokens:      0,
                reasoning_tokens:   0,
                runtime_secs:       0.0,
                total_tokens:       0,
                total_usd_micros:   None,
            },
        };
        return (StatusCode::OK, Json(empty)).into_response();
    };

    let stage_durations = match run_store.list_events().await {
        Ok(events) => fabro_workflow::extract_stage_durations_from_events(&events),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let mut by_model_totals = HashMap::<String, ModelBillingTotals>::new();
    let mut billed_usages = Vec::new();
    let mut runtime_secs = 0.0_f64;
    let mut stages = Vec::new();

    for node_id in &checkpoint.completed_nodes {
        let duration_ms = stage_durations.get(node_id).copied().unwrap_or(0);
        runtime_secs += duration_ms as f64 / 1000.0;

        let usage = checkpoint
            .node_outcomes
            .get(node_id)
            .and_then(|outcome| outcome.usage.as_ref());

        let (billing, model) = if let Some(usage) = usage {
            billed_usages.push(usage.clone());
            let tokens = usage.tokens();
            let billing = BilledTokenCounts {
                cache_read_tokens:  tokens.cache_read_tokens,
                cache_write_tokens: tokens.cache_write_tokens,
                input_tokens:       tokens.input_tokens,
                output_tokens:      tokens.output_tokens,
                reasoning_tokens:   tokens.reasoning_tokens,
                total_tokens:       tokens.total_tokens(),
                total_usd_micros:   usage.total_usd_micros,
            };
            let model_id = usage.model_id().to_string();
            accumulate_model_billing(by_model_totals.entry(model_id.clone()).or_default(), usage);
            (billing, Some(ModelReference { id: model_id }))
        } else {
            (BilledTokenCounts::default(), None)
        };

        stages.push(RunBillingStage {
            billing,
            model,
            runtime_secs: duration_ms as f64 / 1000.0,
            stage: BillingStageRef {
                id:   node_id.clone(),
                name: node_id.clone(),
            },
        });
    }

    let totals = BilledTokenCounts::from_billed_usage(&billed_usages);
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
