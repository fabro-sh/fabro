use std::sync::Arc;

use super::super::{
    ApiError, AppState, BilledTokenCounts, BillingByModel, BillingStageRef, EventEnvelope, HashMap,
    IntoResponse, Json, ListResponse, ModelBillingTotals, ModelReference, PaginationParams, Path,
    Query, RequiredUser, Response, Router, RunBilling, RunBillingStage, RunBillingTotals, RunId,
    RunStage, RunStatus, StageState, State, StatusCode, accumulate_model_billing, get,
    parse_run_id_path,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/stages", get(list_run_stages))
        .route("/runs/{id}/billing", get(get_run_billing))
}

fn active_stage_state_from_events(events: &[EventEnvelope], node_id: &str) -> StageState {
    let latest = events.iter().rev().find(|envelope| {
        envelope.event.node_id.as_deref() == Some(node_id)
            && matches!(
                envelope.event.event_name(),
                "stage.retrying" | "stage.started" | "stage.completed" | "stage.failed"
            )
    });

    if latest.is_some_and(|e| e.event.event_name() == "stage.retrying") {
        StageState::Retrying
    } else {
        StageState::Running
    }
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

    // Try live run first.
    let (checkpoint, run_is_active) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => {
                let active = !matches!(
                    managed_run.status,
                    RunStatus::Succeeded { .. } | RunStatus::Failed { .. } | RunStatus::Dead
                );
                (managed_run.checkpoint.clone(), active)
            }
            None => (None, false),
        }
    };

    // Fall back to stored run.
    let (checkpoint, run_is_active) = if checkpoint.is_some() {
        (checkpoint, run_is_active)
    } else {
        match state.store.open_run_reader(&id).await {
            Ok(run_store) => match run_store.state().await {
                Ok(run_state) => {
                    let active = run_state.status.is_some_and(|status| !status.is_terminal());
                    (run_state.checkpoint, active)
                }
                Err(_) => (None, false),
            },
            Err(_) => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    let Some(checkpoint) = checkpoint else {
        return (
            StatusCode::OK,
            Json(ListResponse::new(Vec::<RunStage>::new())),
        )
            .into_response();
    };

    let events = match state.store.open_run_reader(&id).await {
        Ok(run_store) => run_store.list_events().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let stage_durations = fabro_workflow::extract_stage_durations_from_events(&events);

    let mut stages = Vec::new();
    for node_id in &checkpoint.completed_nodes {
        let duration_ms = stage_durations.get(node_id).copied().unwrap_or(0);
        let status = match checkpoint.node_outcomes.get(node_id) {
            Some(outcome) => StageState::from(outcome.status),
            None => StageState::Succeeded,
        };
        stages.push(RunStage {
            id: node_id.clone(),
            name: node_id.clone(),
            status,
            duration_secs: Some(duration_ms as f64 / 1000.0),
            dot_id: Some(node_id.clone()),
        });
    }

    // Add next node as running if the run is still active.
    // The checkpoint's current_node is the last *completed* stage; next_node_id
    // is the stage that is currently executing.
    if let Some(next_id) = &checkpoint.next_node_id {
        if run_is_active && next_id != "exit" && !checkpoint.completed_nodes.contains(next_id) {
            stages.push(RunStage {
                id:            next_id.clone(),
                name:          next_id.clone(),
                status:        active_stage_state_from_events(&events, next_id),
                duration_secs: None,
                dot_id:        Some(next_id.clone()),
            });
        }
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

        let Some(usage) = checkpoint
            .node_outcomes
            .get(node_id)
            .and_then(|outcome| outcome.usage.as_ref())
        else {
            continue;
        };

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
        stages.push(RunBillingStage {
            billing,
            model: ModelReference { id: model_id },
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
