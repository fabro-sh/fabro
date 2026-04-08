//! Demo mode handlers that return static data for all API endpoints.
//! Activated per-request via the `X-Fabro-Demo: 1` header to showcase the UI without a real backend.
#![allow(clippy::default_trait_access, clippy::unreadable_literal)]

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use fabro_api::types::RunArtifactListResponse;
use serde_json::json;

use crate::error::ApiError;
use crate::jwt_auth::AuthenticatedService;
use crate::server::{AppState, PaginationParams};

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

pub(crate) async fn list_board_runs(
    auth: AuthenticatedService,
    state: State<Arc<AppState>>,
    pagination: Query<PaginationParams>,
) -> Response {
    list_runs(auth, state, pagination).await
}

pub(crate) async fn create_run_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": "demo-run-new", "status": "submitted", "created_at": "2026-03-06T14:30:00Z"})),
    )
        .into_response()
}

pub(crate) async fn start_run_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    (
        StatusCode::OK,
        Json(
            serde_json::json!({"id": id, "status": "queued", "created_at": "2026-03-06T14:30:00Z"}),
        ),
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

pub(crate) async fn list_run_artifacts_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::OK,
        Json(RunArtifactListResponse { data: vec![] }),
    )
        .into_response()
}

pub(crate) async fn get_run_billing(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(runs::billing())).into_response()
}

pub(crate) async fn get_run_settings(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(runs::settings())).into_response()
}

pub(crate) async fn generate_preview_url_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"url": "https://google.com", "token": "demo-preview-token"})),
    )
        .into_response()
}

pub(crate) async fn create_ssh_access_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"command": "ssh demo@fabro.example"})),
    )
        .into_response()
}

pub(crate) async fn list_sandbox_files_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": [
                { "name": "report.txt", "is_dir": false, "size": 12 },
                { "name": "logs", "is_dir": true }
            ]
        })),
    )
        .into_response()
}

pub(crate) async fn get_sandbox_file_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, "demo sandbox file").into_response()
}

pub(crate) async fn put_sandbox_file_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) async fn get_run_status(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match runs::list_items().into_iter().find(|r| r.id == id) {
        Some(item) => {
            let elapsed_ms = item
                .timings
                .as_ref()
                .map(|t| (t.elapsed_secs * 1000.0) as u64);
            (
                StatusCode::OK,
                Json(json!({
                    "run_id": item.id,
                    "goal": item.title,
                    "workflow_slug": item.workflow.slug,
                    "workflow_name": item.workflow.slug,
                    "host_repo_path": format!("/demo/{}", item.repository.name),
                    "labels": {},
                    "start_time": item.created_at.to_rfc3339(),
                    "status": "running",
                    "status_reason": null,
                    "pending_control": null,
                    "duration_ms": elapsed_ms,
                    "total_usd_micros": null,
                })),
            )
                .into_response()
        }
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
    let events = vec![Ok::<_, std::convert::Infallible>(
        Event::default().data(
            json!({
                "seq": 2,
                "payload": {
                    "id": "evt_demo_attach_completed",
                    "ts": "2026-04-06T15:00:02Z",
                    "run_id": "01JQ0000000000000000000001",
                    "event": "run.completed",
                    "properties": {
                        "duration_ms": 42,
                        "artifact_count": 0,
                        "status": "success"
                    }
                }
            })
            .to_string(),
        ),
    )];
    Sse::new(tokio_stream::iter(events)).into_response()
}

pub(crate) async fn checkpoint_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!(null))).into_response()
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

    crate::server::render_graph_bytes(dot_source, fabro_graphviz::render::GraphFormat::Svg).await
}

pub(crate) async fn list_secrets(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "data": [
                {
                    "name": "OPENAI_API_KEY",
                    "created_at": "2026-04-05T12:00:00Z",
                    "updated_at": "2026-04-05T12:00:00Z"
                },
                {
                    "name": "GITHUB_APP_PRIVATE_KEY",
                    "created_at": "2026-04-05T12:05:00Z",
                    "updated_at": "2026-04-05T12:05:00Z"
                }
            ]
        })),
    )
        .into_response()
}

pub(crate) async fn set_secret(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "name": name,
            "created_at": "2026-04-05T12:00:00Z",
            "updated_at": "2026-04-05T12:00:00Z"
        })),
    )
        .into_response()
}

pub(crate) async fn delete_secret(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_name): Path<String>,
) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) async fn get_github_repo(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path((owner, name)): Path<(String, String)>,
) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "owner": owner,
            "name": name,
            "accessible": false,
            "default_branch": null,
            "private": null,
            "permissions": null,
            "install_url": "https://github.com/apps/fabro/installations/new"
        })),
    )
        .into_response()
}

pub(crate) async fn run_diagnostics(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "version": fabro_util::version::FABRO_VERSION,
            "sections": [
                {
                    "title": "Credentials",
                    "checks": [
                        { "name": "LLM Providers", "status": "pass", "summary": "demo configured", "details": [], "remediation": null },
                        { "name": "GitHub App", "status": "pass", "summary": "demo configured", "details": [], "remediation": null },
                        { "name": "Sandbox", "status": "warning", "summary": "not configured", "details": [], "remediation": "Set DAYTONA_API_KEY to enable cloud sandbox execution" },
                        { "name": "Brave Search", "status": "warning", "summary": "not configured", "details": [], "remediation": "Set BRAVE_SEARCH_API_KEY to enable web search" }
                    ]
                },
                {
                    "title": "System",
                    "checks": [
                        { "name": "dot", "status": "pass", "summary": "dot available", "details": [], "remediation": null }
                    ]
                },
                {
                    "title": "Configuration",
                    "checks": [
                        { "name": "Crypto", "status": "pass", "summary": "all keys valid", "details": [], "remediation": null }
                    ]
                }
            ]
        })),
    )
        .into_response()
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

// ── Settings ───────────────────────────────────────────────────────────

pub(crate) async fn get_server_settings(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (StatusCode::OK, Json(settings::server_settings())).into_response()
}

// ── System ────────────────────────────────────────────────────────────

pub(crate) async fn attach_events_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    let events = vec![
        Ok::<_, std::convert::Infallible>(
            Event::default().data(
                json!({
                    "seq": 1,
                    "payload": {
                        "id": "evt_demo_1",
                        "ts": "2026-04-06T15:00:00Z",
                        "run_id": "01JQ0000000000000000000001",
                        "event": "run.started"
                    }
                })
                .to_string(),
            ),
        ),
        Ok::<_, std::convert::Infallible>(
            Event::default().data(
                json!({
                    "seq": 2,
                    "payload": {
                        "id": "evt_demo_2",
                        "ts": "2026-04-06T15:00:01Z",
                        "run_id": "01JQ0000000000000000000001",
                        "event": "stage.started"
                    }
                })
                .to_string(),
            ),
        ),
    ];
    Sse::new(tokio_stream::iter(events)).into_response()
}

pub(crate) async fn get_system_info(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "git_sha": option_env!("FABRO_GIT_SHA"),
            "build_date": option_env!("FABRO_BUILD_DATE"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "storage_engine": "slatedb",
            "storage_dir": "/demo/fabro/storage",
            "uptime_secs": 42,
            "runs": { "total": 3, "active": 1 },
            "sandbox_provider": "local"
        })),
    )
        .into_response()
}

pub(crate) async fn get_system_disk_usage(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(params): Query<crate::server::DfParams>,
) -> Response {
    let runs = params.verbose.then(|| {
        json!([
            {
                "run_id": "01JQ0000000000000000000001",
                "workflow_name": "Demo Workflow",
                "status": "succeeded",
                "start_time": "2026-04-06T15:00:00Z",
                "size_bytes": 1024,
                "reclaimable": true
            }
        ])
    });
    (
        StatusCode::OK,
        Json(json!({
            "summary": [
                {
                    "type": "runs",
                    "count": 1,
                    "active": 0,
                    "size_bytes": 1024,
                    "reclaimable_bytes": 1024
                },
                {
                    "type": "logs",
                    "count": 1,
                    "active": null,
                    "size_bytes": 256,
                    "reclaimable_bytes": 256
                }
            ],
            "total_size_bytes": 1280,
            "total_reclaimable_bytes": 1280,
            "runs": runs
        })),
    )
        .into_response()
}

pub(crate) async fn prune_runs(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "dry_run": true,
            "runs": [
                {
                    "run_id": "01JQ0000000000000000000001",
                    "dir_name": "20260406-01JQ0000000000000000000001",
                    "workflow_name": "Demo Workflow",
                    "size_bytes": 1024
                }
            ],
            "total_count": 1,
            "total_size_bytes": 1024,
            "deleted_count": 0,
            "freed_bytes": 0
        })),
    )
        .into_response()
}

// ── Usage ──────────────────────────────────────────────────────────────

pub(crate) async fn get_aggregate_billing(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (StatusCode::OK, Json(billing::aggregate())).into_response()
}

// ── Data modules ───────────────────────────────────────────────────────

use chrono::{DateTime, Utc};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

mod runs {
    use super::ts;
    use fabro_api::types::*;

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

    pub(super) fn billing() -> RunBilling {
        RunBilling {
            stages: vec![
                RunBillingStage {
                    stage: BillingStageRef {
                        id: "detect-drift".into(),
                        name: "Detect Drift".into(),
                    },
                    model: ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 12480,
                        output_tokens: 3210,
                        reasoning_tokens: None,
                        total_tokens: 15690,
                        total_usd_micros: Some(480_000),
                    },
                    runtime_secs: 72.0,
                },
                RunBillingStage {
                    stage: BillingStageRef {
                        id: "propose-changes".into(),
                        name: "Propose Changes".into(),
                    },
                    model: ModelReference {
                        id: "Gemini 3.1".into(),
                    },
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 28640,
                        output_tokens: 8750,
                        reasoning_tokens: None,
                        total_tokens: 37390,
                        total_usd_micros: Some(720_000),
                    },
                    runtime_secs: 154.0,
                },
                RunBillingStage {
                    stage: BillingStageRef {
                        id: "review-changes".into(),
                        name: "Review Changes".into(),
                    },
                    model: ModelReference {
                        id: "Codex 5.3".into(),
                    },
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 9120,
                        output_tokens: 2640,
                        reasoning_tokens: None,
                        total_tokens: 11760,
                        total_usd_micros: Some(190_000),
                    },
                    runtime_secs: 45.0,
                },
                RunBillingStage {
                    stage: BillingStageRef {
                        id: "apply-changes".into(),
                        name: "Apply Changes".into(),
                    },
                    model: ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 21300,
                        output_tokens: 6480,
                        reasoning_tokens: None,
                        total_tokens: 27780,
                        total_usd_micros: Some(870_000),
                    },
                    runtime_secs: 118.0,
                },
            ],
            totals: RunBillingTotals {
                cache_read_tokens: None,
                cache_write_tokens: None,
                runtime_secs: 389.0,
                input_tokens: 71540,
                output_tokens: 21080,
                reasoning_tokens: None,
                total_tokens: 92620,
                total_usd_micros: Some(2_260_000),
            },
            by_model: vec![
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 33780,
                        output_tokens: 9690,
                        reasoning_tokens: None,
                        total_tokens: 43470,
                        total_usd_micros: Some(1_350_000),
                    },
                    model: ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    stages: 2,
                },
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 28640,
                        output_tokens: 8750,
                        reasoning_tokens: None,
                        total_tokens: 37390,
                        total_usd_micros: Some(720_000),
                    },
                    model: ModelReference {
                        id: "Gemini 3.1".into(),
                    },
                    stages: 1,
                },
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 9120,
                        output_tokens: 2640,
                        reasoning_tokens: None,
                        total_tokens: 11760,
                        total_usd_micros: Some(190_000),
                    },
                    model: ModelReference {
                        id: "Codex 5.3".into(),
                    },
                    stages: 1,
                },
            ],
        }
    }

    pub(super) fn questions() -> Vec<ApiQuestion> {
        vec![
            ApiQuestion {
                id: "q-001".into(),
                text: "Should we proceed with the proposed changes?".into(),
                stage: "review".into(),
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
                timeout_seconds: None,
                context_display: None,
            },
            ApiQuestion {
                id: "q-002".into(),
                text: "Which approach do you prefer for the migration?".into(),
                stage: "migration".into(),
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
                timeout_seconds: None,
                context_display: None,
            },
        ]
    }

    pub(super) fn settings() -> serde_json::Value {
        serde_json::to_value(fabro_types::Settings {
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
            artifacts: None,
            mcp_servers: Default::default(),
            github: None,
            ..Default::default()
        })
        .unwrap()
    }
}

mod billing {
    use fabro_api::types::*;

    pub(super) fn aggregate() -> AggregateBilling {
        AggregateBilling {
            totals: AggregateBillingTotals {
                cache_read_tokens: None,
                cache_write_tokens: None,
                runs: 9,
                input_tokens: 643_860,
                output_tokens: 189_720,
                reasoning_tokens: None,
                runtime_secs: 3_501.0,
                total_tokens: 833_580,
                total_usd_micros: Some(20_340_000),
            },
            by_model: vec![
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 304_020,
                        output_tokens: 87_210,
                        reasoning_tokens: None,
                        total_tokens: 391_230,
                        total_usd_micros: Some(12_150_000),
                    },
                    model: ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    stages: 18,
                },
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 257_760,
                        output_tokens: 78_750,
                        reasoning_tokens: None,
                        total_tokens: 336_510,
                        total_usd_micros: Some(6_480_000),
                    },
                    model: ModelReference {
                        id: "Gemini 3.1".into(),
                    },
                    stages: 9,
                },
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        input_tokens: 82_080,
                        output_tokens: 23_760,
                        reasoning_tokens: None,
                        total_tokens: 105_840,
                        total_usd_micros: Some(1_710_000),
                    },
                    model: ModelReference {
                        id: "Codex 5.3".into(),
                    },
                    stages: 9,
                },
            ],
        }
    }
}

mod insights {
    use super::ts;
    use fabro_api::types::*;

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
    use fabro_config::server::*;
    use fabro_types::Settings;

    pub(super) fn server_settings() -> serde_json::Value {
        serde_json::to_value(Settings {
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
            artifacts: None,
            hooks: vec![],
            mcp_servers: Default::default(),
            github: None,
            ..Default::default()
        })
        .unwrap()
    }
}
