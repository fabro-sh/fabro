use std::sync::Arc;

use super::super::{
    ApiError, AppState, Bytes, DaytonaSandbox, EnvVars, IntoResponse, Json, NamedTempFile, Path,
    PreviewUrlRequest, PreviewUrlResponse, Query, RequiredUser, Response, Router, RunId, Sandbox,
    SandboxFileEntry, SandboxFileListResponse, SandboxProvider, SshAccessRequest,
    SshAccessResponse, State, StatusCode, collect_causes, fs, get, octet_stream_response,
    parse_run_id_path, post, reconnect, reject_if_archived, render_with_causes,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/preview", post(generate_preview_url))
        .route("/runs/{id}/ssh", post(create_ssh_access))
        .route("/runs/{id}/sandbox/files", get(list_sandbox_files))
        .route(
            "/runs/{id}/sandbox/file",
            get(get_sandbox_file).put(put_sandbox_file),
        )
}

#[derive(serde::Deserialize)]
struct SandboxFilesParams {
    path:  String,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(serde::Deserialize)]
struct SandboxFileParams {
    path: String,
}

async fn generate_preview_url(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<PreviewUrlRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let Ok(port) = u16::try_from(request.port) else {
        return ApiError::bad_request("Port must fit in a u16.").into_response();
    };
    let Ok(expires_in_secs) = i32::try_from(request.expires_in_secs.get()) else {
        return ApiError::bad_request("Preview expiry exceeds supported range.").into_response();
    };

    let sandbox = match reconnect_daytona_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };

    let response = if request.signed {
        match sandbox
            .get_signed_preview_url(port, Some(expires_in_secs))
            .await
        {
            Ok(preview) => PreviewUrlResponse {
                token: None,
                url:   preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err.display_with_causes())
                    .into_response();
            }
        }
    } else {
        match sandbox.get_preview_link(port).await {
            Ok(preview) => PreviewUrlResponse {
                token: Some(preview.token),
                url:   preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err.display_with_causes())
                    .into_response();
            }
        }
    };

    (StatusCode::CREATED, Json(response)).into_response()
}

async fn create_ssh_access(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<SshAccessRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let sandbox = match reconnect_daytona_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    match sandbox.create_ssh_access(Some(request.ttl_minutes)).await {
        Ok(command) => (StatusCode::CREATED, Json(SshAccessResponse { command })).into_response(),
        Err(err) => ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response(),
    }
}

async fn list_sandbox_files(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFilesParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    match sandbox.list_directory(&params.path, params.depth).await {
        Ok(entries) => Json(SandboxFileListResponse {
            data: entries
                .into_iter()
                .map(|entry| SandboxFileEntry {
                    is_dir: entry.is_dir,
                    name:   entry.name,
                    size:   entry.size.map(u64::cast_signed),
                })
                .collect(),
        })
        .into_response(),
        Err(err) => ApiError::new(StatusCode::NOT_FOUND, err.display_with_causes()).into_response(),
    }
}

async fn get_sandbox_file(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFileParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let temp = match NamedTempFile::new() {
        Ok(temp) => temp,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Err(err) = sandbox
        .download_file_to_local(&params.path, temp.path())
        .await
    {
        return ApiError::new(StatusCode::NOT_FOUND, err.display_with_causes()).into_response();
    }
    match fs::read(temp.path()).await {
        Ok(bytes) => octet_stream_response(bytes.into()),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn put_sandbox_file(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFileParams>,
    body: Bytes,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let temp = match NamedTempFile::new() {
        Ok(temp) => temp,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Err(err) = fs::write(temp.path(), &body).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    match sandbox
        .upload_file_from_local(temp.path(), &params.path)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.display_with_causes())
            .into_response(),
    }
}

async fn reconnect_run_sandbox(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<Box<dyn Sandbox>, Response> {
    let record = load_run_sandbox_record(state, run_id).await?;
    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    reconnect(&record, daytona_api_key).await.map_err(|err| {
        let detail = render_with_causes(&err.to_string(), &collect_causes(err.as_ref()));
        ApiError::new(StatusCode::CONFLICT, detail).into_response()
    })
}

async fn reconnect_daytona_sandbox(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<DaytonaSandbox, Response> {
    let record = load_run_sandbox_record(state, run_id).await?;
    if record.provider != SandboxProvider::Daytona.to_string() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox provider does not support this capability.",
        )
        .into_response());
    }
    let Some(name) = record.identifier.as_deref() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox record is missing the Daytona identifier.",
        )
        .into_response());
    };
    let Some(repo_cloned) = record.repo_cloned else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox record is missing clone metadata.",
        )
        .into_response());
    };
    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    DaytonaSandbox::reconnect(
        name,
        daytona_api_key,
        repo_cloned,
        record.clone_origin_url.clone(),
        record.clone_branch.clone(),
    )
    .await
    .map_err(|err| ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response())
}

async fn load_run_sandbox_record(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<fabro_types::SandboxRecord, Response> {
    match state.store.open_run_reader(run_id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => run_state.sandbox.ok_or_else(|| {
                ApiError::new(StatusCode::CONFLICT, "Run has no active sandbox.").into_response()
            }),
            Err(err) => Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            ),
        },
        Err(_) => Err(ApiError::not_found("Run not found.").into_response()),
    }
}
