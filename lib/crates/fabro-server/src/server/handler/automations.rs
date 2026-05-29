use std::sync::Arc;

use axum::http::HeaderMap;
use axum_extra::extract::Query as ExtraQuery;
use chrono::Utc;
use fabro_automation::{
    Automation, AutomationDraft, AutomationId, AutomationReplace, AutomationStoreError,
};
use fabro_config::Storage;
use fabro_types::{AutomationRef, RunId};
use serde::Serialize;

use super::super::{
    ApiError, AppState, IntoResponse, Json, PaginationParams, Path, RequiredUser, Response, Router,
    State, StatusCode, get, paginate_items,
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
            "/automations/{id}",
            get(get_automation)
                .put(replace_automation)
                .delete(delete_automation),
        )
}

async fn list_automations(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    let data = state.automation_store().list().await;
    let total = data.len();
    (
        StatusCode::OK,
        Json(AutomationListResponse {
            data,
            meta: AutomationListMeta { total },
        }),
    )
        .into_response()
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
    if state.automation_store().get(&id).await.is_none() {
        return ApiError::not_found(format!("automation not found: {id}")).into_response();
    }

    let entries = match state
        .store
        .list_cached_runs(&fabro_store::ListRunsQuery::default(), Utc::now())
        .await
    {
        Ok(entries) => entries,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let mut runs: Vec<fabro_types::Run> = entries
        .into_iter()
        .map(|entry| entry.summary)
        .filter(|run| {
            run.automation
                .as_ref()
                .is_some_and(|automation| automation.id == id.as_str())
        })
        .collect();
    runs.sort_by(|a, b| {
        b.timestamps
            .created_at
            .cmp(&a.timestamps.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });

    let total = runs.len() as u64;
    let (page, has_more) = paginate_items(runs, &pagination);
    let data = state.decorate_run_summaries(page).await;

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": data,
            "meta": { "has_more": has_more, "total": total }
        })),
    )
        .into_response()
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
    let Some(automation) = state.automation_store().get(&id).await else {
        return ApiError::not_found(format!("automation not found: {id}")).into_response();
    };
    let Some(api_trigger) = automation.enabled_api_trigger() else {
        return ApiError::with_code(
            StatusCode::CONFLICT,
            "automation is disabled or has no enabled API trigger",
            "automation_api_trigger_disabled",
        )
        .into_response();
    };
    let api_trigger_id = api_trigger.id.to_string();

    let run_id = RunId::new();
    let temp_root = Storage::new(state.server_storage_dir())
        .scratch_dir()
        .join("automations");
    let materialized = match state
        .materialize_automation_run(AutomationRunMaterializeInput {
            automation_id: automation.id.clone(),
            target: automation.target.clone(),
            run_id,
            user_settings_path: state.active_config_path().to_path_buf(),
            temp_root,
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
    Ok((StatusCode::CREATED, Json(automation)).into_response())
}

async fn get_automation(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    match state.automation_store().get(&id).await {
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
            // The handlers parse `If-Match` before reaching the store, so a
            // missing-revision error from the store would indicate an internal
            // bug rather than a client problem.
            AutomationStoreError::MissingRevision { .. }
            | AutomationStoreError::InvalidFilename { .. }
            | AutomationStoreError::Parse { .. }
            | AutomationStoreError::InvalidUtf8 { .. }
            | AutomationStoreError::Serialize { .. }
            | AutomationStoreError::Io { .. } => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "automation store operation failed",
            ),
        }
    }
}
