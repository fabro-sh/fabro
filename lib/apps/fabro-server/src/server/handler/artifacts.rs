use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use async_zip::base::write::ZipFileWriter;
use async_zip::error::ZipError;
use async_zip::{Compression, ZipEntryBuilder};
use axum::http::HeaderValue;
use fabro_store::{ArtifactStore, BlobStore, Error as StoreError};
use fabro_types::{BlobHash, RunProjection};
use fabro_util::error::collect_chain;
use futures_util::SinkExt as _;
use futures_util::io::AsyncWriteExt as _;
use tokio::io::{AsyncWrite, BufWriter};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::io::{CopyToBytes, SinkWriter};
use tokio_util::sync::PollSender;
use tracing::warn;

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
        .route("/runs/{id}/blobs/{blobHash}", get(read_run_blob))
        .route("/runs/{id}/artifacts", get(list_run_artifacts))
        .route("/runs/{id}/artifacts/download", get(download_run_artifacts))
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
    match state.load_run_projection(&id).await {
        Ok(projection) => match projection.current_checkpoint() {
            Some(cp) => (StatusCode::OK, Json(cp.clone())).into_response(),
            None => (StatusCode::OK, Json(serde_json::json!(null))).into_response(),
        },
        Err(err) => err.into_response(),
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
    if let Err(err) = state.load_run_projection(&id).await {
        return err.into_response();
    }
    match state.store_ref().blobs().write(&body).await {
        Ok(blob_hash) => Json(WriteBlobResponse { hash: blob_hash }).into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn read_run_blob(
    RequireRunBlob(id, blob_hash): RequireRunBlob,
    State(state): State<Arc<AppState>>,
) -> Response {
    if let Err(err) = state.load_run_projection(&id).await {
        return err.into_response();
    }
    match state.store_ref().blobs().read(&blob_hash).await {
        Ok(Some(bytes)) => octet_stream_response(bytes),
        Ok(None) => ApiError::not_found("Blob not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn ensure_run_exists(state: &AppState, run_id: &RunId) -> Result<(), Response> {
    match state
        .stores
        .run_summaries
        .get(run_id, chrono::Utc::now())
        .await
    {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(ApiError::not_found("Run not found.").into_response()),
        Err(err) => {
            Err(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response())
        }
    }
}

/// Where an artifact's bytes are: the blob table, for one the run's
/// hooks collected, or the artifact store, for one uploaded to the stage
/// artifact endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactBytes {
    Blob(BlobHash),
    Store,
}

/// Every artifact of the run, each once: the ones the run's projection
/// records, with their bytes in the blob table, and the ones uploaded to
/// the artifact store. A path uploaded for a stage and retry the projection
/// also collected is the projection's.
async fn run_artifacts(
    state: &AppState,
    run_id: &RunId,
    projection: &RunProjection,
) -> Result<Vec<(NodeArtifact, ArtifactBytes)>, Response> {
    let mut artifacts: BTreeMap<ArtifactKey, (NodeArtifact, ArtifactBytes)> = BTreeMap::new();
    for artifact in &projection.artifacts {
        let key = ArtifactKey::new(
            artifact.stage_id.clone(),
            artifact.retry,
            artifact.relative_path.clone(),
        );
        artifacts.entry(key).or_insert((
            NodeArtifact {
                node:     artifact.stage_id.clone(),
                retry:    artifact.retry,
                filename: artifact.relative_path.clone(),
                size:     artifact.size,
            },
            ArtifactBytes::Blob(artifact.blob),
        ));
    }
    let uploaded = state
        .artifact_store
        .list_for_run(run_id)
        .await
        .map_err(|err| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        })?;
    for artifact in uploaded {
        let key = ArtifactKey::new(
            artifact.node.clone(),
            artifact.retry,
            artifact.filename.clone(),
        );
        artifacts
            .entry(key)
            .or_insert((artifact, ArtifactBytes::Store));
    }
    let mut artifacts: Vec<_> = artifacts.into_values().collect();
    artifacts.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(artifacts)
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
    let projection = match state.load_run_projection(&id).await {
        Ok(projection) => projection,
        Err(error) => return error.into_response(),
    };
    match run_artifacts(state.as_ref(), &id, &projection).await {
        Ok(entries) => Json(RunArtifactListResponse {
            data: entries
                .into_iter()
                .map(|(entry, _)| run_artifact_entry_from(entry))
                .collect(),
        })
        .into_response(),
        Err(response) => response,
    }
}

const ARCHIVE_STREAM_CHANNEL_CAPACITY: usize = 8;

/// Deflate flushes its output in 8 KiB blocks, and every write below becomes
/// its own allocation, channel send, and HTTP body frame. Batching to 64 KiB
/// cuts all three by eight without meaningfully delaying the stream.
const ARCHIVE_WRITE_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
enum ArtifactArchiveError {
    #[error("archive output failed: {0}")]
    Io(#[from] io::Error),
    #[error("artifact read failed: {0}")]
    Store(#[from] StoreError),
    #[error("ZIP write failed: {0}")]
    Zip(#[from] ZipError),
}

/// One entry per artifact path, holding the newest capture of that path.
///
/// Newest means latest stage, then latest retry, then highest stage ID. That
/// last tiebreaker only decides between two stages the projection does not know
/// about, which both sort oldest (`None` < `Some`); it is here so the winner
/// does not depend on the order the store happens to list objects in. All three
/// keys mirror the artifacts page, which sorts on the same triple and takes the
/// last entry. Boundary stages are dropped: they run no work, so anything they
/// captured was already in the workspace.
///
/// Paths are re-checked here rather than trusted: a path stored before a
/// validation rule existed would otherwise be written straight into a ZIP that
/// somebody extracts. An unsafe path is skipped, not fatal — one bad path must
/// not cost the caller every other artifact.
fn latest_run_artifacts(
    entries: Vec<(NodeArtifact, ArtifactBytes)>,
    projection: &RunProjection,
) -> Vec<(NodeArtifact, ArtifactBytes)> {
    let stage_order = projection
        .iter_stages()
        .enumerate()
        .map(|(order, (stage_id, _))| (stage_id.clone(), order))
        .collect::<HashMap<_, _>>();
    // The stage ID compares as its serialized `node@visit` form, matching the
    // string the artifacts page sorts on rather than `StageId`'s own ordering,
    // which compares the visit numerically and would disagree.
    let capture_rank = |artifact: &NodeArtifact| {
        (
            stage_order.get(&artifact.node).copied(),
            artifact.retry,
            artifact.node.to_string(),
        )
    };
    let mut latest_by_path: HashMap<String, (NodeArtifact, ArtifactBytes)> = HashMap::new();

    for (artifact, bytes) in entries {
        if projection.is_boundary_stage(artifact.node.node_id()) {
            continue;
        }
        if let Err(error) = ArtifactStore::validate_relative_path(&artifact.filename) {
            warn!(
                path = %artifact.filename,
                %error,
                "skipping artifact with an unsafe path"
            );
            continue;
        }

        match latest_by_path.get(&artifact.filename) {
            Some((existing, _)) if capture_rank(existing) >= capture_rank(&artifact) => {}
            _ => {
                latest_by_path.insert(artifact.filename.clone(), (artifact, bytes));
            }
        }
    }

    let mut latest = latest_by_path.into_values().collect::<Vec<_>>();
    latest.sort_by(|left, right| left.0.filename.cmp(&right.0.filename));
    latest
}

/// The bytes of one artifact, from wherever they are; `None` when they are
/// gone.
async fn read_artifact(
    artifact_store: &ArtifactStore,
    blobs: &BlobStore,
    run_id: &RunId,
    key: &ArtifactKey,
    bytes: ArtifactBytes,
) -> Result<Option<Bytes>, StoreError> {
    match bytes {
        ArtifactBytes::Blob(hash) => blobs.read(&hash).await,
        ArtifactBytes::Store => artifact_store.get(run_id, key).await,
    }
}

async fn write_artifact_archive<W>(
    writer: W,
    artifact_store: ArtifactStore,
    blobs: Arc<BlobStore>,
    run_id: RunId,
    artifacts: Vec<(NodeArtifact, ArtifactBytes)>,
) -> Result<(), ArtifactArchiveError>
where
    W: AsyncWrite + Unpin,
{
    let mut archive = ZipFileWriter::with_tokio(writer);
    for (artifact, bytes) in artifacts {
        let key = ArtifactKey::new(artifact.node, artifact.retry, artifact.filename.clone());
        let Some(source) = read_artifact(&artifact_store, &blobs, &run_id, &key, bytes).await?
        else {
            // Deleted between the listing and this read, which in practice means
            // the run was pruned mid-download. Leave it out and keep going: an
            // archive missing one file beats a truncated one missing the rest.
            warn!(%run_id, path = %artifact.filename, "artifact vanished while archiving");
            continue;
        };
        // Deflate, not Stored: artifacts are mostly logs and reports, and the
        // response is excluded from transfer compression precisely because the
        // archive already carries its own.
        let entry = ZipEntryBuilder::new(artifact.filename.into(), Compression::Deflate);
        // `destination` is a futures-io writer from async_zip, while `writer` at
        // the end of this function is a tokio one, so both `AsyncWriteExt`
        // traits are in scope and each call resolves to a different one.
        let mut destination = archive.write_entry_stream(entry).await?;
        destination.write_all(&source).await?;
        destination.close().await?;
    }
    let mut writer = archive.close().await?.into_inner();
    writer.shutdown().await?;
    Ok(())
}

fn artifact_archive_body(
    artifact_store: ArtifactStore,
    blobs: Arc<BlobStore>,
    run_id: RunId,
    artifacts: Vec<(NodeArtifact, ArtifactBytes)>,
) -> Body {
    // A channel of `Result`, rather than `tokio::io::duplex`, so a failure
    // partway through can poison the body. Dropping a duplex writer ends the
    // response with a clean EOF, which would hand the caller a truncated ZIP
    // that looks like a complete download. Sending an `Err` aborts the chunked
    // body instead, so the caller sees a transfer error. Do not "simplify" this
    // to a duplex without replacing that signal.
    let (sender, receiver) =
        mpsc::channel::<Result<Bytes, io::Error>>(ARCHIVE_STREAM_CHANNEL_CAPACITY);
    let error_sender = sender.clone();
    let sink = PollSender::new(sender)
        .sink_map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
        .with(|chunk: Bytes| {
            std::future::ready(Ok::<Result<Bytes, io::Error>, io::Error>(Ok(chunk)))
        });
    let writer = BufWriter::with_capacity(
        ARCHIVE_WRITE_BUFFER_BYTES,
        SinkWriter::new(CopyToBytes::new(sink)),
    );

    tokio::spawn(async move {
        if let Err(error) =
            write_artifact_archive(writer, artifact_store, blobs, run_id, artifacts).await
        {
            // Log before signalling: the send fails when the caller has already
            // gone away, and that is exactly when this log is the only record
            // that the archive failed. The 200 went out long ago.
            warn!(
                %run_id,
                error = %collect_chain(&error).join(": "),
                "artifact archive stream failed"
            );
            let _ = error_sender
                .send(Err(io::Error::other("artifact archive stream failed")))
                .await;
        }
    });

    Body::from_stream(ReceiverStream::new(receiver))
}

async fn download_run_artifacts(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let projection = match state.load_run_projection(&id).await {
        Ok(projection) => projection,
        Err(error) => return error.into_response(),
    };
    let Ok(entries) = run_artifacts(state.as_ref(), &id, &projection).await else {
        warn!(run_id = %id, "failed to list artifacts for ZIP download");
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact archive could not be prepared.",
        )
        .into_response();
    };
    let artifacts = latest_run_artifacts(entries, &projection);

    let content_disposition = format!("attachment; filename=\"fabro-artifacts-{id}.zip\"");
    let body = artifact_archive_body(
        state.artifact_store.clone(),
        state.store_ref().blobs(),
        id,
        artifacts,
    );
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition)
            .expect("run IDs produce valid attachment filenames"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
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
    let projection = match state.load_run_projection(&id).await {
        Ok(projection) => projection,
        Err(error) => return error.into_response(),
    };
    match run_artifacts(state.as_ref(), &id, &projection).await {
        Ok(entries) => Json(ArtifactListResponse {
            data: entries
                .into_iter()
                .filter(|(entry, _)| entry.node == stage_id)
                .map(|(entry, _)| {
                    artifact_entry_from(StageArtifactEntry {
                        retry:    entry.retry,
                        filename: entry.filename,
                        size:     entry.size,
                    })
                })
                .collect(),
        })
        .into_response(),
        Err(response) => response,
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
    if let Err(response) = ensure_run_exists(state.as_ref(), &id).await {
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
    let projection = match state.load_run_projection(&id).await {
        Ok(projection) => projection,
        Err(error) => return error.into_response(),
    };
    let key = ArtifactKey::new(stage_id.clone(), retry, relative_path);
    let bytes = projection
        .artifacts
        .iter()
        .find(|artifact| {
            artifact.stage_id == key.stage_id
                && artifact.retry == key.retry
                && artifact.relative_path == key.relative_path
        })
        .map_or(ArtifactBytes::Store, |artifact| {
            ArtifactBytes::Blob(artifact.blob)
        });
    match read_artifact(
        &state.artifact_store,
        &state.store_ref().blobs(),
        &id,
        &key,
        bytes,
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
