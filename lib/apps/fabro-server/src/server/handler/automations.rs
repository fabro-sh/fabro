use std::sync::Arc;

use axum::http::HeaderMap;
use axum_extra::extract::Query as ExtraQuery;
use fabro_automation::{
    Automation, AutomationDraft, AutomationId, AutomationReplace, AutomationStoreError,
};
use fabro_store::{RunSummaryListQuery, RunSummaryVisibility};
use fabro_types::{AutomationRef, RunId};
use serde::Serialize;

use super::super::{
    ApiError, AppState, IntoResponse, Json, PaginationParams, Path, RequiredUser, Response, Router,
    State, StatusCode, clamp_page_limit, clamp_page_offset, get,
};
use super::{json_with_etag_response, lifecycle, parse_required_if_match, runs};
use crate::automation_materializer::AutomationRunMaterializeInput;
use crate::principal_middleware::RequiredRunToolActor;

#[derive(Serialize)]
struct AutomationListResponse {
    data: Vec<Automation>,
    meta: AutomationListMeta,
}

#[derive(Serialize)]
struct AutomationListMeta {
    total: usize,
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/automations",
            get(list_automations).post(create_automation),
        )
        .route(
            "/automations/{id}/runs",
            get(list_automation_runs).post(create_automation_run),
        )
        .route(
            "/automations/{id}/plane-dispatches",
            get(list_automation_plane_dispatches),
        )
        .route(
            "/automations/{id}",
            get(get_automation)
                .put(replace_automation)
                .delete(delete_automation),
        )
}

async fn list_automations(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
) -> Result<Response, ApiError> {
    let data = state.automation_store().list().await?;
    let total = data.len();
    Ok((
        StatusCode::OK,
        Json(AutomationListResponse {
            data,
            meta: AutomationListMeta { total },
        }),
    )
        .into_response())
}

async fn list_automation_runs(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    ExtraQuery(pagination): ExtraQuery<PaginationParams>,
) -> Response {
    let id = match parse_path_id(id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    match state.automation_store().exists(&id).await {
        Ok(true) => {}
        Ok(false) => {
            return ApiError::not_found(format!("automation not found: {id}")).into_response();
        }
        Err(err) => return ApiError::from(err).into_response(),
    }

    let query = RunSummaryListQuery {
        automation_id: Some(id.to_string()),
        visibility: RunSummaryVisibility::All,
        limit: clamp_page_limit(pagination.limit),
        offset: clamp_page_offset(pagination.offset),
        ..RunSummaryListQuery::default()
    };
    runs::run_summary_page_response(&state, &query).await
}

async fn list_automation_plane_dispatches(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    match state.automation_store().exists(&id).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(ApiError::not_found(format!("automation not found: {id}")));
        }
        Err(err) => return Err(ApiError::from(err)),
    }
    let records = state
        .plane_dispatch_store()
        .list_for_automation(&id)
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let data = records
        .into_iter()
        .map(|record| record.dispatch)
        .collect::<Vec<_>>();
    Ok((
        StatusCode::OK,
        Json(fabro_types::PlaneDispatchListResponse { data }),
    )
        .into_response())
}

async fn create_automation_run(
    RequiredRunToolActor(actor): RequiredRunToolActor,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_path_id(id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let automation = match state.automation_store().get(&id).await {
        Ok(Some(automation)) => automation,
        Ok(None) => {
            return ApiError::not_found(format!("automation not found: {id}")).into_response();
        }
        Err(err) => return ApiError::from(err).into_response(),
    };
    let Some(api_trigger) = automation.enabled_api_trigger() else {
        return ApiError::with_code(
            StatusCode::CONFLICT,
            "automation has no enabled API trigger",
            "automation_api_trigger_disabled",
        )
        .into_response();
    };
    let api_trigger_id = api_trigger.id.to_string();

    let run_id = RunId::new();
    let materialized = match state
        .materialize_automation_run(AutomationRunMaterializeInput {
            automation_id: automation.id.clone(),
            target: automation.target.clone(),
            run_id,
            user_settings_path: state.active_config_path().to_path_buf(),
            temp_root: state.automation_temp_root(),
        })
        .await
    {
        Ok(materialized) => materialized,
        Err(err) => {
            return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, err.to_string())
                .into_response();
        }
    };
    let explicit_title_supplied = materialized.manifest.title.is_some();
    let automation_ref = AutomationRef {
        id:         automation.id.to_string(),
        name:       Some(automation.name.clone()),
        trigger_id: Some(api_trigger_id),
    };

    let response = Box::pin(runs::create_run_from_manifest(
        Arc::clone(&state),
        runs::CreateRunFromManifestRequest {
            manifest: materialized.manifest,
            submitted_manifest_bytes: materialized.submitted_manifest_bytes,
            explicit_run_id: Some(run_id),
            explicit_title_supplied,
            actor: actor.clone(),
            headers,
            automation: Some(automation_ref),
        },
    ))
    .await;

    // An automation's API trigger should both create and start the run; otherwise
    // the run sits in `Submitted` forever because the scheduler only claims
    // `Runnable`. Mirror what the UI does for a manual create-then-start flow.
    if response.status().is_success() {
        if let Err(err) = lifecycle::queue_run_start(state.as_ref(), run_id, false, actor).await {
            tracing::warn!(
                %run_id,
                automation_id = %automation.id,
                error = ?err,
                "Created automation run but failed to start it",
            );
        }
    }

    response
}

async fn create_automation(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(draft): Json<AutomationDraft>,
) -> Result<Response, ApiError> {
    let automation = state.automation_store().create(draft).await?;
    state.notify_automation_scheduler();
    Ok((StatusCode::CREATED, Json(automation)).into_response())
}

async fn get_automation(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    match state.automation_store().get(&id).await? {
        Some(automation) => Ok(automation_with_etag_response(StatusCode::OK, automation)),
        None => Err(ApiError::not_found(format!("automation not found: {id}"))),
    }
}

async fn replace_automation(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(replacement): Json<AutomationReplace>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    let expected = parse_required_if_match(&headers, "automation", &id)?;
    let automation = state
        .automation_store()
        .replace(&id, &expected, replacement)
        .await?;
    state.notify_automation_scheduler();
    Ok(automation_with_etag_response(StatusCode::OK, automation))
}

async fn delete_automation(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    let expected = parse_required_if_match(&headers, "automation", &id)?;
    state.automation_store().delete(&id, &expected).await?;
    state.notify_automation_scheduler();
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn parse_path_id(id: String) -> Result<AutomationId, ApiError> {
    AutomationId::new(id)
        .map_err(|err| ApiError::bad_request(format!("invalid automation id: {err}")))
}

fn automation_with_etag_response(status: StatusCode, automation: Automation) -> Response {
    let revision = automation.revision.clone();
    json_with_etag_response(status, "automation", &revision, automation)
}

impl From<AutomationStoreError> for ApiError {
    fn from(err: AutomationStoreError) -> Self {
        match err {
            AutomationStoreError::NotFound { id } => {
                Self::not_found(format!("automation not found: {id}"))
            }
            AutomationStoreError::AlreadyExists { id } => Self::new(
                StatusCode::CONFLICT,
                format!("automation already exists: {id}"),
            ),
            AutomationStoreError::StaleRevision { id, .. } => Self::new(
                StatusCode::CONFLICT,
                format!("automation revision is stale: {id}"),
            ),
            AutomationStoreError::Validation { source } => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, source.to_string())
            }
            err => {
                tracing::error!(error = ?err, "Automation store operation failed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "automation store operation failed",
                )
            }
        }
    }
}
