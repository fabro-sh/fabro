use std::sync::Arc;

use axum::http::HeaderMap;
use axum::routing::delete;
use axum_extra::extract::Query as ExtraQuery;
use fabro_automation::{
    Automation, AutomationDraft, AutomationId, AutomationReplace, AutomationStoreError,
};
use fabro_environment::EnvironmentId;
use fabro_store::{RunSummaryListQuery, RunSummaryVisibility};
use fabro_types::{AutomationRef, RunId};
use fabro_util::error as error_util;
use serde::Serialize;
use tracing::info;

use super::super::{
    ApiError, AppState, IntoResponse, Json, PaginationParams, Path, RequiredUser, Response, Router,
    State, StatusCode, clamp_page_limit, clamp_page_offset, get,
};
use super::{json_with_etag_response, lifecycle, parse_required_if_match, runs};
use crate::automation_materializer::AutomationRunMaterializeInput;
use crate::principal_middleware::RequiredRunToolActor;
use crate::run_manifest;

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
            "/automations/{id}/jira-issues/{key}",
            delete(release_jira_issue),
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

async fn release_jira_issue(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((id, key)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    if !is_jira_issue_key(&key) {
        return Err(ApiError::bad_request(format!(
            "invalid Jira issue key: {key}"
        )));
    }
    if state.automation_store().get(&id).await?.is_none() {
        return Err(ApiError::not_found(format!("automation not found: {id}")));
    }
    if !state.jira_claims().release(&id, &key).await? {
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            format!("no Jira claim for {key} on automation {id}"),
            "jira_issue_claim_not_found",
        ));
    }
    info!(automation_id = %id, issue_key = %key, "Released Jira issue claim");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Matches `^[A-Z][A-Z0-9_]+-[0-9]+$`.
fn is_jira_issue_key(key: &str) -> bool {
    let Some((project, number)) = key.rsplit_once('-') else {
        return false;
    };
    let mut chars = project.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && project.len() >= 2
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
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
    let environment_id = match resolve_automation_environment(
        state.as_ref(),
        automation.environment_id.as_deref(),
        StatusCode::CONFLICT,
    ) {
        Ok(environment_id) => environment_id,
        Err(err) => return err.into_response(),
    };
    let Some(target) = automation.git_target().cloned() else {
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Stored automation target is not Git-backed",
        )
        .into_response();
    };

    let run_id = RunId::new();
    let materialized = match state
        .materialize_automation_run(AutomationRunMaterializeInput {
            automation_id: automation.id.clone(),
            target,
            workflow_source: automation.workflow_source.clone(),
            workflow: automation.workflow.clone(),
            run_id,
            temp_root: state.automation_temp_root(),
            title: None,
            goal: None,
        })
        .await
    {
        Ok(materialized) => materialized,
        Err(err) => {
            let message = error_util::collect_chain(&err).join(": ");
            return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, message).into_response();
        }
    };
    let automation_ref = AutomationRef {
        id:              automation.id.to_string(),
        name:            Some(automation.name.clone()),
        trigger_id:      Some(api_trigger_id),
        workflow_source: materialized.workflow_source.clone(),
    };

    let response = Box::pin(runs::create_run_from_intent(
        Arc::clone(&state),
        runs::CreateRunFromIntentRequest {
            intent: materialized.into_run_intent(environment_id),
            explicit_run_id: Some(run_id),
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
    Json(mut draft): Json<AutomationDraft>,
) -> Result<Response, ApiError> {
    draft.environment_id = Some(resolve_automation_environment(
        state.as_ref(),
        draft.environment_id.as_deref(),
        StatusCode::UNPROCESSABLE_ENTITY,
    )?);
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
    Json(mut replacement): Json<AutomationReplace>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    let expected = parse_required_if_match(&headers, "automation", &id)?;
    replacement.environment_id = Some(resolve_automation_environment(
        state.as_ref(),
        replacement.environment_id.as_deref(),
        StatusCode::UNPROCESSABLE_ENTITY,
    )?);
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

pub(in crate::server) fn resolve_automation_environment(
    state: &AppState,
    value: Option<&str>,
    status: StatusCode,
) -> Result<String, ApiError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(ApiError::with_code(
            status,
            "automation environment is required",
            "automation_environment_required",
        ));
    };
    let id = value.parse::<EnvironmentId>().map_err(|_| {
        ApiError::with_code(
            status,
            "automation environment id is invalid",
            "automation_environment_invalid",
        )
    })?;
    let Some(environment) = state.environment_store().get(&id) else {
        return Err(ApiError::with_code(
            status,
            format!("automation environment not found: {id}"),
            "automation_environment_not_found",
        ));
    };
    if !environment.settings.provider.clones_workspace() {
        return Err(ApiError::with_code(
            status,
            format!(
                "automation environment `{id}` is incompatible; Git-backed automations require Docker or Daytona"
            ),
            "automation_environment_incompatible",
        ));
    }
    let provider = environment.settings.provider.clone();
    if let Some(message) =
        run_manifest::sandbox_provider_policy_error(&state.server_settings(), &provider)
    {
        return Err(ApiError::with_code(
            status,
            message,
            "automation_environment_provider_disabled",
        ));
    }
    if !state
        .sandbox_inventory()
        .kinds()
        .any(|kind| *kind == provider)
    {
        return Err(ApiError::with_code(
            status,
            format!("sandbox provider `{provider}` is not ready on this server"),
            "automation_environment_provider_unavailable",
        ));
    }
    Ok(id.to_string())
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
