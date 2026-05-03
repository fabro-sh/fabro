use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use fabro_api::types::{RunManifest, RunStatusResponse, SubmitAnswerRequest};
use fabro_config::Storage;
use fabro_interview::AnswerSubmission;
use fabro_types::{
    CommandOutputStream, Principal, RunClientProvenance, RunId, RunProvenance, RunServerProvenance,
    UserPrincipal, parse_blob_ref,
};
use fabro_util::version::FABRO_VERSION;
use fabro_workflow::command_log::{command_log_path, read_json_string_blob, read_log_slice};
use fabro_workflow::run_status::RunStatus;
use fabro_workflow::{Error as WorkflowError, operations};
use tokio::fs;
use tracing::info;

use super::super::{
    AppState, ListResponse, MAX_PAGE_OFFSET, PaginationParams, RunExecutionMode,
    answer_from_request, api_question_from_pending_interview, default_page_limit,
    delete_run_internal, load_pending_interview, managed_run, parse_run_id_path,
    reject_if_archived, resolve_interp_string, submit_pending_interview_answer,
};
use crate::error::ApiError;
use crate::principal_middleware::{
    RequestAuth, RequireCommandLog, RequireRunScoped, RequiredUser, require_user,
};
use crate::run_files::list_run_files;
use crate::run_manifest;
use crate::run_selector::{ResolveRunError, resolve_run_by_selector};

pub(super) fn manifest_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/preflight", post(run_preflight))
        .route("/validate", post(validate_run_manifest))
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs", get(list_runs).post(create_run))
        .route("/runs/resolve", get(resolve_run))
        .route("/boards/runs", get(list_board_runs))
        .route("/runs/{id}", get(get_run_status).delete(delete_run))
        .route("/runs/{id}/questions", get(get_questions))
        .route("/runs/{id}/questions/{qid}/answer", post(submit_answer))
        .route("/runs/{id}/state", get(get_run_state))
        .route("/runs/{id}/logs", get(get_run_logs))
        .route(
            "/runs/{id}/stages/{stageId}/logs/{stream}",
            get(get_run_stage_command_log),
        )
        .route("/runs/{id}/settings", get(get_run_settings))
        .route("/runs/{id}/files", get(list_run_files))
        .merge(manifest_routes())
}

#[derive(serde::Deserialize)]
struct ListRunsParams {
    #[serde(rename = "page[limit]", default = "default_page_limit")]
    limit:            u32,
    #[serde(rename = "page[offset]", default)]
    offset:           u32,
    #[serde(default)]
    include_archived: bool,
}

impl ListRunsParams {
    fn pagination(&self) -> PaginationParams {
        PaginationParams {
            limit:  self.limit,
            offset: self.offset,
        }
    }
}

fn board_column(status: RunStatus) -> Option<&'static str> {
    match status {
        RunStatus::Submitted | RunStatus::Queued | RunStatus::Starting => Some("initializing"),
        RunStatus::Running | RunStatus::Paused { .. } => Some("running"),
        RunStatus::Blocked { .. } => Some("blocked"),
        RunStatus::Succeeded { .. } => Some("succeeded"),
        RunStatus::Failed { .. } | RunStatus::Dead => Some("failed"),
        RunStatus::Removing | RunStatus::Archived { .. } => None,
    }
}

pub(crate) fn board_columns() -> serde_json::Value {
    serde_json::json!([
        {"id": "initializing", "name": "Initializing"},
        {"id": "running", "name": "Running"},
        {"id": "blocked", "name": "Blocked"},
        {"id": "succeeded", "name": "Succeeded"},
        {"id": "failed", "name": "Failed"},
    ])
}

async fn board_run_metadata(
    state: &AppState,
    run_id: RunId,
) -> serde_json::Map<String, serde_json::Value> {
    let mut metadata = serde_json::Map::new();
    let Ok(run_store) = state.store.open_run_reader(&run_id).await else {
        return metadata;
    };
    let Ok(run_state) = run_store.state().await else {
        return metadata;
    };

    if let Some(pull_request) = run_state.pull_request {
        metadata.insert(
            "pull_request".to_string(),
            serde_json::json!({
                "number": pull_request.number,
            }),
        );
    }

    if let Some(sandbox) = run_state.sandbox {
        let mut sandbox_metadata = serde_json::Map::new();
        sandbox_metadata.insert(
            "working_directory".to_string(),
            serde_json::json!(sandbox.working_directory),
        );
        if let Some(identifier) = sandbox.identifier {
            sandbox_metadata.insert("id".to_string(), serde_json::json!(identifier));
        }
        metadata.insert(
            "sandbox".to_string(),
            serde_json::Value::Object(sandbox_metadata),
        );
    }

    if let Some((_, record)) =
        run_state
            .pending_interviews
            .iter()
            .min_by(|(left_id, left), (right_id, right)| {
                left.started_at
                    .cmp(&right.started_at)
                    .then_with(|| left_id.cmp(right_id))
            })
    {
        metadata.insert(
            "question".to_string(),
            serde_json::json!({
                "text": record.question.text,
            }),
        );
    }

    metadata
}

fn paginate_items<T>(items: Vec<T>, pagination: &PaginationParams) -> (Vec<T>, bool) {
    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset.min(MAX_PAGE_OFFSET) as usize;
    let mut data: Vec<_> = items.into_iter().skip(offset).take(limit + 1).collect();
    let has_more = data.len() > limit;
    data.truncate(limit);
    (data, has_more)
}

async fn list_board_runs(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => runs,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let board_summaries: Vec<_> = summaries
        .into_iter()
        .filter_map(|summary| {
            let column = board_column(summary.status)?;
            Some((summary, column))
        })
        .collect();
    let (page_summaries, has_more) = paginate_items(board_summaries, &pagination);

    let mut data = Vec::with_capacity(page_summaries.len());
    for (summary, column) in page_summaries {
        let run_id = summary.run_id;
        let mut item =
            serde_json::to_value(&summary).expect("RunSummary serialization is infallible");
        item["column"] = serde_json::json!(column);
        if let Some(object) = item.as_object_mut() {
            object.extend(board_run_metadata(state.as_ref(), run_id).await);
        }
        data.push(item);
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "columns": board_columns(),
            "data": data,
            "meta": { "has_more": has_more }
        })),
    )
        .into_response()
}

async fn list_runs(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListRunsParams>,
) -> Response {
    match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => {
            let include_archived = params.include_archived;
            let items = runs
                .into_iter()
                .filter(|summary| {
                    include_archived || !matches!(summary.status, RunStatus::Archived { .. })
                })
                .collect::<Vec<_>>();
            let (data, has_more) = paginate_items(items, &params.pagination());
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "data": data,
                    "meta": { "has_more": has_more }
                })),
            )
                .into_response()
        }
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct ResolveRunQuery {
    selector: String,
}

#[derive(Debug, Default, serde::Deserialize)]
struct DeleteRunQuery {
    #[serde(default)]
    force: bool,
}

fn default_command_log_limit() -> u64 {
    65_536
}

#[derive(Debug, serde::Deserialize)]
struct CommandLogQuery {
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_command_log_limit")]
    limit:  u64,
}

#[derive(Debug, serde::Serialize)]
struct CommandLogResponseBody {
    stream:         CommandOutputStream,
    offset:         u64,
    next_offset:    u64,
    total_bytes:    u64,
    bytes_base64:   String,
    eof:            bool,
    cas_ref:        Option<String>,
    live_streaming: bool,
}

async fn resolve_run(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ResolveRunQuery>,
) -> Response {
    let runs = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => runs,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    match resolve_run_by_selector(
        &runs,
        &query.selector,
        |run| run.run_id.to_string(),
        |run| run.workflow_slug.clone(),
        |run| run.workflow_name.clone(),
        |run| run.run_id.created_at(),
        |run| run.run_id.created_at().to_rfc3339(),
        |run| run.repo_origin_url.clone(),
    ) {
        Ok(run) => (StatusCode::OK, Json(run.clone())).into_response(),
        Err(err @ (ResolveRunError::InvalidSelector | ResolveRunError::AmbiguousPrefix { .. })) => {
            ApiError::bad_request(err.to_string()).into_response()
        }
        Err(err @ ResolveRunError::NotFound { .. }) => {
            ApiError::not_found(err.to_string()).into_response()
        }
    }
}

async fn delete_run(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(query): Query<DeleteRunQuery>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    match delete_run_internal(&state, id, query.force).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(response) => response,
    }
}

async fn create_run(
    RequestAuth(auth_slot): RequestAuth,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let subject = match require_user(&auth_slot) {
        Ok(subject) => subject,
        Err(err) => return err.into_response(),
    };
    let req = match serde_json::from_slice::<RunManifest>(&body) {
        Ok(req) => req,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let manifest_run_defaults = state.manifest_run_defaults();
    let prepared = match run_manifest::prepare_manifest(manifest_run_defaults.as_ref(), &req) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let run_id = prepared.run_id.unwrap_or_else(RunId::new);
    info!(run_id = %run_id, "Run created");

    let web_url = state.run_web_url(&run_id);
    let configured_providers = state.llm_source.configured_providers().await;
    let mut create_input = run_manifest::create_run_input(prepared.clone(), configured_providers);
    create_input.run_id = Some(run_id);
    create_input.provenance = Some(run_provenance(&headers, &subject));
    create_input.submitted_manifest_bytes = Some(body.to_vec());
    create_input.web_url = web_url.clone();

    let storage_root = match resolve_interp_string(&state.server_settings().server.storage.root) {
        Ok(path) => PathBuf::from(path),
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to resolve server storage root: {err}"),
            )
            .into_response();
        }
    };
    let created = match Box::pin(operations::create(
        state.store.as_ref(),
        create_input,
        storage_root,
    ))
    .await
    {
        Ok(created) => created,
        Err(WorkflowError::ValidationFailed { .. } | WorkflowError::Parse(_)) => {
            return ApiError::bad_request("Validation failed").into_response();
        }
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to persist run state: {err}"),
            )
            .into_response();
        }
    };
    let created_at = created.run_id.created_at();

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            created.run_id,
            managed_run(
                created.persisted.source().to_string(),
                RunStatus::Submitted,
                created_at,
                created.run_dir,
                RunExecutionMode::Start,
            ),
        );
    }

    (
        StatusCode::CREATED,
        Json(RunStatusResponse {
            id: run_id.to_string(),
            status: RunStatus::Submitted,
            error: None,
            queue_position: None,
            pending_control: None,
            created_at,
            web_url,
        }),
    )
        .into_response()
}

fn run_provenance(headers: &HeaderMap, subject: &UserPrincipal) -> RunProvenance {
    RunProvenance {
        server:  Some(RunServerProvenance {
            version: FABRO_VERSION.to_string(),
        }),
        client:  run_client_provenance(headers),
        subject: Some(Principal::User(subject.clone())),
    }
}

fn run_client_provenance(headers: &HeaderMap) -> Option<RunClientProvenance> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)?;
    let (name, version) = parse_known_fabro_user_agent(&user_agent)
        .map_or((None, None), |(name, version)| {
            (Some(name.to_string()), Some(version.to_string()))
        });
    Some(RunClientProvenance {
        user_agent: Some(user_agent),
        name,
        version,
    })
}

fn parse_known_fabro_user_agent(user_agent: &str) -> Option<(&str, &str)> {
    let token = user_agent.split_whitespace().next()?;
    let (name, version) = token.split_once('/')?;
    if version.is_empty() {
        return None;
    }
    match name {
        "fabro-cli" | "fabro-web" => Some((name, version)),
        _ => None,
    }
}

async fn run_preflight(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunManifest>,
) -> Response {
    let manifest_run_defaults = state.manifest_run_defaults();
    let prepared = match run_manifest::prepare_manifest(manifest_run_defaults.as_ref(), &req) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let validated = match run_manifest::validate_prepared_manifest(&prepared) {
        Ok(validated) => validated,
        Err(WorkflowError::Parse(_)) => {
            return ApiError::bad_request("Validation failed").into_response();
        }
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let response = match run_manifest::run_preflight(&state, &prepared, &validated).await {
        Ok((response, _ok)) => response,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    (StatusCode::OK, Json(response)).into_response()
}

async fn validate_run_manifest(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunManifest>,
) -> Response {
    let manifest_run_defaults = state.manifest_run_defaults();
    let prepared = match run_manifest::prepare_manifest(manifest_run_defaults.as_ref(), &req) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let validated = match run_manifest::validate_prepared_manifest(&prepared) {
        Ok(validated) => validated,
        Err(WorkflowError::Parse(_)) => {
            return ApiError::bad_request("Validation failed").into_response();
        }
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    (
        StatusCode::OK,
        Json(run_manifest::validate_response(&prepared, &validated)),
    )
        .into_response()
}

async fn get_run_status(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => match runs.into_iter().find(|run| run.run_id == id) {
            Some(run) => (StatusCode::OK, Json(run)).into_response(),
            None => ApiError::not_found("Run not found.").into_response(),
        },
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn get_run_settings(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let run_store = match state.store.open_run_reader(&id).await {
        Ok(store) => store,
        Err(fabro_store::Error::RunNotFound(_)) => {
            return ApiError::not_found("Run not found.").into_response();
        }
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let run_state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let Some(run_spec) = run_state.spec else {
        return ApiError::not_found("Run not found.").into_response();
    };
    (StatusCode::OK, Json(run_spec.settings)).into_response()
}

async fn get_questions(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => {
                let questions = run_state
                    .pending_interviews
                    .values()
                    .map(api_question_from_pending_interview)
                    .collect::<Vec<_>>();
                (StatusCode::OK, Json(ListResponse::new(questions))).into_response()
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(fabro_store::Error::RunNotFound(_)) => {
            ApiError::not_found("Run not found.").into_response()
        }
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn submit_answer(
    auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((id, qid)): Path<(String, String)>,
    Json(req): Json<SubmitAnswerRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let pending = match load_pending_interview(state.as_ref(), id, &qid).await {
        Ok(pending) => pending,
        Err(response) => return response,
    };
    let answer = match answer_from_request(req, &pending.question) {
        Ok(answer) => answer,
        Err(response) => return response,
    };
    let submission = AnswerSubmission::new(answer, Principal::User(auth.0));
    match submit_pending_interview_answer(state.as_ref(), &pending, submission).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(response) => response,
    }
}

async fn get_run_state(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => Json(run_state).into_response(),
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn get_run_logs(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    if state.store.open_run_reader(&id).await.is_err() {
        return ApiError::not_found("Run not found.").into_response();
    }

    let path = Storage::new(state.server_storage_dir())
        .run_scratch(&id)
        .runtime_dir()
        .join("server.log");
    match fs::read(&path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], bytes).into_response(),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            ApiError::not_found("Run log not available.").into_response()
        }
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn get_run_stage_command_log(
    RequireCommandLog(id, stage_id, stream): RequireCommandLog,
    State(state): State<Arc<AppState>>,
    Query(query): Query<CommandLogQuery>,
) -> Response {
    const MAX_COMMAND_LOG_LIMIT: u64 = 1_048_576;

    if query.limit == 0 {
        return ApiError::bad_request("limit must be greater than 0").into_response();
    }
    let limit = query.limit.min(MAX_COMMAND_LOG_LIMIT);
    let Ok(run_store) = state.store.open_run_reader(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let run_state = match run_store.state().await {
        Ok(run_state) => run_state,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let Some(node) = run_state.stage(&stage_id) else {
        return ApiError::not_found("Stage not found.").into_response();
    };

    let stream_value = match stream {
        CommandOutputStream::Stdout => node.stdout.as_deref(),
        CommandOutputStream::Stderr => node.stderr.as_deref(),
    };
    let cas_ref = stream_value
        .filter(|value| parse_blob_ref(value).is_some())
        .map(str::to_string);
    let live_streaming = node
        .live_streaming
        .unwrap_or_else(|| cas_ref.is_none() && node.completion.is_none());
    let run_dir = Storage::new(state.server_storage_dir())
        .run_scratch(&id)
        .root()
        .to_path_buf();
    let scratch_path = command_log_path(&run_dir, &stage_id, stream);

    match read_log_slice(&scratch_path, query.offset, limit).await {
        Ok((bytes, total_bytes)) => {
            return build_command_log_response(
                stream,
                query.offset,
                limit,
                LogSource::Sliced { bytes, total_bytes },
                cas_ref.is_some(),
                cas_ref,
                live_streaming,
            );
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    if let Some(cas_ref) = cas_ref {
        let text = match read_json_string_blob(&run_store.clone().into(), &cas_ref).await {
            Ok(Some(text)) => text,
            Ok(None) => String::new(),
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        };
        return build_command_log_response(
            stream,
            query.offset,
            limit,
            LogSource::Full(text.as_bytes()),
            true,
            Some(cas_ref),
            live_streaming,
        );
    }

    if let Some(inline_text) = stream_value {
        return build_command_log_response(
            stream,
            query.offset,
            limit,
            LogSource::Full(inline_text.as_bytes()),
            true,
            None,
            live_streaming,
        );
    }

    build_command_log_response(
        stream,
        query.offset,
        limit,
        LogSource::Full(&[]),
        node.completion.is_some(),
        None,
        live_streaming,
    )
}

enum LogSource<'a> {
    Sliced {
        bytes:       Vec<u8>,
        total_bytes: u64,
    },
    Full(&'a [u8]),
}

fn build_command_log_response(
    stream: CommandOutputStream,
    requested_offset: u64,
    limit: u64,
    source: LogSource<'_>,
    eof: bool,
    cas_ref: Option<String>,
    live_streaming: bool,
) -> Response {
    let (body_bytes, total_bytes, offset) = match source {
        LogSource::Sliced { bytes, total_bytes } => {
            let offset = requested_offset.min(total_bytes);
            (bytes, total_bytes, offset)
        }
        LogSource::Full(bytes) => {
            let total_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let offset = requested_offset.min(total_bytes);
            let start = usize::try_from(offset).unwrap_or(bytes.len());
            let end = start
                .saturating_add(usize::try_from(limit).unwrap_or(usize::MAX))
                .min(bytes.len());
            (bytes[start..end].to_vec(), total_bytes, offset)
        }
    };
    Json(CommandLogResponseBody {
        stream,
        offset,
        next_offset: offset + u64::try_from(body_bytes.len()).unwrap_or(u64::MAX),
        total_bytes,
        bytes_base64: BASE64_STANDARD.encode(body_bytes),
        eof,
        cas_ref,
        live_streaming,
    })
    .into_response()
}
