use std::sync::Arc;

use super::super::{
    ApiError, AppState, ArtifactEntry, ArtifactKey, ArtifactListResponse, AsyncWriteExt, Body,
    Bytes, DefaultBodyLimit, Digest, HashMap, HashSet, HeaderMap, IntoResponse, Json, NodeArtifact,
    Path, Query, RequireRunBlob, RequireRunScoped, RequireStageArtifact, RequiredUser, Response,
    Router, RunArtifactEntry, RunArtifactListResponse, RunId, Sha256, StageArtifactEntry, StageId,
    State, StatusCode, StreamExt, WriteBlobResponse, axum_extract, bad_request_response, get,
    header, octet_stream_response, parse_run_id_path, parse_stage_id_path,
    payload_too_large_response, post, reject_if_archived, required_query_param,
    validate_relative_artifact_path,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/checkpoint", get(get_checkpoint))
        .route("/runs/{id}/blobs", post(write_run_blob))
        .route("/runs/{id}/blobs/{blobId}", get(read_run_blob))
        .route("/runs/{id}/artifacts", get(list_run_artifacts))
        .route(
            "/runs/{id}/stages/{stageId}/artifacts",
            get(list_stage_artifacts)
                .post(put_stage_artifact)
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/runs/{id}/stages/{stageId}/artifacts/download",
            get(get_stage_artifact),
        )
}

#[derive(serde::Deserialize)]
struct ArtifactFilenameParams {
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    retry:    Option<u32>,
}

const MAX_SINGLE_ARTIFACT_BYTES: u64 = 10 * 1024 * 1024;
const MAX_MULTIPART_ARTIFACTS: usize = 100;
const MAX_MULTIPART_REQUEST_BYTES: u64 = 50 * 1024 * 1024;
const MAX_MULTIPART_MANIFEST_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ArtifactBatchUploadManifest {
    entries: Vec<ArtifactBatchUploadEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ArtifactBatchUploadEntry {
    part:           String,
    path:           String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_type:   Option<String>,
}

async fn get_checkpoint(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let live_checkpoint = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => managed_run.checkpoint.clone(),
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };
    if let Some(cp) = live_checkpoint {
        return (StatusCode::OK, Json(cp)).into_response();
    }

    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => match run_state.checkpoint {
                Some(cp) => (StatusCode::OK, Json(cp)).into_response(),
                None => (StatusCode::OK, Json(serde_json::json!(null))).into_response(),
            },
            Err(err) => {
                tracing::warn!(run_id = %id, error = %err, "Failed to load checkpoint state from store");
                (StatusCode::OK, Json(serde_json::json!(null))).into_response()
            }
        },
        Err(err) => {
            tracing::warn!(run_id = %id, error = %err, "Failed to open run store reader");
            ApiError::not_found("Run not found.").into_response()
        }
    }
}

async fn write_run_blob(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    match state.store.open_run(&id).await {
        Ok(run_store) => match run_store.write_blob(&body).await {
            Ok(blob_id) => Json(WriteBlobResponse {
                id: blob_id.to_string(),
            })
            .into_response(),
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn read_run_blob(
    RequireRunBlob(id, blob_id): RequireRunBlob,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.read_blob(&blob_id).await {
            Ok(Some(bytes)) => octet_stream_response(bytes),
            Ok(None) => ApiError::not_found("Blob not found.").into_response(),
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn load_run_spec(state: &AppState, run_id: &RunId) -> Result<fabro_types::RunSpec, Response> {
    let run_store = state
        .store
        .open_run_reader(run_id)
        .await
        .map_err(|_| ApiError::not_found("Run not found.").into_response())?;
    let run_state = run_store.state().await.map_err(|err| {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
    })?;
    run_state.spec.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "run spec missing from store",
        )
        .into_response()
    })
}

async fn list_run_artifacts(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Err(response) = load_run_spec(state.as_ref(), &id).await {
        return response;
    }

    match state.artifact_store.list_for_run(&id).await {
        Ok(entries) => Json(RunArtifactListResponse {
            data: entries.into_iter().map(run_artifact_entry_from).collect(),
        })
        .into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

fn run_artifact_entry_from(entry: NodeArtifact) -> RunArtifactEntry {
    RunArtifactEntry {
        stage_id:      entry.node.to_string(),
        node_slug:     entry.node.node_id().to_string(),
        retry:         entry.retry.cast_signed(),
        relative_path: entry.filename,
        size:          entry.size.cast_signed(),
    }
}

fn artifact_entry_from(entry: StageArtifactEntry) -> ArtifactEntry {
    ArtifactEntry {
        filename: entry.filename,
        retry:    entry.retry.cast_signed(),
        size:     entry.size.cast_signed(),
    }
}

async fn list_stage_artifacts(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((id, stage_id)): Path<(String, String)>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let stage_id = match parse_stage_id_path(&stage_id) {
        Ok(stage_id) => stage_id,
        Err(response) => return response,
    };
    if let Err(response) = load_run_spec(state.as_ref(), &id).await {
        return response;
    }

    match state.artifact_store.list_for_node(&id, &stage_id).await {
        Ok(entries) => Json(ArtifactListResponse {
            data: entries.into_iter().map(artifact_entry_from).collect(),
        })
        .into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

enum ArtifactUploadContentType {
    OctetStream,
    Multipart { boundary: String },
}

struct ValidatedArtifactBatchEntry {
    path:           String,
    sha256:         Option<String>,
    expected_bytes: Option<u64>,
}

#[allow(
    clippy::result_large_err,
    reason = "Upload content-type parsing returns HTTP client errors directly."
)]
fn artifact_upload_content_type(
    headers: &HeaderMap,
) -> Result<ArtifactUploadContentType, Response> {
    let value = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "artifact uploads require a supported Content-Type",
            )
            .into_response()
        })?;

    let mime = value.split(';').next().unwrap_or(value).trim();
    match mime {
        "application/octet-stream" => Ok(ArtifactUploadContentType::OctetStream),
        "multipart/form-data" => multer::parse_boundary(value)
            .map(|boundary| ArtifactUploadContentType::Multipart { boundary })
            .map_err(|err| bad_request_response(format!("invalid multipart boundary: {err}"))),
        _ => Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "artifact uploads only support application/octet-stream or multipart/form-data",
        )
        .into_response()),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "Content-Length parsing returns HTTP client errors directly."
)]
fn content_length_from_headers(headers: &HeaderMap) -> Result<Option<u64>, Response> {
    headers
        .get(header::CONTENT_LENGTH)
        .map(|value| {
            value
                .to_str()
                .map_err(|err| {
                    bad_request_response(format!("invalid content-length header: {err}"))
                })
                .and_then(|value| {
                    value.parse::<u64>().map_err(|err| {
                        bad_request_response(format!("invalid content-length header: {err}"))
                    })
                })
        })
        .transpose()
}

#[allow(
    clippy::result_large_err,
    reason = "Multipart manifest parsing returns HTTP client errors directly."
)]
async fn read_multipart_manifest(
    field: &mut multer::Field<'_>,
) -> Result<ArtifactBatchUploadManifest, Response> {
    let mut manifest_bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|err| bad_request_response(format!("invalid multipart body: {err}")))?
    {
        manifest_bytes.extend_from_slice(&chunk);
        if manifest_bytes.len() > MAX_MULTIPART_MANIFEST_BYTES {
            return Err(payload_too_large_response(
                "multipart manifest exceeds the server limit",
            ));
        }
    }

    serde_json::from_slice(&manifest_bytes)
        .map_err(|err| bad_request_response(format!("invalid multipart manifest: {err}")))
}

#[allow(
    clippy::result_large_err,
    reason = "Artifact batch validation returns HTTP client errors directly."
)]
fn validate_artifact_batch_manifest(
    manifest: ArtifactBatchUploadManifest,
) -> Result<HashMap<String, ValidatedArtifactBatchEntry>, Response> {
    if manifest.entries.is_empty() {
        return Err(bad_request_response(
            "multipart manifest must include at least one artifact entry",
        ));
    }
    if manifest.entries.len() > MAX_MULTIPART_ARTIFACTS {
        return Err(payload_too_large_response(format!(
            "multipart upload exceeds the {MAX_MULTIPART_ARTIFACTS} artifact limit"
        )));
    }

    let mut entries = HashMap::with_capacity(manifest.entries.len());
    let mut seen_paths = HashSet::new();
    let mut expected_total_bytes = 0_u64;

    for entry in manifest.entries {
        if entry.part.is_empty() {
            return Err(bad_request_response(
                "multipart manifest part names must not be empty",
            ));
        }
        if entry.part == "manifest" {
            return Err(bad_request_response(
                "multipart manifest part name 'manifest' is reserved",
            ));
        }
        let path = validate_relative_artifact_path("manifest path", &entry.path)?;
        if !seen_paths.insert(path.clone()) {
            return Err(bad_request_response(format!(
                "duplicate artifact path in multipart manifest: {path}"
            )));
        }
        if let Some(sha256) = entry.sha256.as_ref() {
            if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(bad_request_response(format!(
                    "invalid sha256 for multipart part {}",
                    entry.part
                )));
            }
        }
        if let Some(expected_bytes) = entry.expected_bytes {
            if expected_bytes > MAX_SINGLE_ARTIFACT_BYTES {
                return Err(payload_too_large_response(format!(
                    "artifact {path} exceeds the {MAX_SINGLE_ARTIFACT_BYTES} byte limit"
                )));
            }
            expected_total_bytes = expected_total_bytes.saturating_add(expected_bytes);
            if expected_total_bytes > MAX_MULTIPART_REQUEST_BYTES {
                return Err(payload_too_large_response(format!(
                    "multipart upload exceeds the {MAX_MULTIPART_REQUEST_BYTES} byte limit"
                )));
            }
        }
        if entries
            .insert(entry.part.clone(), ValidatedArtifactBatchEntry {
                path,
                sha256: entry.sha256.map(|value| value.to_ascii_lowercase()),
                expected_bytes: entry.expected_bytes,
            })
            .is_some()
        {
            return Err(bad_request_response(format!(
                "duplicate multipart part name in manifest: {}",
                entry.part
            )));
        }
    }

    Ok(entries)
}

async fn upload_stage_artifact_octet_stream(
    state: &AppState,
    run_id: &RunId,
    stage_id: &StageId,
    retry: u32,
    filename: String,
    body: Body,
    content_length: Option<u64>,
) -> Response {
    let relative_path = match validate_relative_artifact_path("filename", &filename) {
        Ok(path) => path,
        Err(response) => return response,
    };

    if content_length.is_some_and(|length| length > MAX_SINGLE_ARTIFACT_BYTES) {
        return payload_too_large_response(format!(
            "artifact exceeds the {MAX_SINGLE_ARTIFACT_BYTES} byte limit"
        ));
    }

    let mut writer = match state.artifact_store.writer(
        run_id,
        &ArtifactKey::new(stage_id.clone(), retry, relative_path),
    ) {
        Ok(writer) => writer,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let mut bytes_written = 0_u64;
    let mut data_stream = body.into_data_stream();
    while let Some(chunk) = data_stream.next().await {
        let chunk = match chunk
            .map_err(|err| bad_request_response(format!("invalid request body: {err}")))
        {
            Ok(chunk) => chunk,
            Err(response) => return response,
        };
        bytes_written =
            bytes_written.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if bytes_written > MAX_SINGLE_ARTIFACT_BYTES {
            return payload_too_large_response(format!(
                "artifact exceeds the {MAX_SINGLE_ARTIFACT_BYTES} byte limit"
            ));
        }
        if let Err(err) = writer.write_all(&chunk).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    match writer.shutdown().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn upload_stage_artifact_multipart(
    state: &AppState,
    run_id: &RunId,
    stage_id: &StageId,
    retry: u32,
    boundary: String,
    body: Body,
) -> Response {
    let mut multipart = multer::Multipart::new(body.into_data_stream(), boundary);
    let Some(mut manifest_field) = (match multipart
        .next_field()
        .await
        .map_err(|err| bad_request_response(format!("invalid multipart body: {err}")))
    {
        Ok(field) => field,
        Err(response) => return response,
    }) else {
        return bad_request_response("multipart upload must begin with a manifest part");
    };

    if manifest_field.name() != Some("manifest") {
        return bad_request_response("multipart upload must begin with a manifest part");
    }

    let manifest = match read_multipart_manifest(&mut manifest_field).await {
        Ok(manifest) => manifest,
        Err(response) => return response,
    };
    drop(manifest_field);
    let mut expected_parts = match validate_artifact_batch_manifest(manifest) {
        Ok(entries) => entries,
        Err(response) => return response,
    };
    let mut total_bytes = 0_u64;

    while let Some(mut field) = match multipart
        .next_field()
        .await
        .map_err(|err| bad_request_response(format!("invalid multipart body: {err}")))
    {
        Ok(field) => field,
        Err(response) => return response,
    } {
        let Some(part_name) = field.name().map(ToOwned::to_owned) else {
            return bad_request_response("multipart file parts must be named");
        };
        let Some(entry) = expected_parts.remove(&part_name) else {
            return bad_request_response(format!("unexpected multipart part: {part_name}"));
        };

        let mut writer = match state.artifact_store.writer(
            run_id,
            &ArtifactKey::new(stage_id.clone(), retry, entry.path.clone()),
        ) {
            Ok(writer) => writer,
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        };
        let mut bytes_written = 0_u64;
        let mut sha256 = Sha256::new();

        while let Some(chunk) = match field
            .chunk()
            .await
            .map_err(|err| bad_request_response(format!("invalid multipart body: {err}")))
        {
            Ok(chunk) => chunk,
            Err(response) => return response,
        } {
            let chunk_len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            bytes_written = bytes_written.saturating_add(chunk_len);
            total_bytes = total_bytes.saturating_add(chunk_len);

            if bytes_written > MAX_SINGLE_ARTIFACT_BYTES {
                return payload_too_large_response(format!(
                    "artifact {} exceeds the {MAX_SINGLE_ARTIFACT_BYTES} byte limit",
                    entry.path
                ));
            }
            if total_bytes > MAX_MULTIPART_REQUEST_BYTES {
                return payload_too_large_response(format!(
                    "multipart upload exceeds the {MAX_MULTIPART_REQUEST_BYTES} byte limit"
                ));
            }

            sha256.update(&chunk);
            if let Err(err) = writer.write_all(&chunk).await {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        }

        if let Some(expected_bytes) = entry.expected_bytes {
            if bytes_written != expected_bytes {
                return bad_request_response(format!(
                    "multipart part {part_name} expected {expected_bytes} bytes but received {bytes_written}"
                ));
            }
        }
        if let Some(expected_sha256) = entry.sha256.as_ref() {
            let actual_sha256 = hex::encode(sha256.finalize());
            if actual_sha256 != *expected_sha256 {
                return bad_request_response(format!(
                    "multipart part {part_name} sha256 did not match manifest"
                ));
            }
        }

        if let Err(err) = writer.shutdown().await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    if !expected_parts.is_empty() {
        let mut missing = expected_parts.into_keys().collect::<Vec<_>>();
        missing.sort();
        return bad_request_response(format!(
            "multipart upload is missing part(s): {}",
            missing.join(", ")
        ));
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn put_stage_artifact(
    State(state): State<Arc<AppState>>,
    RequireStageArtifact(id, stage_id): RequireStageArtifact,
    Query(params): Query<ArtifactFilenameParams>,
    request: axum_extract::Request,
) -> Response {
    let (parts, body) = request.into_parts();
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    if let Err(response) = load_run_spec(state.as_ref(), &id).await.map(|_| ()) {
        return response;
    }
    let retry = match required_query_param(params.retry.as_ref(), "retry") {
        Ok(retry) => retry,
        Err(response) => return response,
    };

    let content_length = match content_length_from_headers(&parts.headers) {
        Ok(length) => length,
        Err(response) => return response,
    };
    match artifact_upload_content_type(&parts.headers) {
        Ok(ArtifactUploadContentType::OctetStream) => {
            let filename = match required_query_param(params.filename.as_ref(), "filename") {
                Ok(filename) => filename,
                Err(response) => return response,
            };
            upload_stage_artifact_octet_stream(
                state.as_ref(),
                &id,
                &stage_id,
                retry,
                filename,
                body,
                content_length,
            )
            .await
        }
        Ok(ArtifactUploadContentType::Multipart { boundary }) => {
            if content_length.is_some_and(|length| length > MAX_MULTIPART_REQUEST_BYTES) {
                return payload_too_large_response(format!(
                    "multipart upload exceeds the {MAX_MULTIPART_REQUEST_BYTES} byte limit"
                ));
            }
            upload_stage_artifact_multipart(state.as_ref(), &id, &stage_id, retry, boundary, body)
                .await
        }
        Err(response) => response,
    }
}

async fn get_stage_artifact(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((id, stage_id)): Path<(String, String)>,
    Query(params): Query<ArtifactFilenameParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let stage_id = match parse_stage_id_path(&stage_id) {
        Ok(stage_id) => stage_id,
        Err(response) => return response,
    };
    let filename = match required_query_param(params.filename.as_ref(), "filename") {
        Ok(filename) => filename,
        Err(response) => return response,
    };
    let retry = match required_query_param(params.retry.as_ref(), "retry") {
        Ok(retry) => retry,
        Err(response) => return response,
    };
    let relative_path = match validate_relative_artifact_path("filename", &filename) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if let Err(response) = load_run_spec(state.as_ref(), &id).await {
        return response;
    }

    match state
        .artifact_store
        .get(
            &id,
            &ArtifactKey::new(stage_id.clone(), retry, relative_path),
        )
        .await
    {
        Ok(Some(bytes)) => octet_stream_response(bytes),
        Ok(None) => ApiError::not_found("Artifact not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}
