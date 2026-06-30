use std::sync::Arc;

use axum::http::HeaderMap;
use fabro_mcp_store::McpServerStoreError;
use fabro_types::{
    McpServerDefinition, McpServerDraft, McpServerId, McpServerReplace, McpServerView,
};
use serde::Serialize;

use super::super::{
    ApiError, AppState, IntoResponse, Json, Path, RequiredUser, Response, Router, State,
    StatusCode, get,
};
use super::{json_with_etag_response, parse_required_if_match};

#[derive(Serialize)]
struct McpServerListResponse {
    data: Vec<McpServerView>,
    meta: McpServerListMeta,
}

#[derive(Serialize)]
struct McpServerListMeta {
    total: usize,
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/mcp-servers",
            get(list_mcp_servers).post(create_mcp_server),
        )
        .route(
            "/mcp-servers/{id}",
            get(get_mcp_server)
                .put(replace_mcp_server)
                .delete(delete_mcp_server),
        )
}

async fn list_mcp_servers(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    let data = state.mcp_server_store().list_views();
    let total = data.len();
    (
        StatusCode::OK,
        Json(McpServerListResponse {
            data,
            meta: McpServerListMeta { total },
        }),
    )
        .into_response()
}

async fn create_mcp_server(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(draft): Json<McpServerDraft>,
) -> Result<Response, ApiError> {
    // TODO(mcp): credential-literal validation — warn vs hard-reject still open.
    // The store keeps validation structural (id/name/transport); whether the API
    // should reject credential-looking literal values in env/header fields and
    // point the user at `{{ secrets.NAME }}` is undecided. Until then literals
    // are accepted verbatim and secret references are stored as written.
    let definition = state.mcp_server_store().create(draft).await?;
    state.refresh_manifest_run_settings_from_catalogs();
    Ok(mcp_server_with_etag_response(
        StatusCode::CREATED,
        definition,
    ))
}

async fn get_mcp_server(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    match state.mcp_server_store().get(&id) {
        Some(definition) => Ok(mcp_server_with_etag_response(StatusCode::OK, definition)),
        None => Err(ApiError::not_found(format!("mcp server not found: {id}"))),
    }
}

async fn replace_mcp_server(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(replacement): Json<McpServerReplace>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    let expected = parse_required_if_match(&headers, "mcp server", &id)?;
    // TODO(mcp): credential-literal validation — warn vs hard-reject still open.
    // See `create_mcp_server`; validation stays structural here too.
    let definition = state
        .mcp_server_store()
        .replace(&id, &expected, replacement)
        .await?;
    state.refresh_manifest_run_settings_from_catalogs();
    Ok(mcp_server_with_etag_response(StatusCode::OK, definition))
}

async fn delete_mcp_server(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    let expected = parse_required_if_match(&headers, "mcp server", &id)?;
    state.mcp_server_store().delete(&id, &expected).await?;
    state.refresh_manifest_run_settings_from_catalogs();
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn parse_path_id(id: String) -> Result<McpServerId, ApiError> {
    McpServerId::new(id)
        .map_err(|err| ApiError::bad_request(format!("invalid mcp server id: {err}")))
}

fn mcp_server_with_etag_response(status: StatusCode, definition: McpServerDefinition) -> Response {
    let revision = definition.revision.clone();
    let view = McpServerView::from(definition);
    json_with_etag_response(status, "mcp server", &revision, view)
}

impl From<McpServerStoreError> for ApiError {
    fn from(err: McpServerStoreError) -> Self {
        match err {
            McpServerStoreError::NotFound { id } => {
                Self::not_found(format!("mcp server not found: {id}"))
            }
            McpServerStoreError::AlreadyExists { id } => Self::new(
                StatusCode::CONFLICT,
                format!("mcp server already exists: {id}"),
            ),
            McpServerStoreError::StaleRevision { id, .. } => Self::new(
                StatusCode::CONFLICT,
                format!("mcp server revision is stale: {id}"),
            ),
            McpServerStoreError::Validation { source } => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, source.to_string())
            }
            // Remaining variants are persistence/parse faults that indicate an
            // internal problem rather than a client one.
            McpServerStoreError::InvalidFilename { .. }
            | McpServerStoreError::Parse { .. }
            | McpServerStoreError::InvalidUtf8 { .. }
            | McpServerStoreError::Serialize { .. }
            | McpServerStoreError::Io { .. } => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "mcp server store operation failed",
            ),
        }
    }
}
