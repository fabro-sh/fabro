use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use async_zip::base::write::ZipFileWriter;
use async_zip::error::ZipError;
use async_zip::{Compression, ZipEntryBuilder};
use axum::extract::DefaultBodyLimit;
use axum::extract::rejection::BytesRejection;
use axum::http::HeaderValue;
use axum::routing::put;
use fabro_store::{ArtifactStore, BlobStore, Error as StoreError};
use fabro_types::{ARTIFACT_MAX_FILE_BYTES, ArtifactSource, BlobHash, RunProjection};
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
    Bytes, HashMap, IntoResponse, Json, NodeArtifact, Path, Query, RequireRunBlob,
    RequireRunScoped, RequiredUser, Response, Router, RunArtifactEntry, RunArtifactListResponse,
    RunId, StageArtifactEntry, State, StatusCode, WriteBlobResponse, get, header,
    octet_stream_response, parse_run_id_path, parse_stage_id_path, post, reject_if_archived,
    required_query_param, validate_relative_artifact_path,
};
use crate::principal_middleware::RequireWorkerRunSegment;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/runs/{id}/artifacts/content/{digest}",
            put(write_run_artifact_content).layer(DefaultBodyLimit::max(ARTIFACT_MAX_FILE_BYTES)),
        )
        .route("/runs/{id}/blobs", post(write_run_blob))
        .route("/runs/{id}/blobs/{blobHash}", get(read_run_blob))
        .route("/runs/{id}/artifacts", get(list_run_artifacts))
        .route("/runs/{id}/artifacts/download", get(download_run_artifacts))
        .route(
            "/runs/{id}/stages/{stageId}/artifacts",
            get(list_stage_artifacts),
        )
        .route(
            "/runs/{id}/stages/{stageId}/artifacts/download",
            get(get_stage_artifact),
        )
}

async fn write_run_artifact_content(
    RequireWorkerRunSegment(id, digest): RequireWorkerRunSegment,
    State(state): State<Arc<AppState>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(error) => {
            return ApiError::new(
                error.status(),
                "Artifact request body could not be read within the 10 MiB limit.",
            )
            .into_response();
        }
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    if let Err(error) = state.load_run_projection(&id).await {
        return error.into_response();
    }
    let Ok(expected) = digest.parse::<BlobHash>() else {
        return ApiError::bad_request("Invalid artifact digest.").into_response();
    };
    if BlobHash::new(&body) != expected {
        return ApiError::bad_request("Artifact content does not match its digest.")
            .into_response();
    }
    match state.artifact_store.put_capture(&id, &body).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            warn!(run_id = %id, error = %collect_chain(&error).join(": "), "Artifact upload failed");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Artifact storage failed.",
            )
            .into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct ArtifactFilenameParams {
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    retry:    Option<u32>,
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

/// Recorded capture sources and historical stage-keyed objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactBytes {
    Captured(ArtifactSource),
    Store,
}

/// Every artifact of the run, each once: the ones the run's projection
/// records, and historical stage-keyed objects. A path in the store for a stage
/// and retry the projection also collected is the projection's.
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
            ArtifactBytes::Captured(artifact.source),
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
        ArtifactBytes::Captured(ArtifactSource::SqliteBlob(hash)) => blobs.read(&hash).await,
        ArtifactBytes::Captured(ArtifactSource::ObjectStore(hash)) => {
            artifact_store.get_capture(run_id, &hash).await
        }
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
            ArtifactBytes::Captured(artifact.source)
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

#[cfg(test)]
mod tests {
    use async_zip::base::read::mem::ZipFileReader;
    use fabro_types::{RunArtifact, StageId, test_support};

    use super::*;

    #[tokio::test]
    async fn artifact_mixed_sources_keep_precedence_and_zip_contents_without_fallback() {
        let state = crate::test_support::test_app_state();
        let run = RunId::new();
        let stage = StageId::new("write", 1);
        let mut projection = RunProjection::new(
            "capture".to_string(),
            test_support::test_run_spec(),
            chrono::Utc::now(),
        );
        let blobs = state.store_ref().blobs();
        let old = blobs.write(b"old SQLite").await.unwrap();
        let new = state
            .artifact_store
            .put_capture(&run, b"new object")
            .await
            .unwrap();
        for (path, size, source) in [
            ("old.bin", 10, ArtifactSource::SqliteBlob(old)),
            ("new.bin", 10, ArtifactSource::ObjectStore(new)),
        ] {
            projection.artifacts.push(RunArtifact {
                stage_id: stage.clone(),
                retry: 1,
                relative_path: path.to_string(),
                size,
                source,
            });
        }
        // A historical object at the same logical path loses to the recorded capture.
        for (path, bytes) in [
            ("new.bin", b"shadow".as_slice()),
            ("legacy.bin", b"legacy".as_slice()),
        ] {
            state
                .artifact_store
                .put(&run, &ArtifactKey::new(stage.clone(), 1, path), bytes)
                .await
                .unwrap();
        }
        // Unrecorded content is never a file-list entry.
        state
            .artifact_store
            .put_capture(&run, b"orphan")
            .await
            .unwrap();
        let entries = run_artifacts(&state, &run, &projection).await.unwrap();
        assert_eq!(entries.len(), 3);
        let archive = artifact_archive_body(
            state.artifact_store.clone(),
            blobs.clone(),
            run,
            latest_run_artifacts(entries, &projection),
        );
        let bytes = axum::body::to_bytes(archive, 1024 * 1024).await.unwrap();
        let zip = ZipFileReader::new(bytes.to_vec()).await.unwrap();
        let mut contents = BTreeMap::new();
        for (index, entry) in zip.file().entries().iter().enumerate() {
            let mut bytes = Vec::new();
            zip.reader_with_entry(index)
                .await
                .unwrap()
                .read_to_end_checked(&mut bytes)
                .await
                .unwrap();
            contents.insert(entry.filename().as_str().unwrap().to_string(), bytes);
        }
        assert_eq!(
            contents,
            BTreeMap::from([
                ("legacy.bin".to_string(), b"legacy".to_vec()),
                ("new.bin".to_string(), b"new object".to_vec()),
                ("old.bin".to_string(), b"old SQLite".to_vec()),
            ])
        );
        let missing = blobs
            .write(b"SQLite is not the selected source")
            .await
            .unwrap();
        let key = ArtifactKey::new(stage, 1, "new.bin");
        assert!(
            read_artifact(
                &state.artifact_store,
                &blobs,
                &run,
                &key,
                ArtifactBytes::Captured(ArtifactSource::ObjectStore(missing))
            )
            .await
            .unwrap()
            .is_none()
        );
        // Saved projections preserve both physical sources without rewriting history.
        let saved = serde_json::to_value(&projection).unwrap();
        let decoded: RunProjection = serde_json::from_value(saved).unwrap();
        assert_eq!(decoded.artifacts, projection.artifacts);
    }
}
