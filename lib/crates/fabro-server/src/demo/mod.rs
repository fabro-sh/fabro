//! Demo mode handlers that return static data for all API endpoints.
//! Activated per-request via the `X-Fabro-Demo: 1` header to showcase the UI without a real backend.
#![allow(clippy::default_trait_access, clippy::unreadable_literal)]

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::error::ApiError;
use crate::jwt_auth::AuthenticatedService;
use crate::server::{AppState, PaginationParams};

#[derive(serde::Deserialize)]
pub(crate) struct RetroListParams {
    #[serde(rename = "page[limit]", default = "crate::server::default_page_limit")]
    limit: u32,
    #[serde(rename = "page[offset]", default)]
    offset: u32,
    workflow: Option<String>,
    smoothness: Option<fabro_api_types::SmoothnessRating>,
}

fn paginated_response<T: serde::Serialize>(
    items: Vec<T>,
    pagination: &PaginationParams,
) -> Response {
    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset as usize;
    let mut data: Vec<_> = items.into_iter().skip(offset).take(limit + 1).collect();
    let has_more = data.len() > limit;
    data.truncate(limit);
    (
        StatusCode::OK,
        Json(json!({ "data": data, "meta": { "has_more": has_more } })),
    )
        .into_response()
}

// ── Runs ───────────────────────────────────────────────────────────────

pub(crate) async fn list_runs(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(runs::list_items(), &pagination)
}

pub(crate) async fn start_run_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": "demo-run-new", "status": "queued", "created_at": "2026-03-06T14:30:00Z"})),
    )
        .into_response()
}

pub(crate) async fn get_run_stages(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(runs::stages(), &pagination)
}

pub(crate) async fn get_stage_turns(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path((_id, _stage_id)): Path<(String, String)>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(runs::turns(), &pagination)
}

pub(crate) async fn get_run_files(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(runs::files(), &pagination)
}

pub(crate) async fn get_run_usage(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(runs::usage())).into_response()
}

pub(crate) async fn get_run_verification(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(runs::verifications(), &pagination)
}

pub(crate) async fn get_run_settings(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(runs::settings())).into_response()
}

pub(crate) async fn steer_run_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    StatusCode::ACCEPTED.into_response()
}

pub(crate) async fn generate_preview_url_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"url": "https://google.com"})),
    )
        .into_response()
}

pub(crate) async fn get_run_status(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match runs::list_items().into_iter().find(|r| r.id == id) {
        Some(item) => (
            StatusCode::OK,
            Json(fabro_api_types::RunStatusResponse {
                id: id.clone(),
                status: fabro_api_types::RunStatus::Running,
                error: None,
                queue_position: None,
                created_at: item.created_at,
            }),
        )
            .into_response(),
        None => ApiError::not_found("Run not found.").into_response(),
    }
}

pub(crate) async fn get_questions_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(runs::questions(), &pagination)
}

pub(crate) async fn answer_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path((_id, _qid)): Path<(String, String)>,
) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) async fn run_events_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    ApiError::new(StatusCode::GONE, "Event stream closed.").into_response()
}

pub(crate) async fn checkpoint_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!(null))).into_response()
}

pub(crate) async fn context_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!({}))).into_response()
}

pub(crate) async fn cancel_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!({"id": id, "status": "cancelled", "created_at": "2026-03-06T14:30:00Z"}))).into_response()
}

pub(crate) async fn pause_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    (
        StatusCode::OK,
        Json(
            serde_json::json!({"id": id, "status": "paused", "created_at": "2026-03-06T14:30:00Z"}),
        ),
    )
        .into_response()
}

pub(crate) async fn unpause_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!({"id": id, "status": "running", "created_at": "2026-03-06T14:30:00Z"}))).into_response()
}

pub(crate) async fn get_run_graph(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    // Use graphviz to render the demo DOT source
    let dot_source = "digraph demo {\n  graph [goal=\"Demo\"]\n  rankdir=LR\n  start [shape=Mdiamond, label=\"Start\"]\n  detect [label=\"Detect\\nDrift\"]\n  exit [shape=Msquare, label=\"Exit\"]\n  propose [label=\"Propose\\nChanges\"]\n  review [label=\"Review\\nChanges\"]\n  apply [label=\"Apply\\nChanges\"]\n  start -> detect\n  detect -> exit [label=\"No drift\"]\n  detect -> propose [label=\"Drift found\"]\n  propose -> review\n  review -> propose [label=\"Revise\"]\n  review -> apply [label=\"Accept\"]\n  apply -> exit\n}";

    crate::server::render_dot_svg(dot_source).await
}

pub(crate) async fn get_run_retro(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match retros::detail(&id) {
        Some(detail) => (StatusCode::OK, Json(detail)).into_response(),
        None => (StatusCode::OK, Json(json!(null))).into_response(),
    }
}

// ── Workflows ──────────────────────────────────────────────────────────

pub(crate) async fn list_workflows(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(workflows::list_items(), &pagination)
}

pub(crate) async fn get_workflow(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match workflows::detail(&name) {
        Some(detail) => (StatusCode::OK, Json(detail)).into_response(),
        None => ApiError::not_found("Workflow not found.").into_response(),
    }
}

pub(crate) async fn list_workflow_runs(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    let items: Vec<_> = runs::list_items()
        .into_iter()
        .filter(|r| r.workflow.slug == name)
        .collect();
    paginated_response(items, &pagination)
}

// ── Verification ──────────────────────────────────────────────────────

pub(crate) async fn list_verification_criteria(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(verifications::criteria(), &pagination)
}

pub(crate) async fn get_verification_criterion(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match verifications::criterion_detail(&id) {
        Some(detail) => (StatusCode::OK, Json(detail)).into_response(),
        None => ApiError::not_found("Criterion not found.").into_response(),
    }
}

pub(crate) async fn list_verification_controls(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(verifications::controls(), &pagination)
}

pub(crate) async fn get_verification_control(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match verifications::control_detail(&id) {
        Some(detail) => (StatusCode::OK, Json(detail)).into_response(),
        None => ApiError::not_found("Control not found.").into_response(),
    }
}

// ── Signoffs ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub(crate) struct SignoffListParams {
    #[serde(rename = "page[limit]", default = "crate::server::default_page_limit")]
    limit: u32,
    #[serde(rename = "page[offset]", default)]
    offset: u32,
    control: Option<String>,
    repository: Option<String>,
    commit_sha: Option<String>,
}

pub(crate) async fn list_signoffs(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(params): Query<SignoffListParams>,
) -> Response {
    let items = signoffs::list_items(
        params.control.as_deref(),
        params.repository.as_deref(),
        params.commit_sha.as_deref(),
    );
    paginated_response(
        items,
        &PaginationParams {
            limit: params.limit,
            offset: params.offset,
        },
    )
}

pub(crate) async fn get_signoff(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match signoffs::detail(&id) {
        Some(signoff) => (StatusCode::OK, Json(signoff)).into_response(),
        None => ApiError::not_found("Signoff not found.").into_response(),
    }
}

pub(crate) async fn create_signoff_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (StatusCode::CREATED, Json(signoffs::stub_created())).into_response()
}

// ── Retros ─────────────────────────────────────────────────────────────

pub(crate) async fn list_retros(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(params): Query<RetroListParams>,
) -> Response {
    let items: Vec<_> = retros::list_items()
        .into_iter()
        .filter(|r| {
            params
                .workflow
                .as_ref()
                .is_none_or(|w| &r.workflow.slug == w)
        })
        .filter(|r| {
            params
                .smoothness
                .as_ref()
                .is_none_or(|s| r.smoothness.as_ref() == Some(s))
        })
        .collect();
    paginated_response(
        items,
        &PaginationParams {
            limit: params.limit,
            offset: params.offset,
        },
    )
}

// ── Sessions ───────────────────────────────────────────────────────────

pub(crate) async fn list_sessions(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(sessions::list_items(), &pagination)
}

pub(crate) async fn create_session_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    let id = uuid::Uuid::new_v4();
    let now = "2026-03-06T16:00:00Z";
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": id, "title": "New session", "model": {"id": "Opus 4.6"}, "created_at": now, "updated_at": now})),
    )
        .into_response()
}

pub(crate) async fn get_session(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match sessions::detail(&id) {
        Some(detail) => (StatusCode::OK, Json(detail)).into_response(),
        None => ApiError::not_found("Session not found.").into_response(),
    }
}

pub(crate) async fn send_message_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"accepted": true})),
    )
        .into_response()
}

pub(crate) async fn session_events_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response {
    use axum::response::sse::{Event, Sse};

    let Some(session) = sessions::detail(&id) else {
        return ApiError::not_found("Session not found.").into_response();
    };

    let last_event_id: Option<usize> = headers
        .get("Last-Event-ID")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    let mut events: Vec<Result<Event, std::convert::Infallible>> = Vec::new();
    let mut seq: usize = 0;

    for turn in &session.turns {
        let (event_type, data) = match turn {
            fabro_api_types::SessionTurn::UserTurn(_) => continue,
            fabro_api_types::SessionTurn::AssistantTurn(t) => {
                ("assistant_turn", serde_json::to_string(t).unwrap())
            }
            fabro_api_types::SessionTurn::ToolTurn(t) => {
                ("tool_turn", serde_json::to_string(t).unwrap())
            }
        };

        if last_event_id.is_none() || seq > last_event_id.unwrap() {
            events.push(Ok(Event::default()
                .id(seq.to_string())
                .event(event_type)
                .data(data)));
        }
        seq += 1;
    }

    // Append done event
    if last_event_id.is_none() || seq > last_event_id.unwrap() {
        events.push(Ok(Event::default()
            .id(seq.to_string())
            .event("done")
            .data("{}")));
    }

    Sse::new(tokio_stream::iter(events)).into_response()
}

// ── Insights ───────────────────────────────────────────────────────────

pub(crate) async fn list_saved_queries(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(insights::saved_queries(), &pagination)
}

pub(crate) async fn save_query_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": "new-q", "name": "New Query", "sql": "SELECT 1", "created_at": "2026-03-06T16:00:00Z"})),
    )
        .into_response()
}

pub(crate) async fn get_saved_query(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match insights::saved_queries().into_iter().find(|q| q.id == id) {
        Some(query) => (StatusCode::OK, Json(query)).into_response(),
        None => ApiError::not_found("Saved query not found.").into_response(),
    }
}

pub(crate) async fn update_query_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({"id": "1", "name": "Updated", "sql": "SELECT 1", "created_at": "2026-03-01T10:00:00Z", "updated_at": "2026-03-06T16:00:00Z"})),
    )
        .into_response()
}

pub(crate) async fn delete_query_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) async fn execute_query_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "columns": ["workflow_name", "count"],
            "rows": [["implement", 42], ["fix_build", 18], ["sync_drift", 7]],
            "elapsed": 0.342,
            "row_count": 3
        })),
    )
        .into_response()
}

pub(crate) async fn list_query_history(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(insights::history(), &pagination)
}

// ── Models ────────────────────────────────────────────────────────────

pub(crate) async fn list_models(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(
        fabro_model::Catalog::builtin()
            .list(None)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        &pagination,
    )
}

// ── Settings ───────────────────────────────────────────────────────────

pub(crate) async fn get_server_settings(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (StatusCode::OK, Json(settings::server_settings())).into_response()
}

// ── Usage ──────────────────────────────────────────────────────────────

pub(crate) async fn get_aggregate_usage(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (StatusCode::OK, Json(usage::aggregate())).into_response()
}

// ── Data modules ───────────────────────────────────────────────────────

use chrono::{DateTime, Utc};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

mod runs {
    use super::ts;
    use fabro_api_types::*;

    pub(super) fn list_items() -> Vec<RunListItem> {
        vec![
            RunListItem {
                id: "run-1".into(),
                repository: RepositoryReference {
                    name: "api-server".into(),
                },
                title: "Add rate limiting to auth endpoints".into(),
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                status: BoardColumn::Working,
                pull_request: None,
                timings: Some(RunTimings {
                    elapsed_secs: 420.0,
                    elapsed_warning: Some(false),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-a1b2c3d4".into(),
                    resources: Some(SandboxResources { cpu: 4, memory: 8 }),
                }),
                question: None,
                created_at: ts("2026-03-06T14:30:00Z"),
            },
            RunListItem {
                id: "run-2".into(),
                repository: RepositoryReference {
                    name: "web-dashboard".into(),
                },
                title: "Migrate to React Router v7".into(),
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                status: BoardColumn::Working,
                pull_request: None,
                timings: Some(RunTimings {
                    elapsed_secs: 8100.0,
                    elapsed_warning: Some(false),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-e5f6g7h8".into(),
                    resources: Some(SandboxResources { cpu: 8, memory: 16 }),
                }),
                question: None,
                created_at: ts("2026-03-06T12:00:00Z"),
            },
            RunListItem {
                id: "run-3".into(),
                repository: RepositoryReference {
                    name: "cli-tools".into(),
                },
                title: "Fix config parsing for nested values".into(),
                workflow: WorkflowReference {
                    slug: "fix_build".into(),
                },
                status: BoardColumn::Working,
                pull_request: None,
                timings: Some(RunTimings {
                    elapsed_secs: 2700.0,
                    elapsed_warning: Some(false),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-i9j0k1l2".into(),
                    resources: Some(SandboxResources { cpu: 2, memory: 4 }),
                }),
                question: None,
                created_at: ts("2026-03-05T09:20:00Z"),
            },
            RunListItem {
                id: "run-4".into(),
                repository: RepositoryReference {
                    name: "api-server".into(),
                },
                title: "Update OpenAPI spec for v3".into(),
                workflow: WorkflowReference {
                    slug: "expand".into(),
                },
                status: BoardColumn::Pending,
                pull_request: Some(RunPullRequest {
                    number: 0,
                    additions: Some(567),
                    deletions: Some(234),
                    comments: Some(0),
                    checks: vec![],
                }),
                timings: Some(RunTimings {
                    elapsed_secs: 4320.0,
                    elapsed_warning: Some(false),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-q7r8s9t0".into(),
                    resources: None,
                }),
                question: Some(RunQuestion {
                    text: "Accept or push for another round?".into(),
                }),
                created_at: ts("2026-03-04T15:00:00Z"),
            },
            RunListItem {
                id: "run-5".into(),
                repository: RepositoryReference {
                    name: "shared-types".into(),
                },
                title: "Add pipeline event types".into(),
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                status: BoardColumn::Pending,
                pull_request: Some(RunPullRequest {
                    number: 0,
                    additions: Some(145),
                    deletions: Some(23),
                    comments: Some(0),
                    checks: vec![],
                }),
                timings: Some(RunTimings {
                    elapsed_secs: 1680.0,
                    elapsed_warning: Some(false),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-u1v2w3x4".into(),
                    resources: None,
                }),
                question: Some(RunQuestion {
                    text: "Proceed from investigation to fix?".into(),
                }),
                created_at: ts("2026-03-04T10:00:00Z"),
            },
            RunListItem {
                id: "run-6".into(),
                repository: RepositoryReference {
                    name: "web-dashboard".into(),
                },
                title: "Add dark mode toggle".into(),
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                status: BoardColumn::Review,
                pull_request: Some(RunPullRequest {
                    number: 889,
                    additions: Some(234),
                    deletions: Some(67),
                    comments: Some(4),
                    checks: vec![
                        CheckRun {
                            name: "lint".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(23.0),
                        },
                        CheckRun {
                            name: "typecheck".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(72.0),
                        },
                        CheckRun {
                            name: "unit-tests".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(154.0),
                        },
                        CheckRun {
                            name: "integration-tests".into(),
                            status: CheckRunStatus::Failure,
                            duration_secs: Some(296.0),
                        },
                        CheckRun {
                            name: "e2e / chrome".into(),
                            status: CheckRunStatus::Failure,
                            duration_secs: Some(182.0),
                        },
                        CheckRun {
                            name: "build".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(105.0),
                        },
                        CheckRun {
                            name: "coverage".into(),
                            status: CheckRunStatus::Skipped,
                            duration_secs: None,
                        },
                    ],
                }),
                timings: Some(RunTimings {
                    elapsed_secs: 2100.0,
                    elapsed_warning: Some(false),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-m3n4o5p6".into(),
                    resources: None,
                }),
                question: None,
                created_at: ts("2026-03-03T16:45:00Z"),
            },
            RunListItem {
                id: "run-7".into(),
                repository: RepositoryReference {
                    name: "infrastructure".into(),
                },
                title: "Terraform module for Redis cluster".into(),
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                status: BoardColumn::Review,
                pull_request: Some(RunPullRequest {
                    number: 156,
                    additions: Some(412),
                    deletions: Some(0),
                    comments: Some(1),
                    checks: vec![
                        CheckRun {
                            name: "lint".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(18.0),
                        },
                        CheckRun {
                            name: "typecheck".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(56.0),
                        },
                        CheckRun {
                            name: "unit-tests".into(),
                            status: CheckRunStatus::Pending,
                            duration_secs: None,
                        },
                        CheckRun {
                            name: "integration-tests".into(),
                            status: CheckRunStatus::Queued,
                            duration_secs: None,
                        },
                        CheckRun {
                            name: "build".into(),
                            status: CheckRunStatus::Pending,
                            duration_secs: None,
                        },
                    ],
                }),
                timings: Some(RunTimings {
                    elapsed_secs: 720.0,
                    elapsed_warning: Some(false),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-y5z6a7b8".into(),
                    resources: None,
                }),
                question: None,
                created_at: ts("2026-03-03T11:00:00Z"),
            },
            RunListItem {
                id: "run-8".into(),
                repository: RepositoryReference {
                    name: "api-server".into(),
                },
                title: "Implement webhook retry logic".into(),
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                status: BoardColumn::Merge,
                pull_request: Some(RunPullRequest {
                    number: 1249,
                    additions: Some(189),
                    deletions: Some(45),
                    comments: Some(7),
                    checks: vec![
                        CheckRun {
                            name: "lint".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(21.0),
                        },
                        CheckRun {
                            name: "typecheck".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(68.0),
                        },
                        CheckRun {
                            name: "unit-tests".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(192.0),
                        },
                        CheckRun {
                            name: "integration-tests".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(334.0),
                        },
                        CheckRun {
                            name: "e2e / chrome".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(262.0),
                        },
                        CheckRun {
                            name: "e2e / firefox".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(285.0),
                        },
                        CheckRun {
                            name: "build".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(121.0),
                        },
                        CheckRun {
                            name: "deploy-preview".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(93.0),
                        },
                        CheckRun {
                            name: "security-scan".into(),
                            status: CheckRunStatus::Skipped,
                            duration_secs: None,
                        },
                        CheckRun {
                            name: "performance".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(138.0),
                        },
                        CheckRun {
                            name: "bundle-size".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(34.0),
                        },
                        CheckRun {
                            name: "accessibility".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(72.0),
                        },
                    ],
                }),
                timings: Some(RunTimings {
                    elapsed_secs: 259200.0,
                    elapsed_warning: Some(true),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-c9d0e1f2".into(),
                    resources: None,
                }),
                question: None,
                created_at: ts("2026-02-28T14:00:00Z"),
            },
            RunListItem {
                id: "run-9".into(),
                repository: RepositoryReference {
                    name: "cli-tools".into(),
                },
                title: "Add --verbose flag to run command".into(),
                workflow: WorkflowReference {
                    slug: "expand".into(),
                },
                status: BoardColumn::Merge,
                pull_request: Some(RunPullRequest {
                    number: 430,
                    additions: Some(56),
                    deletions: Some(12),
                    comments: Some(2),
                    checks: vec![
                        CheckRun {
                            name: "lint".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(15.0),
                        },
                        CheckRun {
                            name: "typecheck".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(48.0),
                        },
                        CheckRun {
                            name: "unit-tests".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(116.0),
                        },
                        CheckRun {
                            name: "build".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(82.0),
                        },
                        CheckRun {
                            name: "coverage".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(124.0),
                        },
                        CheckRun {
                            name: "bundle-size".into(),
                            status: CheckRunStatus::Skipped,
                            duration_secs: None,
                        },
                    ],
                }),
                timings: Some(RunTimings {
                    elapsed_secs: 3900.0,
                    elapsed_warning: Some(false),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-g3h4i5j6".into(),
                    resources: None,
                }),
                question: None,
                created_at: ts("2026-02-27T09:00:00Z"),
            },
            RunListItem {
                id: "run-10".into(),
                repository: RepositoryReference {
                    name: "shared-types".into(),
                },
                title: "Export utility type helpers".into(),
                workflow: WorkflowReference {
                    slug: "sync_drift".into(),
                },
                status: BoardColumn::Merge,
                pull_request: Some(RunPullRequest {
                    number: 76,
                    additions: Some(34),
                    deletions: Some(8),
                    comments: Some(0),
                    checks: vec![
                        CheckRun {
                            name: "lint".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(12.0),
                        },
                        CheckRun {
                            name: "typecheck".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(34.0),
                        },
                        CheckRun {
                            name: "unit-tests".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(75.0),
                        },
                        CheckRun {
                            name: "build".into(),
                            status: CheckRunStatus::Success,
                            duration_secs: Some(58.0),
                        },
                    ],
                }),
                timings: Some(RunTimings {
                    elapsed_secs: 2880.0,
                    elapsed_warning: Some(false),
                }),
                sandbox: Some(RunSandbox {
                    id: "sb-k7l8m9n0".into(),
                    resources: None,
                }),
                question: None,
                created_at: ts("2026-02-26T08:00:00Z"),
            },
        ]
    }

    pub(super) fn stages() -> Vec<RunStage> {
        vec![
            RunStage {
                id: "detect-drift".into(),
                name: "Detect Drift".into(),
                status: StageStatus::Completed,
                duration_secs: Some(72.0),
                dot_id: Some("detect".into()),
            },
            RunStage {
                id: "propose-changes".into(),
                name: "Propose Changes".into(),
                status: StageStatus::Completed,
                duration_secs: Some(154.0),
                dot_id: Some("propose".into()),
            },
            RunStage {
                id: "review-changes".into(),
                name: "Review Changes".into(),
                status: StageStatus::Completed,
                duration_secs: Some(45.0),
                dot_id: Some("review".into()),
            },
            RunStage {
                id: "apply-changes".into(),
                name: "Apply Changes".into(),
                status: StageStatus::Running,
                duration_secs: Some(118.0),
                dot_id: Some("apply".into()),
            },
        ]
    }

    pub(super) fn turns() -> Vec<StageTurn> {
        vec![
            StageTurn::SystemStageTurn(SystemStageTurn { kind: SystemStageTurnKind::System, content: "You are a drift detection agent. Compare the production and staging environments and identify any configuration or code drift.".into() }),
            StageTurn::AssistantStageTurn(AssistantStageTurn { kind: AssistantStageTurnKind::Assistant, content: "I'll start by loading the environment configurations for both production and staging to compare them.".into() }),
            StageTurn::ToolStageTurn(ToolStageTurn {
                kind: ToolStageTurnKind::Tool, content: None,
                tools: vec![
                    ToolUse { id: "toolu_01".into(), tool_name: "read_file".into(), input: r#"{ "path": "environments/production/config.toml" }"#.into(), result: "[redis]\nhost = \"redis-prod.internal\"\nport = 6379".into(), is_error: false, duration_ms: Some(45) },
                    ToolUse { id: "toolu_02".into(), tool_name: "read_file".into(), input: r#"{ "path": "environments/staging/config.toml" }"#.into(), result: "[redis]\nhost = \"redis-staging.internal\"\nport = 6379".into(), is_error: false, duration_ms: Some(38) },
                ],
            }),
            StageTurn::AssistantStageTurn(AssistantStageTurn { kind: AssistantStageTurnKind::Assistant, content: "I've detected drift in 3 resources between production and staging:\n\n1. **redis.max_connections** — production has 200, staging has 100\n2. **redis.tls** — enabled in production, disabled in staging\n3. **iam.session_duration** — production uses 3600s, staging uses 1800s".into() }),
        ]
    }

    pub(super) fn files() -> Vec<FileDiff> {
        vec![
                FileDiff {
                    old_file: DiffFile { name: "src/commands/run.ts".into(), contents: "import { parseArgs } from \"node:util\";\nimport { loadConfig } from \"../config.js\";\nimport { execute } from \"../executor.js\";\n\ninterface RunOptions {\n  config: string;\n  dryRun: boolean;\n}\n\nexport async function run(argv: string[]) {\n  const { values } = parseArgs({\n    args: argv,\n    options: {\n      config: { type: \"string\", short: \"c\", default: \"fabro.toml\" },\n      \"dry-run\": { type: \"boolean\", default: false },\n    },\n  });\n\n  const opts: RunOptions = {\n    config: values.config ?? \"fabro.toml\",\n    dryRun: values[\"dry-run\"] ?? false,\n  };\n\n  const config = await loadConfig(opts.config);\n  const result = await execute(config, { dryRun: opts.dryRun });\n\n  if (result.success) {\n    console.log(\"Run completed successfully.\");\n  } else {\n    console.error(\"Run failed:\", result.error);\n    process.exitCode = 1;\n  }\n}\n".into() },
                    new_file: DiffFile { name: "src/commands/run.ts".into(), contents: "import { parseArgs } from \"node:util\";\nimport { loadConfig } from \"../config.js\";\nimport { execute } from \"../executor.js\";\nimport { createLogger, type Logger } from \"../logger.js\";\n\ninterface RunOptions {\n  config: string;\n  dryRun: boolean;\n  verbose: boolean;\n}\n\nexport async function run(argv: string[]) {\n  const { values } = parseArgs({\n    args: argv,\n    options: {\n      config: { type: \"string\", short: \"c\", default: \"fabro.toml\" },\n      \"dry-run\": { type: \"boolean\", default: false },\n      verbose: { type: \"boolean\", short: \"v\", default: false },\n    },\n  });\n\n  const opts: RunOptions = {\n    config: values.config ?? \"fabro.toml\",\n    dryRun: values[\"dry-run\"] ?? false,\n    verbose: values.verbose ?? false,\n  };\n\n  const logger: Logger = createLogger({ verbose: opts.verbose });\n\n  const config = await loadConfig(opts.config);\n  logger.debug(\"Loaded config from %s\", opts.config);\n\n  const result = await execute(config, { dryRun: opts.dryRun, logger });\n  logger.debug(\"Execution finished in %dms\", result.elapsed);\n\n  if (result.success) {\n    console.log(\"Run completed successfully.\");\n  } else {\n    console.error(\"Run failed:\", result.error);\n    process.exitCode = 1;\n  }\n}\n".into() },
                },
                FileDiff {
                    old_file: DiffFile { name: "src/logger.ts".into(), contents: String::new() },
                    new_file: DiffFile { name: "src/logger.ts".into(), contents: "export interface Logger {\n  info(message: string, ...args: unknown[]): void;\n  debug(message: string, ...args: unknown[]): void;\n  error(message: string, ...args: unknown[]): void;\n}\n\ninterface LoggerOptions {\n  verbose: boolean;\n}\n\nexport function createLogger({ verbose }: LoggerOptions): Logger {\n  return {\n    info(message, ...args) {\n      console.log(message, ...args);\n    },\n    debug(message, ...args) {\n      if (verbose) {\n        console.log(\"[debug]\", message, ...args);\n      }\n    },\n    error(message, ...args) {\n      console.error(message, ...args);\n    },\n  };\n}\n".into() },
                },
                FileDiff {
                    old_file: DiffFile { name: "src/executor.ts".into(), contents: "import type { Config } from \"./config.js\";\n\ninterface ExecuteOptions {\n  dryRun: boolean;\n}\n\ninterface ExecuteResult {\n  success: boolean;\n  error?: string;\n}\n\nexport async function execute(\n  config: Config,\n  options: ExecuteOptions,\n): Promise<ExecuteResult> {\n  if (options.dryRun) {\n    console.log(\"Dry run — skipping execution.\");\n    return { success: true };\n  }\n\n  try {\n    for (const step of config.steps) {\n      await step.run();\n    }\n    return { success: true };\n  } catch (err) {\n    const message = err instanceof Error ? err.message : String(err);\n    return { success: false, error: message };\n  }\n}\n".into() },
                    new_file: DiffFile { name: "src/executor.ts".into(), contents: "import type { Config } from \"./config.js\";\nimport type { Logger } from \"./logger.js\";\n\ninterface ExecuteOptions {\n  dryRun: boolean;\n  logger: Logger;\n}\n\ninterface ExecuteResult {\n  success: boolean;\n  elapsed: number;\n  error?: string;\n}\n\nexport async function execute(\n  config: Config,\n  options: ExecuteOptions,\n): Promise<ExecuteResult> {\n  const start = performance.now();\n\n  if (options.dryRun) {\n    options.logger.info(\"Dry run — skipping execution.\");\n    return { success: true, elapsed: performance.now() - start };\n  }\n\n  try {\n    for (const step of config.steps) {\n      options.logger.debug(\"Running step: %s\", step.name);\n      await step.run();\n    }\n    return { success: true, elapsed: performance.now() - start };\n  } catch (err) {\n    const message = err instanceof Error ? err.message : String(err);\n    return { success: false, elapsed: performance.now() - start, error: message };\n  }\n}\n".into() },
                },
        ]
    }

    pub(super) fn usage() -> RunUsage {
        RunUsage {
            stages: vec![
                UsageStage {
                    stage: UsageStageRef {
                        id: "detect-drift".into(),
                        name: "Detect Drift".into(),
                    },
                    model: ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    usage: TokenUsage {
                        input_tokens: 12480,
                        output_tokens: 3210,
                        cost: 0.48,
                    },
                    runtime_secs: 72.0,
                },
                UsageStage {
                    stage: UsageStageRef {
                        id: "propose-changes".into(),
                        name: "Propose Changes".into(),
                    },
                    model: ModelReference {
                        id: "Gemini 3.1".into(),
                    },
                    usage: TokenUsage {
                        input_tokens: 28640,
                        output_tokens: 8750,
                        cost: 0.72,
                    },
                    runtime_secs: 154.0,
                },
                UsageStage {
                    stage: UsageStageRef {
                        id: "review-changes".into(),
                        name: "Review Changes".into(),
                    },
                    model: ModelReference {
                        id: "Codex 5.3".into(),
                    },
                    usage: TokenUsage {
                        input_tokens: 9120,
                        output_tokens: 2640,
                        cost: 0.19,
                    },
                    runtime_secs: 45.0,
                },
                UsageStage {
                    stage: UsageStageRef {
                        id: "apply-changes".into(),
                        name: "Apply Changes".into(),
                    },
                    model: ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    usage: TokenUsage {
                        input_tokens: 21300,
                        output_tokens: 6480,
                        cost: 0.87,
                    },
                    runtime_secs: 118.0,
                },
            ],
            totals: UsageTotals {
                runtime_secs: 389.0,
                input_tokens: 71540,
                output_tokens: 21080,
                cost: 2.26,
            },
            by_model: vec![
                UsageByModel {
                    model: ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    stages: 2,
                    usage: TokenUsage {
                        input_tokens: 33780,
                        output_tokens: 9690,
                        cost: 1.35,
                    },
                },
                UsageByModel {
                    model: ModelReference {
                        id: "Gemini 3.1".into(),
                    },
                    stages: 1,
                    usage: TokenUsage {
                        input_tokens: 28640,
                        output_tokens: 8750,
                        cost: 0.72,
                    },
                },
                UsageByModel {
                    model: ModelReference {
                        id: "Codex 5.3".into(),
                    },
                    stages: 1,
                    usage: TokenUsage {
                        input_tokens: 9120,
                        output_tokens: 2640,
                        cost: 0.19,
                    },
                },
            ],
        }
    }

    pub(super) fn verifications() -> Vec<fabro_api_types::RunVerification> {
        super::verifications::run_verifications()
    }

    pub(super) fn questions() -> Vec<ApiQuestion> {
        vec![
            ApiQuestion {
                id: "q-001".into(),
                text: "Should we proceed with the proposed changes?".into(),
                question_type: QuestionType::YesNo,
                options: vec![
                    ApiQuestionOption {
                        key: "yes".into(),
                        label: "Yes".into(),
                    },
                    ApiQuestionOption {
                        key: "no".into(),
                        label: "No".into(),
                    },
                ],
                allow_freeform: false,
            },
            ApiQuestion {
                id: "q-002".into(),
                text: "Which approach do you prefer for the migration?".into(),
                question_type: QuestionType::MultipleChoice,
                options: vec![
                    ApiQuestionOption {
                        key: "incremental".into(),
                        label: "Incremental migration".into(),
                    },
                    ApiQuestionOption {
                        key: "big_bang".into(),
                        label: "Big-bang rewrite".into(),
                    },
                ],
                allow_freeform: true,
            },
        ]
    }

    pub(super) fn settings() -> serde_json::Value {
        serde_json::to_value(fabro_config::FabroSettings {
            version: Some(1),
            goal: Some("Add rate limiting to auth endpoints".into()),
            graph: Some("implement.fabro".into()),
            work_dir: Some("/workspace/api-server".into()),
            llm: Some(fabro_config::run::LlmSettings {
                model: Some("claude-opus-4-6".into()),
                provider: Some("anthropic".into()),
                fallbacks: None,
            }),
            setup: Some(fabro_config::run::SetupSettings {
                commands: vec!["bun install".into(), "bun run typecheck".into()],
                timeout_ms: Some(120_000),
            }),
            sandbox: Some(fabro_config::sandbox::SandboxSettings {
                provider: Some("daytona".into()),
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: Some(fabro_sandbox::daytona::DaytonaConfig {
                    auto_stop_interval: Some(60),
                    labels: Some(std::collections::HashMap::from([(
                        "project".into(),
                        "api-server".into(),
                    )])),
                    snapshot: Some(fabro_sandbox::daytona::DaytonaSnapshotConfig {
                        name: "api-server-dev".into(),
                        cpu: Some(4),
                        memory: Some(8),
                        disk: Some(10),
                        dockerfile: None,
                    }),
                    network: Some(fabro_sandbox::daytona::DaytonaNetwork::Block),
                    skip_clone: false,
                }),
                env: None,
            }),
            vars: Some(std::collections::HashMap::from([
                (
                    "repo_url".into(),
                    "https://github.com/org/api-server".into(),
                ),
                ("branch".into(), "feature/rate-limiting".into()),
            ])),
            hooks: vec![],
            checkpoint: Default::default(),
            pull_request: None,
            assets: None,
            mcp_servers: Default::default(),
            github: None,
            ..Default::default()
        })
        .unwrap()
    }
}

mod usage {
    use fabro_api_types::*;

    pub(super) fn aggregate() -> AggregateUsage {
        AggregateUsage {
            totals: AggregateUsageTotals {
                runs: 9,
                input_tokens: 643_860,
                output_tokens: 189_720,
                cost: 20.34,
                runtime_secs: 3_501.0,
            },
            by_model: vec![
                UsageByModel {
                    model: ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    stages: 18,
                    usage: TokenUsage {
                        input_tokens: 304_020,
                        output_tokens: 87_210,
                        cost: 12.15,
                    },
                },
                UsageByModel {
                    model: ModelReference {
                        id: "Gemini 3.1".into(),
                    },
                    stages: 9,
                    usage: TokenUsage {
                        input_tokens: 257_760,
                        output_tokens: 78_750,
                        cost: 6.48,
                    },
                },
                UsageByModel {
                    model: ModelReference {
                        id: "Codex 5.3".into(),
                    },
                    stages: 9,
                    usage: TokenUsage {
                        input_tokens: 82_080,
                        output_tokens: 23_760,
                        cost: 1.71,
                    },
                },
            ],
        }
    }
}

mod workflows {
    use super::ts;
    use fabro_api_types::*;

    pub(super) fn list_items() -> Vec<WorkflowListItem> {
        vec![
            WorkflowListItem {
                name: "Fix Build".into(),
                slug: "fix_build".into(),
                filename: "fix_build.fabro".into(),
                last_run: Some(WorkflowLastRun {
                    ran_at: ts("2025-09-15T12:00:00Z"),
                }),
                schedule: None,
            },
            WorkflowListItem {
                name: "Implement Feature".into(),
                slug: "implement".into(),
                filename: "implement.fabro".into(),
                last_run: Some(WorkflowLastRun {
                    ran_at: ts("2025-09-11T10:00:00Z"),
                }),
                schedule: None,
            },
            WorkflowListItem {
                name: "Sync Drift".into(),
                slug: "sync_drift".into(),
                filename: "sync_drift.fabro".into(),
                last_run: Some(WorkflowLastRun {
                    ran_at: ts("2025-09-14T14:00:00Z"),
                }),
                schedule: None,
            },
            WorkflowListItem {
                name: "Expand Product".into(),
                slug: "expand".into(),
                filename: "expand.fabro".into(),
                last_run: Some(WorkflowLastRun {
                    ran_at: ts("2025-09-01T08:00:00Z"),
                }),
                schedule: None,
            },
        ]
    }

    fn run_settings_to_api(cfg: fabro_config::FabroSettings) -> RunSettings {
        fn strip_nulls(val: serde_json::Value) -> serde_json::Value {
            match val {
                serde_json::Value::Object(map) => serde_json::Value::Object(
                    map.into_iter()
                        .filter(|(_, v)| !v.is_null())
                        .map(|(k, v)| (k, strip_nulls(v)))
                        .collect(),
                ),
                serde_json::Value::Array(arr) => {
                    serde_json::Value::Array(arr.into_iter().map(strip_nulls).collect())
                }
                other => other,
            }
        }
        let val = strip_nulls(serde_json::to_value(cfg).unwrap());
        serde_json::from_value(val).unwrap()
    }

    pub(super) fn detail(name: &str) -> Option<WorkflowDetail> {
        let items = [
            WorkflowDetail {
                name: "Fix Build".into(), slug: "fix_build".into(), filename: "fix_build.fabro".into(),
                description: "Automatically diagnoses and fixes CI build failures by analyzing error logs, identifying root causes, and applying targeted code changes.".into(),
                settings: run_settings_to_api(fabro_config::FabroSettings {
                    version: Some(1),
                    goal: Some("Diagnose and fix CI build failures".into()),
                    graph: Some("fix_build.fabro".into()),
                    work_dir: None,
                    llm: Some(fabro_config::run::LlmSettings {
                        model: Some("claude-sonnet".into()),
                        provider: None,
                        fallbacks: None,
                    }),
                    setup: None,
                    sandbox: Some(fabro_config::sandbox::SandboxSettings {
                        provider: Some("daytona".into()),
                        preserve: None,
                        devcontainer: None,
                        local: None,
                        daytona: Some(fabro_sandbox::daytona::DaytonaConfig {
                            auto_stop_interval: Some(60),
                            labels: Some(std::collections::HashMap::from([
                                ("project".into(), "fix-build".into()),
                            ])),
                            snapshot: Some(fabro_sandbox::daytona::DaytonaSnapshotConfig {
                                name: "fix-build-dev".into(),
                                cpu: Some(4),
                                memory: Some(8),
                                disk: Some(10),
                                dockerfile: None,
                            }),
                            network: None,
                            skip_clone: false,
                        }),
                        env: None,
                    }),
                    vars: Some(std::collections::HashMap::from([
                        ("repo_url".into(), "https://github.com/org/service".into()),
                        ("branch".into(), "main".into()),
                    ])),
                    hooks: vec![],
                    checkpoint: Default::default(),
                    pull_request: None,
                    assets: None,
                    mcp_servers: Default::default(),
                    github: None,
                    ..Default::default()
                }),
                graph: r#"digraph fix_build {
    graph [
        goal="Diagnose and fix CI build failures",
        label="Fix Build"
    ]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    diagnose [label="Diagnose Failure", prompt="@prompts/fix_build/diagnose.md", reasoning_effort="high"]
    fix      [label="Apply Fix",        prompt="@prompts/fix_build/fix.md"]
    validate [label="Run Build",        prompt="@prompts/fix_build/validate.md", goal_gate=true]
    gate     [shape=diamond,            label="Build passing?"]

    start -> diagnose -> fix -> validate -> gate
    gate -> exit     [label="Yes", condition="outcome=success"]
    gate -> diagnose [label="No",  condition="outcome!=success", max_visits=3]
}
"#.into(),
            },
            WorkflowDetail {
                name: "Implement Feature".into(), slug: "implement".into(), filename: "implement.fabro".into(),
                description: "Generates production-ready code from a technical blueprint, including tests, documentation, and a pull request ready for review.".into(),
                settings: run_settings_to_api(fabro_config::FabroSettings {
                    version: Some(1),
                    goal: Some("Implement feature from technical blueprint".into()),
                    graph: Some("implement.fabro".into()),
                    work_dir: None,
                    llm: Some(fabro_config::run::LlmSettings {
                        model: Some("claude-sonnet".into()),
                        provider: None,
                        fallbacks: None,
                    }),
                    setup: Some(fabro_config::run::SetupSettings {
                        commands: vec!["bun install".into(), "bun run typecheck".into()],
                        timeout_ms: Some(120_000),
                    }),
                    sandbox: Some(fabro_config::sandbox::SandboxSettings {
                        provider: Some("daytona".into()),
                        preserve: None,
                        devcontainer: None,
                        local: None,
                        daytona: Some(fabro_sandbox::daytona::DaytonaConfig {
                            auto_stop_interval: Some(120),
                            labels: Some(std::collections::HashMap::from([
                                ("project".into(), "implement".into()),
                                ("team".into(), "engineering".into()),
                            ])),
                            snapshot: Some(fabro_sandbox::daytona::DaytonaSnapshotConfig {
                                name: "implement-dev".into(),
                                cpu: Some(4),
                                memory: Some(8),
                                disk: Some(20),
                                dockerfile: None,
                            }),
                            network: None,
                            skip_clone: false,
                        }),
                        env: None,
                    }),
                    vars: Some(std::collections::HashMap::from([
                        ("spec_path".into(), "specs/feature.md".into()),
                        ("test_framework".into(), "vitest".into()),
                    ])),
                    hooks: vec![],
                    checkpoint: Default::default(),
                    pull_request: None,
                    assets: None,
                    mcp_servers: Default::default(),
                    github: None,
                    ..Default::default()
                }),
                graph: r#"digraph implement {
    graph [
        goal="",
        label="Implement"
    ]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    strategy [shape=hexagon, label="Choose decomposition strategy:"]

    subgraph cluster_impl {
        label="Implementation Loop"
        node [fidelity="full", thread_id="impl"]

        plan      [label="Plan Implementation", prompt="@prompts/implement/plan.md", reasoning_effort="high"]
        implement [label="Implement",            prompt="@prompts/implement/implement.md"]
        review    [label="Review",               prompt="@prompts/implement/review.md"]
        validate  [label="Validate",             prompt="@prompts/implement/validate.md", goal_gate=true]
        fix       [label="Fix Failures",         prompt="@prompts/implement/fix.md", max_visits=3]
    }

    start -> strategy
    strategy -> plan [label="[L] Layer-by-layer"]
    strategy -> plan [label="[F] Feature slice"]
    strategy -> plan [label="[P] Embarrassingly parallel"]
    strategy -> plan [label="[S] Sequential / linear"]
    plan -> implement -> review -> validate
    validate -> exit [condition="outcome=success"]
    validate -> fix  [condition="outcome!=success", label="Fix"]
    fix -> validate
}
"#.into(),
            },
            WorkflowDetail {
                name: "Sync Drift".into(), slug: "sync_drift".into(), filename: "sync_drift.fabro".into(),
                description: "Detects configuration and code drift between environments, then generates reconciliation patches to bring everything back in sync.".into(),
                settings: run_settings_to_api(fabro_config::FabroSettings {
                    version: Some(1),
                    goal: Some("Detect and reconcile configuration drift across environments".into()),
                    graph: Some("sync_drift.fabro".into()),
                    work_dir: None,
                    llm: Some(fabro_config::run::LlmSettings {
                        model: Some("claude-sonnet".into()),
                        provider: None,
                        fallbacks: None,
                    }),
                    setup: None,
                    sandbox: Some(fabro_config::sandbox::SandboxSettings {
                        provider: Some("daytona".into()),
                        preserve: None,
                        devcontainer: None,
                        local: None,
                        daytona: Some(fabro_sandbox::daytona::DaytonaConfig {
                            auto_stop_interval: Some(120),
                            labels: Some(std::collections::HashMap::from([
                                ("project".into(), "sync-drift".into()),
                                ("team".into(), "platform".into()),
                            ])),
                            snapshot: Some(fabro_sandbox::daytona::DaytonaSnapshotConfig {
                                name: "sync-drift-dev".into(),
                                cpu: Some(2),
                                memory: Some(4),
                                disk: Some(10),
                                dockerfile: None,
                            }),
                            network: None,
                            skip_clone: false,
                        }),
                        env: None,
                    }),
                    vars: Some(std::collections::HashMap::from([
                        ("source_env".into(), "production".into()),
                        ("target_env".into(), "staging".into()),
                        ("drift_threshold".into(), "warn".into()),
                    ])),
                    hooks: vec![],
                    checkpoint: Default::default(),
                    pull_request: None,
                    assets: None,
                    mcp_servers: Default::default(),
                    github: None,
                    ..Default::default()
                }),
                graph: r#"digraph sync {
    graph [
        goal="Detect and resolve drift between product docs, architecture docs, and code",
        label="Sync"
    ]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    detect  [label="Detect Drift",     prompt="@prompts/sync/detect.md", reasoning_effort="high"]
    propose [label="Propose Changes",  prompt="@prompts/sync/propose.md"]
    review  [shape=hexagon,            label="Review Changes"]
    apply   [label="Apply Changes",    prompt="@prompts/sync/apply.md"]

    start -> detect
    detect -> exit    [condition="context.drift_found=false", label="No drift"]
    detect -> propose [condition="context.drift_found=true", label="Drift found"]
    propose -> review
    review -> apply    [label="[A] Accept"]
    review -> propose  [label="[R] Revise"]
    apply -> exit
}
"#.into(),
            },
            WorkflowDetail {
                name: "Expand Product".into(), slug: "expand".into(), filename: "expand.fabro".into(),
                description: "Evolves the product by analyzing usage patterns and specifications to propose and implement incremental improvements.".into(),
                settings: run_settings_to_api(fabro_config::FabroSettings {
                    version: Some(1),
                    goal: Some("Propose and implement incremental product improvements".into()),
                    graph: Some("expand.fabro".into()),
                    work_dir: None,
                    llm: Some(fabro_config::run::LlmSettings {
                        model: Some("claude-sonnet".into()),
                        provider: None,
                        fallbacks: None,
                    }),
                    setup: None,
                    sandbox: Some(fabro_config::sandbox::SandboxSettings {
                        provider: Some("daytona".into()),
                        preserve: None,
                        devcontainer: None,
                        local: None,
                        daytona: Some(fabro_sandbox::daytona::DaytonaConfig {
                            auto_stop_interval: Some(180),
                            labels: Some(std::collections::HashMap::from([
                                ("project".into(), "expand".into()),
                                ("team".into(), "product".into()),
                            ])),
                            snapshot: Some(fabro_sandbox::daytona::DaytonaSnapshotConfig {
                                name: "expand-dev".into(),
                                cpu: Some(2),
                                memory: Some(4),
                                disk: Some(10),
                                dockerfile: None,
                            }),
                            network: None,
                            skip_clone: false,
                        }),
                        env: None,
                    }),
                    vars: Some(std::collections::HashMap::from([
                        ("analytics_window".into(), "30d".into()),
                        ("min_confidence".into(), "0.8".into()),
                    ])),
                    hooks: vec![],
                    checkpoint: Default::default(),
                    pull_request: None,
                    assets: None,
                    mcp_servers: Default::default(),
                    github: None,
                    ..Default::default()
                }),
                graph: r#"digraph expand {
    graph [
        goal="",
        label="Expand"
    ]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    propose [label="Propose Changes",  prompt="@prompts/expand/propose.md", reasoning_effort="high"]
    approve [shape=hexagon,            label="Approve Changes"]
    execute [label="Execute Changes",  prompt="@prompts/expand/execute.md"]

    start -> propose -> approve
    approve -> execute [label="[A] Accept"]
    approve -> propose [label="[R] Revise"]
    execute -> exit
}
"#.into(),
            },
        ];
        items.into_iter().find(|w| w.slug == name)
    }
}

mod verifications {
    use super::ts;
    use fabro_api_types::*;

    // ── Category definitions (name, question, controls) ─────────────────

    struct CategoryDef {
        name: &'static str,
        question: &'static str,
        controls: &'static [ControlDef],
    }

    struct ControlDef {
        name: &'static str,
        slug: &'static str,
        description: &'static str,
        type_: VerificationType,
        mode: VerificationMode,
        f1: Option<f64>,
        pass_at_1: Option<f64>,
        evaluations: &'static [VerificationResult],
        // Run-level status
        run_status: VerificationResult,
        // Detail fields
        detail_description: &'static str,
        checks: &'static [&'static str],
        pass_example: &'static str,
        fail_example: &'static str,
        // Recent results (None means use defaults)
        recent_results: Option<&'static [RecentResultDef]>,
    }

    struct RecentResultDef {
        run_id: &'static str,
        run_title: &'static str,
        workflow: &'static str,
        result: VerificationResult,
        timestamp: &'static str,
    }

    const DEFAULT_RECENT_RESULTS: &[RecentResultDef] = &[
        RecentResultDef {
            run_id: "run-047",
            run_title: "PR #312 \u{2014} Add OAuth2 PKCE flow",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-15T12:00:00Z",
        },
        RecentResultDef {
            run_id: "run-046",
            run_title: "PR #311 \u{2014} Update rate limiter config",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-15T09:00:00Z",
        },
        RecentResultDef {
            run_id: "run-044",
            run_title: "PR #309 \u{2014} Migrate to pnpm",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-14T14:00:00Z",
        },
        RecentResultDef {
            run_id: "run-042",
            run_title: "PR #307 \u{2014} Fix session timeout",
            workflow: "fix_build",
            result: VerificationResult::Pass,
            timestamp: "2025-09-13T14:00:00Z",
        },
        RecentResultDef {
            run_id: "run-040",
            run_title: "PR #305 \u{2014} Add webhook retries",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-12T14:00:00Z",
        },
    ];

    const MOTIVATION_RESULTS: &[RecentResultDef] = &[
        RecentResultDef {
            run_id: "run-047",
            run_title: "PR #312 \u{2014} Add OAuth2 PKCE flow",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-15T12:00:00Z",
        },
        RecentResultDef {
            run_id: "run-046",
            run_title: "PR #311 \u{2014} Update rate limiter config",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-15T09:00:00Z",
        },
        RecentResultDef {
            run_id: "run-044",
            run_title: "PR #309 \u{2014} Migrate to pnpm",
            workflow: "code_review",
            result: VerificationResult::Fail,
            timestamp: "2025-09-14T14:00:00Z",
        },
        RecentResultDef {
            run_id: "run-042",
            run_title: "PR #307 \u{2014} Fix session timeout",
            workflow: "fix_build",
            result: VerificationResult::Pass,
            timestamp: "2025-09-13T14:00:00Z",
        },
        RecentResultDef {
            run_id: "run-040",
            run_title: "PR #305 \u{2014} Add webhook retries",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-12T14:00:00Z",
        },
    ];

    const DOCUMENTATION_RESULTS: &[RecentResultDef] = &[
        RecentResultDef {
            run_id: "run-047",
            run_title: "PR #312 \u{2014} Add OAuth2 PKCE flow",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-15T12:00:00Z",
        },
        RecentResultDef {
            run_id: "run-046",
            run_title: "PR #311 \u{2014} Update rate limiter config",
            workflow: "code_review",
            result: VerificationResult::Fail,
            timestamp: "2025-09-15T09:00:00Z",
        },
        RecentResultDef {
            run_id: "run-044",
            run_title: "PR #309 \u{2014} Migrate to pnpm",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-14T14:00:00Z",
        },
        RecentResultDef {
            run_id: "run-042",
            run_title: "PR #307 \u{2014} Fix session timeout",
            workflow: "fix_build",
            result: VerificationResult::Pass,
            timestamp: "2025-09-13T14:00:00Z",
        },
        RecentResultDef {
            run_id: "run-040",
            run_title: "PR #305 \u{2014} Add webhook retries",
            workflow: "code_review",
            result: VerificationResult::Fail,
            timestamp: "2025-09-12T14:00:00Z",
        },
    ];

    const ROLLOUT_ROLLBACK_RESULTS: &[RecentResultDef] = &[
        RecentResultDef {
            run_id: "run-047",
            run_title: "PR #312 \u{2014} Add OAuth2 PKCE flow",
            workflow: "code_review",
            result: VerificationResult::Fail,
            timestamp: "2025-09-15T12:00:00Z",
        },
        RecentResultDef {
            run_id: "run-046",
            run_title: "PR #311 \u{2014} Update rate limiter config",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-15T09:00:00Z",
        },
        RecentResultDef {
            run_id: "run-044",
            run_title: "PR #309 \u{2014} Migrate to pnpm",
            workflow: "code_review",
            result: VerificationResult::Fail,
            timestamp: "2025-09-14T14:00:00Z",
        },
        RecentResultDef {
            run_id: "run-042",
            run_title: "PR #307 \u{2014} Fix session timeout",
            workflow: "fix_build",
            result: VerificationResult::Fail,
            timestamp: "2025-09-13T14:00:00Z",
        },
        RecentResultDef {
            run_id: "run-040",
            run_title: "PR #305 \u{2014} Add webhook retries",
            workflow: "code_review",
            result: VerificationResult::Pass,
            timestamp: "2025-09-12T14:00:00Z",
        },
    ];

    use VerificationResult::{Fail as F, Pass as P};

    const ALL_CATEGORIES: &[CategoryDef] = &[
        CategoryDef {
            name: "Traceability",
            question: "Do we understand what this change is and why we're making it?",
            controls: &[
                ControlDef {
                    name: "Motivation",
                    slug: "motivation",
                    description: "Origin of proposal identified",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Active,
                    f1: Some(0.87),
                    pass_at_1: Some(0.82),
                    evaluations: &[P, P, F, P, P, P, P, F, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Verifies that every change traces back to a clear origin \u{2014} whether a ticket, RFC, customer request, or incident. Without documented motivation, reviewers lack context for evaluating whether the change is appropriate.",
                    checks: &[
                        "PR body or linked issue explains why the change is needed",
                        "Commit messages reference a ticket or context",
                        "No orphaned changes without traceable origin",
                    ],
                    pass_example: "PR links to JIRA-1234 and explains the user-facing pain point being resolved.",
                    fail_example: "PR description is empty or says only 'fix stuff'.",
                    recent_results: Some(MOTIVATION_RESULTS),
                },
                ControlDef {
                    name: "Specifications",
                    slug: "specifications",
                    description: "Requirements written down",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Active,
                    f1: Some(0.83),
                    pass_at_1: Some(0.78),
                    evaluations: &[P, F, P, P, P, F, P, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Checks that functional and non-functional requirements are written down before implementation begins. Specifications prevent scope creep and ensure everyone agrees on what done looks like.",
                    checks: &[
                        "Acceptance criteria listed in the issue or PR",
                        "Edge cases documented",
                        "Non-functional requirements (performance, security) stated when relevant",
                    ],
                    pass_example: "Issue includes acceptance criteria with three testable scenarios.",
                    fail_example: "Issue body says 'implement the feature' with no acceptance criteria.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Documentation",
                    slug: "documentation",
                    description: "Developer and user docs added",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Active,
                    f1: Some(0.79),
                    pass_at_1: Some(0.74),
                    evaluations: &[P, P, P, F, P, P, F, P, P, F],
                    run_status: VerificationResult::Pass,
                    detail_description: "Ensures developer-facing and user-facing documentation is added or updated alongside code changes. Stale docs degrade team velocity and increase onboarding cost.",
                    checks: &[
                        "README or docs updated for new features",
                        "API documentation reflects endpoint changes",
                        "Inline comments for non-obvious logic",
                    ],
                    pass_example: "New API endpoint has corresponding OpenAPI spec update and usage example in docs.",
                    fail_example: "New CLI flag added with no mention in README or --help text.",
                    recent_results: Some(DOCUMENTATION_RESULTS),
                },
                ControlDef {
                    name: "Minimization",
                    slug: "minimization",
                    description: "No extraneous changes",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Evaluate,
                    f1: Some(0.72),
                    pass_at_1: Some(0.68),
                    evaluations: &[P, F, P, F, P, P, F, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Flags extraneous changes that inflate the diff \u{2014} formatting-only edits, unrelated refactors, or drive-by fixes. Keeping PRs focused improves review quality and reduces revert risk.",
                    checks: &[
                        "No unrelated formatting or whitespace changes",
                        "Refactors separated from feature work",
                        "Each commit addresses a single concern",
                    ],
                    pass_example: "PR touches only files directly related to the new caching layer.",
                    fail_example: "PR adds a feature but also reformats 12 unrelated files.",
                    recent_results: None,
                },
            ],
        },
        CategoryDef {
            name: "Readability",
            question: "Can a human or agent quickly read this and understand what it does?",
            controls: &[
                ControlDef {
                    name: "Formatting",
                    slug: "formatting",
                    description: "Code layout matches standard",
                    type_: VerificationType::Automated,
                    mode: VerificationMode::Active,
                    f1: Some(0.99),
                    pass_at_1: Some(0.98),
                    evaluations: &[P, P, P, P, P, P, P, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Validates that code layout conforms to the project's formatting standard (e.g., Prettier, rustfmt). Automated formatting removes subjective style debates from code review.",
                    checks: &[
                        "All files pass the project formatter",
                        "No manual formatting overrides without justification",
                    ],
                    pass_example: "All changed files pass `prettier --check` and `rustfmt --check`.",
                    fail_example: "Several files have inconsistent indentation that the formatter would fix.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Linting",
                    slug: "linting",
                    description: "Linter issues resolved",
                    type_: VerificationType::Automated,
                    mode: VerificationMode::Active,
                    f1: Some(0.98),
                    pass_at_1: Some(0.97),
                    evaluations: &[P, P, P, P, P, P, P, P, F, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Confirms that static analysis findings are resolved. Linter warnings left unaddressed accumulate into tech debt and mask real issues.",
                    checks: &[
                        "No new linter warnings introduced",
                        "Existing warnings not suppressed without explanation",
                        "Lint config not weakened",
                    ],
                    pass_example: "ESLint and Clippy pass with zero warnings on changed files.",
                    fail_example: "New `// eslint-disable-next-line` added to suppress a legitimate warning.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Style",
                    slug: "style",
                    description: "House style applied",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Active,
                    f1: Some(0.81),
                    pass_at_1: Some(0.76),
                    evaluations: &[P, F, P, P, P, P, F, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Evaluates whether the code follows the team's house style conventions beyond what automated formatters catch \u{2014} naming, file organization, import ordering, and idiomatic patterns.",
                    checks: &[
                        "Naming conventions followed (camelCase, snake_case as appropriate)",
                        "Import ordering matches project convention",
                        "Idiomatic patterns used for the language",
                    ],
                    pass_example: "New TypeScript module uses camelCase variables, groups imports by source, and uses `Map` instead of plain objects for lookups.",
                    fail_example: "Mix of camelCase and snake_case in the same module with random import ordering.",
                    recent_results: None,
                },
            ],
        },
        CategoryDef {
            name: "Reliability",
            question: "Will this behave correctly and safely under real-world conditions and failures?",
            controls: &[
                ControlDef {
                    name: "Completeness",
                    slug: "completeness",
                    description: "Implementation covers requirements",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Active,
                    f1: Some(0.76),
                    pass_at_1: Some(0.71),
                    evaluations: &[P, P, F, P, F, P, P, P, F, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Checks that the implementation fully covers the specified requirements. Partial implementations ship broken experiences and create follow-up tickets that could have been avoided.",
                    checks: &[
                        "All acceptance criteria addressed",
                        "Edge cases handled",
                        "Error states implemented",
                    ],
                    pass_example: "Feature handles all three specified user roles with appropriate permissions.",
                    fail_example: "Only the happy path is implemented; error and empty states are missing.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Defects",
                    slug: "defects",
                    description: "Potential or likely bugs remediated",
                    type_: VerificationType::AiAnalysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.84),
                    pass_at_1: Some(0.79),
                    evaluations: &[P, P, P, F, P, P, P, P, P, F],
                    run_status: VerificationResult::Pass,
                    detail_description: "Identifies potential or likely bugs through static analysis and AI review. Catching defects before merge is orders of magnitude cheaper than finding them in production.",
                    checks: &[
                        "No off-by-one errors in loops or slices",
                        "Null/undefined handled at boundaries",
                        "Race conditions considered in async code",
                    ],
                    pass_example: "API handler validates input, handles missing fields gracefully, and returns appropriate HTTP status codes.",
                    fail_example: "Array index accessed without bounds check; crashes on empty input.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Performance",
                    slug: "performance",
                    description: "Hot path impact identified",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Evaluate,
                    f1: Some(0.69),
                    pass_at_1: Some(0.63),
                    evaluations: &[F, P, P, F, P, F, P, P, F, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Assesses whether the change impacts hot paths or introduces algorithmic regressions. Performance problems that ship to production are expensive to diagnose and fix.",
                    checks: &[
                        "No N+1 queries introduced",
                        "Large collections not processed synchronously",
                        "Caching considered for repeated expensive operations",
                    ],
                    pass_example: "Database query uses a JOIN instead of N separate queries for related records.",
                    fail_example: "Loop makes a separate HTTP call for each item in a 1000-element list.",
                    recent_results: None,
                },
            ],
        },
        CategoryDef {
            name: "Code Coverage",
            question: "Do we have trustworthy, automated evidence that it works and won't regress?",
            controls: &[
                ControlDef {
                    name: "Test Coverage",
                    slug: "test-coverage",
                    description: "Production code exercised by unit tests",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.95),
                    pass_at_1: Some(0.93),
                    evaluations: &[P, P, P, P, P, P, F, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Measures whether production code is exercised by automated tests. Coverage gaps mean regressions can ship undetected.",
                    checks: &[
                        "New code has corresponding unit tests",
                        "Coverage does not decrease",
                        "Critical paths have integration tests",
                    ],
                    pass_example: "New service method has 6 unit tests covering happy path, error cases, and edge cases.",
                    fail_example: "New 200-line module has zero test files.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Test Quality",
                    slug: "test-quality",
                    description: "Tests are robust and clear",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Evaluate,
                    f1: Some(0.71),
                    pass_at_1: Some(0.65),
                    evaluations: &[P, F, F, P, P, F, P, F, P, P],
                    run_status: VerificationResult::Fail,
                    detail_description: "Evaluates whether tests are robust, readable, and actually verify behavior rather than implementation details. Low-quality tests give false confidence.",
                    checks: &[
                        "Tests verify behavior, not implementation",
                        "Assertions are specific and meaningful",
                        "Tests are independent and deterministic",
                    ],
                    pass_example: "Tests assert on API response shape and status codes, not on internal method call counts.",
                    fail_example: "Tests mock every dependency and only verify that mocks were called.",
                    recent_results: None,
                },
                ControlDef {
                    name: "E2E Coverage",
                    slug: "e2e-coverage",
                    description: "Browser automation exercises UX",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.91),
                    pass_at_1: Some(0.88),
                    evaluations: &[P, P, P, F, P, P, P, P, P, P],
                    run_status: VerificationResult::Na,
                    detail_description: "Checks that user-facing workflows are exercised by end-to-end browser automation. E2E tests catch integration issues that unit tests miss.",
                    checks: &[
                        "Critical user flows have Playwright/Cypress tests",
                        "E2E tests run in CI",
                        "No flaky E2E tests introduced",
                    ],
                    pass_example: "New checkout flow has a Playwright test that completes a purchase end-to-end.",
                    fail_example: "New multi-step wizard has no browser automation tests.",
                    recent_results: None,
                },
            ],
        },
        CategoryDef {
            name: "Maintainability",
            question: "Will this be easy to modify or extend later without creating new risk?",
            controls: &[
                ControlDef {
                    name: "Architecture",
                    slug: "architecture",
                    description: "Layering and dependency graph meets design",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.88),
                    pass_at_1: Some(0.84),
                    evaluations: &[P, P, P, P, F, P, P, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Validates that layering and dependency directions conform to the project's architectural design. Architectural violations compound over time and make systems harder to evolve.",
                    checks: &[
                        "Dependencies point inward (domain doesn't depend on infra)",
                        "No circular dependencies introduced",
                        "Module boundaries respected",
                    ],
                    pass_example: "New repository implementation depends on domain interfaces, not the other way around.",
                    fail_example: "Domain model imports directly from the HTTP framework package.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Interfaces",
                    slug: "interfaces",
                    description: "",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Disabled,
                    f1: None,
                    pass_at_1: None,
                    evaluations: &[],
                    run_status: VerificationResult::Pass,
                    detail_description: "Reviews public API surfaces for clarity, consistency, and backward compatibility. Interfaces are contracts \u{2014} once published, they're expensive to change.",
                    checks: &[
                        "Public API types are well-defined",
                        "Breaking changes documented",
                        "Consistent naming across endpoints",
                    ],
                    pass_example: "New endpoint follows existing naming and error format conventions.",
                    fail_example: "New endpoint uses different error format than all other endpoints.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Duplication",
                    slug: "duplication",
                    description: "Similar and identical code blocks identified",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.96),
                    pass_at_1: Some(0.94),
                    evaluations: &[P, P, P, P, P, P, P, F, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Detects similar or identical code blocks that could be consolidated. Duplication increases maintenance burden and creates inconsistency risk.",
                    checks: &[
                        "No copy-pasted logic across files",
                        "Shared utilities used for common patterns",
                        "Similar test setup consolidated",
                    ],
                    pass_example: "Date formatting logic extracted into a shared utility used by 4 components.",
                    fail_example: "Same 15-line validation function copy-pasted into three different handlers.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Simplicity",
                    slug: "simplicity",
                    description: "Extra review for reducing complexity",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Active,
                    f1: Some(0.74),
                    pass_at_1: Some(0.69),
                    evaluations: &[P, F, P, P, F, P, P, F, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Flags unnecessarily complex code that could be simplified without changing behavior. Simpler code is easier to review, debug, and extend.",
                    checks: &[
                        "No premature abstractions",
                        "Control flow is straightforward",
                        "Functions are focused and short",
                    ],
                    pass_example: "Conditional logic uses early returns instead of deeply nested if-else chains.",
                    fail_example: "Three-level generic abstraction for a function called in one place.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Dead Code",
                    slug: "dead-code",
                    description: "Unexecuted code and dependencies removed",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.93),
                    pass_at_1: Some(0.90),
                    evaluations: &[P, P, P, P, P, F, P, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Identifies unexecuted code paths and unused dependencies. Dead code misleads readers and bloats bundles.",
                    checks: &[
                        "No unreachable code paths",
                        "Unused imports and variables removed",
                        "Deprecated functions removed if no longer called",
                    ],
                    pass_example: "Old feature flag and its associated code paths removed after rollout completed.",
                    fail_example: "Commented-out function left in file 'in case we need it later'.",
                    recent_results: None,
                },
            ],
        },
        CategoryDef {
            name: "Security",
            question: "Does this preserve or improve our security posture and avoid vulnerabilities?",
            controls: &[
                ControlDef {
                    name: "Vulnerabilities",
                    slug: "vulnerabilities",
                    description: "Security issues are remediated",
                    type_: VerificationType::AiAnalysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.86),
                    pass_at_1: Some(0.81),
                    evaluations: &[P, P, F, P, P, P, P, P, F, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Scans for known security vulnerabilities using both AI analysis and static scanning tools. Shipping known vulnerabilities exposes users and the organization to risk.",
                    checks: &[
                        "No SQL injection or XSS vectors",
                        "User input sanitized at boundaries",
                        "Authentication/authorization checks present",
                    ],
                    pass_example: "User input passed through parameterized queries; HTML output escaped.",
                    fail_example: "Raw SQL string concatenation with user-supplied values.",
                    recent_results: None,
                },
                ControlDef {
                    name: "IaC Scanning",
                    slug: "iac-scanning",
                    description: "",
                    type_: VerificationType::Automated,
                    mode: VerificationMode::Disabled,
                    f1: None,
                    pass_at_1: None,
                    evaluations: &[],
                    run_status: VerificationResult::Pass,
                    detail_description: "Validates infrastructure-as-code definitions against security best practices. Misconfigured infrastructure is a leading cause of data breaches.",
                    checks: &[
                        "No publicly accessible storage buckets",
                        "Encryption at rest enabled",
                        "Least-privilege IAM policies",
                    ],
                    pass_example: "Terraform module creates S3 bucket with encryption, versioning, and private ACL.",
                    fail_example: "CloudFormation template creates an RDS instance with no encryption and public accessibility.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Dependency Alerts",
                    slug: "dependency-alerts",
                    description: "Known CVEs are patched",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.97),
                    pass_at_1: Some(0.95),
                    evaluations: &[P, P, P, P, P, P, P, P, P, F],
                    run_status: VerificationResult::Pass,
                    detail_description: "Checks that third-party dependencies are free from known CVEs. Vulnerable dependencies are an easy attack vector that automated tools can detect.",
                    checks: &[
                        "No dependencies with known critical CVEs",
                        "Lock file updated to patched versions",
                        "Unused dependencies removed",
                    ],
                    pass_example: "Dependabot alert resolved by updating lodash from 4.17.20 to 4.17.21.",
                    fail_example: "Package.json pins a version of axios with a known SSRF vulnerability.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Security Controls",
                    slug: "security-controls",
                    description: "Organization standards applied",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Active,
                    f1: Some(0.80),
                    pass_at_1: Some(0.75),
                    evaluations: &[P, P, F, P, P, F, P, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Verifies that organization-specific security standards are applied \u{2014} rate limiting, audit logging, CORS policies, and secret management.",
                    checks: &[
                        "Secrets not hardcoded in source",
                        "Rate limiting on public endpoints",
                        "Audit logging for sensitive operations",
                    ],
                    pass_example: "API key loaded from environment variable; rate limiter configured on login endpoint.",
                    fail_example: "AWS credentials committed in a config file.",
                    recent_results: None,
                },
            ],
        },
        CategoryDef {
            name: "Deployability",
            question: "Is this changeset safe to ship to production immediately?",
            controls: &[
                ControlDef {
                    name: "Compatibility",
                    slug: "compatibility",
                    description: "Breaking changes are avoided",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.89),
                    pass_at_1: Some(0.85),
                    evaluations: &[P, P, P, P, F, P, P, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Detects breaking changes in APIs, database schemas, or wire formats that could disrupt consumers. Breaking changes require coordination that surprises prevent.",
                    checks: &[
                        "No removed or renamed public API fields",
                        "Database migrations are backward-compatible",
                        "Wire format changes are additive",
                    ],
                    pass_example: "New field added to API response; no existing fields removed or renamed.",
                    fail_example: "Column renamed in migration while old code is still deployed.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Rollout / Rollback",
                    slug: "rollout-rollback",
                    description: "Known rollback plan if deploy fails",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Evaluate,
                    f1: Some(0.66),
                    pass_at_1: Some(0.60),
                    evaluations: &[F, P, F, P, F, P, P, F, P, F],
                    run_status: VerificationResult::Fail,
                    detail_description: "Confirms that the change has a clear deployment plan and can be safely rolled back if issues arise. Every production deploy should be reversible.",
                    checks: &[
                        "Feature flag available for gradual rollout",
                        "Database migration is reversible",
                        "Rollback procedure documented",
                    ],
                    pass_example: "Feature behind a LaunchDarkly flag with 10% initial rollout and documented rollback steps.",
                    fail_example: "Irreversible database migration with no rollback plan.",
                    recent_results: Some(ROLLOUT_ROLLBACK_RESULTS),
                },
                ControlDef {
                    name: "Observability",
                    slug: "observability",
                    description: "Logging, metrics, tracing instrumented",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Evaluate,
                    f1: Some(0.73),
                    pass_at_1: Some(0.67),
                    evaluations: &[P, F, P, F, P, P, F, P, F, P],
                    run_status: VerificationResult::Fail,
                    detail_description: "Ensures that logging, metrics, and tracing are instrumented for new code paths. Without observability, production issues are invisible until users report them.",
                    checks: &[
                        "Structured logging for new operations",
                        "Metrics emitted for key business events",
                        "Distributed tracing propagated",
                    ],
                    pass_example: "New payment endpoint logs transaction IDs, emits latency metrics, and propagates trace context.",
                    fail_example: "New background job has no logging or metrics; failures are silent.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Cost",
                    slug: "cost",
                    description: "Tech ops costs estimated",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Evaluate,
                    f1: Some(0.78),
                    pass_at_1: Some(0.72),
                    evaluations: &[P, P, F, P, F, P, P, F, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Estimates the infrastructure and operational cost impact of the change. Unchecked cost growth erodes margins and can cause budget surprises.",
                    checks: &[
                        "New infrastructure resources sized appropriately",
                        "No unbounded resource consumption",
                        "Cost estimate provided for significant changes",
                    ],
                    pass_example: "New Lambda function has memory limit set and estimated monthly cost noted in PR.",
                    fail_example: "New service provisions a db.r5.4xlarge for a table with 100 rows.",
                    recent_results: None,
                },
            ],
        },
        CategoryDef {
            name: "Compliance",
            question: "Does this meet our regulatory, contractual, and policy obligations?",
            controls: &[
                ControlDef {
                    name: "Change Control",
                    slug: "change-control",
                    description: "Separation of Duties policy met",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.94),
                    pass_at_1: Some(0.91),
                    evaluations: &[P, P, P, P, P, P, P, P, F, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Validates that separation-of-duties policies are met \u{2014} the author is not the sole reviewer, approvals are obtained, and the change went through the proper process.",
                    checks: &[
                        "PR has at least one approval from non-author",
                        "Required reviewers have signed off",
                        "No self-merging without policy exception",
                    ],
                    pass_example: "PR approved by two team members before merge; CI checks all green.",
                    fail_example: "Author approved and merged their own PR with no other reviewers.",
                    recent_results: None,
                },
                ControlDef {
                    name: "AI Governance",
                    slug: "ai-governance",
                    description: "AI involvement was acceptable",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.85),
                    pass_at_1: Some(0.80),
                    evaluations: &[P, P, P, F, P, P, P, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Checks that AI-generated or AI-assisted code meets the organization's governance requirements \u{2014} attribution, review depth, and acceptable use.",
                    checks: &[
                        "AI-generated code clearly attributed",
                        "Human review of AI suggestions documented",
                        "AI usage within acceptable-use policy",
                    ],
                    pass_example: "PR notes that implementation was AI-assisted; human reviewer verified logic and tests.",
                    fail_example: "Entire module generated by AI with no human review or attribution.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Privacy",
                    slug: "privacy",
                    description: "PII is identified and handled to standards",
                    type_: VerificationType::Ai,
                    mode: VerificationMode::Active,
                    f1: Some(0.77),
                    pass_at_1: Some(0.72),
                    evaluations: &[P, F, P, P, P, F, P, P, P, F],
                    run_status: VerificationResult::Pass,
                    detail_description: "Ensures that personally identifiable information (PII) is identified, classified, and handled according to privacy standards (GDPR, CCPA).",
                    checks: &[
                        "PII fields identified and documented",
                        "Data retention policies applied",
                        "Consent mechanisms in place for data collection",
                    ],
                    pass_example: "New user profile endpoint masks email in logs and respects data deletion requests.",
                    fail_example: "User email addresses logged in plaintext to application logs.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Accessibility",
                    slug: "accessibility",
                    description: "Software meets accessibility requirements",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.90),
                    pass_at_1: Some(0.87),
                    evaluations: &[P, P, P, P, P, F, P, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Verifies that UI changes meet accessibility requirements (WCAG 2.1 AA). Inaccessible software excludes users and creates legal risk.",
                    checks: &[
                        "Semantic HTML elements used",
                        "ARIA labels present on interactive elements",
                        "Color contrast meets WCAG AA standards",
                    ],
                    pass_example: "New modal uses <dialog>, has aria-labelledby, and focus is trapped within.",
                    fail_example: "Custom dropdown built with <div> elements, no keyboard navigation, no ARIA roles.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Licensing",
                    slug: "licensing",
                    description: "Supply chain meets IP policy",
                    type_: VerificationType::Analysis,
                    mode: VerificationMode::Active,
                    f1: Some(0.96),
                    pass_at_1: Some(0.93),
                    evaluations: &[P, P, P, P, P, P, P, P, P, P],
                    run_status: VerificationResult::Pass,
                    detail_description: "Ensures that all third-party dependencies comply with the organization's intellectual property policy. License violations can have severe legal consequences.",
                    checks: &[
                        "No GPL-licensed dependencies in proprietary code",
                        "License file present for new dependencies",
                        "Supply chain attestation where required",
                    ],
                    pass_example: "New dependency uses MIT license; added to approved dependency list.",
                    fail_example: "GPL-licensed library added to a closed-source commercial product.",
                    recent_results: None,
                },
            ],
        },
    ];

    // ── Helpers ──────────────────────────────────────────────────────────

    fn recent_results_from_def(defs: &[RecentResultDef]) -> Vec<RecentControlResult> {
        defs.iter()
            .map(|r| RecentControlResult {
                run: RunReference {
                    id: r.run_id.into(),
                    title: r.run_title.into(),
                },
                workflow: WorkflowReference {
                    slug: r.workflow.into(),
                },
                result: r.result,
                timestamp: ts(r.timestamp),
            })
            .collect()
    }

    fn category_run_status(cat: &CategoryDef) -> VerificationResult {
        if cat
            .controls
            .iter()
            .any(|c| c.run_status == VerificationResult::Fail)
        {
            VerificationResult::Fail
        } else if cat
            .controls
            .iter()
            .all(|c| c.run_status == VerificationResult::Na)
        {
            VerificationResult::Na
        } else {
            VerificationResult::Pass
        }
    }

    fn slugify(name: &str) -> String {
        name.to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }

    // ── Public API ──────────────────────────────────────────────────────

    pub(super) fn criteria() -> Vec<VerificationCriterion> {
        ALL_CATEGORIES
            .iter()
            .map(|cat| VerificationCriterion {
                name: cat.name.into(),
                question: cat.question.into(),
                controls: cat
                    .controls
                    .iter()
                    .map(|c| VerificationControl {
                        name: c.name.into(),
                        slug: c.slug.into(),
                        description: c.description.into(),
                        type_: c.type_,
                        mode: Some(c.mode),
                        f1: c.f1,
                        pass_at_1: c.pass_at_1,
                        evaluations: c.evaluations.to_vec(),
                    })
                    .collect(),
            })
            .collect()
    }

    pub(super) fn criterion_detail(id: &str) -> Option<VerificationCriterionDetail> {
        ALL_CATEGORIES
            .iter()
            .find(|cat| slugify(cat.name) == id)
            .map(|cat| VerificationCriterionDetail {
                name: cat.name.into(),
                question: cat.question.into(),
                controls: cat
                    .controls
                    .iter()
                    .map(|c| VerificationControl {
                        name: c.name.into(),
                        slug: c.slug.into(),
                        description: c.description.into(),
                        type_: c.type_,
                        mode: Some(c.mode),
                        f1: c.f1,
                        pass_at_1: c.pass_at_1,
                        evaluations: c.evaluations.to_vec(),
                    })
                    .collect(),
            })
    }

    pub(super) fn controls() -> Vec<VerificationControlListItem> {
        ALL_CATEGORIES
            .iter()
            .flat_map(|cat| {
                cat.controls
                    .iter()
                    .map(move |c| VerificationControlListItem {
                        name: c.name.into(),
                        slug: c.slug.into(),
                        description: c.description.into(),
                        type_: c.type_,
                        mode: Some(c.mode),
                        f1: c.f1,
                        pass_at_1: c.pass_at_1,
                        criterion: CriterionReference {
                            name: cat.name.into(),
                        },
                    })
            })
            .collect()
    }

    pub(super) fn control_detail(slug: &str) -> Option<VerificationDetailResponse> {
        for cat in ALL_CATEGORIES {
            for (idx, ctrl) in cat.controls.iter().enumerate() {
                if ctrl.slug == slug {
                    let siblings: Vec<SiblingControl> = cat
                        .controls
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != idx)
                        .map(|(_, s)| SiblingControl {
                            name: s.name.into(),
                            slug: s.slug.into(),
                            type_: Some(s.type_),
                            mode: Some(s.mode),
                        })
                        .collect();

                    let results = match ctrl.recent_results {
                        Some(r) => recent_results_from_def(r),
                        None => recent_results_from_def(DEFAULT_RECENT_RESULTS),
                    };

                    return Some(VerificationDetailResponse {
                        control: ControlInfo {
                            name: ctrl.name.into(),
                            slug: ctrl.slug.into(),
                            description: ctrl.description.into(),
                            type_: Some(ctrl.type_),
                            criterion: CriterionReference {
                                name: cat.name.into(),
                            },
                        },
                        performance: ControlPerformance {
                            mode: ctrl.mode,
                            f1: ctrl.f1,
                            pass_at_1: ctrl.pass_at_1,
                            evaluations: ctrl.evaluations.to_vec(),
                        },
                        control_detail: ControlDetail {
                            rationale: ctrl.detail_description.into(),
                            checks: ctrl.checks.iter().map(|s| (*s).into()).collect(),
                            pass_example: ctrl.pass_example.into(),
                            fail_example: ctrl.fail_example.into(),
                        },
                        recent_results: results,
                        siblings,
                    });
                }
            }
        }
        None
    }

    pub(super) fn run_verifications() -> Vec<RunVerification> {
        ALL_CATEGORIES
            .iter()
            .map(|cat| RunVerification {
                name: cat.name.into(),
                question: cat.question.into(),
                status: category_run_status(cat),
                controls: cat
                    .controls
                    .iter()
                    .map(|c| RunVerificationControl {
                        name: c.name.into(),
                        slug: c.slug.into(),
                        description: c.description.into(),
                        type_: c.type_,
                        status: c.run_status,
                    })
                    .collect(),
            })
            .collect()
    }
}

mod signoffs {
    use super::ts;
    use fabro_api_types::*;

    struct SignoffDef {
        id: &'static str,
        control_slug: &'static str,
        repo: &'static str,
        commit_sha: &'static str,
        status: SignoffStatus,
        url: Option<&'static str>,
        description: Option<&'static str>,
        source: Option<&'static str>,
        created_at: &'static str,
    }

    const ALL_SIGNOFFS: &[SignoffDef] = &[
        SignoffDef {
            id: "01JQVKX0001SIGNOFF00001",
            control_slug: "motivation",
            repo: "api-server",
            commit_sha: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
            status: SignoffStatus::Pass,
            url: Some("https://github.com/acme/api-server/actions/runs/12345"),
            description: Some("PR links to JIRA-1234 with clear motivation"),
            source: Some("github-actions"),
            created_at: "2025-09-15T12:00:00Z",
        },
        SignoffDef {
            id: "01JQVKX0001SIGNOFF00002",
            control_slug: "test-coverage",
            repo: "api-server",
            commit_sha: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
            status: SignoffStatus::Pass,
            url: Some("https://github.com/acme/api-server/actions/runs/12346"),
            description: Some("Coverage increased from 78% to 82%"),
            source: Some("github-actions"),
            created_at: "2025-09-15T12:01:00Z",
        },
        SignoffDef {
            id: "01JQVKX0001SIGNOFF00003",
            control_slug: "motivation",
            repo: "web-dashboard",
            commit_sha: "b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3",
            status: SignoffStatus::Fail,
            url: None,
            description: Some("PR description is empty"),
            source: Some("fabro"),
            created_at: "2025-09-14T16:30:00Z",
        },
        SignoffDef {
            id: "01JQVKX0001SIGNOFF00004",
            control_slug: "security-controls",
            repo: "api-server",
            commit_sha: "c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4",
            status: SignoffStatus::Pending,
            url: Some("https://acme.atlassian.net/browse/SEC-42"),
            description: Some("Awaiting security team review"),
            source: Some("jira"),
            created_at: "2025-09-15T09:00:00Z",
        },
        SignoffDef {
            id: "01JQVKX0001SIGNOFF00005",
            control_slug: "test-coverage",
            repo: "web-dashboard",
            commit_sha: "b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3",
            status: SignoffStatus::Pass,
            url: Some("https://github.com/acme/web-dashboard/actions/runs/67890"),
            description: Some("All tests passing, 91% coverage"),
            source: Some("github-actions"),
            created_at: "2025-09-14T17:00:00Z",
        },
    ];

    fn to_signoff(def: &SignoffDef) -> Signoff {
        Signoff {
            id: def.id.into(),
            control: ControlReference {
                slug: def.control_slug.into(),
            },
            repository: RepositoryReference {
                name: def.repo.into(),
            },
            commit_sha: def.commit_sha.into(),
            status: def.status,
            url: def.url.map(Into::into),
            description: def.description.map(Into::into),
            source: def.source.map(Into::into),
            created_at: ts(def.created_at),
        }
    }

    pub(super) fn list_items(
        control: Option<&str>,
        repository: Option<&str>,
        commit_sha: Option<&str>,
    ) -> Vec<Signoff> {
        ALL_SIGNOFFS
            .iter()
            .filter(|s| control.is_none_or(|c| s.control_slug == c))
            .filter(|s| repository.is_none_or(|r| s.repo == r))
            .filter(|s| commit_sha.is_none_or(|sha| s.commit_sha == sha))
            .map(to_signoff)
            .collect()
    }

    pub(super) fn detail(id: &str) -> Option<Signoff> {
        ALL_SIGNOFFS.iter().find(|s| s.id == id).map(to_signoff)
    }

    pub(super) fn stub_created() -> Signoff {
        to_signoff(&ALL_SIGNOFFS[0])
    }
}

mod retros {
    use super::ts;
    use fabro_api_types::*;

    #[allow(clippy::too_many_arguments)]
    fn stage(
        id: &str,
        label: &str,
        status: &str,
        duration_ms: i64,
        retries: i64,
        cost: Option<f64>,
        notes: Option<&str>,
        failure_reason: Option<&str>,
        files_touched: Vec<&str>,
    ) -> StageRetro {
        StageRetro {
            stage_id: id.into(),
            stage_label: label.into(),
            status: status.into(),
            duration_ms,
            retries,
            cost,
            notes: notes.map(Into::into),
            failure_reason: failure_reason.map(Into::into),
            files_touched: files_touched.into_iter().map(Into::into).collect(),
        }
    }

    pub(super) fn detail(run_id: &str) -> Option<RetroDetail> {
        match run_id {
            "run-1" => Some(RetroDetail {
                run_id: "run-1".into(),
                workflow_name: "implement".into(),
                goal: "Add rate limiting to auth endpoints".into(),
                timestamp: ts("2026-02-28T14:32:00Z"),
                smoothness: Some(SmoothnessRating::Smooth),
                intent: Some("Implement token-bucket rate limiting on /auth/login and /auth/register to prevent brute-force attacks.".into()),
                outcome: Some("Rate limiter deployed with configurable per-IP limits. Integration tests added. Redis-backed counter with sliding window.".into()),
                stages: vec![
                    stage("detect-drift", "Detect Drift", "completed", 72000, 0, Some(0.48), None, None, vec!["src/middleware/rate-limit.ts"]),
                    stage("propose-changes", "Propose Changes", "completed", 154000, 0, Some(1.12), None, None, vec!["src/middleware/rate-limit.ts", "src/routes/auth.ts", "src/config.ts"]),
                    stage("review-changes", "Review Changes", "completed", 45000, 0, Some(0.31), None, None, vec![]),
                    stage("apply-changes", "Apply Changes", "completed", 118000, 0, Some(0.87), None, None, vec!["src/middleware/rate-limit.ts", "src/routes/auth.ts", "src/config.ts", "tests/rate-limit.test.ts"]),
                ],
                stats: RetroStats {
                    total_duration_ms: 389000,
                    total_cost: Some(2.78),
                    total_retries: 0,
                    files_touched: vec!["src/middleware/rate-limit.ts".into(), "src/routes/auth.ts".into(), "src/config.ts".into(), "tests/rate-limit.test.ts".into()],
                    stages_completed: 4,
                    stages_failed: 0,
                },
                learnings: vec![
                    Learning { category: LearningCategory::Repo, text: "Redis client is initialized lazily in src/infra/redis.ts -- reuse existing connection pool.".into() },
                    Learning { category: LearningCategory::Code, text: "Auth middleware chain order matters: rate-limit must run before JWT validation.".into() },
                ],
                friction_points: vec![],
                open_items: vec![
                    OpenItem { kind: OpenItemKind::FollowUp, description: "Add rate-limit headers (X-RateLimit-Remaining) to response.".into() },
                ],
            }),
            "run-2" => Some(RetroDetail {
                run_id: "run-2".into(),
                workflow_name: "implement".into(),
                goal: "Migrate to React Router v7".into(),
                timestamp: ts("2026-02-28T10:15:00Z"),
                smoothness: Some(SmoothnessRating::Bumpy),
                intent: Some("Upgrade react-router from v6 to v7, updating all route definitions and loader/action patterns to the new API.".into()),
                outcome: Some("Migration completed but required 3 retries in the apply stage due to breaking changes in nested route handling. All routes now use the v7 data API.".into()),
                stages: vec![
                    stage("detect-drift", "Detect Drift", "completed", 95000, 0, Some(0.62), None, None, vec!["package.json"]),
                    stage("propose-changes", "Propose Changes", "completed", 312000, 1, Some(2.45), Some("First proposal missed nested outlet patterns. Retry produced correct migration."), None, vec!["src/routes.ts", "src/app.tsx", "src/routes/dashboard.tsx", "src/routes/settings.tsx"]),
                    stage("review-changes", "Review Changes", "completed", 88000, 0, Some(0.54), None, None, vec![]),
                    stage("apply-changes", "Apply Changes", "completed", 480000, 3, Some(3.21), Some("Type errors in nested layouts required multiple correction passes."), None, vec!["src/routes.ts", "src/app.tsx", "src/routes/dashboard.tsx", "src/routes/settings.tsx", "src/routes/profile.tsx", "tests/routes.test.tsx"]),
                ],
                stats: RetroStats {
                    total_duration_ms: 975000,
                    total_cost: Some(6.82),
                    total_retries: 4,
                    files_touched: vec!["package.json".into(), "src/routes.ts".into(), "src/app.tsx".into(), "src/routes/dashboard.tsx".into(), "src/routes/settings.tsx".into(), "src/routes/profile.tsx".into(), "tests/routes.test.tsx".into()],
                    stages_completed: 4,
                    stages_failed: 0,
                },
                learnings: vec![
                    Learning { category: LearningCategory::Workflow, text: "Framework migration tasks benefit from running type-check after each stage, not just at the end.".into() },
                    Learning { category: LearningCategory::Code, text: "React Router v7 outlets require explicit type annotations for loader data in nested routes.".into() },
                    Learning { category: LearningCategory::Tool, text: "The codemod tool missed JSX spread patterns -- manual fixup was needed.".into() },
                ],
                friction_points: vec![
                    FrictionPoint { kind: FrictionKind::Retry, description: "Nested route outlet types were incorrect on first 3 attempts.".into(), stage_id: Some("apply-changes".into()) },
                    FrictionPoint { kind: FrictionKind::WrongApproach, description: "Initially tried to keep v6 compat layer, which created more issues than a clean migration.".into(), stage_id: Some("propose-changes".into()) },
                ],
                open_items: vec![
                    OpenItem { kind: OpenItemKind::TechDebt, description: "Leftover v6 compat shims in src/utils/router-compat.ts should be deleted.".into() },
                    OpenItem { kind: OpenItemKind::TestGap, description: "No E2E coverage for the new nested layout error boundaries.".into() },
                ],
            }),
            "run-6" => Some(RetroDetail {
                run_id: "run-6".into(),
                workflow_name: "implement".into(),
                goal: "Add dark mode toggle".into(),
                timestamp: ts("2026-02-27T16:45:00Z"),
                smoothness: Some(SmoothnessRating::Effortless),
                intent: Some("Add a theme toggle component to the dashboard header with system/light/dark options, persisting preference to localStorage.".into()),
                outcome: Some("Dark mode toggle shipped with smooth CSS transitions. All existing components already used CSS variables, so no style refactoring was needed.".into()),
                stages: vec![
                    stage("detect-drift", "Detect Drift", "completed", 42000, 0, Some(0.28), None, None, vec![]),
                    stage("propose-changes", "Propose Changes", "completed", 98000, 0, Some(0.71), None, None, vec!["src/components/ThemeToggle.tsx", "src/hooks/useTheme.ts"]),
                    stage("apply-changes", "Apply Changes", "completed", 76000, 0, Some(0.52), None, None, vec!["src/components/ThemeToggle.tsx", "src/hooks/useTheme.ts", "src/layouts/Header.tsx"]),
                ],
                stats: RetroStats {
                    total_duration_ms: 216000,
                    total_cost: Some(1.51),
                    total_retries: 0,
                    files_touched: vec!["src/components/ThemeToggle.tsx".into(), "src/hooks/useTheme.ts".into(), "src/layouts/Header.tsx".into()],
                    stages_completed: 3,
                    stages_failed: 0,
                },
                learnings: vec![
                    Learning { category: LearningCategory::Repo, text: "CSS variables are defined in src/styles/tokens.css and already support dark values.".into() },
                ],
                friction_points: vec![],
                open_items: vec![],
            }),
            "run-3" => Some(RetroDetail {
                run_id: "run-3".into(),
                workflow_name: "fix_build".into(),
                goal: "Fix config parsing for nested values".into(),
                timestamp: ts("2026-02-27T09:20:00Z"),
                smoothness: Some(SmoothnessRating::Struggled),
                intent: Some("Fix TOML config parser to handle deeply nested table arrays, which was causing silent data loss on certain pipeline configs.".into()),
                outcome: Some("Root cause identified as incorrect recursion depth limit in the TOML walker. Fix applied but exposed a second bug in default value merging that required additional changes.".into()),
                stages: vec![
                    stage("investigate", "Investigate", "completed", 340000, 2, Some(1.85), Some("First investigation looked at wrong parser path. Second attempt found the actual recursion limit."), None, vec!["src/config/parser.ts", "src/config/defaults.ts"]),
                    stage("propose-fix", "Propose Fix", "completed", 210000, 1, Some(1.42), None, None, vec!["src/config/parser.ts", "src/config/defaults.ts", "src/config/merge.ts"]),
                    stage("apply-fix", "Apply Fix", "completed", 185000, 1, Some(1.15), None, Some("Initial fix broke the default value merging path. Required a second pass."), vec!["src/config/parser.ts", "src/config/defaults.ts", "src/config/merge.ts", "tests/config-parser.test.ts"]),
                    stage("verify", "Verify", "completed", 95000, 0, Some(0.55), None, None, vec![]),
                ],
                stats: RetroStats {
                    total_duration_ms: 830000,
                    total_cost: Some(4.97),
                    total_retries: 4,
                    files_touched: vec!["src/config/parser.ts".into(), "src/config/defaults.ts".into(), "src/config/merge.ts".into(), "tests/config-parser.test.ts".into()],
                    stages_completed: 4,
                    stages_failed: 0,
                },
                learnings: vec![
                    Learning { category: LearningCategory::Code, text: "TOML walker in parser.ts has a hardcoded depth limit of 8 -- needs to be configurable.".into() },
                    Learning { category: LearningCategory::Code, text: "Default merging in merge.ts uses shallow spread, which silently drops nested keys.".into() },
                    Learning { category: LearningCategory::Workflow, text: "Bug fix pipelines should include a regression test stage before verification.".into() },
                ],
                friction_points: vec![
                    FrictionPoint { kind: FrictionKind::WrongApproach, description: "Initial investigation focused on the YAML compatibility layer instead of the TOML parser.".into(), stage_id: Some("investigate".into()) },
                    FrictionPoint { kind: FrictionKind::Retry, description: "Fix introduced a regression in default value merging that required rework.".into(), stage_id: Some("apply-fix".into()) },
                    FrictionPoint { kind: FrictionKind::Ambiguity, description: "Config schema docs were outdated, making it unclear which nesting depth was intended.".into(), stage_id: None },
                ],
                open_items: vec![
                    OpenItem { kind: OpenItemKind::TechDebt, description: "Remove the hardcoded depth limit in src/config/parser.ts and make it configurable.".into() },
                    OpenItem { kind: OpenItemKind::Investigation, description: "Audit other parsers for similar shallow-spread bugs in merging logic.".into() },
                    OpenItem { kind: OpenItemKind::TestGap, description: "No tests for configs nested deeper than 4 levels.".into() },
                ],
            }),
            "run-8" => Some(RetroDetail {
                run_id: "run-8".into(),
                workflow_name: "implement".into(),
                goal: "Implement webhook retry logic".into(),
                timestamp: ts("2026-02-26T11:00:00Z"),
                smoothness: Some(SmoothnessRating::Smooth),
                intent: Some("Add exponential backoff retry logic for failed webhook deliveries with configurable max attempts and dead-letter queue.".into()),
                outcome: Some("Webhook retry system implemented with exponential backoff (base 2s, max 5 retries). Failed deliveries route to SQS dead-letter queue. Dashboard shows retry status.".into()),
                stages: vec![
                    stage("detect-drift", "Detect Drift", "completed", 55000, 0, Some(0.35), None, None, vec![]),
                    stage("propose-changes", "Propose Changes", "completed", 178000, 0, Some(1.28), None, None, vec!["src/webhooks/retry.ts", "src/webhooks/dlq.ts", "src/webhooks/dispatcher.ts"]),
                    stage("review-changes", "Review Changes", "completed", 62000, 0, Some(0.41), None, None, vec![]),
                    stage("apply-changes", "Apply Changes", "completed", 145000, 1, Some(1.05), Some("Minor type fix needed on retry delay calculation."), None, vec!["src/webhooks/retry.ts", "src/webhooks/dlq.ts", "src/webhooks/dispatcher.ts", "tests/webhook-retry.test.ts"]),
                ],
                stats: RetroStats {
                    total_duration_ms: 440000,
                    total_cost: Some(3.09),
                    total_retries: 1,
                    files_touched: vec!["src/webhooks/retry.ts".into(), "src/webhooks/dlq.ts".into(), "src/webhooks/dispatcher.ts".into(), "tests/webhook-retry.test.ts".into()],
                    stages_completed: 4,
                    stages_failed: 0,
                },
                learnings: vec![
                    Learning { category: LearningCategory::Repo, text: "SQS client wrapper is in src/infra/sqs.ts with pre-configured DLQ ARNs per environment.".into() },
                    Learning { category: LearningCategory::Code, text: "Webhook dispatcher already had a hook point for retry logic via the onFailure callback.".into() },
                ],
                friction_points: vec![
                    FrictionPoint { kind: FrictionKind::Retry, description: "Retry delay formula had an off-by-one in the exponent calculation.".into(), stage_id: Some("apply-changes".into()) },
                ],
                open_items: vec![
                    OpenItem { kind: OpenItemKind::FollowUp, description: "Add webhook retry metrics to the Grafana dashboard.".into() },
                    OpenItem { kind: OpenItemKind::FollowUp, description: "Document the DLQ reprocessing procedure in the runbook.".into() },
                ],
            }),
            _ => None,
        }
    }

    pub(super) fn list_items() -> Vec<RetroListItem> {
        vec![
            RetroListItem {
                run: RunReference {
                    id: "run-1".into(),
                    title: "Add rate limiting to auth endpoints".into(),
                },
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                timestamp: ts("2026-02-28T14:32:00Z"),
                smoothness: Some(SmoothnessRating::Smooth),
                stats: RetroStats {
                    total_duration_ms: 389000,
                    total_cost: Some(2.78),
                    total_retries: 0,
                    files_touched: vec![
                        "src/middleware/rate-limit.ts".into(),
                        "src/routes/auth.ts".into(),
                        "src/config.ts".into(),
                        "tests/rate-limit.test.ts".into(),
                    ],
                    stages_completed: 4,
                    stages_failed: 0,
                },
                friction_point_count: 0,
            },
            RetroListItem {
                run: RunReference {
                    id: "run-2".into(),
                    title: "Migrate to React Router v7".into(),
                },
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                timestamp: ts("2026-02-28T10:15:00Z"),
                smoothness: Some(SmoothnessRating::Bumpy),
                stats: RetroStats {
                    total_duration_ms: 975000,
                    total_cost: Some(6.82),
                    total_retries: 4,
                    files_touched: vec![
                        "package.json".into(),
                        "src/routes.ts".into(),
                        "src/app.tsx".into(),
                        "src/routes/dashboard.tsx".into(),
                        "src/routes/settings.tsx".into(),
                        "src/routes/profile.tsx".into(),
                        "tests/routes.test.tsx".into(),
                    ],
                    stages_completed: 4,
                    stages_failed: 0,
                },
                friction_point_count: 2,
            },
            RetroListItem {
                run: RunReference {
                    id: "run-6".into(),
                    title: "Add dark mode toggle".into(),
                },
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                timestamp: ts("2026-02-27T16:45:00Z"),
                smoothness: Some(SmoothnessRating::Effortless),
                stats: RetroStats {
                    total_duration_ms: 216000,
                    total_cost: Some(1.51),
                    total_retries: 0,
                    files_touched: vec![
                        "src/components/ThemeToggle.tsx".into(),
                        "src/hooks/useTheme.ts".into(),
                        "src/layouts/Header.tsx".into(),
                    ],
                    stages_completed: 3,
                    stages_failed: 0,
                },
                friction_point_count: 0,
            },
            RetroListItem {
                run: RunReference {
                    id: "run-3".into(),
                    title: "Fix config parsing for nested values".into(),
                },
                workflow: WorkflowReference {
                    slug: "fix_build".into(),
                },
                timestamp: ts("2026-02-27T09:20:00Z"),
                smoothness: Some(SmoothnessRating::Struggled),
                stats: RetroStats {
                    total_duration_ms: 830000,
                    total_cost: Some(4.97),
                    total_retries: 4,
                    files_touched: vec![
                        "src/config/parser.ts".into(),
                        "src/config/defaults.ts".into(),
                        "src/config/merge.ts".into(),
                        "tests/config-parser.test.ts".into(),
                    ],
                    stages_completed: 4,
                    stages_failed: 0,
                },
                friction_point_count: 3,
            },
            RetroListItem {
                run: RunReference {
                    id: "run-8".into(),
                    title: "Implement webhook retry logic".into(),
                },
                workflow: WorkflowReference {
                    slug: "implement".into(),
                },
                timestamp: ts("2026-02-26T11:00:00Z"),
                smoothness: Some(SmoothnessRating::Smooth),
                stats: RetroStats {
                    total_duration_ms: 440000,
                    total_cost: Some(3.09),
                    total_retries: 1,
                    files_touched: vec![
                        "src/webhooks/retry.ts".into(),
                        "src/webhooks/dlq.ts".into(),
                        "src/webhooks/dispatcher.ts".into(),
                        "tests/webhook-retry.test.ts".into(),
                    ],
                    stages_completed: 4,
                    stages_failed: 0,
                },
                friction_point_count: 1,
            },
        ]
    }
}

mod sessions {
    use super::ts;
    use fabro_api_types::*;
    use uuid::Uuid;

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    const S1: u128 = 0x10000000_0000_4000_8000_000000000001;
    const S2: u128 = 0x10000000_0000_4000_8000_000000000002;
    const S3: u128 = 0x10000000_0000_4000_8000_000000000003;
    const S4: u128 = 0x10000000_0000_4000_8000_000000000004;
    const S5: u128 = 0x10000000_0000_4000_8000_000000000005;
    const S6: u128 = 0x10000000_0000_4000_8000_000000000006;
    const S7: u128 = 0x10000000_0000_4000_8000_000000000007;
    const S8: u128 = 0x10000000_0000_4000_8000_000000000008;

    pub(super) fn list_items() -> Vec<SessionListItem> {
        vec![
            SessionListItem {
                id: uid(S1),
                title: "Add rate limiting to auth endpoints".into(),
                model: ModelReference { id: "Opus 4.6".into() },
                last_message_preview: "Done. I've created the rate limiter and wired it up...".into(),
                created_at: ts("2026-03-06T14:30:00Z"),
                updated_at: ts("2026-03-06T15:45:00Z"),
            },
            SessionListItem {
                id: uid(S2),
                title: "Fix config parsing for nested values".into(),
                model: ModelReference { id: "Sonnet 4.6".into() },
                last_message_preview: "Fixed. The parser now tracks the current section header...".into(),
                created_at: ts("2026-03-06T12:30:00Z"),
                updated_at: ts("2026-03-06T13:15:00Z"),
            },
            SessionListItem {
                id: uid(S3),
                title: "Migrate to React Router v7".into(),
                model: ModelReference { id: "Opus 4.6".into() },
                last_message_preview: "You're on React Router 6.22. The migration to v7 involves...".into(),
                created_at: ts("2026-03-05T10:00:00Z"),
                updated_at: ts("2026-03-05T11:30:00Z"),
            },
            SessionListItem {
                id: uid(S4),
                title: "Add dark mode toggle".into(),
                model: ModelReference { id: "Sonnet 4.6".into() },
                last_message_preview: "Added a dark mode toggle to the settings panel with system preference detection.".into(),
                created_at: ts("2026-03-05T09:00:00Z"),
                updated_at: ts("2026-03-05T09:45:00Z"),
            },
            SessionListItem {
                id: uid(S5),
                title: "Update OpenAPI spec for v3".into(),
                model: ModelReference { id: "Opus 4.6".into() },
                last_message_preview: "Updated all endpoint schemas to v3 format with discriminated unions.".into(),
                created_at: ts("2026-03-05T08:00:00Z"),
                updated_at: ts("2026-03-05T08:30:00Z"),
            },
            SessionListItem {
                id: uid(S6),
                title: "Terraform module for Redis cluster".into(),
                model: ModelReference { id: "Opus 4.6".into() },
                last_message_preview: "Created the module with 3-node cluster, automatic failover, and encryption at rest.".into(),
                created_at: ts("2026-03-03T15:00:00Z"),
                updated_at: ts("2026-03-03T16:00:00Z"),
            },
            SessionListItem {
                id: uid(S7),
                title: "Add pipeline event types".into(),
                model: ModelReference { id: "Sonnet 4.6".into() },
                last_message_preview: "Added PipelineStarted, StageCompleted, and PipelineFailed event types.".into(),
                created_at: ts("2026-03-01T11:00:00Z"),
                updated_at: ts("2026-03-01T12:00:00Z"),
            },
            SessionListItem {
                id: uid(S8),
                title: "Implement webhook retry logic".into(),
                model: ModelReference { id: "Opus 4.6".into() },
                last_message_preview: "Implemented exponential backoff with jitter, max 5 retries over 24 hours.".into(),
                created_at: ts("2026-02-28T09:00:00Z"),
                updated_at: ts("2026-02-28T10:00:00Z"),
            },
        ]
    }

    pub(super) fn detail(id: &str) -> Option<SessionDetail> {
        let parsed = id.parse::<Uuid>().ok()?;
        match parsed.as_u128() {
            S1 => Some(SessionDetail {
                id: uid(S1), title: "Add rate limiting to auth endpoints".into(), model: ModelReference { id: "Opus 4.6".into() }, created_at: ts("2026-03-06T14:30:00Z"), updated_at: ts("2026-03-06T15:45:00Z"),
                turns: vec![
                    SessionTurn::UserTurn(UserTurn { kind: UserTurnKind::User, content: "Add rate limiting to the auth endpoints. We're getting hit with brute force attempts on /api/auth/login and /api/auth/register. Use a sliding window approach with Redis, 10 requests per minute per IP.".into(), created_at: ts("2026-02-28T10:00:00Z") }),
                    SessionTurn::AssistantTurn(AssistantTurn { kind: AssistantTurnKind::Assistant, content: "I'll implement sliding window rate limiting using Redis. Let me first look at the existing auth routes and middleware setup.".into(), created_at: ts("2026-02-28T10:00:15Z") }),
                    SessionTurn::ToolTurn(ToolTurn { kind: ToolTurnKind::Tool, tools: vec![
                        ToolUse { id: "toolu_03".into(), tool_name: "read_file".into(), input: r#"{ "path": "src/routes/auth.ts" }"#.into(), result: "import { Router } from \"express\";\nimport { loginHandler } from \"../handlers/auth\";\nimport { registerHandler } from \"../handlers/auth\";\n\nconst router = Router();\nrouter.post(\"/login\", loginHandler);\nrouter.post(\"/register\", registerHandler);\n\nexport default router;".into(), is_error: false, duration_ms: Some(52) },
                        ToolUse { id: "toolu_04".into(), tool_name: "read_file".into(), input: r#"{ "path": "src/middleware/index.ts" }"#.into(), result: "export { authMiddleware } from \"./auth\";\nexport { corsMiddleware } from \"./cors\";\nexport { loggingMiddleware } from \"./logging\";".into(), is_error: false, duration_ms: Some(35) },
                        ToolUse { id: "toolu_05".into(), tool_name: "search_files".into(), input: r#"{ "pattern": "redis", "path": "src/" }"#.into(), result: "src/lib/redis.ts:1: import Redis from \"ioredis\";\nsrc/lib/redis.ts:3: export const redis = new Redis(process.env.REDIS_URL);".into(), is_error: false, duration_ms: Some(180) },
                    ], created_at: ts("2026-02-28T10:00:16Z") }),
                    SessionTurn::AssistantTurn(AssistantTurn { kind: AssistantTurnKind::Assistant, content: "Good \u{2014} there's already a Redis client at `src/lib/redis.ts`. I'll create a rate limiting middleware using the sliding window algorithm and apply it to the auth routes.\n\nHere's the plan:\n1. Create `src/middleware/rate-limit.ts` with sliding window logic\n2. Apply it to `/login` and `/register` routes\n3. Return `429 Too Many Requests` when the limit is exceeded".into(), created_at: ts("2026-02-28T10:00:30Z") }),
                    SessionTurn::ToolTurn(ToolTurn { kind: ToolTurnKind::Tool, tools: vec![
                        ToolUse { id: "toolu_06".into(), tool_name: "write_file".into(), input: r#"{ "path": "src/middleware/rate-limit.ts" }"#.into(), result: "File written: src/middleware/rate-limit.ts (47 lines)".into(), is_error: false, duration_ms: Some(62) },
                        ToolUse { id: "toolu_07".into(), tool_name: "edit_file".into(), input: r#"{ "path": "src/routes/auth.ts", "operations": [{ "type": "insert", "line": 3 }] }"#.into(), result: "Applied 1 edit to src/routes/auth.ts".into(), is_error: false, duration_ms: Some(41) },
                        ToolUse { id: "toolu_08".into(), tool_name: "edit_file".into(), input: r#"{ "path": "src/middleware/index.ts", "operations": [{ "type": "append" }] }"#.into(), result: "Applied 1 edit to src/middleware/index.ts".into(), is_error: false, duration_ms: Some(55) },
                    ], created_at: ts("2026-02-28T10:00:31Z") }),
                    SessionTurn::AssistantTurn(AssistantTurn { kind: AssistantTurnKind::Assistant, content: "Done. I've created the rate limiter and wired it up:\n\n- **`src/middleware/rate-limit.ts`** \u{2014} Sliding window rate limiter using Redis sorted sets. Each request adds a timestamped entry, expired entries are pruned, and the count is checked against the limit.\n- **`src/routes/auth.ts`** \u{2014} Applied `rateLimit({ windowMs: 60_000, max: 10 })` to both `/login` and `/register`.\n- **`src/middleware/index.ts`** \u{2014} Re-exported the new middleware.\n\nWhen the limit is exceeded, clients get a `429` response with a `Retry-After` header indicating seconds until the window resets.".into(), created_at: ts("2026-02-28T10:01:00Z") }),
                ],
            }),
            S2 => Some(SessionDetail {
                id: uid(S2), title: "Fix config parsing for nested values".into(), model: ModelReference { id: "Sonnet 4.6".into() }, created_at: ts("2026-03-06T12:30:00Z"), updated_at: ts("2026-03-06T13:15:00Z"),
                turns: vec![
                    SessionTurn::UserTurn(UserTurn { kind: UserTurnKind::User, content: "The CLI crashes when parsing nested TOML config values like [database.connection]. Can you debug and fix this?".into(), created_at: ts("2026-02-28T10:00:00Z") }),
                    SessionTurn::AssistantTurn(AssistantTurn { kind: AssistantTurnKind::Assistant, content: "Let me look at the config parser to understand how nested keys are handled.".into(), created_at: ts("2026-02-28T10:00:12Z") }),
                    SessionTurn::ToolTurn(ToolTurn { kind: ToolTurnKind::Tool, tools: vec![
                        ToolUse { id: "toolu_09".into(), tool_name: "read_file".into(), input: r#"{ "path": "src/config/parser.ts" }"#.into(), result: "export function parseConfig(raw: string): Record<string, string> {\n  const result: Record<string, string> = {};\n  for (const line of raw.split(\"\\n\")) {\n    const match = line.match(/^(\\w+)\\s*=\\s*(.+)$/);\n    if (match) result[match[1]] = match[2].trim();\n  }\n  return result;\n}".into(), is_error: false, duration_ms: Some(67) },
                    ], created_at: ts("2026-02-28T10:00:13Z") }),
                    SessionTurn::AssistantTurn(AssistantTurn { kind: AssistantTurnKind::Assistant, content: "Found the issue. The parser uses a flat `Record<string, string>` and the regex `^(\\w+)` only matches simple keys \u{2014} it can't handle TOML section headers like `[database.connection]` or dotted keys like `database.host`.\n\nI'll rewrite the parser to support nested sections and dotted keys, producing a properly nested object.".into(), created_at: ts("2026-02-28T10:00:25Z") }),
                    SessionTurn::ToolTurn(ToolTurn { kind: ToolTurnKind::Tool, tools: vec![
                        ToolUse { id: "toolu_10".into(), tool_name: "write_file".into(), input: r#"{ "path": "src/config/parser.ts" }"#.into(), result: "File written: src/config/parser.ts (62 lines)".into(), is_error: false, duration_ms: Some(78) },
                        ToolUse { id: "toolu_11".into(), tool_name: "run_command".into(), input: r#"{ "command": "npm test -- --grep 'config parser'" }"#.into(), result: "PASS src/config/__tests__/parser.test.ts\n  config parser\n    \u{2713} parses flat keys (2ms)\n    \u{2713} parses section headers (1ms)\n    \u{2713} parses dotted keys (1ms)\n    \u{2713} handles nested sections (1ms)\n\nTest Suites: 1 passed\nTests: 4 passed".into(), is_error: false, duration_ms: Some(2150) },
                    ], created_at: ts("2026-02-28T10:00:26Z") }),
                    SessionTurn::AssistantTurn(AssistantTurn { kind: AssistantTurnKind::Assistant, content: "Fixed. The parser now:\n\n1. Tracks the current section header (e.g. `[database.connection]`)\n2. Constructs nested objects from dotted paths\n3. Handles both `key = value` within sections and bare `dotted.key = value` syntax\n\nAll 4 tests pass, including the new nested section case.".into(), created_at: ts("2026-02-28T10:01:00Z") }),
                ],
            }),
            S3 => Some(SessionDetail {
                id: uid(S3), title: "Migrate to React Router v7".into(), model: ModelReference { id: "Opus 4.6".into() }, created_at: ts("2026-03-05T10:00:00Z"), updated_at: ts("2026-03-05T11:30:00Z"),
                turns: vec![
                    SessionTurn::UserTurn(UserTurn { kind: UserTurnKind::User, content: "Help me migrate our app from React Router v6 to v7. We're using createBrowserRouter with data loaders.".into(), created_at: ts("2026-02-26T10:00:00Z") }),
                    SessionTurn::AssistantTurn(AssistantTurn { kind: AssistantTurnKind::Assistant, content: "I'll audit your current router setup and identify what needs to change for v7. Let me scan the codebase.".into(), created_at: ts("2026-02-26T10:00:10Z") }),
                    SessionTurn::ToolTurn(ToolTurn { kind: ToolTurnKind::Tool, tools: vec![
                        ToolUse { id: "toolu_12".into(), tool_name: "search_files".into(), input: r#"{ "pattern": "createBrowserRouter" }"#.into(), result: "src/main.tsx:5: import { createBrowserRouter } from \"react-router-dom\";\nsrc/main.tsx:8: const router = createBrowserRouter([".into(), is_error: false, duration_ms: Some(220) },
                        ToolUse { id: "toolu_13".into(), tool_name: "read_file".into(), input: r#"{ "path": "package.json" }"#.into(), result: "\"react-router-dom\": \"^6.22.0\"".into(), is_error: false, duration_ms: Some(30) },
                    ], created_at: ts("2026-02-26T10:00:11Z") }),
                    SessionTurn::AssistantTurn(AssistantTurn { kind: AssistantTurnKind::Assistant, content: "You're on React Router 6.22. The migration to v7 involves:\n\n1. Replace `react-router-dom` with `react-router` (single package)\n2. Update `createBrowserRouter` import path\n3. Migrate data loaders to the new `Route.LoaderArgs` type\n4. Update any `useLoaderData` calls to be type-safe\n\nLet me make these changes.".into(), created_at: ts("2026-02-26T10:00:30Z") }),
                ],
            }),
            _ => None,
        }
    }
}

mod insights {
    use super::ts;
    use fabro_api_types::*;

    pub(super) fn saved_queries() -> Vec<SavedQuery> {
        vec![
            SavedQuery { id: "1".into(), name: "Run duration by workflow".into(), sql: "SELECT workflow_name, AVG(duration_seconds) as avg_duration,\n       COUNT(*) as run_count\nFROM runs\nGROUP BY workflow_name\nORDER BY avg_duration DESC\nLIMIT 20".into(), created_at: ts("2026-03-01T10:00:00Z"), updated_at: ts("2026-03-05T14:30:00Z") },
            SavedQuery { id: "2".into(), name: "Daily failure rate".into(), sql: "SELECT date_trunc('day', created_at) as day,\n       COUNT(*) FILTER (WHERE status = 'failed') as failures,\n       COUNT(*) as total\nFROM runs\nGROUP BY 1\nORDER BY 1 DESC\nLIMIT 30".into(), created_at: ts("2026-03-02T09:00:00Z"), updated_at: ts("2026-03-02T09:00:00Z") },
            SavedQuery { id: "3".into(), name: "Top repos by activity".into(), sql: "SELECT repo, COUNT(*) as runs\nFROM runs\nGROUP BY repo\nORDER BY runs DESC".into(), created_at: ts("2026-03-03T11:00:00Z"), updated_at: ts("2026-03-03T11:00:00Z") },
        ]
    }

    pub(super) fn history() -> Vec<HistoryEntry> {
        vec![
            HistoryEntry {
                id: "h1".into(),
                sql: "SELECT workflow_name, COUNT(*) FROM runs GROUP BY 1".into(),
                timestamp: ts("2025-09-15T13:58:00Z"),
                elapsed: 0.342,
                row_count: 6,
            },
            HistoryEntry {
                id: "h2".into(),
                sql: "SELECT * FROM runs WHERE status = 'failed' LIMIT 100".into(),
                timestamp: ts("2025-09-15T13:52:00Z"),
                elapsed: 0.127,
                row_count: 23,
            },
            HistoryEntry {
                id: "h3".into(),
                sql: "SELECT date_trunc('day', created_at) as d, COUNT(*) FROM runs GROUP BY 1"
                    .into(),
                timestamp: ts("2025-09-15T13:45:00Z"),
                elapsed: 0.531,
                row_count: 30,
            },
        ]
    }
}

mod settings {
    use fabro_config::FabroSettings;
    use fabro_config::server::*;

    pub(super) fn server_settings() -> serde_json::Value {
        serde_json::to_value(FabroSettings {
            storage_dir: Some("/home/fabro/.fabro".into()),
            max_concurrent_runs: Some(10),
            web: Some(WebSettings {
                url: "https://fabro.example.com".into(),
                auth: AuthSettings {
                    provider: AuthProvider::Github,
                    allowed_usernames: vec!["brynary".into(), "alice".into()],
                },
            }),
            api: Some(ApiSettings {
                base_url: "https://api.fabro.example.com".into(),
                authentication_strategies: vec![ApiAuthStrategy::Jwt],
                tls: None,
            }),
            git: Some(GitSettings {
                provider: GitProvider::Github,
                app_id: Some("12345".into()),
                client_id: Some("Iv1.abc123".into()),
                slug: Some("fabro-dev".into()),
                author: Default::default(),
                webhooks: None,
            }),
            features: Some(FeaturesSettings {
                session_sandboxes: false,
                retros: false,
            }),
            log: Default::default(),
            llm: Some(fabro_config::run::LlmSettings {
                model: Some("claude-sonnet".into()),
                provider: Some("anthropic".into()),
                fallbacks: None,
            }),
            setup: None,
            sandbox: Some(fabro_config::sandbox::SandboxSettings {
                provider: Some("daytona".into()),
                preserve: None,
                devcontainer: None,
                local: None,
                daytona: Some(fabro_sandbox::daytona::DaytonaConfig {
                    auto_stop_interval: Some(60),
                    labels: None,
                    snapshot: None,
                    network: Some(fabro_sandbox::daytona::DaytonaNetwork::Block),
                    skip_clone: false,
                }),
                env: None,
            }),
            vars: None,
            checkpoint: Default::default(),
            pull_request: None,
            assets: None,
            hooks: vec![],
            mcp_servers: Default::default(),
            github: None,
            ..Default::default()
        })
        .unwrap()
    }
}
