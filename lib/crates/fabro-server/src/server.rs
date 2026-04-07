use std::collections::{HashMap, HashSet};
use std::path::{Component, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::bind::Bind;
#[cfg(test)]
use axum::body::to_bytes;
use axum::extract::{self as axum_extract, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use axum_extra::extract::cookie::Key;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use fabro_config::Storage;
use fabro_llm::client::Client as LlmClient;
use fabro_llm::generate::{GenerateParams, generate_object};
use fabro_llm::model_test::{ModelTestMode, run_model_test_with_client};
use fabro_llm::types::{
    ContentPart, FinishReason, Message as LlmMessage, Request as LlmRequest,
    Response as LlmResponse, Role, StreamEvent, ToolChoice, ToolDefinition, Usage,
};
use fabro_store::{ArtifactStore, Database, EventEnvelope, EventPayload, StageId};
use fabro_types::{
    RunBlobId, RunClientProvenance, RunControlAction, RunEvent, RunId, RunProvenance,
    RunServerProvenance, RunSubjectProvenance, Settings,
};
use fabro_util::redact::redact_jsonl_line;
use fabro_util::version::FABRO_VERSION;
use fabro_workflow::artifacts as workflow_artifacts;
use fabro_workflow::error::FabroError;
use fabro_workflow::handler::HandlerRegistry;
use futures_util::stream;
use object_store::memory::InMemory as MemoryObjectStore;
use tempfile::NamedTempFile;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Notify;
use tokio::sync::RwLock as AsyncRwLock;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::oneshot;
use tokio::task::spawn_blocking;
use tokio::time::sleep;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower::{ServiceExt, service_fn};
use ulid::Ulid;

use tracing::{error, info};

use crate::demo;
use crate::diagnostics;
use crate::error::ApiError;
use crate::jwt_auth::{AuthMode, AuthenticatedService, AuthenticatedSubject};
use crate::run_manifest;
use crate::secret_store::{SecretStore, SecretStoreError};
use crate::static_files;
use crate::web_auth;
use fabro_interview::{Answer, Interviewer, Question, QuestionType, WebInterviewer};
use fabro_sandbox::daytona::DaytonaSandbox;
use fabro_sandbox::reconnect::reconnect;
use fabro_sandbox::{Sandbox, SandboxProvider};
use fabro_workflow::event::{self as workflow_event, Emitter};
use fabro_workflow::operations::{self};
use fabro_workflow::pipeline::Persisted;
use fabro_workflow::records::Checkpoint;
use fabro_workflow::run_lookup::{
    RunInfo, StatusFilter, filter_runs, scan_runs_with_summaries, scratch_base,
};
use fabro_workflow::run_status::RunStatus as WorkflowRunStatus;
use fabro_workflow::run_status::StatusReason as WorkflowStatusReason;

use fabro_api::types::AggregateUsageTotals;
pub use fabro_api::types::{
    AggregateUsage, ApiQuestion, ApiQuestionOption, AppendEventResponse, ArtifactEntry,
    ArtifactListResponse, CompletionContentPart, CompletionMessage, CompletionMessageRole,
    CompletionResponse, CompletionToolChoiceMode, CompletionUsage, CreateCompletionRequest,
    DiskUsageResponse, DiskUsageRunRow, DiskUsageSummaryRow, EventEnvelope as ApiEventEnvelope,
    ModelReference, PaginatedEventList, PaginatedRunList, PaginationMeta, PreflightResponse,
    PreviewUrlRequest, PreviewUrlResponse, PruneRunEntry, PruneRunsRequest, PruneRunsResponse,
    QuestionType as ApiQuestionType, RenderWorkflowGraphDirection, RenderWorkflowGraphFormat,
    RenderWorkflowGraphRequest, RunArtifactEntry, RunArtifactListResponse,
    RunControlAction as ApiRunControlAction, RunError, RunEvent as ApiRunEvent, RunManifest,
    RunStatus, RunStatusResponse, SandboxFileEntry, SandboxFileListResponse, ServerSettings,
    SetSecretRequest, SshAccessRequest, SshAccessResponse, StartRunRequest,
    StatusReason as ApiStatusReason, SubmitAnswerRequest, SystemInfoResponse, SystemRunCounts,
    TokenUsage, UsageByModel, WriteBlobResponse,
};
use fabro_graphviz::render::GraphFormat;

pub fn default_page_limit() -> u32 {
    20
}

#[derive(serde::Deserialize)]
pub struct PaginationParams {
    #[serde(rename = "page[limit]", default = "default_page_limit")]
    pub limit: u32,
    #[serde(rename = "page[offset]", default)]
    pub offset: u32,
}

#[derive(serde::Deserialize)]
struct ModelListParams {
    #[serde(rename = "page[limit]", default = "default_page_limit")]
    limit: u32,
    #[serde(rename = "page[offset]", default)]
    offset: u32,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    query: Option<String>,
}

#[derive(serde::Deserialize)]
struct ModelTestParams {
    #[serde(default)]
    mode: Option<String>,
}

#[derive(serde::Deserialize)]
struct EventListParams {
    #[serde(default)]
    since_seq: Option<u32>,
    #[serde(default)]
    limit: Option<usize>,
}

impl EventListParams {
    fn since_seq(&self) -> u32 {
        self.since_seq.unwrap_or(1).max(1)
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 1000)
    }
}

#[derive(serde::Deserialize)]
struct AttachParams {
    #[serde(default)]
    since_seq: Option<u32>,
}

#[derive(serde::Deserialize)]
pub(crate) struct DfParams {
    #[serde(default)]
    pub(crate) verbose: bool,
}

#[derive(serde::Deserialize)]
struct GlobalAttachParams {
    #[serde(default)]
    run_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct ArtifactFilenameParams {
    #[serde(default)]
    filename: Option<String>,
}

#[derive(serde::Deserialize)]
struct SandboxFilesParams {
    path: String,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(serde::Deserialize)]
struct SandboxFileParams {
    path: String,
}

/// Non-paginated list response wrapper with `has_more: false`.
#[derive(serde::Serialize)]
pub struct ListResponse<T: serde::Serialize> {
    data: T,
    meta: PaginationMeta,
}

impl<T: serde::Serialize> ListResponse<T> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            meta: PaginationMeta { has_more: false },
        }
    }
}

/// Snapshot of a managed run.
struct ManagedRun {
    dot_source: String,
    status: RunStatus,
    error: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    enqueued_at: Instant,
    // Populated when running:
    interviewer: Option<Arc<WebInterviewer>>,
    event_tx: Option<broadcast::Sender<RunEvent>>,
    checkpoint: Option<Checkpoint>,
    cancel_tx: Option<oneshot::Sender<()>>,
    cancel_token: Option<Arc<AtomicBool>>,
    worker_pid: Option<u32>,
    worker_pgid: Option<u32>,
    run_dir: Option<std::path::PathBuf>,
    execution_mode: RunExecutionMode,
}

#[derive(Clone, Copy)]
enum RunExecutionMode {
    Start,
    Resume,
}

enum ExecutionResult {
    Completed(Box<Result<operations::Started, FabroError>>),
    CancelledBySignal,
}

const FILE_INTERVIEW_QUESTION_ID: &str = "q-file";
const WORKER_STDERR_LOG: &str = "worker.stderr.log";
const WORKER_CANCEL_GRACE: Duration = Duration::from_secs(5);

/// Per-model usage totals.
#[derive(Default)]
struct ModelUsageTotals {
    stages: i64,
    input_tokens: i64,
    output_tokens: i64,
    cost: f64,
}

/// In-memory aggregate usage counters, reset on server restart.
#[derive(Default)]
struct UsageAccumulator {
    total_runs: i64,
    total_runtime_secs: f64,
    by_model: HashMap<String, ModelUsageTotals>,
}

type RegistryFactoryOverride = dyn Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync;

/// Shared application state for the server.
pub struct AppState {
    runs: Mutex<HashMap<RunId, ManagedRun>>,
    aggregate_usage: Mutex<UsageAccumulator>,
    store: Arc<Database>,
    artifact_store: ArtifactStore,
    started_at: Instant,
    max_concurrent_runs: usize,
    scheduler_notify: Notify,
    global_event_tx: broadcast::Sender<EventEnvelope>,

    pub(crate) secret_store: AsyncRwLock<SecretStore>,
    pub(crate) settings: Arc<RwLock<Settings>>,
    pub(crate) config_path: PathBuf,
    pub(crate) local_daemon_mode: bool,
    shutting_down: AtomicBool,
    registry_factory_override: Option<Box<RegistryFactoryOverride>>,
}

impl AppState {
    pub(crate) fn dry_run(&self) -> bool {
        self.settings.read().unwrap().dry_run_enabled()
    }

    pub(crate) async fn build_llm_client(&self) -> Result<LlmClient, String> {
        let snapshot = self.secret_store.read().await.snapshot();
        LlmClient::from_lookup(|name| {
            snapshot
                .get(name)
                .cloned()
                .or_else(|| std::env::var(name).ok())
        })
        .await
        .map_err(|err| err.to_string())
    }

    pub(crate) fn secret_or_env(&self, name: &str) -> Option<String> {
        self.secret_store
            .try_read()
            .ok()
            .and_then(|store| store.get(name).map(str::to_string))
            .or_else(|| std::env::var(name).ok())
    }

    pub(crate) async fn session_key(&self) -> Option<Key> {
        let secret = self
            .secret_store
            .read()
            .await
            .get("SESSION_SECRET")
            .map(str::to_string);
        secret
            .or_else(|| std::env::var("SESSION_SECRET").ok())
            .map(|value| Key::derive_from(value.as_bytes()))
    }

    pub(crate) async fn github_app_credentials(
        &self,
        app_id: Option<&str>,
    ) -> Result<Option<fabro_github::GitHubAppCredentials>, String> {
        let Some(app_id) = app_id else {
            return Ok(None);
        };
        let raw = self
            .secret_store
            .read()
            .await
            .get("GITHUB_APP_PRIVATE_KEY")
            .map(str::to_string)
            .or_else(|| std::env::var("GITHUB_APP_PRIVATE_KEY").ok());
        let Some(raw) = raw else {
            return Ok(None);
        };
        let private_key_pem = decode_secret_pem("GITHUB_APP_PRIVATE_KEY", &raw)?;
        Ok(Some(fabro_github::GitHubAppCredentials {
            app_id: app_id.to_string(),
            private_key_pem,
        }))
    }

    fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Relaxed);
        self.scheduler_notify.notify_waiters();
    }

    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Relaxed)
    }
}

fn decode_secret_pem(name: &str, raw: &str) -> Result<String, String> {
    if raw.starts_with("-----") {
        return Ok(raw.to_string());
    }
    let pem_bytes = BASE64_STANDARD
        .decode(raw)
        .map_err(|err| format!("{name} is not valid PEM or base64: {err}"))?;
    String::from_utf8(pem_bytes)
        .map_err(|err| format!("{name} base64 decoded to invalid UTF-8: {err}"))
}

/// Build the axum Router with all run endpoints and embedded static assets.
pub fn build_router(state: Arc<AppState>, auth_mode: AuthMode) -> Router {
    let middleware_state = Arc::clone(&state);
    let api_common = Router::new()
        .route("/openapi.json", get(openapi_spec))
        .merge(web_auth::api_routes());

    let demo_router = Router::new()
        .nest("/api/v1", api_common.clone().merge(demo_routes()))
        .layer(axum::Extension(AuthMode::Disabled))
        .with_state(state.clone());

    let real_router = Router::new()
        .nest("/api/v1", api_common.merge(real_routes()))
        .nest("/auth", web_auth::routes())
        .layer(axum::Extension(auth_mode))
        .with_state(state);

    let dispatch = service_fn(move |req: axum_extract::Request| {
        let demo = demo_router.clone();
        let real = real_router.clone();
        async move {
            if req.headers().get("x-fabro-demo").is_some_and(|v| v == "1") {
                demo.oneshot(req).await
            } else {
                real.oneshot(req).await
            }
        }
    });

    Router::new()
        .route("/health", get(health))
        .layer(middleware::from_fn_with_state(
            middleware_state,
            cookie_and_demo_middleware,
        ))
        .fallback_service(service_fn(move |req: axum_extract::Request| {
            let dispatch = dispatch.clone();
            async move {
                let path = req.uri().path().to_string();
                if path.starts_with("/api/v1/") || path.starts_with("/auth/") || path == "/health" {
                    dispatch.oneshot(req).await
                } else if matches!(req.method(), &Method::GET | &Method::HEAD) {
                    Ok::<_, std::convert::Infallible>(static_files::serve(&path))
                } else {
                    Ok::<_, std::convert::Infallible>(StatusCode::NOT_FOUND.into_response())
                }
            }
        }))
}

fn demo_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs", get(demo::list_runs).post(demo::create_run_stub))
        .route("/preflight", post(run_preflight))
        .route("/graph/render", post(render_graph_from_manifest))
        .route("/attach", get(demo::attach_events_stub))
        .route("/runs/{id}", get(demo::get_run_status))
        .route("/runs/{id}/questions", get(demo::get_questions_stub))
        .route("/runs/{id}/questions/{qid}/answer", post(demo::answer_stub))
        .route("/runs/{id}/state", get(not_implemented))
        .route(
            "/runs/{id}/events",
            get(not_implemented).post(not_implemented),
        )
        .route("/runs/{id}/attach", get(demo::run_events_stub))
        .route("/runs/{id}/blobs", post(not_implemented))
        .route("/runs/{id}/blobs/{blobId}", get(not_implemented))
        .route("/runs/{id}/checkpoint", get(demo::checkpoint_stub))
        .route("/runs/{id}/cancel", post(demo::cancel_stub))
        .route("/runs/{id}/start", post(demo::start_run_stub))
        .route("/runs/{id}/pause", post(demo::pause_stub))
        .route("/runs/{id}/unpause", post(demo::unpause_stub))
        .route("/runs/{id}/graph", get(demo::get_run_graph))
        .route("/runs/{id}/stages", get(demo::get_run_stages))
        .route("/runs/{id}/artifacts", get(demo::list_run_artifacts_stub))
        .route(
            "/runs/{id}/stages/{stageId}/turns",
            get(demo::get_stage_turns),
        )
        .route(
            "/runs/{id}/stages/{stageId}/artifacts",
            get(not_implemented).post(not_implemented),
        )
        .route(
            "/runs/{id}/stages/{stageId}/artifacts/download",
            get(not_implemented),
        )
        .route("/runs/{id}/usage", get(demo::get_run_usage))
        .route("/runs/{id}/settings", get(demo::get_run_settings))
        .route("/runs/{id}/preview", post(demo::generate_preview_url_stub))
        .route("/runs/{id}/ssh", post(demo::create_ssh_access_stub))
        .route(
            "/runs/{id}/sandbox/files",
            get(demo::list_sandbox_files_stub),
        )
        .route(
            "/runs/{id}/sandbox/file",
            get(demo::get_sandbox_file_stub).put(demo::put_sandbox_file_stub),
        )
        .route(
            "/insights/queries",
            get(demo::list_saved_queries).post(demo::save_query_stub),
        )
        .route(
            "/insights/queries/{id}",
            get(demo::get_saved_query)
                .put(demo::update_query_stub)
                .delete(demo::delete_query_stub),
        )
        .route("/insights/execute", post(demo::execute_query_stub))
        .route("/insights/history", get(demo::list_query_history))
        .route("/models", get(list_models))
        .route("/models/{id}/test", post(test_model))
        .route("/secrets", get(demo::list_secrets))
        .route(
            "/secrets/{name}",
            put(demo::set_secret).delete(demo::delete_secret),
        )
        .route("/repos/github/{owner}/{name}", get(demo::get_github_repo))
        .route("/health/diagnostics", post(demo::run_diagnostics))
        .route("/completions", post(create_completion))
        .route("/settings", get(demo::get_server_settings))
        .route("/system/info", get(demo::get_system_info))
        .route("/system/df", get(demo::get_system_disk_usage))
        .route("/system/prune/runs", post(demo::prune_runs))
        .route("/usage", get(demo::get_aggregate_usage))
}

fn real_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs", get(list_runs).post(create_run))
        .route("/preflight", post(run_preflight))
        .route("/graph/render", post(render_graph_from_manifest))
        .route("/attach", get(attach_events))
        .route("/boards/runs", get(list_board_runs))
        .route("/runs/{id}", get(get_run_status).delete(delete_run))
        .route("/runs/{id}/questions", get(get_questions))
        .route("/runs/{id}/questions/{qid}/answer", post(submit_answer))
        .route("/runs/{id}/state", get(get_run_state))
        .route(
            "/runs/{id}/events",
            get(list_run_events).post(append_run_event),
        )
        .route("/runs/{id}/attach", get(attach_run_events))
        .route("/runs/{id}/blobs", post(write_run_blob))
        .route("/runs/{id}/blobs/{blobId}", get(read_run_blob))
        .route("/runs/{id}/checkpoint", get(get_checkpoint))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/start", post(start_run))
        .route("/runs/{id}/pause", post(pause_run))
        .route("/runs/{id}/unpause", post(unpause_run))
        .route("/runs/{id}/graph", get(get_graph))
        .route("/runs/{id}/stages", get(not_implemented))
        .route("/runs/{id}/artifacts", get(list_run_artifacts))
        .route("/runs/{id}/stages/{stageId}/turns", get(not_implemented))
        .route(
            "/runs/{id}/stages/{stageId}/artifacts",
            get(list_stage_artifacts).post(put_stage_artifact),
        )
        .route(
            "/runs/{id}/stages/{stageId}/artifacts/download",
            get(get_stage_artifact),
        )
        .route("/runs/{id}/usage", get(not_implemented))
        .route("/runs/{id}/settings", get(not_implemented))
        .route("/runs/{id}/steer", post(not_implemented))
        .route("/runs/{id}/preview", post(generate_preview_url))
        .route("/runs/{id}/ssh", post(create_ssh_access))
        .route("/runs/{id}/sandbox/files", get(list_sandbox_files))
        .route(
            "/runs/{id}/sandbox/file",
            get(get_sandbox_file).put(put_sandbox_file),
        )
        .route("/workflows", get(not_implemented))
        .route("/workflows/{name}", get(not_implemented))
        .route("/workflows/{name}/runs", get(not_implemented))
        .route(
            "/insights/queries",
            get(not_implemented).post(not_implemented),
        )
        .route(
            "/insights/queries/{id}",
            get(not_implemented)
                .put(not_implemented)
                .delete(not_implemented),
        )
        .route("/insights/execute", post(not_implemented))
        .route("/insights/history", get(not_implemented))
        .route("/models", get(list_models))
        .route("/models/{id}/test", post(test_model))
        .route("/secrets", get(list_secrets))
        .route("/secrets/{name}", put(set_secret).delete(delete_secret))
        .route("/repos/github/{owner}/{name}", get(get_github_repo))
        .route("/health/diagnostics", post(run_diagnostics))
        .route("/completions", post(create_completion))
        .route("/settings", get(get_server_settings))
        .route("/system/info", get(get_system_info))
        .route("/system/df", get(get_system_df))
        .route("/system/prune/runs", post(prune_runs))
        .route("/usage", get(get_aggregate_usage))
}

async fn not_implemented() -> Response {
    ApiError::new(StatusCode::NOT_IMPLEMENTED, "Not implemented.").into_response()
}

async fn health() -> Response {
    Json(serde_json::json!({
        "status": "ok",
        "version": FABRO_VERSION,
    }))
    .into_response()
}

async fn get_server_settings(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
) -> Response {
    let settings = state.settings.read().unwrap().clone();
    let response = match api_server_settings(&settings) {
        Ok(response) => response,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    (StatusCode::OK, Json(response)).into_response()
}

fn api_server_settings(settings: &Settings) -> anyhow::Result<ServerSettings> {
    let mut value = serde_json::to_value(settings)?;
    strip_nulls(&mut value);
    serde_json::from_value(value).map_err(Into::into)
}

fn strip_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                strip_nulls(child);
            }
            map.retain(|_, child| !child.is_null());
        }
        serde_json::Value::Array(values) => {
            for child in values {
                strip_nulls(child);
            }
        }
        _ => {}
    }
}

async fn get_system_info(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
) -> Response {
    let settings = state.settings.read().unwrap().clone();
    let (total_runs, active_runs) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        let active = runs
            .values()
            .filter(|run| {
                matches!(
                    run.status,
                    RunStatus::Queued
                        | RunStatus::Starting
                        | RunStatus::Running
                        | RunStatus::Paused
                )
            })
            .count();
        (runs.len(), active)
    };

    let response = SystemInfoResponse {
        version: Some(FABRO_VERSION.to_string()),
        git_sha: option_env!("FABRO_GIT_SHA").map(str::to_string),
        build_date: option_env!("FABRO_BUILD_DATE").map(str::to_string),
        os: Some(std::env::consts::OS.to_string()),
        arch: Some(std::env::consts::ARCH.to_string()),
        storage_engine: Some("slatedb".to_string()),
        storage_dir: Some(settings.storage_dir().display().to_string()),
        uptime_secs: Some(to_i64(state.started_at.elapsed().as_secs())),
        runs: Some(SystemRunCounts {
            total: Some(to_i64(total_runs)),
            active: Some(to_i64(active_runs)),
        }),
        sandbox_provider: Some(system_sandbox_provider(&settings)),
    };
    (StatusCode::OK, Json(response)).into_response()
}

async fn get_system_df(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Query(params): Query<DfParams>,
) -> Response {
    let storage_dir = state.settings.read().unwrap().storage_dir();
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(summaries) => summaries,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let response = match spawn_blocking(move || {
        build_disk_usage_response(&summaries, &storage_dir, params.verbose)
    })
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    (StatusCode::OK, Json(response)).into_response()
}

async fn prune_runs(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(body): Json<PruneRunsRequest>,
) -> Response {
    let storage_dir = state.settings.read().unwrap().storage_dir();
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(summaries) => summaries,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let dry_run = body.dry_run;
    let body_for_plan = body.clone();
    let prune_plan =
        match spawn_blocking(move || build_prune_plan(&body_for_plan, &summaries, &storage_dir))
            .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(err)) => {
                return ApiError::new(StatusCode::BAD_REQUEST, err.to_string()).into_response();
            }
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        };

    if dry_run {
        return (
            StatusCode::OK,
            Json(PruneRunsResponse {
                dry_run: Some(true),
                runs: Some(prune_plan.rows),
                total_count: Some(to_i64(prune_plan.run_ids.len())),
                total_size_bytes: Some(to_i64(prune_plan.total_size_bytes)),
                deleted_count: Some(0),
                freed_bytes: Some(0),
            }),
        )
            .into_response();
    }

    for run_id in &prune_plan.run_ids {
        if let Err(response) = delete_run_internal(&state, *run_id).await {
            return response;
        }
    }

    (
        StatusCode::OK,
        Json(PruneRunsResponse {
            dry_run: Some(false),
            runs: None,
            total_count: Some(to_i64(prune_plan.run_ids.len())),
            total_size_bytes: Some(to_i64(prune_plan.total_size_bytes)),
            deleted_count: Some(to_i64(prune_plan.run_ids.len())),
            freed_bytes: Some(to_i64(prune_plan.total_size_bytes)),
        }),
    )
        .into_response()
}

async fn attach_events(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Query(params): Query<GlobalAttachParams>,
) -> Response {
    let run_filter = match parse_global_run_filter(params.run_id.as_deref()) {
        Ok(filter) => filter,
        Err(err) => return ApiError::new(StatusCode::BAD_REQUEST, err).into_response(),
    };

    let stream =
        BroadcastStream::new(state.global_event_tx.subscribe()).filter_map(move |result| {
            match result {
                Ok(event) => {
                    if !event_matches_run_filter(&event, run_filter.as_ref()) {
                        return None;
                    }
                    sse_event_from_store(&event).map(Ok::<Event, std::convert::Infallible>)
                }
                Err(_) => None,
            }
        });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

struct PrunePlan {
    run_ids: Vec<RunId>,
    rows: Vec<PruneRunEntry>,
    total_size_bytes: u64,
}

fn build_disk_usage_response(
    summaries: &[fabro_store::RunSummary],
    storage_dir: &std::path::Path,
    verbose: bool,
) -> anyhow::Result<DiskUsageResponse> {
    let scratch_base_dir = scratch_base(storage_dir);
    let logs_base_dir = Storage::new(storage_dir).logs_dir();
    let runs = scan_runs_with_summaries(summaries, &scratch_base_dir)?;

    let mut active_count = 0u64;
    let mut total_run_size = 0u64;
    let mut reclaimable_run_size = 0u64;
    let mut run_rows = Vec::new();

    for run in &runs {
        let size = dir_size(&run.path);
        total_run_size += size;
        if run.status().is_active() {
            active_count += 1;
        } else {
            reclaimable_run_size += size;
        }
        if verbose {
            run_rows.push(DiskUsageRunRow {
                run_id: Some(run.run_id().to_string()),
                workflow_name: Some(run.workflow_name()),
                status: Some(run.status().to_string()),
                start_time: Some(run.start_time()),
                size_bytes: Some(to_i64(size)),
                reclaimable: Some(!run.status().is_active()),
            });
        }
    }

    let mut log_count = 0u64;
    let mut total_log_size = 0u64;
    if let Ok(entries) = std::fs::read_dir(logs_base_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|ext| ext != "log") {
                continue;
            }
            if let Ok(metadata) = path.metadata() {
                log_count += 1;
                total_log_size += metadata.len();
            }
        }
    }

    Ok(DiskUsageResponse {
        summary: vec![
            DiskUsageSummaryRow {
                type_: Some("runs".to_string()),
                count: Some(to_i64(runs.len())),
                active: Some(to_i64(active_count)),
                size_bytes: Some(to_i64(total_run_size)),
                reclaimable_bytes: Some(to_i64(reclaimable_run_size)),
            },
            DiskUsageSummaryRow {
                type_: Some("logs".to_string()),
                count: Some(to_i64(log_count)),
                active: None,
                size_bytes: Some(to_i64(total_log_size)),
                reclaimable_bytes: Some(to_i64(total_log_size)),
            },
        ],
        total_size_bytes: Some(to_i64(total_run_size + total_log_size)),
        total_reclaimable_bytes: Some(to_i64(reclaimable_run_size + total_log_size)),
        runs: verbose.then_some(run_rows),
    })
}

fn build_prune_plan(
    request: &PruneRunsRequest,
    summaries: &[fabro_store::RunSummary],
    storage_dir: &std::path::Path,
) -> anyhow::Result<PrunePlan> {
    let scratch_base_dir = scratch_base(storage_dir);
    let runs = scan_runs_with_summaries(summaries, &scratch_base_dir)?;
    let label_filters = request
        .labels
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();

    let mut filtered = filter_runs(
        &runs,
        request.before.as_deref(),
        request.workflow.as_deref(),
        &label_filters,
        request.orphans,
        StatusFilter::All,
    );

    let has_explicit_filters =
        request.before.is_some() || request.workflow.is_some() || !label_filters.is_empty();
    let staleness_threshold = if let Some(duration) = request.older_than.as_deref() {
        Some(parse_system_duration(duration)?)
    } else if !has_explicit_filters {
        Some(chrono::Duration::hours(24))
    } else {
        None
    };

    if let Some(threshold) = staleness_threshold {
        let cutoff = chrono::Utc::now() - threshold;
        filtered.retain(|run| {
            run.end_time
                .or(run.start_time_dt)
                .is_some_and(|time| time < cutoff)
        });
    }

    filtered.retain(|run| !run.status().is_active());

    let rows = filtered
        .iter()
        .map(|run| PruneRunEntry {
            run_id: Some(run.run_id().to_string()),
            dir_name: Some(run.dir_name.clone()),
            workflow_name: Some(run.workflow_name()),
            size_bytes: Some(to_i64(dir_size(&run.path))),
        })
        .collect::<Vec<_>>();
    let total_size_bytes = rows
        .iter()
        .map(|row| row.size_bytes.unwrap_or_default())
        .sum::<i64>()
        .max(0)
        .try_into()
        .unwrap_or_default();

    Ok(PrunePlan {
        run_ids: filtered.iter().map(RunInfo::run_id).collect(),
        rows,
        total_size_bytes,
    })
}

fn system_sandbox_provider(settings: &Settings) -> String {
    settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.provider.clone())
        .unwrap_or_else(|| SandboxProvider::default().to_string())
}

fn parse_system_duration(raw: &str) -> anyhow::Result<chrono::Duration> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "empty duration string");
    let (num_str, unit) = raw.split_at(raw.len().saturating_sub(1));
    let amount = num_str.parse::<u64>()?;
    match unit {
        "h" => Ok(chrono::Duration::hours(
            i64::try_from(amount).unwrap_or(i64::MAX),
        )),
        "d" => Ok(chrono::Duration::days(
            i64::try_from(amount).unwrap_or(i64::MAX),
        )),
        _ => anyhow::bail!("invalid duration unit '{unit}' in '{raw}' (expected 'h' or 'd')"),
    }
}

fn parse_global_run_filter(raw: Option<&str>) -> Result<Option<HashSet<RunId>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };

    let mut run_ids = HashSet::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let run_id = part
            .parse::<RunId>()
            .map_err(|err| format!("invalid run_id '{part}': {err}"))?;
        run_ids.insert(run_id);
    }

    if run_ids.is_empty() {
        Ok(None)
    } else {
        Ok(Some(run_ids))
    }
}

fn event_matches_run_filter(event: &EventEnvelope, run_filter: Option<&HashSet<RunId>>) -> bool {
    let Some(run_filter) = run_filter else {
        return true;
    };
    let Some(run_id) = event
        .payload
        .as_value()
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<RunId>().ok())
    else {
        return false;
    };
    run_filter.contains(&run_id)
}

fn sse_event_from_store(event: &EventEnvelope) -> Option<Event> {
    let event = api_event_envelope_from_store(event).ok()?;
    let data = serde_json::to_string(&event).ok()?;
    let data = redact_jsonl_line(&data);
    Some(Event::default().data(data))
}

fn dir_size(path: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|metadata| metadata.len())
        .sum()
}

fn to_i64<T>(value: T) -> i64
where
    i64: TryFrom<T>,
{
    i64::try_from(value).unwrap_or(i64::MAX)
}

async fn list_secrets(_auth: AuthenticatedService, State(state): State<Arc<AppState>>) -> Response {
    let data = state.secret_store.read().await.list();
    (StatusCode::OK, Json(serde_json::json!({ "data": data }))).into_response()
}

async fn set_secret(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<SetSecretRequest>,
) -> Response {
    let state_for_write = Arc::clone(&state);
    let result = spawn_blocking(move || {
        let mut store = state_for_write.secret_store.blocking_write();
        store.set(&name, &body.value)
    })
    .await;

    match result {
        Ok(Ok(meta)) => (StatusCode::OK, Json(meta)).into_response(),
        Ok(Err(SecretStoreError::InvalidName(_))) => {
            ApiError::bad_request("invalid secret name").into_response()
        }
        Ok(Err(SecretStoreError::Io(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(Err(SecretStoreError::Serde(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(Err(SecretStoreError::NotFound(_))) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secret unexpectedly missing",
        )
        .into_response(),
        Err(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("secret write task failed: {err}"),
        )
        .into_response(),
    }
}

async fn delete_secret(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let state_for_write = Arc::clone(&state);
    let result = spawn_blocking(move || {
        let mut store = state_for_write.secret_store.blocking_write();
        store.remove(&name)
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(SecretStoreError::InvalidName(_))) => {
            ApiError::bad_request("invalid secret name").into_response()
        }
        Ok(Err(SecretStoreError::NotFound(name))) => {
            ApiError::new(StatusCode::NOT_FOUND, format!("secret not found: {name}"))
                .into_response()
        }
        Ok(Err(SecretStoreError::Io(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(Err(SecretStoreError::Serde(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Err(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("secret delete task failed: {err}"),
        )
        .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct GitHubRepoResponse {
    default_branch: String,
    private: bool,
    permissions: Option<serde_json::Value>,
}

async fn get_github_repo(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path((owner, name)): Path<(String, String)>,
) -> Response {
    let settings = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let app_id = match settings.app_id() {
        Some(app_id) => app_id.to_string(),
        None => {
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "git.app_id is not configured",
            )
            .into_response();
        }
    };

    let creds = match state.github_app_credentials(Some(&app_id)).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "GITHUB_APP_PRIVATE_KEY is not configured",
            )
            .into_response();
        }
        Err(err) => {
            return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err).into_response();
        }
    };

    let jwt = match fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem) {
        Ok(jwt) => jwt,
        Err(err) => {
            return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err).into_response();
        }
    };

    let base_url = fabro_github::github_api_base_url();
    let client = reqwest::Client::new();
    let install_url = settings.slug().map_or_else(
        || format!("https://github.com/organizations/{owner}/settings/installations"),
        |slug| format!("https://github.com/apps/{slug}/installations/new"),
    );

    let installed =
        match fabro_github::check_app_installed(&client, &jwt, &owner, &name, &base_url).await {
            Ok(installed) => installed,
            Err(err) => {
                return ApiError::new(StatusCode::BAD_GATEWAY, err).into_response();
            }
        };

    if !installed {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "owner": owner,
                "name": name,
                "accessible": false,
                "default_branch": null,
                "private": null,
                "permissions": null,
                "install_url": install_url,
            })),
        )
            .into_response();
    }

    let token = match fabro_github::create_installation_access_token_with_permissions(
        &client,
        &jwt,
        &owner,
        &name,
        &base_url,
        serde_json::json!({ "contents": "write", "pull_requests": "write" }),
    )
    .await
    {
        Ok(token) => token,
        Err(err) => return ApiError::new(StatusCode::BAD_GATEWAY, err).into_response(),
    };

    let repo_response = match client
        .get(format!("{base_url}/repos/{owner}/{name}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro-server")
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("GitHub repo lookup failed: {}", response.status()),
            )
            .into_response();
        }
        Err(err) => return ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
    };

    let repo = match repo_response.json::<GitHubRepoResponse>().await {
        Ok(repo) => repo,
        Err(err) => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("Failed to parse GitHub repo response: {err}"),
            )
            .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "owner": owner,
            "name": name,
            "accessible": true,
            "default_branch": repo.default_branch,
            "private": repo.private,
            "permissions": repo.permissions,
            "install_url": serde_json::Value::Null,
        })),
    )
        .into_response()
}

async fn run_diagnostics(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::OK,
        Json(diagnostics::run_all(state.as_ref()).await),
    )
        .into_response()
}

async fn openapi_spec() -> Response {
    let yaml = include_str!("../../../../docs/api-reference/fabro-api.yaml");
    let value: serde_json::Value =
        serde_yaml::from_str(yaml).expect("embedded OpenAPI YAML is invalid");
    Json(value).into_response()
}

async fn cookie_and_demo_middleware(
    State(state): State<Arc<AppState>>,
    mut req: axum_extract::Request,
    next: Next,
) -> Response {
    let cookies = web_auth::parse_cookie_header(req.headers());
    if cookies
        .get("fabro-demo")
        .is_some_and(|cookie| cookie.value() == "1")
    {
        req.headers_mut()
            .insert("x-fabro-demo", HeaderValue::from_static("1"));
    }
    if let Some(key) = state.session_key().await {
        if let Some(session) = web_auth::read_private_session(req.headers(), &key) {
            req.extensions_mut().insert(session);
        }
    }
    next.run(req).await
}

async fn get_aggregate_usage(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
) -> Response {
    let agg = state
        .aggregate_usage
        .lock()
        .expect("aggregate_usage lock poisoned");
    let by_model: Vec<UsageByModel> = agg
        .by_model
        .iter()
        .map(|(model, totals)| UsageByModel {
            model: ModelReference { id: model.clone() },
            stages: totals.stages,
            usage: TokenUsage {
                input_tokens: totals.input_tokens,
                output_tokens: totals.output_tokens,
                cost: totals.cost,
            },
        })
        .collect();
    let response = AggregateUsage {
        totals: AggregateUsageTotals {
            runs: agg.total_runs,
            input_tokens: by_model.iter().map(|m| m.usage.input_tokens).sum(),
            output_tokens: by_model.iter().map(|m| m.usage.output_tokens).sum(),
            cost: by_model.iter().map(|m| m.usage.cost).sum(),
            runtime_secs: agg.total_runtime_secs,
        },
        by_model,
    };
    (StatusCode::OK, Json(response)).into_response()
}

/// Create an `AppState` with default settings.
pub fn create_app_state() -> Arc<AppState> {
    create_app_state_with_options(Settings::default(), 5)
}

#[doc(hidden)]
pub fn create_app_state_with_registry_factory(
    registry_factory_override: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    create_app_state_with_settings_and_registry_factory(
        Settings::default(),
        registry_factory_override,
    )
}

#[doc(hidden)]
pub fn create_app_state_with_settings_and_registry_factory(
    settings: Settings,
    registry_factory_override: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    let (store, artifact_store) = test_store_bundle();
    build_app_state_with_path(
        Arc::new(RwLock::new(settings)),
        Some(Box::new(registry_factory_override)),
        5,
        store,
        artifact_store,
        test_secret_store_path(),
        test_config_path(),
        false,
    )
    .expect("test app state should build")
}

/// Create an `AppState` with the given settings and concurrency limit.
pub fn create_app_state_with_options(
    settings: Settings,
    max_concurrent_runs: usize,
) -> Arc<AppState> {
    let (store, artifact_store) = test_store_bundle();
    create_app_state_with_store(
        Arc::new(RwLock::new(settings)),
        max_concurrent_runs,
        store,
        artifact_store,
    )
}

fn test_store_bundle() -> (Arc<Database>, ArtifactStore) {
    let object_store: Arc<dyn object_store::ObjectStore> = Arc::new(MemoryObjectStore::new());
    let store = Arc::new(fabro_store::Database::new(
        Arc::clone(&object_store),
        "",
        Duration::from_millis(1),
    ));
    let artifact_store = ArtifactStore::new(object_store, "artifacts");
    (store, artifact_store)
}

pub fn create_app_state_with_store(
    settings: Arc<RwLock<Settings>>,
    max_concurrent_runs: usize,
    store: Arc<Database>,
    artifact_store: ArtifactStore,
) -> Arc<AppState> {
    build_app_state_with_path(
        settings,
        None,
        max_concurrent_runs,
        store,
        artifact_store,
        test_secret_store_path(),
        test_config_path(),
        false,
    )
    .expect("test app state should build")
}

pub(crate) fn build_app_state_with_path(
    settings: Arc<RwLock<Settings>>,
    registry_factory_override: Option<Box<RegistryFactoryOverride>>,
    max_concurrent_runs: usize,
    store: Arc<Database>,
    artifact_store: ArtifactStore,
    secret_store_path: PathBuf,
    config_path: PathBuf,
    local_daemon_mode: bool,
) -> anyhow::Result<Arc<AppState>> {
    let secret_store = SecretStore::load(secret_store_path)?;
    let (global_event_tx, _) = broadcast::channel(4096);
    Ok(Arc::new(AppState {
        runs: Mutex::new(HashMap::new()),
        aggregate_usage: Mutex::new(UsageAccumulator::default()),
        store,
        artifact_store,
        started_at: Instant::now(),
        max_concurrent_runs,
        scheduler_notify: Notify::new(),
        global_event_tx,
        secret_store: AsyncRwLock::new(secret_store),
        settings,
        config_path,
        local_daemon_mode,
        shutting_down: AtomicBool::new(false),
        registry_factory_override,
    }))
}

fn test_secret_store_path() -> PathBuf {
    std::env::temp_dir().join(format!("fabro-test-secrets-{}.json", Ulid::new()))
}

fn test_config_path() -> PathBuf {
    std::env::temp_dir().join(format!("fabro-test-settings-{}.toml", Ulid::new()))
}

async fn list_board_runs(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    let live_runs = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        let queue_positions = compute_queue_positions(&runs);
        runs.iter()
            .map(|(id, managed_run)| {
                (
                    *id,
                    managed_run.status.clone(),
                    managed_run.error.clone(),
                    queue_positions.get(id).copied(),
                    managed_run.created_at.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => runs
            .into_iter()
            .map(|summary| (summary.run_id, summary))
            .collect::<HashMap<_, _>>(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset as usize;
    let all_items: Vec<RunStatusResponse> = live_runs
        .iter()
        .map(|(id, status, error, queue_position, created_at)| {
            let summary = summaries.get(id);
            RunStatusResponse {
                id: id.to_string(),
                status: status.clone(),
                error: error.as_ref().map(|msg| RunError {
                    message: msg.clone(),
                }),
                queue_position: *queue_position,
                status_reason: summary
                    .and_then(|summary| summary.status_reason.map(api_status_reason)),
                pending_control: summary
                    .and_then(|summary| summary.pending_control.map(api_pending_control)),
                created_at: created_at.clone(),
            }
        })
        .collect();
    let page: Vec<_> = all_items.into_iter().skip(offset).take(limit + 1).collect();
    let has_more = page.len() > limit;
    let data: Vec<_> = page.into_iter().take(limit).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": data,
            "meta": { "has_more": has_more }
        })),
    )
        .into_response()
}

async fn list_runs(_auth: AuthenticatedService, State(state): State<Arc<AppState>>) -> Response {
    match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => (StatusCode::OK, Json(runs)).into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn delete_run(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    match delete_run_internal(&state, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(response) => response,
    }
}

async fn delete_run_internal(state: &Arc<AppState>, id: RunId) -> Result<(), Response> {
    let managed_run = if let Ok(mut runs) = state.runs.lock() {
        runs.remove(&id)
    } else {
        None
    };

    if let Some(mut managed_run) = managed_run {
        if let Some(token) = &managed_run.cancel_token {
            token.store(true, Ordering::Relaxed);
        }
        if let Some(interviewer) = &managed_run.interviewer {
            interviewer.abort_pending();
        }
        if let Some(cancel_tx) = managed_run.cancel_tx.take() {
            let _ = cancel_tx.send(());
        }
        if let Some(run_dir) = managed_run.run_dir.take() {
            remove_run_dir(&run_dir).map_err(|err| {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            })?;
        }
    } else {
        let storage = Storage::new(state.settings.read().unwrap().storage_dir());
        let run_dir = storage.run_scratch(&id).root().to_path_buf();
        remove_run_dir(&run_dir).map_err(|err| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        })?;
    }

    state.store.delete_run(&id).await.map_err(|err| {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
    })?;
    state
        .artifact_store
        .delete_for_run(&id)
        .await
        .map_err(|err| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        })?;
    Ok(())
}

fn remove_run_dir(run_dir: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(run_dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn compute_queue_positions(runs: &HashMap<RunId, ManagedRun>) -> HashMap<RunId, i64> {
    let mut queued: Vec<(&RunId, &ManagedRun)> = runs
        .iter()
        .filter(|(_, r)| r.status == RunStatus::Queued)
        .collect();
    queued.sort_by_key(|(_, r)| r.created_at);
    queued
        .into_iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i64::try_from(i + 1).unwrap()))
        .collect()
}

#[allow(clippy::result_large_err)]
fn parse_run_id_path(id: &str) -> Result<RunId, Response> {
    id.parse::<RunId>()
        .map_err(|_| ApiError::bad_request("Invalid run ID.").into_response())
}

#[allow(clippy::result_large_err)]
fn parse_stage_id_path(stage_id: &str) -> Result<StageId, Response> {
    StageId::from_str(stage_id)
        .map_err(|_| ApiError::bad_request("Invalid stage ID.").into_response())
}

#[allow(clippy::result_large_err)]
fn parse_blob_id_path(blob_id: &str) -> Result<RunBlobId, Response> {
    RunBlobId::from_str(blob_id)
        .map_err(|_| ApiError::bad_request("Invalid blob ID.").into_response())
}

#[allow(clippy::result_large_err)]
fn required_filename(params: ArtifactFilenameParams) -> Result<String, Response> {
    match params.filename {
        Some(filename) if !filename.is_empty() => Ok(filename),
        _ => Err(ApiError::bad_request("Missing filename query parameter.").into_response()),
    }
}

#[allow(clippy::result_large_err)]
fn validate_relative_artifact_path(kind: &str, value: &str) -> Result<PathBuf, Response> {
    let mut normalized = PathBuf::new();
    for component in PathBuf::from(value).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ApiError::bad_request(format!(
                    "{kind} must be a relative path without '..'"
                ))
                .into_response());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(ApiError::bad_request(format!("{kind} must not be empty")).into_response());
    }
    Ok(normalized)
}

#[allow(clippy::result_large_err)]
fn run_artifacts_dir(run: &fabro_types::RunRecord, run_id: &RunId) -> PathBuf {
    Storage::new(run.settings.storage_dir())
        .run_scratch(run_id)
        .artifact_files_dir()
}

#[allow(clippy::result_large_err)]
fn scan_run_artifacts(
    run: &fabro_types::RunRecord,
    run_id: &RunId,
    node_filter: Option<&str>,
    retry_filter: Option<u32>,
) -> Result<Vec<workflow_artifacts::ArtifactEntry>, Response> {
    workflow_artifacts::scan_artifacts(&run_artifacts_dir(run, run_id), node_filter, retry_filter)
        .map_err(|err| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        })
}

fn octet_stream_response(bytes: Bytes) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/octet-stream")],
        bytes,
    )
        .into_response()
}

#[allow(clippy::result_large_err)]
fn api_run_event_from_store(payload: &EventPayload) -> Result<ApiRunEvent, Response> {
    serde_json::from_value(payload.as_value().clone()).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to serialize stored event: {err}"),
        )
        .into_response()
    })
}

#[allow(clippy::result_large_err)]
fn api_event_envelope_from_store(event: &EventEnvelope) -> Result<ApiEventEnvelope, Response> {
    Ok(ApiEventEnvelope {
        payload: api_run_event_from_store(&event.payload)?,
        seq: i64::from(event.seq),
    })
}

fn clear_live_run_state(run: &mut ManagedRun) {
    run.interviewer = None;
    run.event_tx = None;
    run.cancel_tx = None;
    run.cancel_token = None;
    run.worker_pid = None;
    run.worker_pgid = None;
}

#[derive(Clone, Copy)]
struct LiveWorkerProcess {
    run_id: RunId,
    process_group_id: u32,
}

fn failure_for_incomplete_run(
    pending_control: Option<RunControlAction>,
    terminated_message: String,
) -> (FabroError, Option<WorkflowStatusReason>) {
    if pending_control == Some(RunControlAction::Cancel) {
        (FabroError::Cancelled, Some(WorkflowStatusReason::Cancelled))
    } else {
        (
            FabroError::engine(terminated_message),
            Some(WorkflowStatusReason::Terminated),
        )
    }
}

fn should_reconcile_run_on_startup(status: WorkflowRunStatus) -> bool {
    matches!(
        status,
        WorkflowRunStatus::Starting
            | WorkflowRunStatus::Running
            | WorkflowRunStatus::Paused
            | WorkflowRunStatus::Removing
    )
}

pub(crate) async fn reconcile_incomplete_runs_on_startup(
    state: &Arc<AppState>,
) -> anyhow::Result<usize> {
    let summaries = state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await?;
    let mut reconciled = 0usize;

    for summary in summaries {
        let Some(status) = summary.status else {
            continue;
        };
        if !should_reconcile_run_on_startup(status) {
            continue;
        }

        let run_store = state.store.open_run(&summary.run_id).await?;
        let (error, reason) = failure_for_incomplete_run(
            summary.pending_control,
            "Fabro server restarted before the run reached a terminal state.".to_string(),
        );
        workflow_event::append_event(
            &run_store,
            &summary.run_id,
            &workflow_event::Event::WorkflowRunFailed {
                error,
                duration_ms: 0,
                reason,
                git_commit_sha: None,
            },
        )
        .await?;
        reconciled += 1;
    }

    Ok(reconciled)
}

fn live_worker_processes(state: &AppState) -> Vec<LiveWorkerProcess> {
    let runs = state.runs.lock().expect("runs lock poisoned");
    runs.iter()
        .filter_map(|(run_id, managed_run)| {
            managed_run
                .worker_pgid
                .or(managed_run.worker_pid)
                .map(|process_group_id| LiveWorkerProcess {
                    run_id: *run_id,
                    process_group_id,
                })
        })
        .collect()
}

async fn persist_shutdown_run_failures(
    state: &Arc<AppState>,
    workers: &[LiveWorkerProcess],
) -> anyhow::Result<()> {
    let run_ids = workers
        .iter()
        .map(|worker| worker.run_id)
        .collect::<HashSet<_>>();

    for run_id in run_ids {
        let run_store = state.store.open_run(&run_id).await?;
        let run_state = run_store.state().await?;
        if run_state
            .status
            .as_ref()
            .is_some_and(|status| status.status.is_terminal())
        {
            continue;
        }

        let (error, reason) = failure_for_incomplete_run(
            run_state.pending_control,
            "Fabro server shut down before the run reached a terminal state.".to_string(),
        );
        workflow_event::append_event(
            &run_store,
            &run_id,
            &workflow_event::Event::WorkflowRunFailed {
                error,
                duration_ms: 0,
                reason,
                git_commit_sha: None,
            },
        )
        .await?;
    }

    Ok(())
}

pub(crate) async fn shutdown_active_workers(state: &Arc<AppState>) -> anyhow::Result<usize> {
    shutdown_active_workers_with_grace(state, WORKER_CANCEL_GRACE, Duration::from_millis(50)).await
}

async fn shutdown_active_workers_with_grace(
    state: &Arc<AppState>,
    grace: Duration,
    poll_interval: Duration,
) -> anyhow::Result<usize> {
    state.begin_shutdown();
    let workers = live_worker_processes(state.as_ref());

    #[cfg(unix)]
    {
        let process_groups = workers
            .iter()
            .map(|worker| worker.process_group_id)
            .collect::<HashSet<_>>();

        for process_group_id in &process_groups {
            fabro_proc::sigterm_process_group(*process_group_id);
        }

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline
            && process_groups
                .iter()
                .any(|process_group_id| fabro_proc::process_group_alive(*process_group_id))
        {
            sleep(poll_interval).await;
        }

        let survivors = process_groups
            .into_iter()
            .filter(|process_group_id| fabro_proc::process_group_alive(*process_group_id))
            .collect::<Vec<_>>();
        for process_group_id in &survivors {
            fabro_proc::sigkill_process_group(*process_group_id);
        }
        if !survivors.is_empty() {
            let kill_deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < kill_deadline
                && survivors
                    .iter()
                    .any(|process_group_id| fabro_proc::process_group_alive(*process_group_id))
            {
                sleep(poll_interval).await;
            }
        }
    }

    persist_shutdown_run_failures(state, &workers).await?;
    Ok(workers.len())
}

async fn persist_cancelled_run_status(state: &AppState, run_id: RunId) -> anyhow::Result<()> {
    let run_store = state.store.open_run(&run_id).await?;
    workflow_event::append_event(
        &run_store,
        &run_id,
        &workflow_event::Event::WorkflowRunFailed {
            error: FabroError::Cancelled,
            duration_ms: 0,
            reason: Some(WorkflowStatusReason::Cancelled),
            git_commit_sha: None,
        },
    )
    .await
}

async fn forward_run_events_to_global(
    mut run_events: broadcast::Receiver<EventEnvelope>,
    global_event_tx: broadcast::Sender<EventEnvelope>,
) {
    loop {
        match run_events.recv().await {
            Ok(event) => {
                let _ = global_event_tx.send(event);
            }
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => break,
        }
    }
}

fn managed_run(
    dot_source: String,
    status: RunStatus,
    created_at: chrono::DateTime<chrono::Utc>,
    run_dir: std::path::PathBuf,
    execution_mode: RunExecutionMode,
) -> ManagedRun {
    ManagedRun {
        dot_source,
        status,
        error: None,
        created_at,
        enqueued_at: Instant::now(),
        interviewer: None,
        event_tx: None,
        checkpoint: None,
        cancel_tx: None,
        cancel_token: None,
        worker_pid: None,
        worker_pgid: None,
        run_dir: Some(run_dir),
        execution_mode,
    }
}

fn api_status_from_workflow(
    status: WorkflowRunStatus,
    reason: Option<WorkflowStatusReason>,
) -> RunStatus {
    match status {
        WorkflowRunStatus::Submitted => RunStatus::Submitted,
        WorkflowRunStatus::Starting => RunStatus::Starting,
        WorkflowRunStatus::Running | WorkflowRunStatus::Removing => RunStatus::Running,
        WorkflowRunStatus::Paused => RunStatus::Paused,
        WorkflowRunStatus::Succeeded => RunStatus::Completed,
        WorkflowRunStatus::Failed if reason == Some(WorkflowStatusReason::Cancelled) => {
            RunStatus::Cancelled
        }
        WorkflowRunStatus::Failed | WorkflowRunStatus::Dead => RunStatus::Failed,
    }
}

fn worker_mode_arg(mode: RunExecutionMode) -> &'static str {
    match mode {
        RunExecutionMode::Start => "start",
        RunExecutionMode::Resume => "resume",
    }
}

fn api_status_reason(reason: WorkflowStatusReason) -> ApiStatusReason {
    match reason {
        WorkflowStatusReason::Completed => ApiStatusReason::Completed,
        WorkflowStatusReason::PartialSuccess => ApiStatusReason::PartialSuccess,
        WorkflowStatusReason::WorkflowError => ApiStatusReason::WorkflowError,
        WorkflowStatusReason::Cancelled => ApiStatusReason::Cancelled,
        WorkflowStatusReason::Terminated => ApiStatusReason::Terminated,
        WorkflowStatusReason::TransientInfra => ApiStatusReason::TransientInfra,
        WorkflowStatusReason::BudgetExhausted => ApiStatusReason::BudgetExhausted,
        WorkflowStatusReason::LaunchFailed => ApiStatusReason::LaunchFailed,
        WorkflowStatusReason::BootstrapFailed => ApiStatusReason::BootstrapFailed,
        WorkflowStatusReason::SandboxInitFailed => ApiStatusReason::SandboxInitFailed,
        WorkflowStatusReason::SandboxInitializing => ApiStatusReason::SandboxInitializing,
    }
}

fn api_pending_control(action: RunControlAction) -> ApiRunControlAction {
    match action {
        RunControlAction::Cancel => ApiRunControlAction::Cancel,
        RunControlAction::Pause => ApiRunControlAction::Pause,
        RunControlAction::Unpause => ApiRunControlAction::Unpause,
    }
}

async fn load_run_status_metadata(
    state: &AppState,
    run_id: RunId,
) -> (Option<ApiStatusReason>, Option<ApiRunControlAction>) {
    match state.store.runs().find(&run_id).await {
        Ok(Some(summary)) => (
            summary.status_reason.map(api_status_reason),
            summary.pending_control.map(api_pending_control),
        ),
        _ => (None, None),
    }
}

async fn load_pending_control(
    state: &AppState,
    run_id: RunId,
) -> anyhow::Result<Option<RunControlAction>> {
    Ok(state
        .store
        .runs()
        .find(&run_id)
        .await?
        .and_then(|summary| summary.pending_control))
}

fn fail_managed_run(state: &Arc<AppState>, run_id: RunId, message: String) {
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        managed_run.status = RunStatus::Failed;
        managed_run.error = Some(message);
        clear_live_run_state(managed_run);
    }
}

fn update_live_run_from_event(state: &Arc<AppState>, run_id: RunId, event: &RunEvent) {
    use fabro_types::EventBody;

    let mut runs = state.runs.lock().expect("runs lock poisoned");
    let Some(managed_run) = runs.get_mut(&run_id) else {
        return;
    };

    match &event.body {
        EventBody::RunStarting(_) => managed_run.status = RunStatus::Starting,
        EventBody::RunRunning(_) | EventBody::RunUnpaused(_) => {
            managed_run.status = RunStatus::Running
        }
        EventBody::RunPaused(_) => managed_run.status = RunStatus::Paused,
        EventBody::RunCompleted(_) => {
            managed_run.status = RunStatus::Completed;
            managed_run.error = None;
        }
        EventBody::RunFailed(props) => {
            managed_run.status = if props.reason == Some(WorkflowStatusReason::Cancelled) {
                RunStatus::Cancelled
            } else {
                RunStatus::Failed
            };
            managed_run.error = Some(props.error.clone());
        }
        _ => {}
    }
}

async fn drain_worker_stderr(
    run_id: RunId,
    run_dir: PathBuf,
    stderr: tokio::process::ChildStderr,
) -> anyhow::Result<()> {
    let log_path = run_dir.join("runtime").join(WORKER_STDERR_LOG);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let mut log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await?;
    let mut lines = BufReader::new(stderr).lines();

    while let Some(line) = lines.next_line().await? {
        log_file.write_all(line.as_bytes()).await?;
        log_file.write_all(b"\n").await?;
        tracing::warn!(run_id = %run_id, worker_stderr = %line);
    }

    log_file.flush().await?;
    Ok(())
}

async fn append_worker_exit_failure(
    run_store: &fabro_store::RunDatabase,
    run_id: RunId,
    wait_status: &std::process::ExitStatus,
) {
    let state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Failed to load run state after worker exit");
            return;
        }
    };

    let terminal = state
        .status
        .as_ref()
        .is_some_and(|status| status.status.is_terminal());
    if terminal {
        return;
    }

    let (error, reason) = failure_for_incomplete_run(
        state.pending_control,
        format!("Worker exited before emitting a terminal run event: {wait_status}"),
    );

    if let Err(err) = workflow_event::append_event(
        run_store,
        &run_id,
        &workflow_event::Event::WorkflowRunFailed {
            error,
            duration_ms: 0,
            reason,
            git_commit_sha: None,
        },
    )
    .await
    {
        tracing::warn!(run_id = %run_id, error = %err, "Failed to append worker exit failure");
    }
}

#[derive(serde::Deserialize)]
struct WorkerServerRecord {
    bind: Bind,
}

fn current_server_target(storage_dir: &std::path::Path) -> anyhow::Result<String> {
    let record_path = Storage::new(storage_dir).server_state().record_path();
    let content = std::fs::read_to_string(&record_path)
        .map_err(|err| anyhow::anyhow!("failed to read {}: {err}", record_path.display()))?;
    let record: WorkerServerRecord = serde_json::from_str(&content).map_err(|err| {
        anyhow::anyhow!(
            "failed to parse server record {}: {err}",
            record_path.display()
        )
    })?;

    Ok(match record.bind {
        Bind::Unix(path) => path.to_string_lossy().to_string(),
        Bind::Tcp(addr) => format!("http://{addr}"),
    })
}

fn worker_command(
    state: &AppState,
    run_id: RunId,
    mode: RunExecutionMode,
    run_dir: &std::path::Path,
) -> anyhow::Result<Command> {
    let exe = std::env::var_os("CARGO_BIN_EXE_fabro")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?);
    let storage_dir = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .storage_dir();
    let server_target = current_server_target(&storage_dir)?;
    let mut cmd = Command::new(exe);
    cmd.arg("__run-worker")
        .arg("--server")
        .arg(server_target)
        .arg("--run-dir")
        .arg(run_dir)
        .arg("--run-id")
        .arg(run_id.to_string())
        .arg("--mode")
        .arg(worker_mode_arg(mode))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    cmd.env_remove("FABRO_JSON");

    #[cfg(unix)]
    fabro_proc::pre_exec_setpgid(cmd.as_std_mut());

    Ok(cmd)
}

fn api_question_from_interview_question(id: &str, question: &Question) -> ApiQuestion {
    ApiQuestion {
        id: id.to_string(),
        text: question.text.clone(),
        question_type: match question.question_type {
            QuestionType::YesNo => ApiQuestionType::YesNo,
            QuestionType::MultipleChoice => ApiQuestionType::MultipleChoice,
            QuestionType::MultiSelect => ApiQuestionType::MultiSelect,
            QuestionType::Freeform => ApiQuestionType::Freeform,
            QuestionType::Confirmation => ApiQuestionType::Confirmation,
        },
        options: question
            .options
            .iter()
            .map(|option| ApiQuestionOption {
                key: option.key.clone(),
                label: option.label.clone(),
            })
            .collect(),
        allow_freeform: question.allow_freeform,
    }
}

fn answer_from_request(req: SubmitAnswerRequest, question: &Question) -> Result<Answer, Response> {
    if let Some(key) = req.selected_option_key {
        let option = question
            .options
            .iter()
            .find(|option| option.key == key)
            .cloned();
        match option {
            Some(option) => Ok(Answer::selected(key, option)),
            None => Err(ApiError::bad_request("Invalid option key.").into_response()),
        }
    } else if !req.selected_option_keys.is_empty() {
        for key in &req.selected_option_keys {
            let valid = question.options.iter().any(|option| option.key == *key);
            if !valid {
                return Err(ApiError::bad_request("Invalid option key.").into_response());
            }
        }
        Ok(Answer::multi_selected(req.selected_option_keys))
    } else if let Some(value) = req.value {
        Ok(Answer::text(value))
    } else {
        Err(ApiError::bad_request(
            "One of value, selected_option_key, or selected_option_keys is required.",
        )
        .into_response())
    }
}

async fn load_file_question(run_dir: &std::path::Path) -> anyhow::Result<Option<Question>> {
    let request_path = fabro_config::RunScratch::new(run_dir).interview_request_path();
    match fs::read_to_string(&request_path).await {
        Ok(data) => Ok(Some(serde_json::from_str(&data)?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

async fn write_file_answer(run_dir: &std::path::Path, answer: &Answer) -> anyhow::Result<()> {
    let response_path = fabro_config::RunScratch::new(run_dir).interview_response_path();
    if let Some(parent) = response_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let data = serde_json::to_string_pretty(answer)?;
    fs::write(response_path, data).await?;
    Ok(())
}

async fn create_run(
    subject: AuthenticatedSubject,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RunManifest>,
) -> Response {
    let prepared = match run_manifest::prepare_manifest_with_mode(
        &state.settings.read().unwrap(),
        &req,
        state.local_daemon_mode,
    ) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let run_id = prepared.run_id.unwrap_or_else(RunId::new);
    info!(run_id = %run_id, "Run created");

    let mut create_input = run_manifest::create_run_input(prepared.clone());
    create_input.run_id = Some(run_id);
    create_input.provenance = Some(run_provenance(&headers, &subject));

    let created = match Box::pin(operations::create(state.store.as_ref(), create_input)).await {
        Ok(created) => created,
        Err(FabroError::ValidationFailed { .. } | FabroError::Parse(_)) => {
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
            status_reason: None,
            pending_control: None,
            created_at,
        }),
    )
        .into_response()
}

fn run_provenance(headers: &HeaderMap, subject: &AuthenticatedSubject) -> RunProvenance {
    RunProvenance {
        server: Some(RunServerProvenance {
            version: FABRO_VERSION.to_string(),
        }),
        client: run_client_provenance(headers),
        subject: Some(RunSubjectProvenance {
            login: subject.login.clone(),
            auth_method: subject.auth_method,
        }),
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
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunManifest>,
) -> Response {
    let prepared = match run_manifest::prepare_manifest_with_mode(
        &state.settings.read().unwrap(),
        &req,
        state.local_daemon_mode,
    ) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let validated = match run_manifest::validate_prepared_manifest(&prepared) {
        Ok(validated) => validated,
        Err(FabroError::Parse(_)) => {
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

async fn render_graph_from_manifest(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(req): Json<RenderWorkflowGraphRequest>,
) -> Response {
    let prepared = match run_manifest::prepare_manifest_with_mode(
        &state.settings.read().unwrap(),
        &req.manifest,
        state.local_daemon_mode,
    ) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let validated = match run_manifest::validate_prepared_manifest(&prepared) {
        Ok(validated) => validated,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    if validated.has_errors() {
        return ApiError::bad_request("Validation failed").into_response();
    }

    let format = match req.format.unwrap_or(RenderWorkflowGraphFormat::Svg) {
        RenderWorkflowGraphFormat::Svg => GraphFormat::Svg,
        RenderWorkflowGraphFormat::Png => GraphFormat::Png,
    };
    let direction = req.direction.as_ref().map(|direction| match direction {
        RenderWorkflowGraphDirection::Lr => "LR",
        RenderWorkflowGraphDirection::Tb => "TB",
    });
    let dot_source = run_manifest::graph_source(&prepared, direction);
    render_graph_bytes(&dot_source, format).await
}

async fn start_run(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<StartRunRequest>>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let resume = body.is_some_and(|Json(req)| req.resume);

    {
        let runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get(&id) {
            if matches!(
                managed_run.status,
                RunStatus::Queued | RunStatus::Starting | RunStatus::Running
            ) {
                return ApiError::new(
                    StatusCode::CONFLICT,
                    if resume {
                        "an engine process is still running for this run — cannot resume"
                    } else {
                        "an engine process is still running for this run — cannot start"
                    },
                )
                .into_response();
            }
        }
    }

    let Ok(run_store) = state.store.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let run_state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to load run state: {err}"),
            )
            .into_response();
        }
    };

    if resume {
        if run_state.checkpoint.is_none() {
            return ApiError::new(StatusCode::CONFLICT, "no checkpoint to resume from")
                .into_response();
        }
    } else if let Some(record) = run_state.status.as_ref() {
        if !matches!(
            record.status,
            WorkflowRunStatus::Submitted | WorkflowRunStatus::Starting
        ) {
            return ApiError::new(
                StatusCode::CONFLICT,
                format!(
                    "cannot start run: status is {:?}, expected submitted",
                    record.status
                ),
            )
            .into_response();
        }
    }

    let Some(run_record) = run_state.run.as_ref() else {
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "run record missing from store",
        )
        .into_response();
    };
    let run_dir = Storage::new(run_record.settings.storage_dir())
        .run_scratch(&id)
        .root()
        .to_path_buf();
    let dot_source = run_state.graph_source.unwrap_or_default();

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            id,
            managed_run(
                dot_source,
                RunStatus::Queued,
                id.created_at(),
                run_dir,
                if resume {
                    RunExecutionMode::Resume
                } else {
                    RunExecutionMode::Start
                },
            ),
        );
    }

    state.scheduler_notify.notify_one();
    (
        StatusCode::OK,
        Json(RunStatusResponse {
            id: id.to_string(),
            status: RunStatus::Queued,
            error: None,
            queue_position: None,
            status_reason: None,
            pending_control: None,
            created_at: id.created_at(),
        }),
    )
        .into_response()
}

/// Execute a single run: transitions queued → starting → running → completed/failed/cancelled.
async fn execute_run(state: Arc<AppState>, run_id: RunId) {
    if state.is_shutting_down() {
        return;
    }

    if state.registry_factory_override.is_some() {
        execute_run_in_process(state, run_id).await;
        return;
    }

    execute_run_subprocess(state, run_id).await;
}

async fn execute_run_in_process(state: Arc<AppState>, run_id: RunId) {
    // Transition to Starting and set up cancel infrastructure
    let (cancel_rx, run_dir, event_tx, cancel_token, execution_mode, queued_for) = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = match runs.get_mut(&run_id) {
            Some(r) if r.status == RunStatus::Queued => r,
            _ => return,
        };
        let Some(run_dir) = managed_run.run_dir.clone() else {
            return;
        };

        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let cancel_token = Arc::new(AtomicBool::new(false));
        let (event_tx, _) = broadcast::channel(256);

        managed_run.status = RunStatus::Starting;
        managed_run.cancel_tx = Some(cancel_tx);
        managed_run.cancel_token = Some(Arc::clone(&cancel_token));
        managed_run.event_tx = Some(event_tx);

        (
            cancel_rx,
            run_dir,
            managed_run.event_tx.clone(),
            cancel_token,
            managed_run.execution_mode,
            managed_run.enqueued_at.elapsed(),
        )
    };
    let _ = queued_for;

    // Create interviewer and event plumbing (this is the "provisioning" phase)
    let interviewer = Arc::new(WebInterviewer::new());
    let emitter = Emitter::new(run_id);
    if let Some(tx_clone) = event_tx {
        emitter.on_event(move |event| {
            let _ = tx_clone.send(event.clone());
        });
    }
    let registry_override = state
        .registry_factory_override
        .as_ref()
        .map(|factory| Arc::new(factory(Arc::clone(&interviewer) as Arc<dyn Interviewer>)));
    let emitter = Arc::new(emitter);

    // Transition to Running, populate interviewer
    let cancelled_during_setup = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&run_id) {
            if managed_run.status == RunStatus::Starting {
                managed_run.status = RunStatus::Running;
                managed_run.interviewer = Some(Arc::clone(&interviewer));
                false
            } else {
                // Was cancelled during setup
                clear_live_run_state(managed_run);
                state.scheduler_notify.notify_one();
                true
            }
        } else {
            false
        }
    };
    if cancelled_during_setup {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), run_id).await {
            error!(run_id = %run_id, error = %err, "Failed to persist cancelled run status");
        }
        return;
    }

    let run_store = match state.store.open_run(&run_id).await {
        Ok(run_store) => run_store,
        Err(e) => {
            tracing::error!(run_id = %run_id, error = %e, "Failed to open run store");
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            if let Some(managed_run) = runs.get_mut(&run_id) {
                managed_run.status = RunStatus::Failed;
                managed_run.error = Some(format!("Failed to open run store: {e}"));
                clear_live_run_state(managed_run);
            }
            state.scheduler_notify.notify_one();
            return;
        }
    };
    tokio::spawn(forward_run_events_to_global(
        run_store.subscribe(),
        state.global_event_tx.clone(),
    ));
    let persisted = match Persisted::load_from_store(&run_store.clone().into(), &run_dir).await {
        Ok(persisted) => persisted,
        Err(e) => {
            tracing::error!(run_id = %run_id, error = %e, "Failed to load persisted run");
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            if let Some(managed_run) = runs.get_mut(&run_id) {
                managed_run.status = RunStatus::Failed;
                managed_run.error = Some(format!("Failed to load persisted run: {e}"));
                clear_live_run_state(managed_run);
            }
            state.scheduler_notify.notify_one();
            return;
        }
    };
    let github_app = match state
        .github_app_credentials(persisted.run_record().settings.app_id())
        .await
    {
        Ok(github_app) => github_app,
        Err(e) => {
            tracing::error!(run_id = %run_id, error = %e, "Invalid GitHub App credentials");
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            if let Some(managed_run) = runs.get_mut(&run_id) {
                managed_run.status = RunStatus::Failed;
                managed_run.error = Some(format!("Invalid GitHub App credentials: {e}"));
                clear_live_run_state(managed_run);
            }
            state.scheduler_notify.notify_one();
            return;
        }
    };
    let services = operations::StartServices {
        run_id,
        cancel_token: Some(Arc::clone(&cancel_token)),
        emitter: Arc::clone(&emitter),
        interviewer: Arc::clone(&interviewer) as Arc<dyn Interviewer>,
        run_store: run_store.clone().into(),
        event_sink: workflow_event::RunEventSink::store(run_store.clone()),
        run_control: None,
        github_app,
        on_node: None,
        registry_override,
    };

    let execution = async {
        match execution_mode {
            RunExecutionMode::Start => operations::start(&run_dir, services).await,
            RunExecutionMode::Resume => operations::resume(&run_dir, services).await,
        }
    };

    let result = tokio::select! {
        result = execution => ExecutionResult::Completed(Box::new(result)),
        _ = cancel_rx => {
            cancel_token.store(true, Ordering::SeqCst);
            ExecutionResult::CancelledBySignal
        }
    };

    if matches!(&result, ExecutionResult::CancelledBySignal) {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), run_id).await {
            error!(run_id = %run_id, error = %err, "Failed to persist cancelled run status");
        }
    }

    // Save final checkpoint
    let checkpoint = match run_store.state().await {
        Ok(state) => state.checkpoint,
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Failed to load run state from store");
            None
        }
    };

    // Accumulate aggregate usage after execution completes.
    if let Some(ref cp) = checkpoint {
        let stage_durations = match run_store.list_events().await {
            Ok(events) => fabro_workflow::extract_stage_durations_from_events(&events),
            Err(err) => {
                tracing::warn!(run_id = %run_id, error = %err, "Failed to load run events from store");
                HashMap::default()
            }
        };
        let mut agg = state
            .aggregate_usage
            .lock()
            .expect("aggregate_usage lock poisoned");
        agg.total_runs += 1;
        let mut run_runtime: f64 = 0.0;
        for (node_id, outcome) in &cp.node_outcomes {
            if let Some(usage) = &outcome.usage {
                let entry = agg.by_model.entry(usage.model.clone()).or_default();
                entry.stages += 1;
                entry.input_tokens += usage.input_tokens;
                entry.output_tokens += usage.output_tokens;
                entry.cost += usage.cost.unwrap_or(0.0);
            }
            let duration_ms = stage_durations.get(node_id).copied().unwrap_or(0);
            run_runtime += duration_ms as f64 / 1000.0;
        }
        agg.total_runtime_secs += run_runtime;
    }

    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        match &result {
            ExecutionResult::Completed(result) => match result.as_ref() {
                Ok(started) => match &started.finalized.outcome {
                    Ok(_) => {
                        info!(run_id = %run_id, "Run completed");
                        managed_run.status = RunStatus::Completed;
                    }
                    Err(FabroError::Cancelled) => {
                        info!(run_id = %run_id, "Run cancelled");
                        managed_run.status = RunStatus::Cancelled;
                    }
                    Err(e) => {
                        error!(run_id = %run_id, error = %e, "Run failed");
                        managed_run.status = RunStatus::Failed;
                        managed_run.error = Some(e.to_string());
                    }
                },
                Err(FabroError::Cancelled) => {
                    info!(run_id = %run_id, "Run cancelled");
                    managed_run.status = RunStatus::Cancelled;
                }
                Err(e) => {
                    error!(run_id = %run_id, error = %e, "Run failed");
                    managed_run.status = RunStatus::Failed;
                    managed_run.error = Some(e.to_string());
                }
            },
            ExecutionResult::CancelledBySignal => {
                info!(run_id = %run_id, "Run cancelled");
                managed_run.status = RunStatus::Cancelled;
            }
        }
        managed_run.checkpoint = checkpoint;
        managed_run.run_dir = Some(run_dir);
        clear_live_run_state(managed_run);
    }
    drop(runs);
    state.scheduler_notify.notify_one();
}

async fn execute_run_subprocess(state: Arc<AppState>, run_id: RunId) {
    let (run_dir, execution_mode) = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if state.is_shutting_down() {
            return;
        }
        let managed_run = match runs.get_mut(&run_id) {
            Some(run) if run.status == RunStatus::Queued => run,
            _ => return,
        };
        let Some(run_dir) = managed_run.run_dir.clone() else {
            return;
        };
        managed_run.status = RunStatus::Starting;
        (run_dir, managed_run.execution_mode)
    };

    let run_store = match state.store.open_run(&run_id).await {
        Ok(run_store) => run_store,
        Err(err) => {
            tracing::error!(run_id = %run_id, error = %err, "Failed to open run store");
            fail_managed_run(&state, run_id, format!("Failed to open run store: {err}"));
            state.scheduler_notify.notify_one();
            return;
        }
    };
    tokio::spawn(forward_run_events_to_global(
        run_store.subscribe(),
        state.global_event_tx.clone(),
    ));

    let mut child = match worker_command(state.as_ref(), run_id, execution_mode, &run_dir)
        .and_then(|mut cmd| cmd.spawn().map_err(anyhow::Error::from))
    {
        Ok(child) => child,
        Err(err) => {
            tracing::error!(run_id = %run_id, error = %err, "Failed to spawn worker");
            let _ = workflow_event::append_event(
                &run_store,
                &run_id,
                &workflow_event::Event::WorkflowRunFailed {
                    error: FabroError::engine(err.to_string()),
                    duration_ms: 0,
                    reason: Some(WorkflowStatusReason::LaunchFailed),
                    git_commit_sha: None,
                },
            )
            .await;
            fail_managed_run(&state, run_id, format!("Failed to spawn worker: {err}"));
            state.scheduler_notify.notify_one();
            return;
        }
    };

    let Some(worker_pid) = child.id() else {
        let message = "Worker process did not report a PID".to_string();
        tracing::error!(run_id = %run_id, "{message}");
        let _ = child.start_kill();
        let _ = workflow_event::append_event(
            &run_store,
            &run_id,
            &workflow_event::Event::WorkflowRunFailed {
                error: FabroError::engine(message.clone()),
                duration_ms: 0,
                reason: Some(WorkflowStatusReason::LaunchFailed),
                git_commit_sha: None,
            },
        )
        .await;
        fail_managed_run(&state, run_id, message);
        state.scheduler_notify.notify_one();
        return;
    };

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&run_id) {
            managed_run.worker_pid = Some(worker_pid);
            managed_run.worker_pgid = Some(worker_pid);
            managed_run.run_dir = Some(run_dir.clone());
        }
    }

    let Some(stderr) = child.stderr.take() else {
        let message = "Worker stderr pipe was unavailable".to_string();
        tracing::error!(run_id = %run_id, "{message}");
        let _ = child.start_kill();
        let _ = workflow_event::append_event(
            &run_store,
            &run_id,
            &workflow_event::Event::WorkflowRunFailed {
                error: FabroError::engine(message.clone()),
                duration_ms: 0,
                reason: Some(WorkflowStatusReason::LaunchFailed),
                git_commit_sha: None,
            },
        )
        .await;
        fail_managed_run(&state, run_id, message);
        state.scheduler_notify.notify_one();
        return;
    };

    let stderr_task = tokio::spawn(drain_worker_stderr(run_id, run_dir.clone(), stderr));

    let wait_status = match child.wait().await {
        Ok(status) => status,
        Err(err) => {
            tracing::error!(run_id = %run_id, error = %err, "Failed while waiting on worker");
            let _ = child.start_kill();
            let _ = workflow_event::append_event(
                &run_store,
                &run_id,
                &workflow_event::Event::WorkflowRunFailed {
                    error: FabroError::engine(err.to_string()),
                    duration_ms: 0,
                    reason: Some(WorkflowStatusReason::Terminated),
                    git_commit_sha: None,
                },
            )
            .await;
            fail_managed_run(&state, run_id, format!("Worker wait failed: {err}"));
            state.scheduler_notify.notify_one();
            return;
        }
    };

    match stderr_task.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!(run_id = %run_id, error = %err, "Worker stderr drain failed");
        }
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Worker stderr task panicked");
        }
    }

    append_worker_exit_failure(&run_store, run_id, &wait_status).await;

    let final_state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Failed to load final run state from store");
            fail_managed_run(
                &state,
                run_id,
                format!("Failed to load final run state: {err}"),
            );
            state.scheduler_notify.notify_one();
            return;
        }
    };

    if let Some(ref checkpoint) = final_state.checkpoint {
        let stage_durations = match run_store.list_events().await {
            Ok(events) => fabro_workflow::extract_stage_durations_from_events(&events),
            Err(err) => {
                tracing::warn!(run_id = %run_id, error = %err, "Failed to load run events from store");
                HashMap::default()
            }
        };
        let mut agg = state
            .aggregate_usage
            .lock()
            .expect("aggregate_usage lock poisoned");
        agg.total_runs += 1;
        let mut run_runtime: f64 = 0.0;
        for (node_id, outcome) in &checkpoint.node_outcomes {
            if let Some(usage) = &outcome.usage {
                let entry = agg.by_model.entry(usage.model.clone()).or_default();
                entry.stages += 1;
                entry.input_tokens += usage.input_tokens;
                entry.output_tokens += usage.output_tokens;
                entry.cost += usage.cost.unwrap_or(0.0);
            }
            let duration_ms = stage_durations.get(node_id).copied().unwrap_or(0);
            run_runtime += duration_ms as f64 / 1000.0;
        }
        agg.total_runtime_secs += run_runtime;
    }

    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        if let Some(status) = final_state.status.as_ref() {
            managed_run.status = api_status_from_workflow(status.status, status.reason);
        } else if !wait_status.success() {
            managed_run.status = RunStatus::Failed;
        }
        managed_run.error = final_state
            .conclusion
            .as_ref()
            .and_then(|conclusion| conclusion.failure_reason.clone())
            .or_else(|| managed_run.error.clone());
        managed_run.checkpoint = final_state.checkpoint;
        managed_run.run_dir = Some(run_dir);
        clear_live_run_state(managed_run);
    }
    drop(runs);
    state.scheduler_notify.notify_one();
}

/// Background task that promotes queued runs when capacity is available.
pub fn spawn_scheduler(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = state.scheduler_notify.notified() => {},
                () = sleep(std::time::Duration::from_secs(1)) => {},
            }
            if state.is_shutting_down() {
                break;
            }
            // Promote as many queued runs as capacity allows
            loop {
                if state.is_shutting_down() {
                    break;
                }
                let run_to_start = {
                    let runs = state.runs.lock().expect("runs lock poisoned");
                    let active = runs
                        .values()
                        .filter(|r| {
                            r.status == RunStatus::Starting || r.status == RunStatus::Running
                        })
                        .count();
                    if active >= state.max_concurrent_runs {
                        break;
                    }
                    runs.iter()
                        .filter(|(_, r)| r.status == RunStatus::Queued)
                        .min_by_key(|(_, r)| r.created_at)
                        .map(|(id, _)| *id)
                };
                match run_to_start {
                    Some(id) => {
                        let state_clone = Arc::clone(&state);
                        tokio::spawn(execute_run(state_clone, id));
                    }
                    None => break,
                }
            }
        }
    });
}

async fn get_run_status(
    _auth: AuthenticatedService,
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

async fn get_questions(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (interviewer, run_dir) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => (managed_run.interviewer.clone(), managed_run.run_dir.clone()),
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if let Some(interviewer) = interviewer {
        let questions: Vec<ApiQuestion> = interviewer
            .pending_questions()
            .into_iter()
            .map(|pending| api_question_from_interview_question(&pending.id, &pending.question))
            .collect();
        return (StatusCode::OK, Json(ListResponse::new(questions))).into_response();
    }

    let Some(run_dir) = run_dir else {
        return (
            StatusCode::OK,
            Json(ListResponse::new(Vec::<ApiQuestion>::new())),
        )
            .into_response();
    };

    match load_file_question(&run_dir).await {
        Ok(Some(question)) => (
            StatusCode::OK,
            Json(ListResponse::new(vec![
                api_question_from_interview_question(FILE_INTERVIEW_QUESTION_ID, &question),
            ])),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::OK,
            Json(ListResponse::new(Vec::<ApiQuestion>::new())),
        )
            .into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn submit_answer(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path((id, qid)): Path<(String, String)>,
    Json(req): Json<SubmitAnswerRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (interviewer, run_dir) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => (managed_run.interviewer.clone(), managed_run.run_dir.clone()),
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if let Some(interviewer) = interviewer {
        let pending = interviewer.pending_questions();
        let Some(question) = pending.iter().find(|pending| pending.id == qid) else {
            return ApiError::new(
                StatusCode::CONFLICT,
                "Question no longer exists or was already answered.",
            )
            .into_response();
        };
        let answer = match answer_from_request(req, &question.question) {
            Ok(answer) => answer,
            Err(response) => return response,
        };
        if interviewer.submit_answer(&qid, answer) {
            return StatusCode::NO_CONTENT.into_response();
        }
        return ApiError::new(
            StatusCode::CONFLICT,
            "Question no longer exists or was already answered.",
        )
        .into_response();
    }

    let Some(run_dir) = run_dir else {
        return ApiError::new(StatusCode::CONFLICT, "Run is not yet running.").into_response();
    };
    if qid != FILE_INTERVIEW_QUESTION_ID {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Question no longer exists or was already answered.",
        )
        .into_response();
    }
    let question = match load_file_question(&run_dir).await {
        Ok(Some(question)) => question,
        Ok(None) => {
            return ApiError::new(StatusCode::CONFLICT, "Run is not yet running.").into_response();
        }
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let answer = match answer_from_request(req, &question) {
        Ok(answer) => answer,
        Err(response) => return response,
    };
    match write_file_answer(&run_dir, &answer).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn get_run_state(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
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

async fn append_run_event(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(value): Json<serde_json::Value>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let event = match RunEvent::from_value(value.clone()) {
        Ok(event) => event,
        Err(err) => {
            return ApiError::bad_request(format!("Invalid run event: {err}")).into_response();
        }
    };
    if event.run_id != id {
        return ApiError::bad_request("Event run_id does not match path run ID.").into_response();
    }
    let payload = match EventPayload::new(value, &id) {
        Ok(payload) => payload,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };

    match state.store.open_run(&id).await {
        Ok(run_store) => match run_store.append_event(&payload).await {
            Ok(seq) => {
                update_live_run_from_event(&state, id, &event);
                Json(AppendEventResponse {
                    seq: i64::from(seq),
                })
                .into_response()
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn list_run_events(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<EventListParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let since_seq = params.since_seq();
    let limit = params.limit();
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store
            .list_events_from_with_limit(since_seq, limit)
            .await
        {
            Ok(mut events) => {
                let has_more = events.len() > limit;
                events.truncate(limit);
                let mut data = Vec::with_capacity(events.len());
                for event in events {
                    let event = match api_event_envelope_from_store(&event) {
                        Ok(event) => event,
                        Err(response) => return response,
                    };
                    data.push(event);
                }
                Json(PaginatedEventList {
                    data,
                    meta: PaginationMeta { has_more },
                })
                .into_response()
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn attach_run_events(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<AttachParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    {
        let runs = state.runs.lock().expect("runs lock poisoned");
        let Some(managed_run) = runs.get(&id) else {
            return ApiError::not_found("Run not found.").into_response();
        };
        if !matches!(
            managed_run.status,
            RunStatus::Queued | RunStatus::Starting | RunStatus::Running | RunStatus::Paused
        ) {
            return ApiError::new(StatusCode::GONE, "Run is not live on this server.")
                .into_response();
        }
    }

    let Ok(run_store) = state.store.open_run_reader(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let start_seq = match params.since_seq {
        Some(seq) if seq >= 1 => seq,
        Some(_) => 1,
        None => match run_store.list_events().await {
            Ok(events) => events.last().map_or(1, |event| event.seq.saturating_add(1)),
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        },
    };
    let stream = match run_store.watch_events_from(start_seq) {
        Ok(stream) => stream,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let stream = stream.filter_map(|result| match result {
        Ok(event) => sse_event_from_store(&event).map(Ok::<Event, std::convert::Infallible>),
        Err(_) => None,
    });

    Sse::new(stream).into_response()
}

async fn get_checkpoint(
    _auth: AuthenticatedService,
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
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
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
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path((id, blob_id)): Path<(String, String)>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let blob_id = match parse_blob_id_path(&blob_id) {
        Ok(blob_id) => blob_id,
        Err(response) => return response,
    };
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

async fn list_run_artifacts(
    _auth: AuthenticatedService,
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
                let Some(run) = run_state.run.as_ref() else {
                    return ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "run record missing from store",
                    )
                    .into_response();
                };
                match scan_run_artifacts(run, &id, None, None) {
                    Ok(entries) => Json(RunArtifactListResponse {
                        data: entries
                            .into_iter()
                            .map(|entry| RunArtifactEntry {
                                stage_id: StageId::new(entry.node_slug.clone(), entry.retry)
                                    .to_string(),
                                node_slug: entry.node_slug,
                                retry: entry.retry.cast_signed(),
                                relative_path: entry.relative_path,
                                size: entry.size.cast_signed(),
                            })
                            .collect(),
                    })
                    .into_response(),
                    Err(response) => response,
                }
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn list_stage_artifacts(
    _auth: AuthenticatedService,
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
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => {
                let Some(run) = run_state.run.as_ref() else {
                    return ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "run record missing from store",
                    )
                    .into_response();
                };
                match state.artifact_store.list_for_node(&id, &stage_id).await {
                    Ok(filenames) if !filenames.is_empty() => Json(ArtifactListResponse {
                        data: filenames
                            .into_iter()
                            .map(|filename| ArtifactEntry { filename })
                            .collect(),
                    })
                    .into_response(),
                    Ok(_) => match scan_run_artifacts(
                        run,
                        &id,
                        Some(stage_id.node_id()),
                        Some(stage_id.visit()),
                    ) {
                        Ok(entries) => Json(ArtifactListResponse {
                            data: entries
                                .into_iter()
                                .map(|entry| ArtifactEntry {
                                    filename: entry.relative_path,
                                })
                                .collect(),
                        })
                        .into_response(),
                        Err(response) => response,
                    },
                    Err(err) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                        .into_response(),
                }
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn put_stage_artifact(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path((id, stage_id)): Path<(String, String)>,
    Query(params): Query<ArtifactFilenameParams>,
    body: Bytes,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let stage_id = match parse_stage_id_path(&stage_id) {
        Ok(stage_id) => stage_id,
        Err(response) => return response,
    };
    let filename = match required_filename(params) {
        Ok(filename) => filename,
        Err(response) => return response,
    };
    match state.store.open_run_reader(&id).await {
        Ok(_) => match state
            .artifact_store
            .put(&id, &stage_id, &filename, &body)
            .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn get_stage_artifact(
    _auth: AuthenticatedService,
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
    let filename = match required_filename(params) {
        Ok(filename) => filename,
        Err(response) => return response,
    };
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => {
                let Some(run) = run_state.run.as_ref() else {
                    return ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "run record missing from store",
                    )
                    .into_response();
                };
                match state.artifact_store.get(&id, &stage_id, &filename).await {
                    Ok(Some(bytes)) => octet_stream_response(bytes),
                    Ok(None) => {
                        let relative_path =
                            match validate_relative_artifact_path("filename", &filename) {
                                Ok(path) => path,
                                Err(response) => return response,
                            };
                        let artifact_path = run_artifacts_dir(run, &id)
                            .join(stage_id.node_id())
                            .join(format!("retry_{}", stage_id.visit()))
                            .join(relative_path);
                        match std::fs::read(&artifact_path) {
                            Ok(bytes) => octet_stream_response(Bytes::from(bytes)),
                            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                                ApiError::not_found("Artifact not found.").into_response()
                            }
                            Err(err) => {
                                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                                    .into_response()
                            }
                        }
                    }
                    Err(err) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                        .into_response(),
                }
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn generate_preview_url(
    _auth: AuthenticatedService,
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
                url: preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err).into_response();
            }
        }
    } else {
        match sandbox.get_preview_link(port).await {
            Ok(preview) => PreviewUrlResponse {
                token: Some(preview.token),
                url: preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err).into_response();
            }
        }
    };

    (StatusCode::CREATED, Json(response)).into_response()
}

async fn create_ssh_access(
    _auth: AuthenticatedService,
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
        Err(err) => ApiError::new(StatusCode::CONFLICT, err).into_response(),
    }
}

async fn list_sandbox_files(
    _auth: AuthenticatedService,
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
                    name: entry.name,
                    size: entry.size.map(u64::cast_signed),
                })
                .collect(),
        })
        .into_response(),
        Err(err) => ApiError::new(StatusCode::NOT_FOUND, err).into_response(),
    }
}

async fn get_sandbox_file(
    _auth: AuthenticatedService,
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
        return ApiError::new(StatusCode::NOT_FOUND, err).into_response();
    }
    match fs::read(temp.path()).await {
        Ok(bytes) => octet_stream_response(bytes.into()),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn put_sandbox_file(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFileParams>,
    body: Bytes,
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
    if let Err(err) = fs::write(temp.path(), &body).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    match sandbox
        .upload_file_from_local(temp.path(), &params.path)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    }
}

async fn reconnect_run_sandbox(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<Box<dyn Sandbox>, Response> {
    let record = load_run_sandbox_record(state, run_id).await?;
    reconnect(&record)
        .await
        .map_err(|err| ApiError::new(StatusCode::CONFLICT, format!("{err}")).into_response())
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
    DaytonaSandbox::reconnect(name)
        .await
        .map_err(|err| ApiError::new(StatusCode::CONFLICT, err.clone()).into_response())
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

async fn append_control_request(
    state: &AppState,
    run_id: RunId,
    action: RunControlAction,
) -> anyhow::Result<()> {
    let run_store = state.store.open_run(&run_id).await?;
    let event = match action {
        RunControlAction::Cancel => workflow_event::Event::RunCancelRequested,
        RunControlAction::Pause => workflow_event::Event::RunPauseRequested,
        RunControlAction::Unpause => workflow_event::Event::RunUnpauseRequested,
    };
    workflow_event::append_event(&run_store, &run_id, &event).await
}

fn schedule_worker_kill(state: Arc<AppState>, run_id: RunId, worker_pid: u32) {
    tokio::spawn(async move {
        sleep(WORKER_CANCEL_GRACE).await;
        let current_pid = {
            let runs = state.runs.lock().expect("runs lock poisoned");
            runs.get(&run_id).and_then(|run| run.worker_pid)
        };
        if current_pid == Some(worker_pid) && fabro_proc::process_group_alive(worker_pid) {
            #[cfg(unix)]
            fabro_proc::sigkill_process_group(worker_pid);
        }
    });
}

async fn cancel_run(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let (
        created_at,
        response_status,
        persist_cancelled_status,
        cancel_token,
        cancel_tx,
        interviewer,
        worker_pid,
    ) = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get_mut(&id) {
            Some(managed_run) => match managed_run.status {
                RunStatus::Submitted
                | RunStatus::Queued
                | RunStatus::Starting
                | RunStatus::Running
                | RunStatus::Paused => {
                    let persist_cancelled_status =
                        matches!(managed_run.status, RunStatus::Submitted | RunStatus::Queued);
                    let response_status = if persist_cancelled_status {
                        managed_run.status = RunStatus::Cancelled;
                        RunStatus::Cancelled
                    } else {
                        managed_run.status
                    };
                    (
                        managed_run.created_at,
                        response_status,
                        persist_cancelled_status,
                        managed_run.cancel_token.clone(),
                        managed_run.cancel_tx.take(),
                        managed_run.interviewer.clone(),
                        managed_run.worker_pid,
                    )
                }
                _ => {
                    return ApiError::new(StatusCode::CONFLICT, "Run is not cancellable.")
                        .into_response();
                }
            },
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if pending_control != Some(RunControlAction::Cancel) {
        if let Err(err) = append_control_request(state.as_ref(), id, RunControlAction::Cancel).await
        {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    if let Some(token) = &cancel_token {
        token.store(true, Ordering::Relaxed);
    }
    if let Some(interviewer) = &interviewer {
        interviewer.abort_pending();
    }
    if let Some(cancel_tx) = cancel_tx {
        let _ = cancel_tx.send(());
    }
    if let Some(worker_pid) = worker_pid {
        #[cfg(unix)]
        fabro_proc::sigterm(worker_pid);
        schedule_worker_kill(Arc::clone(&state), id, worker_pid);
    }

    if persist_cancelled_status {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), id).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }
    let (status_reason, pending_control) = load_run_status_metadata(state.as_ref(), id).await;

    (
        StatusCode::OK,
        Json(RunStatusResponse {
            id: id.to_string(),
            status: response_status,
            error: None,
            queue_position: None,
            status_reason,
            pending_control,
            created_at,
        }),
    )
        .into_response()
}

async fn pause_run(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let (created_at, worker_pid) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) if managed_run.status == RunStatus::Running => {
                (managed_run.created_at, managed_run.worker_pid)
            }
            Some(_) => {
                return ApiError::new(StatusCode::CONFLICT, "Run is not pausable.").into_response();
            }
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if pending_control.is_some() {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Run control request is already pending.",
        )
        .into_response();
    }
    let Some(worker_pid) = worker_pid else {
        return ApiError::new(StatusCode::CONFLICT, "Run worker is not available.").into_response();
    };
    if let Err(err) = append_control_request(state.as_ref(), id, RunControlAction::Pause).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    #[cfg(unix)]
    fabro_proc::sigusr1(worker_pid);
    let (status_reason, pending_control) = load_run_status_metadata(state.as_ref(), id).await;

    (
        StatusCode::OK,
        Json(RunStatusResponse {
            id: id.to_string(),
            status: RunStatus::Running,
            error: None,
            queue_position: None,
            status_reason,
            pending_control,
            created_at,
        }),
    )
        .into_response()
}

async fn unpause_run(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let (created_at, worker_pid) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) if managed_run.status == RunStatus::Paused => {
                (managed_run.created_at, managed_run.worker_pid)
            }
            Some(_) => {
                return ApiError::new(StatusCode::CONFLICT, "Run is not paused.").into_response();
            }
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if pending_control.is_some() {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Run control request is already pending.",
        )
        .into_response();
    }
    let Some(worker_pid) = worker_pid else {
        return ApiError::new(StatusCode::CONFLICT, "Run worker is not available.").into_response();
    };
    if let Err(err) = append_control_request(state.as_ref(), id, RunControlAction::Unpause).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    #[cfg(unix)]
    fabro_proc::sigusr2(worker_pid);
    let (status_reason, pending_control) = load_run_status_metadata(state.as_ref(), id).await;

    (
        StatusCode::OK,
        Json(RunStatusResponse {
            id: id.to_string(),
            status: RunStatus::Paused,
            error: None,
            queue_position: None,
            status_reason,
            pending_control,
            created_at,
        }),
    )
        .into_response()
}

async fn list_models(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(params): Query<ModelListParams>,
) -> Response {
    let provider = match params.provider.as_deref() {
        Some(value) => match fabro_model::Provider::from_str(value) {
            Ok(provider) => Some(provider),
            Err(err) => return ApiError::new(StatusCode::BAD_REQUEST, err).into_response(),
        },
        None => None,
    };

    let query = params.query.as_ref().map(|value| value.to_lowercase());
    let limit = params.limit.clamp(1, 100) as usize;
    let offset = params.offset as usize;

    let mut models = fabro_model::Catalog::builtin()
        .list(provider)
        .into_iter()
        .filter(|model| match &query {
            Some(query) => {
                model.id.to_lowercase().contains(query)
                    || model.display_name.to_lowercase().contains(query)
                    || model
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase().contains(query))
            }
            None => true,
        })
        .cloned()
        .collect::<Vec<_>>();

    let has_more = models.len() > offset.saturating_add(limit);
    let data = models.drain(offset..models.len().min(offset.saturating_add(limit)));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": data.collect::<Vec<_>>(),
            "meta": { "has_more": has_more }
        })),
    )
        .into_response()
}

async fn test_model(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ModelTestParams>,
) -> Response {
    let mode = match params.mode.as_deref() {
        Some(value) => match ModelTestMode::from_str(value) {
            Ok(mode) => mode,
            Err(err) => return ApiError::new(StatusCode::BAD_REQUEST, err).into_response(),
        },
        None => ModelTestMode::Basic,
    };
    let Some(info) = fabro_model::Catalog::builtin().get(&id) else {
        return ApiError::not_found(format!("Model not found: {id}")).into_response();
    };

    if state.dry_run() {
        return Json(serde_json::json!({
            "model_id": info.id,
            "status": "ok",
        }))
        .into_response();
    }

    let client = match state.build_llm_client().await {
        Ok(client) => Arc::new(client),
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build LLM client: {err}"),
            )
            .into_response();
        }
    };

    let outcome = run_model_test_with_client(info, mode, client).await;
    Json(serde_json::json!({
        "model_id": info.id,
        "status": outcome.status.as_str(),
        "error_message": outcome.error_message,
    }))
    .into_response()
}

fn finish_reason_to_api_stop_reason(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop => "end_turn".to_string(),
        FinishReason::Length => "max_tokens".to_string(),
        FinishReason::ToolCalls => "tool_calls".to_string(),
        FinishReason::ContentFilter => "content_filter".to_string(),
        FinishReason::Error => "error".to_string(),
        FinishReason::Other(s) => s.clone(),
    }
}

fn convert_api_message(msg: &CompletionMessage) -> LlmMessage {
    let role = match msg.role {
        CompletionMessageRole::System => Role::System,
        CompletionMessageRole::User => Role::User,
        CompletionMessageRole::Assistant => Role::Assistant,
        CompletionMessageRole::Tool => Role::Tool,
        CompletionMessageRole::Developer => Role::Developer,
    };
    let content: Vec<ContentPart> = msg
        .content
        .iter()
        .filter_map(|part| {
            let json = serde_json::to_value(part).ok()?;
            serde_json::from_value(json).ok()
        })
        .collect();
    LlmMessage {
        role,
        content,
        name: msg.name.clone(),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

fn convert_llm_message(msg: &LlmMessage) -> CompletionMessage {
    let role = match msg.role {
        Role::System => CompletionMessageRole::System,
        Role::User => CompletionMessageRole::User,
        Role::Assistant => CompletionMessageRole::Assistant,
        Role::Tool => CompletionMessageRole::Tool,
        Role::Developer => CompletionMessageRole::Developer,
    };
    let content: Vec<CompletionContentPart> = msg
        .content
        .iter()
        .filter_map(|part| {
            let json = serde_json::to_value(part).ok()?;
            serde_json::from_value(json).ok()
        })
        .collect();
    CompletionMessage {
        role,
        content,
        name: msg.name.clone(),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

async fn create_completion(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateCompletionRequest>,
) -> Response {
    // Resolve model
    let model_id = req.model.unwrap_or_else(|| {
        fabro_model::Catalog::builtin()
            .list(None)
            .first()
            .map_or_else(|| "claude-sonnet-4-5".to_string(), |m| m.id.clone())
    });

    let catalog_info = fabro_model::Catalog::builtin().get(&model_id);

    // Resolve provider: explicit request > catalog > None
    let provider_name = req
        .provider
        .or_else(|| catalog_info.map(|i| i.provider.to_string()));

    info!(model = %model_id, provider = ?provider_name, "Completion request received");

    // Build messages list
    let mut messages: Vec<LlmMessage> = Vec::new();
    if let Some(system) = req.system {
        messages.push(LlmMessage::system(system));
    }
    for msg in &req.messages {
        messages.push(convert_api_message(msg));
    }

    // Convert tools
    let tools: Option<Vec<ToolDefinition>> = if req.tools.is_empty() {
        None
    } else {
        Some(
            req.tools
                .into_iter()
                .map(|t| ToolDefinition {
                    name: t.name,
                    description: t.description,
                    parameters: t.parameters,
                })
                .collect(),
        )
    };

    // Convert tool_choice
    let tool_choice: Option<ToolChoice> = req.tool_choice.map(|tc| match tc.mode {
        CompletionToolChoiceMode::Auto => ToolChoice::Auto,
        CompletionToolChoiceMode::None => ToolChoice::None,
        CompletionToolChoiceMode::Required => ToolChoice::Required,
        CompletionToolChoiceMode::Named => ToolChoice::named(tc.tool_name.unwrap_or_default()),
    });

    // Build the LLM request
    let request = LlmRequest {
        model: model_id.clone(),
        messages,
        provider: provider_name,
        tools,
        tool_choice,
        response_format: None,
        temperature: req.temperature,
        top_p: req.top_p,
        max_tokens: req.max_tokens,
        stop_sequences: if req.stop_sequences.is_empty() {
            None
        } else {
            Some(req.stop_sequences)
        },
        reasoning_effort: req.reasoning_effort.as_deref().and_then(|s| s.parse().ok()),
        speed: None,
        metadata: None,
        provider_options: req.provider_options,
    };

    // Force non-streaming for structured output
    let use_stream = req.stream && req.schema.is_none();

    // Dry-run mode returns a stub response
    if state.dry_run() {
        let msg_id = Ulid::new().to_string();
        if use_stream {
            let finish_event = StreamEvent::finish(
                FinishReason::Stop,
                Usage::default(),
                LlmResponse {
                    id: msg_id.clone(),
                    model: model_id.clone(),
                    provider: String::new(),
                    message: LlmMessage::assistant(""),
                    finish_reason: FinishReason::Stop,
                    usage: Usage::default(),
                    raw: None,
                    warnings: vec![],
                    rate_limit: None,
                },
            );
            let json = serde_json::to_string(&finish_event).unwrap_or_default();
            let sse_stream = stream::iter(vec![Ok::<_, std::convert::Infallible>(
                Event::default().event("stream_event").data(json),
            )]);
            return Sse::new(sse_stream).into_response();
        }
        let empty_msg = CompletionMessage {
            role: CompletionMessageRole::Assistant,
            content: vec![],
            name: None,
            tool_call_id: None,
        };
        return Json(CompletionResponse {
            id: msg_id,
            model: model_id,
            message: empty_msg,
            stop_reason: "end_turn".to_string(),
            usage: CompletionUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
            output: None,
        })
        .into_response();
    }

    // Get or create LLM client (cached in AppState)
    let client = match state.build_llm_client().await {
        Ok(client) => client,
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create LLM client: {err}"),
            )
            .into_response();
        }
    };

    if use_stream {
        // Streaming path: forward all StreamEvents as SSE
        let stream_result = match client.stream(&request).await {
            Ok(s) => s,
            Err(e) => {
                return ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response();
            }
        };

        let sse_stream = tokio_stream::StreamExt::filter_map(stream_result, |event| match event {
            Ok(ref evt) => match serde_json::to_string(evt) {
                Ok(json) => Some(Ok::<_, std::convert::Infallible>(
                    Event::default().event("stream_event").data(json),
                )),
                Err(e) => Some(Ok(Event::default().event("stream_event").data(
                    serde_json::json!({
                        "type": "error",
                        "error": {"Stream": {"message": format!("failed to serialize event: {e}")}},
                        "raw": null
                    })
                    .to_string(),
                ))),
            },
            Err(e) => Some(Ok(Event::default().event("stream_event").data(
                serde_json::json!({
                    "type": "error",
                    "error": {"Stream": {"message": e.to_string()}},
                    "raw": null
                })
                .to_string(),
            ))),
        });

        Sse::new(sse_stream)
            .keep_alive(
                KeepAlive::new().interval(Duration::from_secs(15)).event(
                    Event::default()
                        .event("ping")
                        .data(serde_json::json!({"type": "ping"}).to_string()),
                ),
            )
            .into_response()
    } else {
        // Non-streaming path
        let msg_id = Ulid::new().to_string();

        if let Some(schema) = req.schema {
            // Structured output uses generate_object for JSON parsing logic
            let mut params = GenerateParams::new(&request.model)
                .messages(request.messages)
                .client(std::sync::Arc::new(client.clone()));
            if let Some(ref p) = request.provider {
                params = params.provider(p);
            }
            if let Some(temp) = request.temperature {
                params = params.temperature(temp);
            }
            if let Some(max_tokens) = request.max_tokens {
                params = params.max_tokens(max_tokens);
            }
            if let Some(top_p) = request.top_p {
                params = params.top_p(top_p);
            }
            match generate_object(params, schema).await {
                Ok(result) => Json(CompletionResponse {
                    id: msg_id,
                    model: model_id,
                    message: convert_llm_message(&result.response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&result.finish_reason),
                    usage: CompletionUsage {
                        input_tokens: result.usage.input_tokens,
                        output_tokens: result.usage.output_tokens,
                    },
                    output: result.output,
                })
                .into_response(),
                Err(e) => ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response(),
            }
        } else {
            match client.complete(&request).await {
                Ok(response) => Json(CompletionResponse {
                    id: response.id,
                    model: response.model,
                    message: convert_llm_message(&response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&response.finish_reason),
                    usage: CompletionUsage {
                        input_tokens: response.usage.input_tokens,
                        output_tokens: response.usage.output_tokens,
                    },
                    output: None,
                })
                .into_response(),
                Err(e) => ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response(),
            }
        }
    }
}

/// Render DOT source to a styled image via `render_dot` on a blocking thread.
pub(crate) async fn render_graph_bytes(dot_source: &str, format: GraphFormat) -> Response {
    use fabro_graphviz::render::render_dot;

    let content_type = match format {
        GraphFormat::Svg => "image/svg+xml",
        GraphFormat::Png => "image/png",
    };
    let source = dot_source.to_owned();
    match spawn_blocking(move || render_dot(&source, format)).await {
        Ok(Ok(bytes)) => (StatusCode::OK, [("content-type", content_type)], bytes).into_response(),
        Ok(Err(e)) => ApiError::new(StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        Err(e) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_graph(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let live_dot_source = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => managed_run.dot_source.clone(),
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };
    if !live_dot_source.is_empty() {
        return render_graph_bytes(&live_dot_source, GraphFormat::Svg).await;
    }

    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => match run_state.graph_source {
                Some(dot_source) => render_graph_bytes(&dot_source, GraphFormat::Svg).await,
                None => ApiError::new(StatusCode::NOT_FOUND, "Graph not found.").into_response(),
            },
            Err(err) => ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
        },
        Err(_) => ApiError::new(StatusCode::NOT_FOUND, "Run not found.").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use fabro_config::server::{
        AuthProvider, AuthSettings, GitAuthorSettings, GitProvider, GitSettings, WebSettings,
    };
    use fabro_types::fixtures;
    #[cfg(unix)]
    use std::process::Stdio;
    use tower::ServiceExt;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Test"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    fn dry_run_settings() -> Settings {
        Settings {
            dry_run: Some(true),
            ..Default::default()
        }
    }

    fn test_app_with() -> Router {
        let state = create_app_state();
        build_router(state, AuthMode::Disabled)
    }

    fn test_app_with_scheduler(state: Arc<AppState>) -> Router {
        spawn_scheduler(Arc::clone(&state));
        build_router(state, AuthMode::Disabled)
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn api(path: &str) -> String {
        format!("/api/v1{path}")
    }

    fn minimal_manifest_json(dot_source: &str) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "cwd": "/tmp",
            "target": {
                "identifier": "workflow.fabro",
                "path": "workflow.fabro",
            },
            "workflows": {
                "workflow.fabro": {
                    "source": dot_source,
                    "files": {},
                },
            },
        })
    }

    fn manifest_body(dot_source: &str) -> Body {
        Body::from(serde_json::to_string(&minimal_manifest_json(dot_source)).unwrap())
    }

    /// Create a run via POST /runs, then start it via POST /runs/{id}/start.
    /// Returns the run_id string.
    async fn create_and_start_run(app: &Router, dot_source: &str) -> String {
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(dot_source))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/start")))
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(req).await.unwrap();

        run_id
    }

    async fn create_durable_run_with_events(
        state: &Arc<AppState>,
        run_id: RunId,
        events: &[workflow_event::Event],
    ) {
        let run_store = state.store.create_run(&run_id).await.unwrap();
        for event in events {
            workflow_event::append_event(&run_store, &run_id, event)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn test_model_unknown_returns_404() {
        let app = test_app_with();

        let req = Request::builder()
            .method("POST")
            .uri(api("/models/nonexistent-model-xyz/test"))
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_model_known_returns_200_with_status() {
        let app = test_app_with();

        let req = Request::builder()
            .method("POST")
            .uri(api("/models/claude-opus-4-6/test"))
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["model_id"], "claude-opus-4-6");
        assert!(body["status"] == "ok" || body["status"] == "error");
    }

    #[tokio::test]
    async fn test_model_alias_returns_canonical_model_id() {
        let state = create_app_state_with_options(dry_run_settings(), 5);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/models/sonnet/test"))
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["model_id"], "claude-sonnet-4-6");
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn test_model_invalid_mode_returns_400() {
        let state = create_app_state_with_options(dry_run_settings(), 5);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/models/claude-opus-4-6/test?mode=bogus"))
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_models_filters_by_provider() {
        let app = test_app_with();

        let req = Request::builder()
            .method("GET")
            .uri(api("/models?provider=anthropic"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        let models = body["data"].as_array().unwrap();
        assert!(!models.is_empty());
        assert!(
            models
                .iter()
                .all(|model| model["provider"] == serde_json::Value::String("anthropic".into()))
        );
    }

    #[tokio::test]
    async fn list_models_filters_by_query_across_aliases() {
        let app = test_app_with();

        let req = Request::builder()
            .method("GET")
            .uri(api("/models?query=codex"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        let model_ids = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            model_ids,
            vec![
                "gpt-5.2-codex".to_string(),
                "gpt-5.3-codex".to_string(),
                "gpt-5.3-codex-spark".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn list_models_invalid_provider_returns_400() {
        let app = test_app_with();

        let req = Request::builder()
            .method("GET")
            .uri(api("/models?provider=not-a-provider"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)]
    async fn auth_login_github_redirects_to_github() {
        let mut settings = Settings::default();
        settings.web = Some(WebSettings {
            url: "http://localhost:3000".to_string(),
            auth: AuthSettings {
                provider: AuthProvider::Github,
                allowed_usernames: vec!["brynary".to_string()],
            },
        });
        settings.git = Some(GitSettings {
            provider: GitProvider::Github,
            app_id: Some("123".to_string()),
            client_id: Some("Iv1.testclient".to_string()),
            slug: Some("fabro".to_string()),
            author: GitAuthorSettings::default(),
            webhooks: None,
        });
        let app = build_router(
            create_app_state_with_options(settings, 5),
            AuthMode::Disabled,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/login/github")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(location.starts_with("https://github.com/login/oauth/authorize?"));
    }

    #[tokio::test]
    async fn logout_redirects_to_login_page() {
        let app = test_app_with();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/logout")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/login")
        );
    }

    #[tokio::test]
    async fn static_favicon_is_served() {
        let app = test_app_with();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/favicon.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/svg+xml")
        );
    }

    #[tokio::test]
    async fn test_model_dry_run_returns_ok() {
        let state = create_app_state_with_options(dry_run_settings(), 5);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/models/claude-opus-4-6/test"))
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["model_id"], "claude-opus-4-6");
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn test_model_dry_run_unknown_returns_404() {
        let state = create_app_state_with_options(dry_run_settings(), 5);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/models/nonexistent-model-xyz/test"))
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_runs_starts_run_and_returns_id() {
        let app = test_app_with();

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let body = body_json(response.into_body()).await;
        assert!(body["id"].is_string());
        assert!(!body["id"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_runs_invalid_dot_returns_bad_request() {
        let app = test_app_with();

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body("not a graph"))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_run_status_returns_status() {
        let state = create_app_state();
        let app = test_app_with_scheduler(state);

        let run_id = create_and_start_run(&app, MINIMAL_DOT).await;

        // Give run a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Check status
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["run_id"].as_str().unwrap(), run_id);
        assert!(body["labels"].is_object());
    }

    #[tokio::test]
    async fn get_run_status_not_found() {
        let app = test_app_with();
        let missing_run_id = fixtures::RUN_64;

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{missing_run_id}")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_questions_returns_empty_list() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Get questions (should be empty for a run without wait.human nodes)
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/questions")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert!(body["data"].is_array());
        assert_eq!(body["meta"]["has_more"], false);
    }

    #[tokio::test]
    async fn submit_answer_not_found_run() {
        let app = test_app_with();
        let missing_run_id = fixtures::RUN_64;

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{missing_run_id}/questions/q1/answer")))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"value": "yes"})).unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_events_not_found() {
        let app = test_app_with();
        let missing_run_id = fixtures::RUN_64;

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{missing_run_id}/events")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_run_state_returns_projection() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/state")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert!(body["nodes"].is_object());
    }

    #[tokio::test]
    async fn get_run_state_includes_provenance_from_user_agent() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .header("user-agent", "fabro-cli/1.2.3")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/state")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(
            body["run"]["provenance"]["server"]["version"],
            FABRO_VERSION
        );
        assert_eq!(
            body["run"]["provenance"]["client"]["user_agent"],
            "fabro-cli/1.2.3"
        );
        assert_eq!(body["run"]["provenance"]["client"]["name"], "fabro-cli");
        assert_eq!(body["run"]["provenance"]["client"]["version"], "1.2.3");
        assert_eq!(
            body["run"]["provenance"]["subject"]["auth_method"],
            "disabled"
        );
        assert!(body["run"]["provenance"]["subject"]["login"].is_null());
    }

    #[tokio::test]
    async fn list_run_events_returns_paginated_json() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/events?since_seq=1&limit=5")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert!(body["data"].is_array());
        assert!(body["meta"]["has_more"].is_boolean());
    }

    #[tokio::test]
    async fn append_run_event_rejects_run_id_mismatch() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/events")))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "id": "evt-test",
                    "ts": "2026-03-27T12:00:00Z",
                    "run_id": fixtures::RUN_64.to_string(),
                    "event": "run.submitted",
                    "properties": {}
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_checkpoint_returns_null_initially() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Get checkpoint immediately (before run completes, may be null)
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/checkpoint")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn write_and_read_run_blob_round_trip() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/blobs")))
            .header("content-type", "application/octet-stream")
            .body(Body::from("hello blob"))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        let blob_id = body["id"].as_str().unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/blobs/{blob_id}")))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&bytes[..], b"hello blob");
    }

    #[tokio::test]
    async fn stage_artifacts_round_trip() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap();
        let stage_id = "code@2";

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!(
                "/runs/{run_id}/stages/{stage_id}/artifacts?filename=src/lib.rs"
            )))
            .header("content-type", "application/octet-stream")
            .body(Body::from("fn main() {}"))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/stages/{stage_id}/artifacts")))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["data"][0]["filename"], "src/lib.rs");

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!(
                "/runs/{run_id}/stages/{stage_id}/artifacts/download?filename=src/lib.rs"
            )))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&bytes[..], b"fn main() {}");
    }

    #[tokio::test]
    async fn create_run_returns_submitted() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"], "submitted");
    }

    #[tokio::test]
    async fn start_run_transitions_to_queued() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Create a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap();

        // Start it
        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/start")))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"], "queued");
    }

    #[tokio::test]
    async fn start_run_conflict_when_not_submitted() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Create a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap();

        // Start it (transitions to queued)
        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/start")))
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(req).await.unwrap();

        // Start it again — should 409
        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/start")))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn cancel_run_succeeds() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Cancel it
        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/cancel")))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        // Could be OK (cancelled) or CONFLICT (already completed)
        let status = response.status();
        assert!(
            status == StatusCode::OK || status == StatusCode::CONFLICT,
            "unexpected status: {status}"
        );
    }

    #[tokio::test]
    async fn cancel_nonexistent_run_returns_not_found() {
        let app = test_app_with();
        let missing_run_id = fixtures::RUN_64;

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{missing_run_id}/cancel")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_graph_returns_svg() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "version": 1,
                    "cwd": "/tmp",
                    "target": {
                        "identifier": "workflow.fabro",
                        "path": "workflow.fabro",
                    },
                    "workflows": {
                        "workflow.fabro": {
                            "source": MINIMAL_DOT,
                            "files": {},
                        },
                    },
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Request graph SVG
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/graph")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        // If graphviz is not installed, we get 502 — skip assertion
        if response.status() == StatusCode::BAD_GATEWAY {
            return;
        }

        assert_eq!(response.status(), StatusCode::OK);

        let content_type = response
            .headers()
            .get("content-type")
            .expect("content-type header should be present")
            .to_str()
            .unwrap();
        assert_eq!(content_type, "image/svg+xml");

        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let svg = String::from_utf8_lossy(&bytes);
        assert!(
            svg.contains("<?xml") || svg.contains("<svg"),
            "expected SVG content, got: {}",
            &svg[..svg.len().min(200)]
        );
    }

    #[tokio::test]
    async fn render_graph_from_manifest_returns_svg() {
        let app = test_app_with();

        let req = Request::builder()
            .method("POST")
            .uri(api("/graph/render"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "manifest": {
                        "version": 1,
                        "cwd": "/tmp",
                        "target": {
                            "identifier": "workflow.fabro",
                            "path": "workflow.fabro",
                        },
                        "workflows": {
                            "workflow.fabro": {
                                "source": MINIMAL_DOT,
                                "files": {},
                            },
                        },
                    },
                    "format": "svg",
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        if response.status() == StatusCode::BAD_GATEWAY {
            return;
        }

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .expect("content-type header should be present")
                .to_str()
                .unwrap(),
            "image/svg+xml"
        );

        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let svg = String::from_utf8_lossy(&bytes);
        assert!(
            svg.contains("<?xml") || svg.contains("<svg"),
            "expected SVG content, got: {}",
            &svg[..svg.len().min(200)]
        );
    }

    #[tokio::test]
    async fn get_graph_not_found() {
        let app = test_app_with();
        let missing_run_id = fixtures::RUN_64;

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{missing_run_id}/graph")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_runs_returns_started_run() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // List should be empty initially
        let req = Request::builder()
            .method("GET")
            .uri(api("/runs"))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 0);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // List should now contain one run
        let req = Request::builder()
            .method("GET")
            .uri(api("/runs"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        let items = body.as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["run_id"].as_str().unwrap(), run_id.to_string());
        assert!(items[0]["status"].as_str().is_some());
    }

    #[tokio::test]
    async fn delete_run_removes_durable_run() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap();

        let req = Request::builder()
            .method("DELETE")
            .uri(api(&format!("/runs/{run_id}")))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}")))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_aggregate_usage_returns_zeros_initially() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("GET")
            .uri(api("/usage"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["totals"]["runs"].as_i64().unwrap(), 0);
        assert_eq!(body["totals"]["input_tokens"].as_i64().unwrap(), 0);
        assert_eq!(body["totals"]["output_tokens"].as_i64().unwrap(), 0);
        assert_eq!(body["totals"]["cost"].as_f64().unwrap(), 0.0);
        assert_eq!(body["totals"]["runtime_secs"].as_f64().unwrap(), 0.0);
        assert!(body["by_model"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_runs_returns_submitted_status() {
        let state = create_app_state();
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Check status is submitted (no start, no scheduler running)
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "submitted");
    }

    #[tokio::test]
    async fn start_run_persists_full_settings_snapshot() {
        let settings = Settings {
            dry_run: Some(true),
            llm: Some(fabro_config::run::LlmSettings {
                model: Some("claude-sonnet-4-5".to_string()),
                provider: Some("anthropic".to_string()),
                fallbacks: None,
            }),
            sandbox: Some(fabro_config::sandbox::SandboxSettings {
                provider: Some("local".to_string()),
                ..Default::default()
            }),
            hooks: vec![fabro_hooks::HookDefinition {
                name: Some("snapshot-hook".to_string()),
                event: fabro_hooks::HookEvent::RunStart,
                command: Some("echo snapshot".to_string()),
                hook_type: None,
                matcher: None,
                blocking: Some(false),
                timeout_ms: Some(1_000),
                sandbox: Some(false),
            }],
            git: Some(fabro_config::server::GitSettings {
                app_id: Some("12345".to_string()),
                author: fabro_config::server::GitAuthorSettings {
                    name: Some("Snapshot Bot".to_string()),
                    email: Some("snapshot@example.com".to_string()),
                },
                ..Default::default()
            }),
            web: Some(fabro_config::server::WebSettings {
                url: "http://example.test".to_string(),
                ..Default::default()
            }),
            api: Some(fabro_config::server::ApiSettings {
                base_url: "http://api.example.test".to_string(),
                ..Default::default()
            }),
            log: Some(fabro_config::server::LogSettings {
                level: Some("debug".to_string()),
            }),
            ..Default::default()
        };
        let state = create_app_state_with_options(settings.clone(), 5);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        let _run_dir = {
            let runs = state.runs.lock().expect("runs lock poisoned");
            runs.get(&run_id)
                .and_then(|run| run.run_dir.clone())
                .expect("run_dir should be recorded")
        };
        let run_record = state
            .store
            .open_run_reader(&run_id)
            .await
            .unwrap()
            .state()
            .await
            .unwrap()
            .run
            .expect("run record should exist");
        let mut expected_settings = settings;
        expected_settings.goal = Some("Test".to_string());
        expected_settings.dry_run = None;

        assert_eq!(run_record.settings, expected_settings);
    }

    #[tokio::test]
    async fn cancel_queued_run_succeeds() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Submit a run (no start, stays submitted)
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Cancel it
        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/cancel")))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify status is cancelled
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}")))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "failed");
        assert_eq!(body["status_reason"].as_str().unwrap(), "cancelled");

        let req = Request::builder()
            .method("GET")
            .uri(api("/boards/runs"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id_str = run_id.to_string();
        let item = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"].as_str() == Some(run_id_str.as_str()))
            .expect("board item should exist");
        assert_eq!(item["status_reason"].as_str(), Some("cancelled"));
        assert!(item["pending_control"].is_null());

        let run_store = state.store.open_run_reader(&run_id).await.unwrap();
        let status = run_store.state().await.unwrap().status.unwrap();
        assert_eq!(status.status, WorkflowRunStatus::Failed);
        assert_eq!(status.reason, Some(WorkflowStatusReason::Cancelled));
    }

    #[tokio::test]
    async fn cancel_run_overwrites_pending_pause_request() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);
        let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
        let run_id = run_id_str.parse::<RunId>().unwrap();

        {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            let managed_run = runs.get_mut(&run_id).expect("run should exist");
            managed_run.status = RunStatus::Running;
            managed_run.worker_pid = Some(u32::MAX);
        }
        append_control_request(state.as_ref(), run_id, RunControlAction::Pause)
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/cancel")))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["pending_control"].as_str(), Some("cancel"));

        let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
        assert_eq!(summary.pending_control, Some(RunControlAction::Cancel));
    }

    #[tokio::test]
    async fn pause_run_rejects_when_control_is_already_pending() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);
        let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
        let run_id = run_id_str.parse::<RunId>().unwrap();

        {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            let managed_run = runs.get_mut(&run_id).expect("run should exist");
            managed_run.status = RunStatus::Running;
            managed_run.worker_pid = Some(u32::MAX);
        }
        append_control_request(state.as_ref(), run_id, RunControlAction::Cancel)
            .await
            .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/pause")))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
        assert_eq!(summary.pending_control, Some(RunControlAction::Cancel));
    }

    #[tokio::test]
    async fn pause_run_sets_pending_control_on_board_response() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);
        let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
        let run_id = run_id_str.parse::<RunId>().unwrap();

        {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            let managed_run = runs.get_mut(&run_id).expect("run should exist");
            managed_run.status = RunStatus::Running;
            managed_run.worker_pid = Some(u32::MAX);
        }

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/pause")))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str(), Some("running"));
        assert_eq!(body["pending_control"].as_str(), Some("pause"));

        let req = Request::builder()
            .method("GET")
            .uri(api("/boards/runs"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let item = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"].as_str() == Some(run_id_str.as_str()))
            .expect("board item should exist");
        assert_eq!(item["pending_control"].as_str(), Some("pause"));
    }

    #[tokio::test]
    async fn unpause_run_sets_pending_control() {
        let state = create_app_state();
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);
        let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
        let run_id = run_id_str.parse::<RunId>().unwrap();

        {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            let managed_run = runs.get_mut(&run_id).expect("run should exist");
            managed_run.status = RunStatus::Paused;
            managed_run.worker_pid = Some(u32::MAX);
        }

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/unpause")))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str(), Some("paused"));
        assert_eq!(body["pending_control"].as_str(), Some("unpause"));

        let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
        assert_eq!(summary.pending_control, Some(RunControlAction::Unpause));
    }

    #[tokio::test]
    async fn startup_reconciliation_marks_inflight_runs_terminal() {
        let state = create_app_state();

        create_durable_run_with_events(
            &state,
            fixtures::RUN_1,
            &[workflow_event::Event::RunSubmitted { reason: None }],
        )
        .await;
        create_durable_run_with_events(
            &state,
            fixtures::RUN_2,
            &[
                workflow_event::Event::RunSubmitted { reason: None },
                workflow_event::Event::RunStarting { reason: None },
                workflow_event::Event::RunRunning { reason: None },
            ],
        )
        .await;
        create_durable_run_with_events(
            &state,
            fixtures::RUN_3,
            &[
                workflow_event::Event::RunSubmitted { reason: None },
                workflow_event::Event::RunStarting { reason: None },
                workflow_event::Event::RunRunning { reason: None },
                workflow_event::Event::RunPaused,
                workflow_event::Event::RunCancelRequested,
            ],
        )
        .await;

        let reconciled = reconcile_incomplete_runs_on_startup(&state).await.unwrap();
        assert_eq!(reconciled, 2);

        let run_1 = state
            .store
            .open_run_reader(&fixtures::RUN_1)
            .await
            .unwrap()
            .state()
            .await
            .unwrap();
        assert_eq!(run_1.status.unwrap().status, WorkflowRunStatus::Submitted);

        let run_2 = state
            .store
            .open_run_reader(&fixtures::RUN_2)
            .await
            .unwrap()
            .state()
            .await
            .unwrap();
        let run_2_status = run_2.status.unwrap();
        assert_eq!(run_2_status.status, WorkflowRunStatus::Failed);
        assert_eq!(run_2_status.reason, Some(WorkflowStatusReason::Terminated));

        let run_3 = state
            .store
            .open_run_reader(&fixtures::RUN_3)
            .await
            .unwrap()
            .state()
            .await
            .unwrap();
        let run_3_status = run_3.status.unwrap();
        assert_eq!(run_3_status.status, WorkflowRunStatus::Failed);
        assert_eq!(run_3_status.reason, Some(WorkflowStatusReason::Cancelled));
        assert_eq!(run_3.pending_control, None);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_active_workers_terminates_process_groups() {
        let state = create_app_state();
        let run_id = fixtures::RUN_4;

        create_durable_run_with_events(
            &state,
            run_id,
            &[
                workflow_event::Event::RunSubmitted { reason: None },
                workflow_event::Event::RunStarting { reason: None },
                workflow_event::Event::RunRunning { reason: None },
            ],
        )
        .await;

        let temp_dir = tempfile::tempdir().unwrap();
        let mut child = tokio::process::Command::new("sh");
        child
            .arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        fabro_proc::pre_exec_setpgid(child.as_std_mut());
        let mut child = child.spawn().unwrap();
        let worker_pid = child.id().expect("worker pid should be available");

        {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            let mut run = managed_run(
                String::new(),
                RunStatus::Running,
                chrono::Utc::now(),
                temp_dir.path().join(run_id.to_string()),
                RunExecutionMode::Start,
            );
            run.worker_pid = Some(worker_pid);
            run.worker_pgid = Some(worker_pid);
            runs.insert(run_id, run);
        }

        let terminated = shutdown_active_workers_with_grace(
            &state,
            Duration::from_millis(50),
            Duration::from_millis(10),
        )
        .await
        .unwrap();
        assert_eq!(terminated, 1);
        assert!(!fabro_proc::process_group_alive(worker_pid));

        let exit_status = tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .expect("worker should exit after shutdown")
            .expect("wait should succeed");
        assert!(!exit_status.success());

        let run_state = state
            .store
            .open_run_reader(&run_id)
            .await
            .unwrap()
            .state()
            .await
            .unwrap();
        let run_status = run_state.status.unwrap();
        assert_eq!(run_status.status, WorkflowRunStatus::Failed);
        assert_eq!(run_status.reason, Some(WorkflowStatusReason::Terminated));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_during_startup_persists_cancelled_reason() {
        let settings = Settings {
            setup: Some(fabro_config::run::SetupSettings {
                commands: vec!["sleep 5".to_string()],
                timeout_ms: Some(30_000),
            }),
            ..Default::default()
        };
        let state = create_app_state_with_settings_and_registry_factory(settings, |interviewer| {
            fabro_workflow::handler::default_registry(interviewer, || None)
        });
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
        let run_id = run_id_str.parse::<RunId>().unwrap();

        let runner = tokio::spawn(execute_run(Arc::clone(&state), run_id));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/cancel")))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        runner.await.unwrap();

        let runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get(&run_id).expect("run should exist");
        assert_eq!(managed_run.status, RunStatus::Cancelled);
        drop(runs);

        let run_store = state.store.open_run_reader(&run_id).await.unwrap();

        let mut status_record = None;
        for _ in 0..50 {
            if let Some(record) = run_store.state().await.unwrap().status {
                if record.status == WorkflowRunStatus::Failed
                    && record.reason == Some(WorkflowStatusReason::Cancelled)
                {
                    status_record = Some(record);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let status_record = status_record.expect("status record should be persisted");
        assert_eq!(status_record.status, WorkflowRunStatus::Failed);
        assert_eq!(status_record.reason, Some(WorkflowStatusReason::Cancelled));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_before_run_transitions_to_running_closes_event_stream() {
        let state = create_app_state_with_registry_factory(|interviewer| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            fabro_workflow::handler::default_registry(interviewer, || None)
        });
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
        let run_id = run_id_str.parse::<RunId>().unwrap();

        let runner = tokio::spawn(execute_run(Arc::clone(&state), run_id));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/cancel")))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        runner.await.unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/attach")))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn queue_position_reported_for_queued_runs() {
        let state = create_app_state();
        let app = build_router(state, AuthMode::Disabled);

        // Create and start two runs (no scheduler, both stay queued)
        let first_run_id = create_and_start_run(&app, MINIMAL_DOT).await;
        let second_run_id = create_and_start_run(&app, MINIMAL_DOT).await;

        // Check queue positions via the live board endpoint.
        let req = Request::builder()
            .method("GET")
            .uri(api("/boards/runs"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let items = body["data"].as_array().unwrap();

        let first = items
            .iter()
            .find(|item| item["id"].as_str() == Some(first_run_id.as_str()))
            .unwrap();
        assert_eq!(first["queue_position"].as_i64().unwrap(), 1);

        let second = items
            .iter()
            .find(|item| item["id"].as_str() == Some(second_run_id.as_str()))
            .unwrap();
        assert_eq!(second["queue_position"].as_i64().unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrency_limit_respected() {
        let state = create_app_state_with_options(Settings::default(), 1);
        let app = test_app_with_scheduler(state);

        // Create and start two runs with max_concurrent_runs=1
        create_and_start_run(&app, MINIMAL_DOT).await;
        create_and_start_run(&app, MINIMAL_DOT).await;

        // Give scheduler time to pick up the first run
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Check statuses: at most 1 should be starting/running, the other queued
        let req = Request::builder()
            .method("GET")
            .uri(api("/boards/runs"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let items = body["data"].as_array().unwrap();
        let active_count = items
            .iter()
            .filter(|item| {
                let s = item["status"].as_str().unwrap();
                s == "starting" || s == "running"
            })
            .count();
        // With max_concurrent_runs=1, at most 1 should be active
        // (the first one might have completed already, so active could be 0 or 1)
        assert!(
            active_count <= 1,
            "expected at most 1 active run, got {active_count}"
        );
    }

    #[tokio::test]
    async fn submit_answer_to_queued_run_returns_conflict() {
        let state = create_app_state();
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(manifest_body(MINIMAL_DOT))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Try to submit an answer to a queued run
        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/questions/q1/answer")))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"value": "yes"})).unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_completion_non_streaming_returns_json() {
        let state = create_app_state_with_options(dry_run_settings(), 5);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/completions"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "messages": [{"role": "user", "content": [{"kind": "text", "data": "Hello"}]}],
                    "stream": false
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert!(body["id"].is_string());
        assert!(body["model"].is_string());
        assert_eq!(body["stop_reason"], "end_turn");
        assert!(body["message"].is_object());
        assert!(body["usage"]["input_tokens"].is_number());
        assert!(body["usage"]["output_tokens"].is_number());
    }

    #[tokio::test]
    async fn create_completion_streaming_returns_sse() {
        let state = create_app_state_with_options(dry_run_settings(), 5);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/completions"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "messages": [{"role": "user", "content": [{"kind": "text", "data": "Hello"}]}],
                    "stream": true
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "text/event-stream"
        );
    }

    #[tokio::test]
    async fn create_completion_missing_messages_returns_422() {
        let app = test_app_with();

        let req = Request::builder()
            .method("POST")
            .uri(api("/completions"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
