use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

#[cfg(test)]
use axum::body::to_bytes;
use axum::extract::{self as axum_extract, Path, Query, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::Key;
use fabro_llm::client::Client as LlmClient;
use fabro_llm::generate::{GenerateParams, generate, generate_object};
use fabro_llm::types::{
    ContentPart, FinishReason, Message as LlmMessage, Request as LlmRequest,
    Response as LlmResponse, Role, StreamEvent, ToolChoice, ToolDefinition, Usage,
};
use fabro_store::{InMemoryStore, Store};
use fabro_types::{RunId, Settings};
use fabro_util::redact::redact_jsonl_line;
use fabro_workflow::error::FabroError;
use fabro_workflow::handler::HandlerRegistry;
use futures_util::stream;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tokio::sync::{Notify, OnceCell};
use tokio::task::spawn_blocking;
use tokio::time::{sleep, timeout};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower::{ServiceExt, service_fn};
use ulid::Ulid;

use tracing::{error, info};

use crate::demo;
use crate::error::ApiError;
use crate::jwt_auth::{AuthMode, AuthenticatedService};
use crate::sessions as sessions_mod;
use crate::sessions::{SessionStore, new_session_store};
use crate::static_files;
use crate::web_auth;
use fabro_interview::{Answer, Interviewer, QuestionType, WebInterviewer};
use fabro_workflow::context::Context;
use fabro_workflow::event::{EventEmitter, RunEventEnvelope};
use fabro_workflow::operations::{self, CreateRunInput, WorkflowInput};
use fabro_workflow::pipeline::Persisted;
use fabro_workflow::records::Checkpoint;

pub use fabro_api_types::{
    ApiQuestion, ApiQuestionOption, PaginatedRunList, PaginationMeta,
    QuestionType as ApiQuestionType, RunStatus, RunStatusResponse, StartRunRequest,
    SubmitAnswerRequest,
};

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
    // Populated when running:
    interviewer: Option<Arc<WebInterviewer>>,
    event_tx: Option<broadcast::Sender<RunEventEnvelope>>,
    context: Option<Context>,
    checkpoint: Option<Checkpoint>,
    cancel_tx: Option<oneshot::Sender<()>>,
    cancel_token: Option<Arc<AtomicBool>>,
    run_dir: Option<std::path::PathBuf>,
}

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
struct AggregateUsageTotals {
    total_runs: i64,
    total_runtime_secs: f64,
    by_model: HashMap<String, ModelUsageTotals>,
}

type RegistryFactoryOverride = dyn Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync;

/// Shared application state for the server.
pub struct AppState {
    runs: Mutex<HashMap<RunId, ManagedRun>>,
    aggregate_usage: Mutex<AggregateUsageTotals>,
    store: Arc<dyn Store>,
    pub db: sqlx::SqlitePool,
    max_concurrent_runs: usize,
    scheduler_notify: Notify,
    pub sessions: SessionStore,
    llm_client: OnceCell<LlmClient>,
    pub(crate) settings: Arc<RwLock<Settings>>,
    pub(crate) session_key: Option<Key>,
    registry_factory_override: Option<Box<RegistryFactoryOverride>>,
}

impl AppState {
    pub(crate) fn dry_run(&self) -> bool {
        self.settings.read().unwrap().dry_run_enabled()
    }
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
        .route("/runs", get(demo::list_runs).post(demo::start_run_stub))
        .route("/runs/{id}", get(demo::get_run_status))
        .route("/runs/{id}/questions", get(demo::get_questions_stub))
        .route("/runs/{id}/questions/{qid}/answer", post(demo::answer_stub))
        .route("/runs/{id}/events", get(demo::run_events_stub))
        .route("/runs/{id}/checkpoint", get(demo::checkpoint_stub))
        .route("/runs/{id}/context", get(demo::context_stub))
        .route("/runs/{id}/cancel", post(demo::cancel_stub))
        .route("/runs/{id}/pause", post(demo::pause_stub))
        .route("/runs/{id}/unpause", post(demo::unpause_stub))
        .route("/runs/{id}/graph", get(demo::get_run_graph))
        .route("/runs/{id}/retro", get(demo::get_run_retro))
        .route("/runs/{id}/stages", get(demo::get_run_stages))
        .route(
            "/runs/{id}/stages/{stageId}/turns",
            get(demo::get_stage_turns),
        )
        .route("/runs/{id}/files", get(demo::get_run_files))
        .route("/runs/{id}/usage", get(demo::get_run_usage))
        .route("/runs/{id}/verification", get(demo::get_run_verification))
        .route("/runs/{id}/settings", get(demo::get_run_settings))
        .route("/runs/{id}/steer", post(demo::steer_run_stub))
        .route("/runs/{id}/preview", post(demo::generate_preview_url_stub))
        .route("/workflows", get(demo::list_workflows))
        .route("/workflows/{name}", get(demo::get_workflow))
        .route("/workflows/{name}/runs", get(demo::list_workflow_runs))
        .route(
            "/verification/criteria",
            get(demo::list_verification_criteria),
        )
        .route(
            "/verification/criteria/{id}",
            get(demo::get_verification_criterion),
        )
        .route(
            "/verification/controls",
            get(demo::list_verification_controls),
        )
        .route(
            "/verification/controls/{id}",
            get(demo::get_verification_control),
        )
        .route(
            "/verification/signoffs",
            get(demo::list_signoffs).post(demo::create_signoff_stub),
        )
        .route("/verification/signoffs/{id}", get(demo::get_signoff))
        .route("/retros", get(demo::list_retros))
        .route(
            "/sessions",
            get(demo::list_sessions).post(demo::create_session_stub),
        )
        .route("/sessions/{id}", get(demo::get_session))
        .route("/sessions/{id}/messages", post(demo::send_message_stub))
        .route("/sessions/{id}/events", get(demo::session_events_stub))
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
        .route("/models", get(demo::list_models))
        .route("/models/{id}/test", post(test_model))
        .route("/completions", post(create_completion))
        .route("/settings", get(demo::get_server_settings))
        .route("/usage", get(demo::get_aggregate_usage))
}

fn real_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs", get(list_runs).post(start_run))
        .route("/runs/{id}", get(get_run_status))
        .route("/runs/{id}/questions", get(get_questions))
        .route("/runs/{id}/questions/{qid}/answer", post(submit_answer))
        .route("/runs/{id}/events", get(get_events))
        .route("/runs/{id}/checkpoint", get(get_checkpoint))
        .route("/runs/{id}/context", get(get_context))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/pause", post(pause_run))
        .route("/runs/{id}/unpause", post(unpause_run))
        .route("/runs/{id}/graph", get(get_graph))
        .route("/runs/{id}/retro", get(get_retro))
        .route("/runs/{id}/stages", get(not_implemented))
        .route("/runs/{id}/stages/{stageId}/turns", get(not_implemented))
        .route("/runs/{id}/files", get(not_implemented))
        .route("/runs/{id}/usage", get(not_implemented))
        .route("/runs/{id}/verification", get(not_implemented))
        .route("/runs/{id}/settings", get(not_implemented))
        .route("/runs/{id}/steer", post(not_implemented))
        .route("/runs/{id}/preview", post(not_implemented))
        .route("/workflows", get(not_implemented))
        .route("/workflows/{name}", get(not_implemented))
        .route("/workflows/{name}/runs", get(not_implemented))
        .route("/verification/criteria", get(not_implemented))
        .route("/verification/criteria/{id}", get(not_implemented))
        .route("/verification/controls", get(not_implemented))
        .route("/verification/controls/{id}", get(not_implemented))
        .route(
            "/verification/signoffs",
            get(not_implemented).post(not_implemented),
        )
        .route("/verification/signoffs/{id}", get(not_implemented))
        .route("/retros", get(not_implemented))
        .route(
            "/sessions",
            get(sessions_mod::list_sessions).post(sessions_mod::create_session),
        )
        .route("/sessions/{id}", get(sessions_mod::retrieve_session))
        .route("/sessions/{id}/messages", post(sessions_mod::send_message))
        .route(
            "/sessions/{id}/events",
            get(sessions_mod::stream_session_events),
        )
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
        .route("/models", get(demo::list_models))
        .route("/models/{id}/test", post(test_model))
        .route("/completions", post(create_completion))
        .route("/settings", get(not_implemented))
        .route("/usage", get(get_aggregate_usage))
}

async fn not_implemented() -> Response {
    ApiError::new(StatusCode::NOT_IMPLEMENTED, "Not implemented.").into_response()
}

async fn health() -> Response {
    Json(serde_json::json!({"status": "ok"})).into_response()
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
    if let Some(key) = &state.session_key {
        if let Some(session) = web_auth::read_private_session(req.headers(), key) {
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
    let by_model: Vec<fabro_api_types::UsageByModel> = agg
        .by_model
        .iter()
        .map(|(model, totals)| fabro_api_types::UsageByModel {
            model: fabro_api_types::ModelReference { id: model.clone() },
            stages: totals.stages,
            usage: fabro_api_types::TokenUsage {
                input_tokens: totals.input_tokens,
                output_tokens: totals.output_tokens,
                cost: totals.cost,
            },
        })
        .collect();
    let response = fabro_api_types::AggregateUsage {
        totals: fabro_api_types::AggregateUsageTotals {
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

/// Create an `AppState` with the given LLM spec factory and database pool.
pub fn create_app_state(db: sqlx::SqlitePool) -> Arc<AppState> {
    create_app_state_with_options(db, Settings::default(), 5)
}

#[doc(hidden)]
pub fn create_app_state_with_registry_factory(
    db: sqlx::SqlitePool,
    registry_factory_override: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    build_app_state(
        db,
        Arc::new(RwLock::new(Settings::default())),
        Some(Box::new(registry_factory_override)),
        5,
        Arc::new(InMemoryStore::default()),
    )
}

/// Create an `AppState` with the given database pool, settings, and concurrency limit.
pub fn create_app_state_with_options(
    db: sqlx::SqlitePool,
    settings: Settings,
    max_concurrent_runs: usize,
) -> Arc<AppState> {
    create_app_state_with_store(
        db,
        Arc::new(RwLock::new(settings)),
        max_concurrent_runs,
        Arc::new(InMemoryStore::default()),
    )
}

pub fn create_app_state_with_store(
    db: sqlx::SqlitePool,
    settings: Arc<RwLock<Settings>>,
    max_concurrent_runs: usize,
    store: Arc<dyn Store>,
) -> Arc<AppState> {
    build_app_state(db, settings, None, max_concurrent_runs, store)
}

fn build_app_state(
    db: sqlx::SqlitePool,
    settings: Arc<RwLock<Settings>>,
    registry_factory_override: Option<Box<RegistryFactoryOverride>>,
    max_concurrent_runs: usize,
    store: Arc<dyn Store>,
) -> Arc<AppState> {
    Arc::new(AppState {
        runs: Mutex::new(HashMap::new()),
        aggregate_usage: Mutex::new(AggregateUsageTotals::default()),
        store,
        db,
        max_concurrent_runs,
        scheduler_notify: Notify::new(),
        sessions: new_session_store(),
        llm_client: OnceCell::new(),
        session_key: web_auth::session_key_from_env(),
        settings,
        registry_factory_override,
    })
}

async fn list_runs(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    let runs = state.runs.lock().expect("runs lock poisoned");
    let queue_positions = compute_queue_positions(&runs);
    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset as usize;
    let all_items: Vec<RunStatusResponse> = runs
        .iter()
        .map(|(id, managed_run)| RunStatusResponse {
            id: id.to_string(),
            status: managed_run.status,
            error: managed_run
                .error
                .as_ref()
                .map(|msg| fabro_api_types::RunError {
                    message: msg.clone(),
                }),
            queue_position: queue_positions.get(id).copied(),
            created_at: managed_run.created_at,
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

fn clear_live_run_state(run: &mut ManagedRun) {
    run.interviewer = None;
    run.event_tx = None;
    run.cancel_tx = None;
    run.cancel_token = None;
}

async fn start_run(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(req): Json<StartRunRequest>,
) -> Response {
    let run_id = RunId::new();
    info!(run_id = %run_id, "Run queued");
    let run_dir = std::env::temp_dir().join(format!("fabro-{}", uuid::Uuid::new_v4()));
    let settings = state.settings.read().unwrap().clone();
    let created = match operations::create(
        state.store.as_ref(),
        CreateRunInput {
            workflow: WorkflowInput::DotSource {
                source: req.dot_source.clone(),
                base_dir: None,
            },
            settings,
            cwd: std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir()),
            workflow_slug: None,
            run_dir: Some(run_dir.clone()),
            run_id: Some(run_id),
            host_repo_path: None,
            base_branch: None,
        },
    )
    .await
    {
        Ok(created) => created,
        Err(ref err @ FabroError::ValidationFailed { ref diagnostics }) => {
            let message = if diagnostics.is_empty() {
                err.to_string()
            } else {
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            return ApiError::bad_request(message).into_response();
        }
        Err(err @ FabroError::Parse(_)) => {
            return ApiError::bad_request(err.to_string()).into_response();
        }
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to persist run state: {err}"),
            )
            .into_response();
        }
    };
    let persisted = created.persisted;
    let created_at = persisted.run_record().created_at;

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            run_id,
            ManagedRun {
                dot_source: req.dot_source,
                status: RunStatus::Queued,
                error: None,
                created_at,
                interviewer: None,
                event_tx: None,
                context: None,
                checkpoint: None,
                cancel_tx: None,
                cancel_token: None,
                run_dir: Some(run_dir),
            },
        );
    }

    state.scheduler_notify.notify_one();

    (
        StatusCode::CREATED,
        Json(RunStatusResponse {
            id: run_id.to_string(),
            status: RunStatus::Queued,
            error: None,
            queue_position: None,
            created_at,
        }),
    )
        .into_response()
}

/// Execute a single run: transitions queued → starting → running → completed/failed/cancelled.
async fn execute_run(state: Arc<AppState>, run_id: RunId) {
    // Transition to Starting and set up cancel infrastructure
    let (cancel_rx, run_dir, event_tx, cancel_token) = {
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
        )
    };

    // Create interviewer and event plumbing (this is the "provisioning" phase)
    let interviewer = Arc::new(WebInterviewer::new());
    let context = Context::new();
    let emitter = EventEmitter::new(run_id);
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

    // Transition to Running, populate interviewer + context
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&run_id) {
            if managed_run.status != RunStatus::Starting {
                // Was cancelled during setup
                clear_live_run_state(managed_run);
                state.scheduler_notify.notify_one();
                return;
            }
            managed_run.status = RunStatus::Running;
            managed_run.interviewer = Some(Arc::clone(&interviewer));
            managed_run.context = Some(context);
        }
    }

    let run_store = match state.store.open_run(&run_id).await {
        Ok(Some(run_store)) => run_store,
        Ok(None) => {
            tracing::error!(run_id = %run_id, "Run store missing");
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            if let Some(managed_run) = runs.get_mut(&run_id) {
                managed_run.status = RunStatus::Failed;
                managed_run.error = Some("Run store missing".to_string());
                clear_live_run_state(managed_run);
            }
            state.scheduler_notify.notify_one();
            return;
        }
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
    let persisted = match Persisted::load_from_store(run_store.as_ref(), &run_dir).await {
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
    let github_app = match fabro_github::GitHubAppCredentials::from_env(
        persisted.run_record().settings.app_id(),
    ) {
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
        run_store: Arc::clone(&run_store),
        github_app,
        on_node: None,
        registry_override,
    };

    let result = tokio::select! {
        result = operations::start(&run_dir, services) => result,
        _ = cancel_rx => {
            cancel_token.store(true, Ordering::SeqCst);
            Err(FabroError::Cancelled)
        }
    };

    // Save final checkpoint
    let checkpoint = match run_store.get_checkpoint().await {
        Ok(checkpoint) => checkpoint,
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Failed to load checkpoint from store");
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
        }
        managed_run.checkpoint = checkpoint;
        if let Ok(started) = &result {
            if let Some(ctx) = &started.final_context {
                managed_run.context = Some(ctx.clone());
            }
        }
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
            // Promote as many queued runs as capacity allows
            loop {
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
    let runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get(&id) {
        Some(managed_run) => {
            let queue_position = if managed_run.status == RunStatus::Queued {
                let positions = compute_queue_positions(&runs);
                positions.get(&id).copied()
            } else {
                None
            };
            (
                StatusCode::OK,
                Json(RunStatusResponse {
                    id: id.to_string(),
                    status: managed_run.status,
                    error: managed_run
                        .error
                        .as_ref()
                        .map(|msg| fabro_api_types::RunError {
                            message: msg.clone(),
                        }),
                    created_at: managed_run.created_at,
                    queue_position,
                }),
            )
                .into_response()
        }
        None => ApiError::not_found("Run not found.").into_response(),
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
    let runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get(&id) {
        Some(managed_run) => {
            let Some(interviewer) = &managed_run.interviewer else {
                return (
                    StatusCode::OK,
                    Json(ListResponse::new(Vec::<ApiQuestion>::new())),
                )
                    .into_response();
            };
            let pending = interviewer.pending_questions();
            let questions: Vec<ApiQuestion> = pending
                .into_iter()
                .map(|pq| ApiQuestion {
                    id: pq.id,
                    text: pq.question.text.clone(),
                    question_type: match pq.question.question_type {
                        QuestionType::YesNo => ApiQuestionType::YesNo,
                        QuestionType::MultipleChoice => ApiQuestionType::MultipleChoice,
                        QuestionType::MultiSelect => ApiQuestionType::MultiSelect,
                        QuestionType::Freeform => ApiQuestionType::Freeform,
                        QuestionType::Confirmation => ApiQuestionType::Confirmation,
                    },
                    options: pq
                        .question
                        .options
                        .iter()
                        .map(|o| ApiQuestionOption {
                            key: o.key.clone(),
                            label: o.label.clone(),
                        })
                        .collect(),
                    allow_freeform: pq.question.allow_freeform,
                })
                .collect();
            (StatusCode::OK, Json(ListResponse::new(questions))).into_response()
        }
        None => ApiError::not_found("Run not found.").into_response(),
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
    let runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get(&id) {
        Some(managed_run) => {
            let Some(interviewer) = &managed_run.interviewer else {
                return ApiError::new(StatusCode::CONFLICT, "Run is not yet running.")
                    .into_response();
            };
            let answer = if let Some(key) = &req.selected_option_key {
                let option = interviewer
                    .pending_questions()
                    .iter()
                    .find(|pq| pq.id == qid)
                    .and_then(|pq| pq.question.options.iter().find(|o| o.key == *key))
                    .cloned();
                match option {
                    Some(opt) => Answer::selected(key.clone(), opt),
                    None => {
                        return ApiError::bad_request("Invalid option key.").into_response();
                    }
                }
            } else if !req.selected_option_keys.is_empty() {
                let pending = interviewer.pending_questions();
                let pq = pending.iter().find(|pq| pq.id == qid);
                for key in &req.selected_option_keys {
                    let valid = pq
                        .and_then(|pq| pq.question.options.iter().find(|o| o.key == *key))
                        .is_some();
                    if !valid {
                        return ApiError::bad_request("Invalid option key.").into_response();
                    }
                }
                Answer::multi_selected(req.selected_option_keys)
            } else if let Some(v) = req.value {
                Answer::text(v)
            } else {
                return ApiError::bad_request(
                    "One of value, selected_option_key, or selected_option_keys is required.",
                )
                .into_response();
            };
            let accepted = interviewer.submit_answer(&qid, answer);
            if accepted {
                StatusCode::NO_CONTENT.into_response()
            } else {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "Question no longer exists or was already answered.",
                )
                .into_response()
            }
        }
        None => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn get_events(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let rx = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => match &managed_run.event_tx {
                Some(tx) => tx.subscribe(),
                None => {
                    return ApiError::new(StatusCode::GONE, "Event stream closed.").into_response();
                }
            },
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            let data = redact_jsonl_line(&data);
            Some(Ok::<Event, std::convert::Infallible>(
                Event::default().data(data),
            ))
        }
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
    let runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get(&id) {
        Some(managed_run) => match &managed_run.checkpoint {
            Some(cp) => (StatusCode::OK, Json(cp.clone())).into_response(),
            None => (StatusCode::OK, Json(serde_json::json!(null))).into_response(),
        },
        None => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn get_context(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get(&id) {
        Some(managed_run) => match &managed_run.context {
            Some(ctx) => (StatusCode::OK, Json(ctx.snapshot())).into_response(),
            None => (StatusCode::OK, Json(serde_json::json!({}))).into_response(),
        },
        None => ApiError::not_found("Run not found.").into_response(),
    }
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
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get_mut(&id) {
        Some(managed_run) => match managed_run.status {
            RunStatus::Queued | RunStatus::Starting | RunStatus::Running => {
                if let Some(token) = &managed_run.cancel_token {
                    token.store(true, Ordering::Relaxed);
                }
                if let Some(cancel_tx) = managed_run.cancel_tx.take() {
                    let _ = cancel_tx.send(());
                }
                managed_run.status = RunStatus::Cancelled;
                let created_at = managed_run.created_at;
                (
                    StatusCode::OK,
                    Json(RunStatusResponse {
                        id: id.to_string(),
                        status: RunStatus::Cancelled,
                        error: None,
                        queue_position: None,
                        created_at,
                    }),
                )
                    .into_response()
            }
            _ => ApiError::new(StatusCode::CONFLICT, "Run is not cancellable.").into_response(),
        },
        None => ApiError::not_found("Run not found.").into_response(),
    }
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
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get_mut(&id) {
        Some(managed_run) => match managed_run.status {
            RunStatus::Running => {
                managed_run.status = RunStatus::Paused;
                let created_at = managed_run.created_at;
                (
                    StatusCode::OK,
                    Json(RunStatusResponse {
                        id: id.to_string(),
                        status: RunStatus::Paused,
                        error: None,
                        queue_position: None,
                        created_at,
                    }),
                )
                    .into_response()
            }
            _ => ApiError::new(StatusCode::CONFLICT, "Run is not pausable.").into_response(),
        },
        None => ApiError::not_found("Run not found.").into_response(),
    }
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
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get_mut(&id) {
        Some(managed_run) => match managed_run.status {
            RunStatus::Paused => {
                managed_run.status = RunStatus::Running;
                let created_at = managed_run.created_at;
                (
                    StatusCode::OK,
                    Json(RunStatusResponse {
                        id: id.to_string(),
                        status: RunStatus::Running,
                        error: None,
                        queue_position: None,
                        created_at,
                    }),
                )
                    .into_response()
            }
            _ => ApiError::new(StatusCode::CONFLICT, "Run is not paused.").into_response(),
        },
        None => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn test_model(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(info) = fabro_model::Catalog::builtin().get(&id) else {
        return ApiError::not_found(format!("Model not found: {id}")).into_response();
    };

    if state.dry_run() {
        return Json(serde_json::json!({
            "model_id": id,
            "status": "ok",
        }))
        .into_response();
    }

    let params = GenerateParams::new(&info.id)
        .provider(info.provider.as_str())
        .prompt("Say OK")
        .max_tokens(16);

    let result = timeout(Duration::from_secs(30), generate(params)).await;

    match result {
        Ok(Ok(_)) => Json(serde_json::json!({
            "model_id": id,
            "status": "ok",
        }))
        .into_response(),
        Ok(Err(e)) => Json(serde_json::json!({
            "model_id": id,
            "status": "error",
            "error_message": e.to_string(),
        }))
        .into_response(),
        Err(_) => Json(serde_json::json!({
            "model_id": id,
            "status": "error",
            "error_message": "timeout (30s)",
        }))
        .into_response(),
    }
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

fn convert_api_message(msg: &fabro_api_types::CompletionMessage) -> LlmMessage {
    let role = match msg.role {
        fabro_api_types::CompletionMessageRole::System => Role::System,
        fabro_api_types::CompletionMessageRole::User => Role::User,
        fabro_api_types::CompletionMessageRole::Assistant => Role::Assistant,
        fabro_api_types::CompletionMessageRole::Tool => Role::Tool,
        fabro_api_types::CompletionMessageRole::Developer => Role::Developer,
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

fn convert_llm_message(msg: &LlmMessage) -> fabro_api_types::CompletionMessage {
    let role = match msg.role {
        Role::System => fabro_api_types::CompletionMessageRole::System,
        Role::User => fabro_api_types::CompletionMessageRole::User,
        Role::Assistant => fabro_api_types::CompletionMessageRole::Assistant,
        Role::Tool => fabro_api_types::CompletionMessageRole::Tool,
        Role::Developer => fabro_api_types::CompletionMessageRole::Developer,
    };
    let content: Vec<fabro_api_types::CompletionContentPart> = msg
        .content
        .iter()
        .filter_map(|part| {
            let json = serde_json::to_value(part).ok()?;
            serde_json::from_value(json).ok()
        })
        .collect();
    fabro_api_types::CompletionMessage {
        role,
        content,
        name: msg.name.clone(),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

async fn create_completion(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(req): Json<fabro_api_types::CreateCompletionRequest>,
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
        fabro_api_types::CompletionToolChoiceMode::Auto => ToolChoice::Auto,
        fabro_api_types::CompletionToolChoiceMode::None => ToolChoice::None,
        fabro_api_types::CompletionToolChoiceMode::Required => ToolChoice::Required,
        fabro_api_types::CompletionToolChoiceMode::Named => {
            ToolChoice::named(tc.tool_name.unwrap_or_default())
        }
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
        let empty_msg = fabro_api_types::CompletionMessage {
            role: fabro_api_types::CompletionMessageRole::Assistant,
            content: vec![],
            name: None,
            tool_call_id: None,
        };
        return Json(fabro_api_types::CompletionResponse {
            id: msg_id,
            model: model_id,
            message: empty_msg,
            stop_reason: "end_turn".to_string(),
            usage: fabro_api_types::CompletionUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
            output: None,
        })
        .into_response();
    }

    // Get or create LLM client (cached in AppState)
    let client = match state.llm_client.get_or_try_init(LlmClient::from_env).await {
        Ok(c) => c,
        Err(e) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create LLM client: {e}"),
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
                Ok(result) => Json(fabro_api_types::CompletionResponse {
                    id: msg_id,
                    model: model_id,
                    message: convert_llm_message(&result.response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&result.finish_reason),
                    usage: fabro_api_types::CompletionUsage {
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
                Ok(response) => Json(fabro_api_types::CompletionResponse {
                    id: response.id,
                    model: response.model,
                    message: convert_llm_message(&response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&response.finish_reason),
                    usage: fabro_api_types::CompletionUsage {
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

async fn get_retro(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    {
        let runs = state.runs.lock().expect("runs lock poisoned");
        if !runs.contains_key(&id) {
            return ApiError::not_found("Run not found.").into_response();
        }
    }

    match state.store.open_run_reader(&id).await {
        Ok(Some(run_store)) => match run_store.get_retro().await {
            Ok(Some(retro)) => (StatusCode::OK, Json(retro)).into_response(),
            Ok(None) => (StatusCode::OK, Json(serde_json::json!(null))).into_response(),
            Err(err) => {
                tracing::warn!(run_id = %id, error = %err, "Failed to load retro from store");
                (StatusCode::OK, Json(serde_json::json!(null))).into_response()
            }
        },
        Ok(None) => (StatusCode::OK, Json(serde_json::json!(null))).into_response(),
        Err(err) => {
            tracing::warn!(run_id = %id, error = %err, "Failed to open run store reader");
            (StatusCode::OK, Json(serde_json::json!(null))).into_response()
        }
    }
}

/// Render DOT source to a styled SVG via `render_dot` on a blocking thread.
pub(crate) async fn render_dot_svg(dot_source: &str) -> Response {
    use fabro_graphviz::render::{GraphFormat, render_dot};

    let source = dot_source.to_owned();
    match spawn_blocking(move || render_dot(&source, GraphFormat::Svg)).await {
        Ok(Ok(bytes)) => {
            (StatusCode::OK, [("content-type", "image/svg+xml")], bytes).into_response()
        }
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
    let dot_source = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => managed_run.dot_source.clone(),
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    render_dot_svg(&dot_source).await
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
    use tower::ServiceExt;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Test"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
    const POLL_ATTEMPTS: usize = 500;

    fn dry_run_settings() -> Settings {
        Settings {
            dry_run: Some(true),
            ..Default::default()
        }
    }

    fn command_dot(command: &str) -> String {
        format!(
            r#"digraph Test {{
                graph [goal="Test"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                command [shape=parallelogram, tool_command="{command}"]
                start -> command -> exit
            }}"#
        )
    }

    async fn test_db() -> sqlx::SqlitePool {
        let pool = fabro_db::connect_memory().await.unwrap();
        fabro_db::initialize_db(&pool).await.unwrap();
        pool
    }

    fn test_app_with(db: sqlx::SqlitePool) -> Router {
        let state = create_app_state(db);
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

    #[tokio::test]
    async fn test_model_unknown_returns_404() {
        let app = test_app_with(test_db().await);

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
        let app = test_app_with(test_db().await);

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
            create_app_state_with_options(test_db().await, settings, 5),
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
        let app = test_app_with(test_db().await);

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
        let app = test_app_with(test_db().await);

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
        let state = create_app_state_with_options(test_db().await, dry_run_settings(), 5);
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
        let state = create_app_state_with_options(test_db().await, dry_run_settings(), 5);
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
        let app = test_app_with(test_db().await);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let body = body_json(response.into_body()).await;
        assert!(body["id"].is_string());
        assert!(!body["id"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_runs_invalid_dot_returns_bad_request() {
        let app = test_app_with(test_db().await);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": "not a graph"})).unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_run_status_returns_status() {
        let state = create_app_state(test_db().await);
        let app = test_app_with_scheduler(state);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

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
        assert_eq!(body["id"].as_str().unwrap(), run_id.to_string());
        let status = body["status"].as_str().unwrap();
        assert!(
            status == "queued"
                || status == "starting"
                || status == "running"
                || status == "completed",
            "unexpected status: {status}"
        );
    }

    #[tokio::test]
    async fn get_run_status_not_found() {
        let app = test_app_with(test_db().await);
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
        let state = create_app_state(test_db().await);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
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
        let app = test_app_with(test_db().await);
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
        let app = test_app_with(test_db().await);
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
    async fn get_checkpoint_returns_null_initially() {
        let state = create_app_state(test_db().await);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
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
    async fn get_context_returns_map() {
        let state = create_app_state(test_db().await);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Get context
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/context")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert!(body.is_object());
    }

    #[tokio::test]
    async fn cancel_run_succeeds() {
        let state = create_app_state(test_db().await);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
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
        let app = test_app_with(test_db().await);
        let missing_run_id = fixtures::RUN_64;

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{missing_run_id}/cancel")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_events_returns_sse_stream() {
        let state = create_app_state(test_db().await);
        let app = test_app_with_scheduler(state);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Wait for scheduler to promote run (creates event_tx)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Request the SSE stream
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/events")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        // May be 200 (stream open) or 410 (run completed before we connect)
        let status = response.status();
        assert!(
            status == StatusCode::OK || status == StatusCode::GONE,
            "unexpected status: {status}"
        );

        if status == StatusCode::OK {
            let content_type = response
                .headers()
                .get("content-type")
                .expect("content-type header should be present")
                .to_str()
                .unwrap();
            assert!(
                content_type.contains("text/event-stream"),
                "expected text/event-stream, got: {content_type}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completes_and_status_is_completed() {
        let state = create_app_state_with_options(test_db().await, dry_run_settings(), 5);
        let app = test_app_with_scheduler(state);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Poll until run completes
        let mut status = String::new();
        for _ in 0..POLL_ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
            let req = Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}")))
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response.into_body()).await;
            status = body["status"].as_str().unwrap().to_string();
            if status == "completed" || status == "failed" {
                break;
            }
        }
        assert_eq!(status, "completed");
    }

    #[tokio::test]
    async fn get_graph_returns_svg() {
        let state = create_app_state(test_db().await);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
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
    async fn get_graph_not_found() {
        let app = test_app_with(test_db().await);
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
        let state = create_app_state(test_db().await);
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
        assert_eq!(body["data"].as_array().unwrap().len(), 0);
        assert!(!body["meta"]["has_more"].as_bool().unwrap());

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
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
        let items = body["data"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"].as_str().unwrap(), run_id.to_string());
        assert!(items[0]["status"].as_str().is_some());
        assert!(!body["meta"]["has_more"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn get_aggregate_usage_returns_zeros_initially() {
        let state = create_app_state(test_db().await);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aggregate_usage_increments_after_run_completes() {
        let state = create_app_state_with_options(test_db().await, dry_run_settings(), 5);
        let app = test_app_with_scheduler(state);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Poll until run completes
        let mut status = String::new();
        for _ in 0..POLL_ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
            let req = Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}")))
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            let body = body_json(response.into_body()).await;
            status = body["status"].as_str().unwrap().to_string();
            if status == "completed" || status == "failed" {
                break;
            }
        }
        assert_eq!(status, "completed");

        // Check aggregate usage
        let req = Request::builder()
            .method("GET")
            .uri(api("/usage"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["totals"]["runs"].as_i64().unwrap(), 1);
    }

    #[tokio::test]
    async fn post_runs_returns_queued_status() {
        let state = create_app_state(test_db().await);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        // Check status is queued (no scheduler running)
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "queued");
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
        let state = create_app_state_with_options(test_db().await, settings.clone(), 5);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
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
            .expect("run store should exist")
            .get_run()
            .await
            .unwrap()
            .expect("run record should exist");
        let mut expected_settings = settings;
        expected_settings.goal = Some("Test".to_string());

        assert_eq!(run_record.settings, expected_settings);
    }

    #[tokio::test]
    async fn config_change_after_submission_does_not_affect_execution() {
        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("executed.txt");
        let dot = command_dot(&format!("printf snapshot > {}", output_path.display()));
        let initial_settings = dry_run_settings();
        let state = create_app_state_with_options(test_db().await, initial_settings.clone(), 5);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({ "dot_source": dot })).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        *state.settings.write().unwrap() = Settings::default();

        execute_run(Arc::clone(&state), run_id).await;

        let runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get(&run_id).expect("run should still exist");
        assert_eq!(managed_run.status, RunStatus::Completed);
        drop(runs);

        assert!(
            !output_path.exists(),
            "run should still use snapshotted dry-run settings"
        );
    }

    #[tokio::test]
    async fn cancel_queued_run_succeeds() {
        let state = create_app_state(test_db().await);
        let app = build_router(state, AuthMode::Disabled);

        // Submit a run (no scheduler, stays queued)
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
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

        let response = app.oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "cancelled");
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
        let state = create_app_state_with_options(test_db().await, settings, 5);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

        let runner = tokio::spawn(execute_run(Arc::clone(&state), run_id));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            let managed_run = runs.get_mut(&run_id).expect("run should exist");
            if let Some(token) = &managed_run.cancel_token {
                token.store(true, Ordering::SeqCst);
            }
            if let Some(cancel_tx) = managed_run.cancel_tx.take() {
                let _ = cancel_tx.send(());
            }
        }

        runner.await.unwrap();

        let runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get(&run_id).expect("run should exist");
        assert_eq!(managed_run.status, RunStatus::Cancelled);
        drop(runs);

        let run_store = state
            .store
            .open_run_reader(&run_id)
            .await
            .unwrap()
            .expect("run store should exist");

        let mut status_record = None;
        for _ in 0..50 {
            if let Some(record) = run_store.get_status().await.unwrap() {
                if record.status == fabro_workflow::run_status::RunStatus::Failed
                    && record.reason == Some(fabro_workflow::run_status::StatusReason::Cancelled)
                {
                    status_record = Some(record);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let status_record = status_record.expect("status record should be persisted");
        assert_eq!(
            status_record.status,
            fabro_workflow::run_status::RunStatus::Failed
        );
        assert_eq!(
            status_record.reason,
            Some(fabro_workflow::run_status::StatusReason::Cancelled)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_before_run_transitions_to_running_closes_event_stream() {
        let state = create_app_state_with_registry_factory(test_db().await, |interviewer| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            fabro_workflow::handler::default_registry(interviewer, || None)
        });
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

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
            .uri(api(&format!("/runs/{run_id}/events")))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn queue_position_reported_for_queued_runs() {
        let state = create_app_state(test_db().await);
        let app = build_router(state, AuthMode::Disabled);

        // Submit two runs (no scheduler, both stay queued)
        let mut run_ids = Vec::new();
        for _ in 0..2 {
            let req = Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
                ))
                .unwrap();

            let response = app.clone().oneshot(req).await.unwrap();
            let body = body_json(response.into_body()).await;
            run_ids.push(body["id"].as_str().unwrap().to_string());
        }

        // Check queue positions via individual status
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{}", run_ids[0])))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["queue_position"].as_i64().unwrap(), 1);

        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{}", run_ids[1])))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["queue_position"].as_i64().unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrency_limit_respected() {
        let state = create_app_state_with_options(test_db().await, Settings::default(), 1);
        let app = test_app_with_scheduler(state);

        // Submit two runs with max_concurrent_runs=1
        let mut run_ids = Vec::new();
        for _ in 0..2 {
            let req = Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
                ))
                .unwrap();

            let response = app.clone().oneshot(req).await.unwrap();
            let body = body_json(response.into_body()).await;
            run_ids.push(body["id"].as_str().unwrap().to_string());
        }

        // Give scheduler time to pick up the first run
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Check statuses: at most 1 should be starting/running, the other queued
        let req = Request::builder()
            .method("GET")
            .uri(api("/runs"))
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
        let state = create_app_state(test_db().await);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
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
        let state = create_app_state_with_options(test_db().await, dry_run_settings(), 5);
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
        let state = create_app_state_with_options(test_db().await, dry_run_settings(), 5);
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
        let app = test_app_with(test_db().await);

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
