use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, header};
use fabro_automation::{
    Automation, AutomationDraft, AutomationId, AutomationReplace, AutomationRevision,
    AutomationStoreError,
};
use serde::Serialize;

use super::super::{
    ApiError, AppState, IntoResponse, Json, Path, RequiredUser, Response, Router, State,
    StatusCode, get,
};

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
    let expected = parse_required_if_match(&headers, &id)?;
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
    let expected = parse_required_if_match(&headers, &id)?;
    state.automation_store().delete(&id, &expected).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn parse_path_id(id: String) -> Result<AutomationId, ApiError> {
    AutomationId::new(id)
        .map_err(|err| ApiError::bad_request(format!("invalid automation id: {err}")))
}

fn parse_required_if_match(
    headers: &HeaderMap,
    id: &AutomationId,
) -> Result<AutomationRevision, ApiError> {
    let Some(value) = headers.get(header::IF_MATCH) else {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            format!("If-Match header is required for automation: {id}"),
        ));
    };
    let value = value
        .to_str()
        .map_err(|_| ApiError::bad_request("If-Match header must be visible ASCII"))?;
    let value = unquote_etag(value.trim());
    value.parse::<AutomationRevision>().map_err(|err| {
        ApiError::bad_request(format!("invalid If-Match automation revision: {err}"))
    })
}

fn unquote_etag(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|unquoted| unquoted.strip_suffix('"'))
        .unwrap_or(value)
}

fn automation_with_etag_response(status: StatusCode, automation: Automation) -> Response {
    let etag = HeaderValue::from_str(&format!("\"{}\"", automation.revision))
        .expect("automation revisions are valid ETag header values");
    let mut response = (status, Json(automation)).into_response();
    response.headers_mut().insert(header::ETAG, etag);
    response
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
