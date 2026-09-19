//! The Petri run store over HTTP: the endpoints a run's worker uses to reach
//! the run's Petri records (`fabro_petri::HttpRunStore` is the client). Every
//! endpoint is worker-scoped, and the server answers from the handles
//! `crate::petri_runs::PetriRuns` holds, so the lease and the `(log, seq)`
//! rule are the store's own.
//!
//! Each store error answers with a machine-readable `code`:
//! `petri_run_exists` and `petri_run_leased` (with the holder under
//! `meta.owner`) on `open`, `petri_run_not_found` wherever the run is
//! missing, `petri_stale_owner` and `petri_record_conflict` (with the
//! position under `meta.log` and `meta.seq`) on a write, `petri_read_only`
//! should a reader ever be asked to write, `petri_blob_not_found` on a blob
//! read, and `petri_store_failed` for the backend itself, whose cause goes to
//! the server log and not to the worker.

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use fabro_api::types::{
    PetriAccess, PetriAppendRequest, PetriOpenRequest, PetriOpenResponse, PetriPlatformRecord,
    PetriPlatformRecordAppendRequest, PetriPlatformRecordList, PetriRecord, PetriRecordList,
    PetriReleaseRequest, WriteBlobResponse,
};
use fabro_petri::petri::{Access, Digest, OwnerId, Record, StoreError};
use fabro_petri::run_store::{log_id_text, parse_log_id};
use fabro_store::{PlatformRecord, PlatformRecordKind, StagePosition, StoredPlatformRecord};
use fabro_types::BlobHash;
use fabro_util::error::collect_chain;
use serde_json::{Map, Value, json};

use super::super::{
    ApiError, AppState, Bytes, IntoResponse, Json, Query, RequireWorkerRunScoped,
    RequireWorkerRunSegment, Response, Router, RunId, State, StatusCode, octet_stream_response,
};

/// The largest batch of records one append may carry. Petri batches an
/// execution's records per step, and a step's output can be large.
const RECORD_BATCH_BODY_LIMIT: usize = 256 * 1024 * 1024;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/petri/open", post(open_run))
        .route("/runs/{id}/petri/release", post(release_run))
        .route(
            "/runs/{id}/petri/logs/{log}/records",
            get(list_records)
                .post(append_records)
                .layer(DefaultBodyLimit::max(RECORD_BATCH_BODY_LIMIT)),
        )
        .route(
            "/runs/{id}/petri/blobs",
            post(write_blob).layer(DefaultBodyLimit::disable()),
        )
        .route("/runs/{id}/petri/blobs/{blobHash}", get(read_blob))
        .route(
            "/runs/{id}/petri/platform-records",
            get(list_platform_records).post(append_platform_record),
        )
}

#[derive(serde::Deserialize)]
struct OwnerQuery {
    owner: String,
}

#[derive(serde::Deserialize)]
struct KindQuery {
    kind: Option<String>,
}

async fn open_run(
    RequireWorkerRunScoped(id): RequireWorkerRunScoped,
    State(state): State<Arc<AppState>>,
    Json(request): Json<PetriOpenRequest>,
) -> Response {
    let access = match (request.access, request.owner) {
        (PetriAccess::Create, Some(owner)) => Access::Create {
            owner: OwnerId::new(owner),
        },
        (PetriAccess::Write, Some(owner)) => Access::Write {
            owner: OwnerId::new(owner),
        },
        (PetriAccess::Read, _) => Access::Read,
        (PetriAccess::Create | PetriAccess::Write, None) => {
            return ApiError::bad_request("`owner` is required to open a Petri run for writing.")
                .into_response();
        }
    };
    match state.petri_runs.open(id, access).await {
        Ok(handle) => Json(PetriOpenResponse {
            locator: handle.locator(),
        })
        .into_response(),
        Err(err) => store_error_response(id, &err),
    }
}

async fn release_run(
    RequireWorkerRunScoped(id): RequireWorkerRunScoped,
    State(state): State<Arc<AppState>>,
    Json(request): Json<PetriReleaseRequest>,
) -> Response {
    state.petri_runs.release(id, &OwnerId::new(request.owner));
    StatusCode::NO_CONTENT.into_response()
}

async fn list_records(
    RequireWorkerRunSegment(id, log): RequireWorkerRunSegment,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(log) = parse_log_id(&log) else {
        return unknown_log(&log);
    };
    let reader = match state.petri_runs.reader(id).await {
        Ok(reader) => reader,
        Err(err) => return store_error_response(id, &err),
    };
    let records = match reader.read(&log).await {
        Ok(records) => records,
        Err(err) => return store_error_response(id, &err),
    };
    match records
        .into_iter()
        .map(wire_record)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(records) => Json(PetriRecordList { records }).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn append_records(
    RequireWorkerRunSegment(id, log): RequireWorkerRunSegment,
    State(state): State<Arc<AppState>>,
    Json(request): Json<PetriAppendRequest>,
) -> Response {
    let Some(log) = parse_log_id(&log) else {
        return unknown_log(&log);
    };
    let records = match request
        .records
        .into_iter()
        .map(stored_record)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(records) => records,
        Err(err) => return err.into_response(),
    };
    let writer = match state
        .petri_runs
        .writer(id, &OwnerId::new(request.owner))
        .await
    {
        Ok(writer) => writer,
        Err(err) => return store_error_response(id, &err),
    };
    match writer.append(&log, &records).await {
        Ok(()) => {
            // The records are durable; the projection trails them from here.
            state.petri_projector.signal(id);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => store_error_response(id, &err),
    }
}

async fn write_blob(
    RequireWorkerRunScoped(id): RequireWorkerRunScoped,
    State(state): State<Arc<AppState>>,
    Query(query): Query<OwnerQuery>,
    body: Bytes,
) -> Response {
    let writer = match state
        .petri_runs
        .writer(id, &OwnerId::new(query.owner))
        .await
    {
        Ok(writer) => writer,
        Err(err) => return store_error_response(id, &err),
    };
    let digest = match writer.put_blob(&body).await {
        Ok(digest) => digest,
        Err(err) => return store_error_response(id, &err),
    };
    match digest.to_hex().parse::<BlobHash>() {
        Ok(hash) => Json(WriteBlobResponse { hash }).into_response(),
        Err(err) => ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("The stored blob's digest is not a blob hash: {err}"),
            "petri_store_failed",
        )
        .into_response(),
    }
}

async fn read_blob(
    RequireWorkerRunSegment(id, blob_hash): RequireWorkerRunSegment,
    State(state): State<Arc<AppState>>,
) -> Response {
    let digest = match blob_hash
        .parse::<BlobHash>()
        .map(|hash| hash.to_string().parse::<Digest>())
    {
        Ok(Ok(digest)) => digest,
        Ok(Err(err)) => return ApiError::bad_request(err.to_string()).into_response(),
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let reader = match state.petri_runs.reader(id).await {
        Ok(reader) => reader,
        Err(err) => return store_error_response(id, &err),
    };
    match reader.get_blob(digest).await {
        Ok(Some(bytes)) => octet_stream_response(Bytes::from(bytes)),
        Ok(None) => ApiError::with_code(
            StatusCode::NOT_FOUND,
            "The run holds no blob with this digest.",
            "petri_blob_not_found",
        )
        .into_response(),
        Err(err) => store_error_response(id, &err),
    }
}

/// The run's platform records, of one kind when the query names it.
async fn list_platform_records(
    RequireWorkerRunScoped(id): RequireWorkerRunScoped,
    State(state): State<Arc<AppState>>,
    Query(query): Query<KindQuery>,
) -> Response {
    let store = state.stores.run_summaries.platform_records();
    let records = match query.kind.as_deref() {
        Some(kind) => match kind.parse::<PlatformRecordKind>() {
            Ok(kind) => store.read_kind(&id, kind).await,
            Err(_) => {
                return ApiError::bad_request(format!("`{kind}` is not a platform record kind."))
                    .into_response();
            }
        },
        None => store.read(&id).await,
    };
    match records {
        Ok(records) => match records
            .into_iter()
            .map(|stored| wire_platform_record(&stored))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(records) => Json(PetriPlatformRecordList { records }).into_response(),
            Err(err) => err.into_response(),
        },
        Err(err) => platform_store_error_response(id, &err),
    }
}

/// Store one platform record for the run and wake its projector.
async fn append_platform_record(
    RequireWorkerRunScoped(id): RequireWorkerRunScoped,
    State(state): State<Arc<AppState>>,
    Json(request): Json<PetriPlatformRecordAppendRequest>,
) -> Response {
    let record: PlatformRecord = match serde_json::from_value(Value::Object(request.record)) {
        Ok(record) => record,
        Err(err) => {
            return ApiError::bad_request(format!("Invalid platform record: {err}"))
                .into_response();
        }
    };
    let position = match (request.execution, request.firing) {
        (Some(execution), Some(firing)) => Some(StagePosition { execution, firing }),
        _ => None,
    };
    let summaries = &state.stores.run_summaries;
    match summaries
        .platform_records()
        .append(&id, &record, position)
        .await
    {
        Ok(stored) => {
            summaries.notify_platform_record(id);
            match wire_platform_record(&stored) {
                Ok(record) => Json(record).into_response(),
                Err(err) => err.into_response(),
            }
        }
        Err(err) => platform_store_error_response(id, &err),
    }
}

/// A stored platform record as the wire carries it.
fn wire_platform_record(stored: &StoredPlatformRecord) -> Result<PetriPlatformRecord, ApiError> {
    let record = serde_json::to_value(&stored.record).map_err(|err| {
        ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "The stored platform record at seq {} does not encode: {err}",
                stored.seq
            ),
            "petri_store_failed",
        )
    })?;
    let Value::Object(record) = record else {
        return Err(ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "The stored platform record at seq {} is not a JSON object.",
                stored.seq
            ),
            "petri_store_failed",
        ));
    };
    Ok(PetriPlatformRecord {
        seq: stored.seq,
        recorded_at: stored.recorded_at,
        record,
        execution: stored.position.map(|position| position.execution),
        firing: stored.position.map(|position| position.firing),
    })
}

fn platform_store_error_response(run_id: RunId, err: &fabro_store::Error) -> Response {
    tracing::error!(
        run_id = %run_id,
        error = %collect_chain(err).join(": "),
        "platform record store failed"
    );
    ApiError::with_code(
        StatusCode::INTERNAL_SERVER_ERROR,
        "The platform record store failed; see the server log.",
        "petri_store_failed",
    )
    .into_response()
}

fn unknown_log(log: &str) -> Response {
    ApiError::bad_request(format!(
        "`{log}` is not a Petri log: expected `coordinator`, `resources` or `execution <n>`."
    ))
    .into_response()
}

/// A wire record into the record the store keeps: its JSON, with `seq` and
/// `recorded_at` lifted from it, which must agree with the ones sent
/// beside it.
fn stored_record(wire: PetriRecord) -> Result<Record, ApiError> {
    let record = Record::from_value(Value::Object(wire.record))
        .map_err(|err| ApiError::bad_request(format!("Invalid Petri record: {err}")))?;
    if record.seq != wire.seq || record.recorded_at != wire.recorded_at {
        return Err(ApiError::bad_request(format!(
            "Invalid Petri record: it carries seq {} and recorded_at {}, but was sent as seq {} \
             and recorded_at {}.",
            record.seq, record.recorded_at, wire.seq, wire.recorded_at
        )));
    }
    Ok(record)
}

/// A stored record as the wire carries it. A stored record is always a JSON
/// object; one that is not is the backend's fault.
fn wire_record(record: Record) -> Result<PetriRecord, ApiError> {
    match record.record {
        Value::Object(map) => Ok(PetriRecord {
            seq:         record.seq,
            recorded_at: record.recorded_at,
            record:      map,
        }),
        _ => Err(ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "The stored record at seq {} is not a JSON object.",
                record.seq
            ),
            "petri_store_failed",
        )),
    }
}

/// The store's answer as the worker's client maps it back: a status, a
/// code, and the members the code documents under `meta`.
fn store_error_response(run_id: RunId, err: &StoreError) -> Response {
    let error = match err {
        StoreError::Exists { .. } => {
            ApiError::with_code(StatusCode::CONFLICT, err.to_string(), "petri_run_exists")
        }
        StoreError::NotFound { .. } => ApiError::with_code(
            StatusCode::NOT_FOUND,
            err.to_string(),
            "petri_run_not_found",
        ),
        StoreError::Leased { owner, .. } => ApiError::with_code_and_meta(
            StatusCode::CONFLICT,
            err.to_string(),
            "petri_run_leased",
            members([("owner", json!(owner.as_str()))]),
        ),
        StoreError::StaleOwner => {
            ApiError::with_code(StatusCode::CONFLICT, err.to_string(), "petri_stale_owner")
        }
        StoreError::ReadOnly => {
            ApiError::with_code(StatusCode::CONFLICT, err.to_string(), "petri_read_only")
        }
        StoreError::Conflict { log, seq } => ApiError::with_code_and_meta(
            StatusCode::CONFLICT,
            err.to_string(),
            "petri_record_conflict",
            members([("log", json!(log_id_text(log))), ("seq", json!(seq))]),
        ),
        StoreError::Backend { .. } => {
            tracing::error!(
                run_id = %run_id,
                error = %collect_chain(err).join(": "),
                "Petri run store failed"
            );
            ApiError::with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The Petri run store failed; see the server log.",
                "petri_store_failed",
            )
        }
    };
    error.into_response()
}

fn members<const N: usize>(members: [(&str, Value); N]) -> Map<String, Value> {
    members
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect()
}
