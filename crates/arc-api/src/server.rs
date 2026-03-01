use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use arc_agent::LocalExecutionEnvironment;

use crate::jwt_auth::{AuthMode, AuthenticatedService};
use arc_workflows::checkpoint::Checkpoint;
use arc_workflows::context::Context;
use arc_workflows::engine::{PipelineEngine, RunConfig};
use arc_workflows::event::{EventEmitter, PipelineEvent};
use arc_workflows::handler::HandlerRegistry;
use arc_workflows::interviewer::web::WebInterviewer;
use arc_workflows::interviewer::{Answer, Interviewer};

/// Status of a managed pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// An option for a multiple-choice question exposed via the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiQuestionOption {
    pub key: String,
    pub label: String,
}

/// A pending question exposed via the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiQuestion {
    pub id: String,
    pub text: String,
    pub question_type: String,
    pub options: Vec<ApiQuestionOption>,
    pub allow_freeform: bool,
}

/// Snapshot of a managed pipeline.
struct ManagedPipeline {
    dot_source: String,
    status: PipelineStatus,
    error: Option<String>,
    interviewer: Arc<WebInterviewer>,
    event_tx: Option<broadcast::Sender<PipelineEvent>>,
    context: Option<Context>,
    checkpoint: Option<Checkpoint>,
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    cancel_token: Arc<AtomicBool>,
    logs_root: Option<std::path::PathBuf>,
}

/// Shared application state for the server.
pub struct AppState {
    pipelines: Mutex<HashMap<String, ManagedPipeline>>,
    registry_factory: Box<dyn Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync>,
    dry_run: bool,
    pub db: sqlx::SqlitePool,
}

/// Request body for POST /pipelines.
#[derive(Debug, Deserialize)]
pub struct StartPipelineRequest {
    pub dot_source: String,
}

/// Response body for POST /pipelines.
#[derive(Debug, Serialize)]
pub struct StartPipelineResponse {
    pub id: String,
}

/// Response body for GET /pipelines/{id}.
#[derive(Debug, Serialize)]
pub struct PipelineStatusResponse {
    pub id: String,
    pub status: PipelineStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Request body for POST /pipelines/{id}/questions/{qid}/answer.
#[derive(Debug, Deserialize)]
pub struct SubmitAnswerRequest {
    pub value: String,
    pub selected_option_key: Option<String>,
}

/// Response for answer submission.
#[derive(Debug, Serialize)]
pub struct SubmitAnswerResponse {
    pub accepted: bool,
}

/// Build the axum Router with all pipeline endpoints.
pub fn build_router(state: Arc<AppState>, auth_mode: AuthMode) -> Router {
    Router::new()
        .route("/pipelines", get(list_pipelines).post(start_pipeline))
        .route("/pipelines/{id}", get(get_pipeline_status))
        .route("/pipelines/{id}/questions", get(get_questions))
        .route(
            "/pipelines/{id}/questions/{qid}/answer",
            post(submit_answer),
        )
        .route("/pipelines/{id}/events", get(get_events))
        .route("/pipelines/{id}/checkpoint", get(get_checkpoint))
        .route("/pipelines/{id}/context", get(get_context))
        .route("/pipelines/{id}/cancel", post(cancel_pipeline))
        .route("/pipelines/{id}/graph", get(get_graph))
        .route("/pipelines/{id}/retro", get(get_retro))
        .layer(axum::Extension(auth_mode))
        .with_state(state)
}

/// Create an `AppState` with the given registry factory and database pool.
///
/// The factory receives the pipeline's `WebInterviewer` so it can wire it
/// into handlers that need human-in-the-loop interaction (e.g., `WaitHumanHandler`).
pub fn create_app_state(
    db: sqlx::SqlitePool,
    registry_factory: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    create_app_state_with_options(db, registry_factory, false)
}

/// Create an `AppState` with the given database pool, registry factory, and dry-run flag.
pub fn create_app_state_with_options(
    db: sqlx::SqlitePool,
    registry_factory: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
    dry_run: bool,
) -> Arc<AppState> {
    Arc::new(AppState {
        pipelines: Mutex::new(HashMap::new()),
        registry_factory: Box::new(registry_factory),
        dry_run,
        db,
    })
}

async fn list_pipelines(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
) -> Response {
    let pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
    let items: Vec<PipelineStatusResponse> = pipelines
        .iter()
        .map(|(id, pipeline)| PipelineStatusResponse {
            id: id.clone(),
            status: pipeline.status.clone(),
            error: pipeline.error.clone(),
        })
        .collect();
    (StatusCode::OK, Json(items)).into_response()
}

async fn start_pipeline(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(req): Json<StartPipelineRequest>,
) -> Response {
    // Parse the DOT source
    let graph = match arc_workflows::pipeline::prepare_pipeline(&req.dot_source) {
        Ok(g) => g,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let run_id = ulid::Ulid::new().to_string();
    let interviewer = Arc::new(WebInterviewer::new());
    let (event_tx, _) = broadcast::channel(256);
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let cancel_token = Arc::new(AtomicBool::new(false));

    let context = Context::new();

    // Set up event emitter that broadcasts to the channel
    let mut emitter = EventEmitter::new();
    let tx_clone = event_tx.clone();
    emitter.on_event(move |event| {
        let _ = tx_clone.send(event.clone());
    });

    let registry = (state.registry_factory)(Arc::clone(&interviewer) as Arc<dyn Interviewer>);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let execution_env: Arc<dyn arc_agent::ExecutionEnvironment> =
        Arc::new(LocalExecutionEnvironment::new(cwd));
    let engine = PipelineEngine::with_interviewer(
        registry,
        Arc::new(emitter),
        Arc::clone(&interviewer) as Arc<dyn Interviewer>,
        execution_env,
    );

    {
        let mut pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
        pipelines.insert(
            run_id.clone(),
            ManagedPipeline {
                dot_source: req.dot_source,
                status: PipelineStatus::Running,
                error: None,
                interviewer: Arc::clone(&interviewer),
                event_tx: Some(event_tx),
                context: Some(context),
                checkpoint: None,
                cancel_tx: Some(cancel_tx),
                cancel_token: Arc::clone(&cancel_token),
                logs_root: None,
            },
        );
    }

    // Spawn pipeline execution
    let state_clone = Arc::clone(&state);
    let run_id_clone = run_id.clone();
    tokio::spawn(async move {
        let logs_root = std::env::temp_dir().join(format!("arc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&logs_root).expect("failed to create logs directory");
        let config = RunConfig {
            logs_root,
            cancel_token: Some(cancel_token),
            dry_run: state_clone.dry_run,
            run_id: run_id_clone.clone(),
            git_checkpoint: None,
            base_sha: None,
            run_branch: None,
            meta_branch: None,
        };

        let result = tokio::select! {
            result = engine.run(&graph, &config) => result,
            _ = cancel_rx => {
                let mut pipelines = state_clone.pipelines.lock().expect("pipelines lock poisoned");
                if let Some(pipeline) = pipelines.get_mut(&run_id_clone) {
                    pipeline.status = PipelineStatus::Cancelled;
                    pipeline.event_tx = None;
                }
                return;
            }
        };

        // Save final checkpoint
        let checkpoint = Checkpoint::load(&config.logs_root.join("checkpoint.json")).ok();

        // Auto-derive retro
        if let Some(ref cp) = checkpoint {
            let (failed, failure_reason) = match &result {
                Ok(_) => (false, None),
                Err(e) => (true, Some(e.to_string())),
            };
            let stage_durations =
                arc_workflows::retro::extract_stage_durations(&config.logs_root);
            let retro = arc_workflows::retro::derive_retro(
                &run_id_clone,
                "pipeline",
                "",
                cp,
                failed,
                failure_reason.as_deref(),
                0,
                &stage_durations,
            );
            let _ = retro.save(&config.logs_root);
        }

        let mut pipelines = state_clone
            .pipelines
            .lock()
            .expect("pipelines lock poisoned");
        if let Some(pipeline) = pipelines.get_mut(&run_id_clone) {
            match result {
                Ok(_) => {
                    pipeline.status = PipelineStatus::Completed;
                }
                Err(arc_workflows::error::ArcError::Cancelled) => {
                    pipeline.status = PipelineStatus::Cancelled;
                }
                Err(e) => {
                    pipeline.status = PipelineStatus::Failed;
                    pipeline.error = Some(e.to_string());
                }
            }
            pipeline.checkpoint = checkpoint;
            pipeline.logs_root = Some(config.logs_root.clone());
            pipeline.event_tx = None;
        }
    });

    (
        StatusCode::CREATED,
        Json(StartPipelineResponse { id: run_id }),
    )
        .into_response()
}

async fn get_pipeline_status(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
    match pipelines.get(&id) {
        Some(pipeline) => (
            StatusCode::OK,
            Json(PipelineStatusResponse {
                id: id.clone(),
                status: pipeline.status.clone(),
                error: pipeline.error.clone(),
            }),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn get_questions(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
    match pipelines.get(&id) {
        Some(pipeline) => {
            let pending = pipeline.interviewer.pending_questions();
            let questions: Vec<ApiQuestion> = pending
                .into_iter()
                .map(|pq| ApiQuestion {
                    id: pq.id,
                    text: pq.question.text.clone(),
                    question_type: format!("{:?}", pq.question.question_type),
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
            (StatusCode::OK, Json(questions)).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn submit_answer(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path((id, qid)): Path<(String, String)>,
    Json(req): Json<SubmitAnswerRequest>,
) -> Response {
    let pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
    match pipelines.get(&id) {
        Some(pipeline) => {
            let answer = match &req.selected_option_key {
                Some(key) => {
                    let option = pipeline
                        .interviewer
                        .pending_questions()
                        .iter()
                        .find(|pq| pq.id == qid)
                        .and_then(|pq| pq.question.options.iter().find(|o| o.key == *key))
                        .cloned();
                    match option {
                        Some(opt) => Answer::selected(key.clone(), opt),
                        None => {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({"error": "invalid option key"})),
                            )
                                .into_response();
                        }
                    }
                }
                None => Answer::text(req.value),
            };
            let accepted = pipeline.interviewer.submit_answer(&qid, answer);
            (StatusCode::OK, Json(SubmitAnswerResponse { accepted })).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn get_events(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let rx = {
        let pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
        match pipelines.get(&id) {
            Some(pipeline) => match &pipeline.event_tx {
                Some(tx) => tx.subscribe(),
                None => return StatusCode::GONE.into_response(),
            },
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            let data = arc_util::redact::redact_jsonl_line(&data);
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
    let pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
    match pipelines.get(&id) {
        Some(pipeline) => match &pipeline.checkpoint {
            Some(cp) => (StatusCode::OK, Json(cp.clone())).into_response(),
            None => (StatusCode::OK, Json(serde_json::json!(null))).into_response(),
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn get_context(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
    match pipelines.get(&id) {
        Some(pipeline) => match &pipeline.context {
            Some(ctx) => (StatusCode::OK, Json(ctx.snapshot())).into_response(),
            None => (StatusCode::OK, Json(serde_json::json!({}))).into_response(),
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn cancel_pipeline(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let mut pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
    match pipelines.get_mut(&id) {
        Some(pipeline) => {
            if pipeline.status != PipelineStatus::Running {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"error": "pipeline is not running"})),
                )
                    .into_response();
            }
            pipeline.cancel_token.store(true, Ordering::Relaxed);
            if let Some(cancel_tx) = pipeline.cancel_tx.take() {
                let _ = cancel_tx.send(());
            }
            pipeline.status = PipelineStatus::Cancelled;
            (StatusCode::OK, Json(serde_json::json!({"cancelled": true}))).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn get_retro(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let logs_root = {
        let pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
        match pipelines.get(&id) {
            Some(pipeline) => pipeline.logs_root.clone(),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let Some(logs_root) = logs_root else {
        return (StatusCode::OK, Json(serde_json::json!(null))).into_response();
    };

    match arc_workflows::retro::Retro::load(&logs_root) {
        Ok(retro) => (StatusCode::OK, Json(retro)).into_response(),
        Err(_) => (StatusCode::OK, Json(serde_json::json!(null))).into_response(),
    }
}

async fn get_graph(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let dot_source = {
        let pipelines = state.pipelines.lock().expect("pipelines lock poisoned");
        match pipelines.get(&id) {
            Some(pipeline) => pipeline.dot_source.clone(),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let mut child = match tokio::process::Command::new("dot")
        .arg("-Tsvg")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "graphviz dot command not available"})),
            )
                .into_response();
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(dot_source.as_bytes()).await;
        // stdin is dropped here, closing the pipe
    }

    match child.wait_with_output().await {
        Ok(output) if output.status.success() => (
            StatusCode::OK,
            [("content-type", "image/svg+xml")],
            output.stdout,
        )
            .into_response(),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("dot failed: {stderr}")})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("dot process error: {e}")})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use arc_workflows::handler::exit::ExitHandler;
    use arc_workflows::handler::start::StartHandler;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Test"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    fn test_registry(
        _interviewer: Arc<dyn arc_workflows::interviewer::Interviewer>,
    ) -> HandlerRegistry {
        let mut registry = HandlerRegistry::new(Box::new(StartHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry
    }

    async fn test_db() -> sqlx::SqlitePool {
        let pool = arc_db::connect_memory().await.unwrap();
        arc_db::initialize_db(&pool).await.unwrap();
        pool
    }

    fn test_app_with(db: sqlx::SqlitePool) -> Router {
        let state = create_app_state(db, test_registry);
        build_router(state, AuthMode::Disabled)
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn post_pipelines_starts_pipeline_and_returns_id() {
        let app = test_app_with(test_db().await);

        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
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
    async fn post_pipelines_invalid_dot_returns_bad_request() {
        let app = test_app_with(test_db().await);

        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": "not a graph"})).unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_pipeline_status_returns_status() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Give pipeline a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Check status
        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{run_id}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["id"].as_str().unwrap(), run_id);
        // Status should be either "running" or "completed"
        let status = body["status"].as_str().unwrap();
        assert!(
            status == "running" || status == "completed",
            "unexpected status: {status}"
        );
    }

    #[tokio::test]
    async fn get_pipeline_status_not_found() {
        let app = test_app_with(test_db().await);

        let req = Request::builder()
            .method("GET")
            .uri("/pipelines/nonexistent")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_questions_returns_empty_list() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Get questions (should be empty for a pipeline without wait.human nodes)
        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{run_id}/questions"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert!(body.is_array());
    }

    #[tokio::test]
    async fn submit_answer_not_found_pipeline() {
        let app = test_app_with(test_db().await);

        let req = Request::builder()
            .method("POST")
            .uri("/pipelines/nonexistent/questions/q1/answer")
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
            .uri("/pipelines/nonexistent/events")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_checkpoint_returns_null_initially() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Get checkpoint immediately (before pipeline completes, may be null)
        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{run_id}/checkpoint"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_context_returns_map() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
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
            .uri(format!("/pipelines/{run_id}/context"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert!(body.is_object());
    }

    #[tokio::test]
    async fn cancel_pipeline_succeeds() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
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
            .uri(format!("/pipelines/{run_id}/cancel"))
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
    async fn cancel_nonexistent_pipeline_returns_not_found() {
        let app = test_app_with(test_db().await);

        let req = Request::builder()
            .method("POST")
            .uri("/pipelines/nonexistent/cancel")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_events_returns_sse_stream() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Request the SSE stream
        let req = Request::builder()
            .method("GET")
            .uri(format!("/pipelines/{run_id}/events"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Check content-type is text/event-stream
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

    #[tokio::test]
    async fn pipeline_completes_and_status_is_completed() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // Start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Poll until pipeline completes
        let mut status = String::new();
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let req = Request::builder()
                .method("GET")
                .uri(format!("/pipelines/{run_id}"))
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

        // Start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
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
            .uri(format!("/pipelines/{run_id}/graph"))
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
            .uri("/pipelines/nonexistent/graph")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_pipelines_returns_started_pipeline() {
        let state = create_app_state(test_db().await, test_registry);
        let app = build_router(Arc::clone(&state), AuthMode::Disabled);

        // List should be empty initially
        let req = Request::builder()
            .method("GET")
            .uri("/pipelines")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 0);

        // Start a pipeline
        let req = Request::builder()
            .method("POST")
            .uri("/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // List should now contain one pipeline
        let req = Request::builder()
            .method("GET")
            .uri("/pipelines")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        let items = body.as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"].as_str().unwrap(), run_id);
        assert!(items[0]["status"].as_str().is_some());
    }
}
