use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower::ServiceExt;

use tracing::{error, info};

use fabro_agent::LocalSandbox;

use crate::error::ApiError;
use crate::jwt_auth::{AuthMode, AuthenticatedService, AuthenticatedUser};
use fabro_interview::{Answer, Interviewer, QuestionType, WebInterviewer};
use fabro_workflows::context::Context;
use fabro_workflows::event::{EventEmitter, WorkflowRunEvent};
use fabro_workflows::handler::HandlerRegistry;
use fabro_workflows::operations::{self, CreateOptions};
use fabro_workflows::pipeline::{self, InitOptions};
use fabro_workflows::records::Checkpoint;
use fabro_workflows::run_settings::LifecycleConfig;
use fabro_workflows::run_settings::RunSettings;

pub use fabro_types::{
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
    graph: fabro_graphviz::graph::Graph,
    status: RunStatus,
    error: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    // Populated when running:
    interviewer: Option<Arc<WebInterviewer>>,
    event_tx: Option<broadcast::Sender<WorkflowRunEvent>>,
    context: Option<Context>,
    checkpoint: Option<Checkpoint>,
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
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

/// Shared application state for the server.
pub struct AppState {
    runs: Mutex<HashMap<String, ManagedRun>>,
    aggregate_usage: Mutex<AggregateUsageTotals>,
    registry_factory: Box<dyn Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync>,
    pub dry_run: bool,
    pub db: sqlx::SqlitePool,
    max_concurrent_runs: usize,
    scheduler_notify: tokio::sync::Notify,
    pub hooks: Vec<fabro_hooks::HookDefinition>,
    git_author: fabro_workflows::git::GitAuthor,
    pub sessions: crate::sessions::SessionStore,
    llm_client: tokio::sync::OnceCell<fabro_llm::client::Client>,
}

/// Build the axum Router with all run endpoints.
///
/// Both a demo router and a real router are constructed. Incoming requests
/// with the `X-Fabro-Demo: 1` header are dispatched to the demo router;
/// all other requests go to the real router.
pub fn build_router(state: Arc<AppState>, auth_mode: AuthMode) -> Router {
    let common = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/openapi.json", get(openapi_spec))
        .route("/user", get(get_user));

    let demo_router = common
        .clone()
        .merge(demo_routes())
        .layer(axum::Extension(AuthMode::Disabled))
        .with_state(state.clone());

    let real_router = common
        .merge(real_routes())
        .layer(axum::Extension(auth_mode))
        .with_state(state);

    let dispatch = tower::service_fn(move |req: axum::extract::Request| {
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

    Router::new().fallback_service(dispatch)
}

fn demo_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/runs",
            get(crate::demo::list_runs).post(crate::demo::start_run_stub),
        )
        .route("/runs/{id}", get(crate::demo::get_run_status))
        .route("/runs/{id}/questions", get(crate::demo::get_questions_stub))
        .route(
            "/runs/{id}/questions/{qid}/answer",
            post(crate::demo::answer_stub),
        )
        .route("/runs/{id}/events", get(crate::demo::run_events_stub))
        .route("/runs/{id}/checkpoint", get(crate::demo::checkpoint_stub))
        .route("/runs/{id}/context", get(crate::demo::context_stub))
        .route("/runs/{id}/cancel", post(crate::demo::cancel_stub))
        .route("/runs/{id}/pause", post(crate::demo::pause_stub))
        .route("/runs/{id}/unpause", post(crate::demo::unpause_stub))
        .route("/runs/{id}/graph", get(crate::demo::get_run_graph))
        .route("/runs/{id}/retro", get(crate::demo::get_run_retro))
        .route("/runs/{id}/stages", get(crate::demo::get_run_stages))
        .route(
            "/runs/{id}/stages/{stageId}/turns",
            get(crate::demo::get_stage_turns),
        )
        .route("/runs/{id}/files", get(crate::demo::get_run_files))
        .route("/runs/{id}/usage", get(crate::demo::get_run_usage))
        .route(
            "/runs/{id}/verification",
            get(crate::demo::get_run_verification),
        )
        .route(
            "/runs/{id}/configuration",
            get(crate::demo::get_run_configuration),
        )
        .route("/runs/{id}/steer", post(crate::demo::steer_run_stub))
        .route(
            "/runs/{id}/preview",
            post(crate::demo::generate_preview_url_stub),
        )
        .route("/workflows", get(crate::demo::list_workflows))
        .route("/workflows/{name}", get(crate::demo::get_workflow))
        .route(
            "/workflows/{name}/runs",
            get(crate::demo::list_workflow_runs),
        )
        .route(
            "/verification/criteria",
            get(crate::demo::list_verification_criteria),
        )
        .route(
            "/verification/criteria/{id}",
            get(crate::demo::get_verification_criterion),
        )
        .route(
            "/verification/controls",
            get(crate::demo::list_verification_controls),
        )
        .route(
            "/verification/controls/{id}",
            get(crate::demo::get_verification_control),
        )
        .route(
            "/verification/signoffs",
            get(crate::demo::list_signoffs).post(crate::demo::create_signoff_stub),
        )
        .route("/verification/signoffs/{id}", get(crate::demo::get_signoff))
        .route("/retros", get(crate::demo::list_retros))
        .route(
            "/sessions",
            get(crate::demo::list_sessions).post(crate::demo::create_session_stub),
        )
        .route("/sessions/{id}", get(crate::demo::get_session))
        .route(
            "/sessions/{id}/messages",
            post(crate::demo::send_message_stub),
        )
        .route(
            "/sessions/{id}/events",
            get(crate::demo::session_events_stub),
        )
        .route(
            "/insights/queries",
            get(crate::demo::list_saved_queries).post(crate::demo::save_query_stub),
        )
        .route(
            "/insights/queries/{id}",
            get(crate::demo::get_saved_query)
                .put(crate::demo::update_query_stub)
                .delete(crate::demo::delete_query_stub),
        )
        .route("/insights/execute", post(crate::demo::execute_query_stub))
        .route("/insights/history", get(crate::demo::list_query_history))
        .route("/models", get(crate::demo::list_models))
        .route("/models/{id}/test", post(test_model))
        .route("/completions", post(create_completion))
        .route("/settings", get(crate::demo::get_server_configuration))
        .route("/usage", get(crate::demo::get_aggregate_usage))
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
        .route("/runs/{id}/configuration", get(not_implemented))
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
            get(crate::sessions::list_sessions).post(crate::sessions::create_session),
        )
        .route("/sessions/{id}", get(crate::sessions::retrieve_session))
        .route(
            "/sessions/{id}/messages",
            post(crate::sessions::send_message),
        )
        .route(
            "/sessions/{id}/events",
            get(crate::sessions::stream_session_events),
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
        .route("/models", get(crate::demo::list_models))
        .route("/models/{id}/test", post(test_model))
        .route("/completions", post(create_completion))
        .route("/settings", get(not_implemented))
        .route("/usage", get(get_aggregate_usage))
}

async fn not_implemented() -> Response {
    ApiError::new(StatusCode::NOT_IMPLEMENTED, "Not implemented.").into_response()
}

async fn root() -> Response {
    Json(serde_json::json!({
        "urls": {
            "openapi_url": "/openapi.json",
            "current_user_url": "/user",
            "health_url": "/health"
        }
    }))
    .into_response()
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

async fn get_user(user: AuthenticatedUser) -> Response {
    Json(serde_json::json!({"login": user.login})).into_response()
}

async fn get_aggregate_usage(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
) -> Response {
    let agg = state
        .aggregate_usage
        .lock()
        .expect("aggregate_usage lock poisoned");
    let by_model: Vec<fabro_types::UsageByModel> = agg
        .by_model
        .iter()
        .map(|(model, totals)| fabro_types::UsageByModel {
            model: fabro_types::ModelReference { id: model.clone() },
            stages: totals.stages,
            usage: fabro_types::TokenUsage {
                input_tokens: totals.input_tokens,
                output_tokens: totals.output_tokens,
                cost: totals.cost,
            },
        })
        .collect();
    let response = fabro_types::AggregateUsage {
        totals: fabro_types::AggregateUsageTotals {
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

/// Create an `AppState` with the given registry factory and database pool.
///
/// The factory receives the run's `WebInterviewer` so it can wire it
/// into handlers that need human-in-the-loop interaction (e.g., `HumanHandler`).
pub fn create_app_state(
    db: sqlx::SqlitePool,
    registry_factory: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    create_app_state_with_options(
        db,
        registry_factory,
        false,
        5,
        fabro_workflows::git::GitAuthor::default(),
        Vec::new(),
    )
}

/// Create an `AppState` with the given database pool, registry factory, dry-run flag, and concurrency limit.
pub fn create_app_state_with_options(
    db: sqlx::SqlitePool,
    registry_factory: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
    dry_run: bool,
    max_concurrent_runs: usize,
    git_author: fabro_workflows::git::GitAuthor,
    hooks: Vec<fabro_hooks::HookDefinition>,
) -> Arc<AppState> {
    Arc::new(AppState {
        runs: Mutex::new(HashMap::new()),
        aggregate_usage: Mutex::new(AggregateUsageTotals::default()),
        registry_factory: Box::new(registry_factory),
        dry_run,
        db,
        max_concurrent_runs,
        scheduler_notify: tokio::sync::Notify::new(),
        hooks,
        git_author,
        sessions: crate::sessions::new_session_store(),
        llm_client: tokio::sync::OnceCell::new(),
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
            id: id.clone(),
            status: managed_run.status,
            error: managed_run.error.as_ref().map(|msg| fabro_types::RunError {
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

fn compute_queue_positions(runs: &HashMap<String, ManagedRun>) -> HashMap<String, i64> {
    let mut queued: Vec<(&String, &ManagedRun)> = runs
        .iter()
        .filter(|(_, r)| r.status == RunStatus::Queued)
        .collect();
    queued.sort_by_key(|(_, r)| r.created_at);
    queued
        .into_iter()
        .enumerate()
        .map(|(i, (id, _))| (id.clone(), (i + 1) as i64))
        .collect()
}

async fn start_run(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(req): Json<StartRunRequest>,
) -> Response {
    // Parse the DOT source
    let graph = match operations::create(&req.dot_source, CreateOptions::default()) {
        Ok(validated) => {
            if let Err(e) = validated.raise_on_errors() {
                return ApiError::bad_request(e.to_string()).into_response();
            }
            let (graph, _, _) = validated.into_parts();
            graph
        }
        Err(e) => {
            return ApiError::bad_request(e.to_string()).into_response();
        }
    };

    let run_id = ulid::Ulid::new().to_string();
    info!(run_id = %run_id, "Run queued");

    let created_at = chrono::Utc::now();
    let run_dir = std::env::temp_dir().join(format!("fabro-{}", uuid::Uuid::new_v4()));
    if let Err(err) = std::fs::create_dir_all(&run_dir) {
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create run directory: {err}"),
        )
        .into_response();
    }

    let run_record = fabro_workflows::records::RunRecord {
        run_id: run_id.clone(),
        created_at,
        config: fabro_config::config::FabroConfig {
            dry_run: Some(state.dry_run),
            hooks: state.hooks.clone(),
            sandbox: Some(fabro_config::sandbox::SandboxConfig {
                provider: Some("local".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        },
        graph: graph.clone(),
        workflow_slug: None,
        working_directory: std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from(".")),
        host_repo_path: None,
        base_branch: None,
        labels: std::collections::HashMap::new(),
    };
    if let Err(err) = run_record.save(&run_dir) {
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to persist run record: {err}"),
        )
        .into_response();
    }

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            run_id.clone(),
            ManagedRun {
                dot_source: req.dot_source,
                graph,
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
            id: run_id,
            status: RunStatus::Queued,
            error: None,
            queue_position: None,
            created_at,
        }),
    )
        .into_response()
}

/// Execute a single run: transitions queued → starting → running → completed/failed/cancelled.
async fn execute_run(state: Arc<AppState>, run_id: String) {
    // Transition to Starting and set up cancel infrastructure
    let (cancel_rx, graph, run_dir) = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = match runs.get_mut(&run_id) {
            Some(r) if r.status == RunStatus::Queued => r,
            _ => return,
        };
        let run_dir = match managed_run.run_dir.clone() {
            Some(path) => path,
            None => return,
        };

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let cancel_token = Arc::new(AtomicBool::new(false));
        let (event_tx, _) = broadcast::channel(256);

        managed_run.status = RunStatus::Starting;
        managed_run.cancel_tx = Some(cancel_tx);
        managed_run.cancel_token = Some(Arc::clone(&cancel_token));
        managed_run.event_tx = Some(event_tx);

        (cancel_rx, managed_run.graph.clone(), run_dir)
    };

    // Create interviewer, sandbox, engine (this is the "provisioning" phase)
    let interviewer = Arc::new(WebInterviewer::new());
    let context = Context::new();

    let event_tx = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        runs.get(&run_id).and_then(|r| r.event_tx.clone())
    };

    let emitter = EventEmitter::new();
    if let Some(tx_clone) = event_tx {
        emitter.on_event(move |event| {
            let _ = tx_clone.send(event.clone());
        });
    }

    let cancel_token = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        runs.get(&run_id).and_then(|r| r.cancel_token.clone())
    };
    let cancel_token = match cancel_token {
        Some(ct) => ct,
        None => return,
    };

    let registry = (state.registry_factory)(Arc::clone(&interviewer) as Arc<dyn Interviewer>);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(
        fabro_agent::ReadBeforeWriteSandbox::new(Arc::new(LocalSandbox::new(cwd))),
    );
    let emitter = Arc::new(emitter);

    // Transition to Running, populate interviewer + context
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&run_id) {
            if managed_run.status != RunStatus::Starting {
                // Was cancelled during setup
                state.scheduler_notify.notify_one();
                return;
            }
            managed_run.status = RunStatus::Running;
            managed_run.interviewer = Some(Arc::clone(&interviewer));
            managed_run.context = Some(context);
        }
    }

    let run_record = match fabro_workflows::records::RunRecord::load(&run_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(run_id = %run_id, error = %e, "Failed to load RunRecord");
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            if let Some(managed_run) = runs.get_mut(&run_id) {
                managed_run.status = RunStatus::Failed;
                managed_run.error = Some(format!("Failed to load run record: {e}"));
                managed_run.event_tx = None;
            }
            state.scheduler_notify.notify_one();
            return;
        }
    };
    let config = RunSettings {
        config: run_record.config,
        run_dir: run_dir.clone(),
        cancel_token: Some(cancel_token),
        dry_run: state.dry_run,
        run_id: run_id.clone(),
        labels: run_record.labels,
        git_author: state.git_author.clone(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };

    let execution = {
        let emitter = Arc::clone(&emitter);
        let sandbox = Arc::clone(&sandbox);
        let registry = Arc::new(registry);
        let graph = graph.clone();
        let run_dir = run_dir.clone();
        let run_id = run_id.clone();
        let config = config.clone();
        let hooks = state.hooks.clone();
        let dry_run = state.dry_run;
        async move {
            let validated = operations::create_from_graph(graph, String::new());
            let initialized = pipeline::initialize(
                validated,
                InitOptions {
                    run_id,
                    run_dir,
                    dry_run,
                    emitter,
                    sandbox,
                    registry,
                    lifecycle: LifecycleConfig {
                        setup_commands: Vec::new(),
                        setup_command_timeout_ms: 300_000,
                        devcontainer_phases: Vec::new(),
                    },
                    run_settings: config,
                    hooks: fabro_hooks::HookConfig { hooks },
                    sandbox_env: HashMap::new(),
                    checkpoint: None,
                    seed_context: None,
                },
            )
            .await?;
            Ok::<_, fabro_workflows::error::FabroError>(pipeline::execute(initialized).await)
        }
    };

    let (result, final_context) = tokio::select! {
        result = execution => match result {
            Ok(executed) => (executed.outcome, Some(executed.final_context)),
            Err(err) => (Err(err), None),
        },
        _ = cancel_rx => {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            if let Some(managed_run) = runs.get_mut(&run_id) {
                managed_run.status = RunStatus::Cancelled;
                managed_run.event_tx = None;
            }
            state.scheduler_notify.notify_one();
            return;
        }
    };

    // Save final checkpoint
    let checkpoint = Checkpoint::load(&config.run_dir.join("checkpoint.json")).ok();

    // Auto-derive retro and accumulate aggregate usage
    if let Some(ref cp) = checkpoint {
        let failed = result.is_err();
        let completed_stages = fabro_workflows::build_completed_stages(cp, failed);
        let stage_durations = fabro_retro::retro::extract_stage_durations(&config.run_dir);
        let retro = fabro_retro::retro::derive_retro(
            &run_id,
            "workflow",
            "",
            completed_stages,
            0,
            &stage_durations,
        );
        let _ = retro.save(&config.run_dir);

        // Accumulate aggregate usage
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
        match result {
            Ok(_) => {
                info!(run_id = %run_id, "Run completed");
                managed_run.status = RunStatus::Completed;
            }
            Err(fabro_workflows::error::FabroError::Cancelled) => {
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
        if let Some(ctx) = final_context {
            managed_run.context = Some(ctx);
        }
        managed_run.run_dir = Some(config.run_dir.clone());
        managed_run.event_tx = None;
    }
    drop(runs);
    state.scheduler_notify.notify_one();
}

/// Background task that promotes queued runs when capacity is available.
pub fn spawn_scheduler(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = state.scheduler_notify.notified() => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
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
                        .map(|(id, _)| id.clone())
                };
                match run_to_start {
                    Some(id) => {
                        let state_clone = Arc::clone(&state);
                        tokio::spawn(execute_run(state_clone, id));
                    }
                    None => break,
                };
            }
        }
    });
}

async fn get_run_status(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
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
                    id: id.clone(),
                    status: managed_run.status,
                    error: managed_run.error.as_ref().map(|msg| fabro_types::RunError {
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
    let runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get(&id) {
        Some(managed_run) => {
            let interviewer = match &managed_run.interviewer {
                Some(i) => i,
                None => {
                    return (
                        StatusCode::OK,
                        Json(ListResponse::new(Vec::<ApiQuestion>::new())),
                    )
                        .into_response()
                }
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
    let runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get(&id) {
        Some(managed_run) => {
            let interviewer = match &managed_run.interviewer {
                Some(i) => i,
                None => {
                    return ApiError::new(StatusCode::CONFLICT, "Run is not yet running.")
                        .into_response();
                }
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
    let rx = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => match &managed_run.event_tx {
                Some(tx) => tx.subscribe(),
                None => {
                    return ApiError::new(StatusCode::GONE, "Event stream closed.").into_response()
                }
            },
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            let data = fabro_util::redact::redact_jsonl_line(&data);
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
                        id: id.clone(),
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
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get_mut(&id) {
        Some(managed_run) => match managed_run.status {
            RunStatus::Running => {
                managed_run.status = RunStatus::Paused;
                let created_at = managed_run.created_at;
                (
                    StatusCode::OK,
                    Json(RunStatusResponse {
                        id: id.clone(),
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
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    match runs.get_mut(&id) {
        Some(managed_run) => match managed_run.status {
            RunStatus::Paused => {
                managed_run.status = RunStatus::Running;
                let created_at = managed_run.created_at;
                (
                    StatusCode::OK,
                    Json(RunStatusResponse {
                        id: id.clone(),
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

    if state.dry_run {
        return Json(serde_json::json!({
            "model_id": id,
            "status": "ok",
        }))
        .into_response();
    }

    let params = fabro_llm::generate::GenerateParams::new(&info.id)
        .provider(info.provider.as_str())
        .prompt("Say OK")
        .max_tokens(16);

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        fabro_llm::generate::generate(params),
    )
    .await;

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

fn finish_reason_to_api_stop_reason(reason: &fabro_llm::types::FinishReason) -> String {
    match reason {
        fabro_llm::types::FinishReason::Stop => "end_turn".to_string(),
        fabro_llm::types::FinishReason::Length => "max_tokens".to_string(),
        fabro_llm::types::FinishReason::ToolCalls => "tool_calls".to_string(),
        fabro_llm::types::FinishReason::ContentFilter => "content_filter".to_string(),
        fabro_llm::types::FinishReason::Error => "error".to_string(),
        fabro_llm::types::FinishReason::Other(s) => s.clone(),
    }
}

fn convert_api_message(msg: &fabro_types::CompletionMessage) -> fabro_llm::types::Message {
    let role = match msg.role {
        fabro_types::CompletionMessageRole::System => fabro_llm::types::Role::System,
        fabro_types::CompletionMessageRole::User => fabro_llm::types::Role::User,
        fabro_types::CompletionMessageRole::Assistant => fabro_llm::types::Role::Assistant,
        fabro_types::CompletionMessageRole::Tool => fabro_llm::types::Role::Tool,
        fabro_types::CompletionMessageRole::Developer => fabro_llm::types::Role::Developer,
    };
    let content: Vec<fabro_llm::types::ContentPart> = msg
        .content
        .iter()
        .filter_map(|part| {
            let json = serde_json::to_value(part).ok()?;
            serde_json::from_value(json).ok()
        })
        .collect();
    fabro_llm::types::Message {
        role,
        content,
        name: msg.name.clone(),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

fn convert_llm_message(msg: &fabro_llm::types::Message) -> fabro_types::CompletionMessage {
    let role = match msg.role {
        fabro_llm::types::Role::System => fabro_types::CompletionMessageRole::System,
        fabro_llm::types::Role::User => fabro_types::CompletionMessageRole::User,
        fabro_llm::types::Role::Assistant => fabro_types::CompletionMessageRole::Assistant,
        fabro_llm::types::Role::Tool => fabro_types::CompletionMessageRole::Tool,
        fabro_llm::types::Role::Developer => fabro_types::CompletionMessageRole::Developer,
    };
    let content: Vec<fabro_types::CompletionContentPart> = msg
        .content
        .iter()
        .filter_map(|part| {
            let json = serde_json::to_value(part).ok()?;
            serde_json::from_value(json).ok()
        })
        .collect();
    fabro_types::CompletionMessage {
        role,
        content,
        name: msg.name.clone(),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

async fn create_completion(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(req): Json<fabro_types::CreateCompletionRequest>,
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
    let mut messages: Vec<fabro_llm::types::Message> = Vec::new();
    if let Some(system) = req.system {
        messages.push(fabro_llm::types::Message::system(system));
    }
    for msg in &req.messages {
        messages.push(convert_api_message(msg));
    }

    // Convert tools
    let tools: Option<Vec<fabro_llm::types::ToolDefinition>> = if req.tools.is_empty() {
        None
    } else {
        Some(
            req.tools
                .into_iter()
                .map(|t| fabro_llm::types::ToolDefinition {
                    name: t.name,
                    description: t.description,
                    parameters: t.parameters,
                })
                .collect(),
        )
    };

    // Convert tool_choice
    let tool_choice: Option<fabro_llm::types::ToolChoice> =
        req.tool_choice.map(|tc| match tc.mode {
            fabro_types::CompletionToolChoiceMode::Auto => fabro_llm::types::ToolChoice::Auto,
            fabro_types::CompletionToolChoiceMode::None => fabro_llm::types::ToolChoice::None,
            fabro_types::CompletionToolChoiceMode::Required => {
                fabro_llm::types::ToolChoice::Required
            }
            fabro_types::CompletionToolChoiceMode::Named => {
                fabro_llm::types::ToolChoice::named(tc.tool_name.unwrap_or_default())
            }
        });

    // Build the LLM request
    let request = fabro_llm::types::Request {
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
    if state.dry_run {
        let msg_id = ulid::Ulid::new().to_string();
        if use_stream {
            let finish_event = fabro_llm::types::StreamEvent::finish(
                fabro_llm::types::FinishReason::Stop,
                fabro_llm::types::Usage::default(),
                fabro_llm::types::Response {
                    id: msg_id.clone(),
                    model: model_id.clone(),
                    provider: String::new(),
                    message: fabro_llm::types::Message::assistant(""),
                    finish_reason: fabro_llm::types::FinishReason::Stop,
                    usage: fabro_llm::types::Usage::default(),
                    raw: None,
                    warnings: vec![],
                    rate_limit: None,
                },
            );
            let json = serde_json::to_string(&finish_event).unwrap_or_default();
            let sse_stream = futures_util::stream::iter(vec![Ok::<_, std::convert::Infallible>(
                Event::default().event("stream_event").data(json),
            )]);
            return Sse::new(sse_stream).into_response();
        }
        let empty_msg = fabro_types::CompletionMessage {
            role: fabro_types::CompletionMessageRole::Assistant,
            content: vec![],
            name: None,
            tool_call_id: None,
        };
        return Json(fabro_types::CompletionResponse {
            id: msg_id,
            model: model_id,
            message: empty_msg,
            stop_reason: "end_turn".to_string(),
            usage: fabro_types::CompletionUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
            output: None,
        })
        .into_response();
    }

    // Get or create LLM client (cached in AppState)
    let client = match state
        .llm_client
        .get_or_try_init(fabro_llm::client::Client::from_env)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create LLM client: {e}"),
            )
            .into_response()
        }
    };

    if use_stream {
        // Streaming path: forward all StreamEvents as SSE
        let stream_result = match client.stream(&request).await {
            Ok(s) => s,
            Err(e) => {
                return ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response()
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
                axum::response::sse::KeepAlive::new()
                    .interval(Duration::from_secs(15))
                    .event(
                        Event::default()
                            .event("ping")
                            .data(serde_json::json!({"type": "ping"}).to_string()),
                    ),
            )
            .into_response()
    } else {
        // Non-streaming path
        let msg_id = ulid::Ulid::new().to_string();

        if let Some(schema) = req.schema {
            // Structured output uses generate_object for JSON parsing logic
            let mut params = fabro_llm::generate::GenerateParams::new(&request.model)
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
            match fabro_llm::generate::generate_object(params, schema).await {
                Ok(result) => Json(fabro_types::CompletionResponse {
                    id: msg_id,
                    model: model_id,
                    message: convert_llm_message(&result.response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&result.finish_reason),
                    usage: fabro_types::CompletionUsage {
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
                Ok(response) => Json(fabro_types::CompletionResponse {
                    id: response.id,
                    model: response.model,
                    message: convert_llm_message(&response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&response.finish_reason),
                    usage: fabro_types::CompletionUsage {
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
    let run_dir = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => managed_run.run_dir.clone(),
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    let Some(run_dir) = run_dir else {
        return (StatusCode::OK, Json(serde_json::json!(null))).into_response();
    };

    match fabro_retro::retro::Retro::load(&run_dir) {
        Ok(retro) => (StatusCode::OK, Json(retro)).into_response(),
        Err(_) => (StatusCode::OK, Json(serde_json::json!(null))).into_response(),
    }
}

/// Render DOT source to a styled SVG via `render_dot` on a blocking thread.
pub(crate) async fn render_dot_svg(dot_source: &str) -> Response {
    use fabro_graphviz::render::{render_dot, GraphFormat};

    let source = dot_source.to_owned();
    match tokio::task::spawn_blocking(move || render_dot(&source, GraphFormat::Svg)).await {
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
    use tower::ServiceExt;

    use fabro_workflows::handler::exit::ExitHandler;
    use fabro_workflows::handler::start::StartHandler;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Test"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    fn test_registry(_interviewer: Arc<dyn fabro_interview::Interviewer>) -> HandlerRegistry {
        let mut registry = HandlerRegistry::new(Box::new(StartHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry
    }

    async fn test_db() -> sqlx::SqlitePool {
        let pool = fabro_db::connect_memory().await.unwrap();
        fabro_db::initialize_db(&pool).await.unwrap();
        pool
    }

    fn test_app_with(db: sqlx::SqlitePool) -> Router {
        let state = create_app_state(db, test_registry);
        build_router(state, AuthMode::Disabled)
    }

    fn test_app_with_scheduler(state: Arc<AppState>) -> Router {
        spawn_scheduler(Arc::clone(&state));
        build_router(state, AuthMode::Disabled)
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_model_unknown_returns_404() {
        let app = test_app_with(test_db().await);

        let req = Request::builder()
            .method("POST")
            .uri("/models/nonexistent-model-xyz/test")
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
            .uri("/models/claude-opus-4-6/test")
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
    async fn test_model_dry_run_returns_ok() {
        let state = create_app_state_with_options(
            test_db().await,
            test_registry,
            true,
            5,
            fabro_workflows::git::GitAuthor::default(),
            Vec::new(),
        );
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri("/models/claude-opus-4-6/test")
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
        let state = create_app_state_with_options(
            test_db().await,
            test_registry,
            true,
            5,
            fabro_workflows::git::GitAuthor::default(),
            Vec::new(),
        );
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri("/models/nonexistent-model-xyz/test")
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
            .uri("/runs")
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
            .uri("/runs")
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
        let state = create_app_state(test_db().await, test_registry);
        let app = test_app_with_scheduler(state);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Give run a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Check status
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["id"].as_str().unwrap(), run_id);
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

        let req = Request::builder()
            .method("GET")
            .uri("/runs/nonexistent")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_questions_returns_empty_list() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Get questions (should be empty for a run without wait.human nodes)
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}/questions"))
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

        let req = Request::builder()
            .method("POST")
            .uri("/runs/nonexistent/questions/q1/answer")
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

        let req = Request::builder()
            .method("GET")
            .uri("/runs/nonexistent/events")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_checkpoint_returns_null_initially() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Get checkpoint immediately (before run completes, may be null)
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}/checkpoint"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_context_returns_map() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Get context
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}/context"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert!(body.is_object());
    }

    #[tokio::test]
    async fn cancel_run_succeeds() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Cancel it
        let req = Request::builder()
            .method("POST")
            .uri(format!("/runs/{run_id}/cancel"))
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

        let req = Request::builder()
            .method("POST")
            .uri("/runs/nonexistent/cancel")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_events_returns_sse_stream() {
        let state = create_app_state(test_db().await, test_registry);
        let app = test_app_with_scheduler(state);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Wait for scheduler to promote run (creates event_tx)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Request the SSE stream
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}/events"))
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
        let state = create_app_state(test_db().await, test_registry);
        let app = test_app_with_scheduler(state);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Poll until run completes
        let mut status = String::new();
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let req = Request::builder()
                .method("GET")
                .uri(format!("/runs/{run_id}"))
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
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Request graph SVG
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}/graph"))
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

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
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

        let req = Request::builder()
            .method("GET")
            .uri("/runs/nonexistent/graph")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_runs_returns_started_run() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // List should be empty initially
        let req = Request::builder()
            .method("GET")
            .uri("/runs")
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
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // List should now contain one run
        let req = Request::builder()
            .method("GET")
            .uri("/runs")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        let items = body["data"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"].as_str().unwrap(), run_id);
        assert!(items[0]["status"].as_str().is_some());
        assert!(!body["meta"]["has_more"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn get_aggregate_usage_returns_zeros_initially() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        let req = Request::builder()
            .method("GET")
            .uri("/usage")
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
        let state = create_app_state(test_db().await, test_registry);
        let app = test_app_with_scheduler(state);

        // Start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Poll until run completes
        let mut status = String::new();
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let req = Request::builder()
                .method("GET")
                .uri(format!("/runs/{run_id}"))
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
            .uri("/usage")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["totals"]["runs"].as_i64().unwrap(), 1);
    }

    #[tokio::test]
    async fn post_runs_returns_queued_status() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Check status is queued (no scheduler running)
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "queued");
    }

    #[tokio::test]
    async fn cancel_queued_run_succeeds() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(state, AuthMode::Disabled);

        // Submit a run (no scheduler, stays queued)
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Cancel it
        let req = Request::builder()
            .method("POST")
            .uri(format!("/runs/{run_id}/cancel"))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify status is cancelled
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "cancelled");
    }

    #[tokio::test]
    async fn queue_position_reported_for_queued_runs() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(state, AuthMode::Disabled);

        // Submit two runs (no scheduler, both stay queued)
        let mut run_ids = Vec::new();
        for _ in 0..2 {
            let req = Request::builder()
                .method("POST")
                .uri("/runs")
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
            .uri(format!("/runs/{}", run_ids[0]))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["queue_position"].as_i64().unwrap(), 1);

        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{}", run_ids[1]))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["queue_position"].as_i64().unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrency_limit_respected() {
        let state = create_app_state_with_options(
            test_db().await,
            test_registry,
            false,
            1,
            fabro_workflows::git::GitAuthor::default(),
            Vec::new(),
        );
        let app = test_app_with_scheduler(state);

        // Submit two runs with max_concurrent_runs=1
        let mut run_ids = Vec::new();
        for _ in 0..2 {
            let req = Request::builder()
                .method("POST")
                .uri("/runs")
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
            .uri("/runs")
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
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
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
            .uri(format!("/runs/{run_id}/questions/q1/answer"))
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
        let state = create_app_state_with_options(
            test_db().await,
            test_registry,
            true,
            5,
            fabro_workflows::git::GitAuthor::default(),
            Vec::new(),
        );
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri("/completions")
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
        let state = create_app_state_with_options(
            test_db().await,
            test_registry,
            true,
            5,
            fabro_workflows::git::GitAuthor::default(),
            Vec::new(),
        );
        let app = build_router(state, AuthMode::Disabled);

        let req = Request::builder()
            .method("POST")
            .uri("/completions")
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
            .uri("/completions")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
