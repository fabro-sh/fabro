use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
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
    let store = state.automation_store();
    let data = store.list().await;
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
    body: Result<Json<AutomationDraft>, JsonRejection>,
) -> Response {
    let draft = match parse_json_body(body) {
        Ok(draft) => draft,
        Err(err) => return err.into_response(),
    };
    let store = state.automation_store();
    match store.create(draft).await {
        Ok(automation) => (StatusCode::CREATED, Json(automation)).into_response(),
        Err(err) => automation_store_error_response(err),
    }
}

async fn get_automation(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_path_id(id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store = state.automation_store();
    match store.get(&id).await {
        Some(automation) => automation_with_etag_response(StatusCode::OK, automation),
        None => ApiError::not_found(format!("automation not found: {id}")).into_response(),
    }
}

async fn replace_automation(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<AutomationReplace>, JsonRejection>,
) -> Response {
    let id = match parse_path_id(id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let expected = match parse_required_if_match(&headers, &id) {
        Ok(revision) => revision,
        Err(err) => return err.into_response(),
    };
    let replacement = match parse_json_body(body) {
        Ok(replacement) => replacement,
        Err(err) => return err.into_response(),
    };

    let store = state.automation_store();
    match store.replace(&id, &expected, replacement).await {
        Ok(automation) => automation_with_etag_response(StatusCode::OK, automation),
        Err(err) => automation_store_error_response(err),
    }
}

async fn delete_automation(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_path_id(id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let expected = match parse_required_if_match(&headers, &id) {
        Ok(revision) => revision,
        Err(err) => return err.into_response(),
    };

    let store = state.automation_store();
    match store.delete(&id, &expected).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => automation_store_error_response(err),
    }
}

fn parse_path_id(id: String) -> Result<AutomationId, ApiError> {
    AutomationId::new(id)
        .map_err(|err| ApiError::bad_request(format!("invalid automation id: {err}")))
}

fn parse_json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    match body {
        Ok(Json(value)) => Ok(value),
        Err(JsonRejection::JsonDataError(err)) => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid automation request: {err}"),
        )),
        Err(JsonRejection::JsonSyntaxError(err)) => Err(ApiError::bad_request(format!(
            "malformed JSON request body: {err}"
        ))),
        Err(JsonRejection::MissingJsonContentType(_)) => {
            Err(ApiError::bad_request("request body must be JSON"))
        }
        Err(JsonRejection::BytesRejection(err)) => Err(ApiError::bad_request(format!(
            "failed to read request body: {err}"
        ))),
        Err(rejection) => Err(ApiError::bad_request(rejection.to_string())),
    }
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

fn etag_value(revision: &AutomationRevision) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{revision}\""))
        .expect("automation revisions are valid ETag header values")
}

fn automation_with_etag_response(status: StatusCode, automation: Automation) -> Response {
    let etag = etag_value(&automation.revision);
    let mut response = (status, Json(automation)).into_response();
    response.headers_mut().insert(header::ETAG, etag);
    response
}

fn automation_store_error_response(err: AutomationStoreError) -> Response {
    match err {
        AutomationStoreError::NotFound { id } => {
            ApiError::not_found(format!("automation not found: {id}")).into_response()
        }
        AutomationStoreError::AlreadyExists { id } => ApiError::new(
            StatusCode::CONFLICT,
            format!("automation already exists: {id}"),
        )
        .into_response(),
        AutomationStoreError::MissingRevision { id } => ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            format!("automation revision is required: {id}"),
        )
        .into_response(),
        AutomationStoreError::StaleRevision { id, .. } => ApiError::new(
            StatusCode::CONFLICT,
            format!("automation revision is stale: {id}"),
        )
        .into_response(),
        AutomationStoreError::Validation { source } => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, source.to_string()).into_response()
        }
        AutomationStoreError::InvalidFilename { .. }
        | AutomationStoreError::Parse { .. }
        | AutomationStoreError::InvalidUtf8 { .. }
        | AutomationStoreError::Serialize { .. }
        | AutomationStoreError::Io { .. } => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "automation store operation failed",
        )
        .into_response(),
    }
}
