//! Demo mode handlers that return static data for all API endpoints.
//! Activated per-request via the `X-Fabro-Demo: 1` header to showcase the UI
//! without a real backend.
#![allow(clippy::default_trait_access, clippy::unreadable_literal)]

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use fabro_api::types::{CreateSecretRequest, DeleteSecretRequest, RunArtifactListResponse};
use serde_json::json;

use crate::error::ApiError;
use crate::jwt_auth::AuthenticatedService;
use crate::server::{AppState, PaginationParams};
use crate::settings_view;

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
    paginated_response(runs::summaries(), &pagination)
}

pub(crate) async fn list_board_runs(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    let items = runs::board_items();
    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset as usize;
    let mut data: Vec<_> = items.into_iter().skip(offset).take(limit + 1).collect();
    let has_more = data.len() > limit;
    data.truncate(limit);
    (
        StatusCode::OK,
        Json(json!({
            "columns": runs::columns(),
            "data": data,
            "meta": { "has_more": has_more }
        })),
    )
        .into_response()
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
    match runs::summaries().into_iter().find(|run| run.run_id == id) {
        Some(run) => (StatusCode::OK, Json(run)).into_response(),
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
                "id": "evt_demo_attach_completed",
                "ts": "2026-04-06T15:00:02Z",
                "run_id": "01JQ0000000000000000000001",
                "event": "run.completed",
                "properties": {
                    "duration_ms": 42,
                    "artifact_count": 0,
                    "status": "success"
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
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": id,
            "status": "failed",
            "status_reason": "cancelled",
            "created_at": "2026-03-06T14:30:00Z"
        })),
    )
        .into_response()
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

    crate::server::render_graph_bytes(dot_source).await
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
                    "type": "environment",
                    "created_at": "2026-04-05T12:00:00Z",
                    "updated_at": "2026-04-05T12:00:00Z"
                },
                {
                    "name": "GITHUB_APP_PRIVATE_KEY",
                    "type": "environment",
                    "created_at": "2026-04-05T12:05:00Z",
                    "updated_at": "2026-04-05T12:05:00Z"
                }
            ]
        })),
    )
        .into_response()
}

pub(crate) async fn create_secret(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Json(body): Json<CreateSecretRequest>,
) -> Response {
    let mut payload = serde_json::Map::new();
    payload.insert("name".to_string(), json!(body.name));
    payload.insert("type".to_string(), json!(body.type_));
    if let Some(description) = body.description {
        payload.insert("description".to_string(), json!(description));
    }
    payload.insert("created_at".to_string(), json!("2026-04-05T12:00:00Z"));
    payload.insert("updated_at".to_string(), json!("2026-04-05T12:00:00Z"));

    (StatusCode::OK, Json(serde_json::Value::Object(payload))).into_response()
}

pub(crate) async fn delete_secret_by_name(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Json(_body): Json<DeleteSecretRequest>,
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
    Query(query): Query<settings_view::SettingsQuery>,
) -> Response {
    match query.view {
        settings_view::SettingsApiView::Layer => {
            (StatusCode::OK, Json(settings::server_settings())).into_response()
        }
        settings_view::SettingsApiView::Resolved => {
            let mut response =
                (StatusCode::OK, Json(settings::resolved_server_settings())).into_response();
            response.headers_mut().insert(
                settings_view::RESOLVED_VIEW_HEADER_NAME,
                axum::http::HeaderValue::from_static(settings_view::RESOLVED_VIEW_HEADER_VALUE),
            );
            response
        }
    }
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
                    "id": "evt_demo_1",
                    "ts": "2026-04-06T15:00:00Z",
                    "run_id": "01JQ0000000000000000000001",
                    "event": "run.started"
                })
                .to_string(),
            ),
        ),
        Ok::<_, std::convert::Infallible>(
            Event::default().data(
                json!({
                    "seq": 2,
                    "id": "evt_demo_2",
                    "ts": "2026-04-06T15:00:01Z",
                    "run_id": "01JQ0000000000000000000001",
                    "event": "stage.started"
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
            "profile": option_env!("FABRO_BUILD_PROFILE"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "storage_engine": "slatedb",
            "storage_dir": "/demo/fabro/storage",
            "uptime_secs": 42,
            "runs": { "total": 3, "active": 1 },
            "sandbox_provider": "local",
            "features": { "session_sandboxes": false, "retros": false }
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
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::time::Duration;

    use fabro_api::types::*;

    use super::ts;
    use crate::server::truncate_goal;

    fn labels(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn summary(
        run_id: &str,
        repo_name: &str,
        workflow_slug: &str,
        workflow_name: &str,
        goal: &str,
        status: &str,
        created_at: &str,
        elapsed_secs: Option<f64>,
        status_reason: Option<&str>,
        pending_control: Option<RunControlAction>,
        total_usd_micros: Option<i64>,
        entries: &[(&str, &str)],
    ) -> StoreRunSummary {
        let status_reason = status_reason.and_then(parse_status_reason);

        StoreRunSummary {
            created_at: ts(created_at),
            duration_ms: elapsed_secs.and_then(duration_ms_from_secs),
            elapsed_secs,
            goal: goal.into(),
            host_repo_path: Some(format!("/demo/{repo_name}")),
            labels: labels(entries),
            pending_control,
            repository: RepositoryReference {
                name: repo_name.into(),
            },
            run_id: run_id.into(),
            start_time: Some(ts(created_at)),
            status: RunStatus::from_str(status)
                .unwrap_or_else(|_| panic!("invalid demo run status: {status}")),
            status_reason,
            blocked_reason: None,
            title: truncate_goal(goal),
            total_usd_micros,
            workflow_name: Some(workflow_name.into()),
            workflow_slug: Some(workflow_slug.into()),
        }
    }

    fn parse_status_reason(reason: &str) -> Option<StatusReason> {
        StatusReason::from_str(reason).ok()
    }

    fn duration_ms_from_secs(secs: f64) -> Option<i64> {
        let duration = Duration::try_from_secs_f64(secs).ok()?;
        duration.as_millis().try_into().ok()
    }

    fn take_summary(
        summaries: &mut HashMap<String, StoreRunSummary>,
        run_id: &str,
    ) -> StoreRunSummary {
        summaries
            .remove(run_id)
            .unwrap_or_else(|| panic!("missing demo summary: {run_id}"))
    }

    fn board_item(
        summary: StoreRunSummary,
        column: BoardColumn,
        pull_request: Option<RunPullRequest>,
        sandbox: Option<RunSandbox>,
        question: Option<RunQuestion>,
    ) -> RunListItem {
        RunListItem {
            column,
            created_at: summary.created_at,
            duration_ms: summary.duration_ms,
            elapsed_secs: summary.elapsed_secs,
            goal: summary.goal,
            host_repo_path: summary.host_repo_path,
            labels: summary.labels,
            pending_control: summary.pending_control,
            pull_request,
            question,
            repository: summary.repository,
            run_id: summary.run_id,
            sandbox,
            start_time: summary.start_time,
            status: summary.status.to_string(),
            status_reason: summary.status_reason,
            title: summary.title,
            total_usd_micros: summary.total_usd_micros,
            workflow_name: summary.workflow_name,
            workflow_slug: summary.workflow_slug,
        }
    }

    fn check(name: &str, status: CheckRunStatus, duration_secs: Option<f64>) -> CheckRun {
        CheckRun {
            name: name.into(),
            status,
            duration_secs,
        }
    }

    fn sandbox(id: &str, cpu: i64, memory: i64) -> RunSandbox {
        RunSandbox {
            id:        id.into(),
            resources: Some(SandboxResources { cpu, memory }),
        }
    }

    fn pull_request(
        number: i64,
        additions: i64,
        deletions: i64,
        comments: i64,
        checks: Vec<CheckRun>,
    ) -> RunPullRequest {
        RunPullRequest {
            number,
            additions: Some(additions),
            deletions: Some(deletions),
            comments: Some(comments),
            checks,
        }
    }

    pub(super) fn columns() -> Vec<BoardColumnDefinition> {
        vec![
            BoardColumnDefinition {
                id:   "initializing".into(),
                name: "Initializing".into(),
            },
            BoardColumnDefinition {
                id:   "running".into(),
                name: "Running".into(),
            },
            BoardColumnDefinition {
                id:   "blocked".into(),
                name: "Blocked".into(),
            },
            BoardColumnDefinition {
                id:   "succeeded".into(),
                name: "Succeeded".into(),
            },
            BoardColumnDefinition {
                id:   "failed".into(),
                name: "Failed".into(),
            },
        ]
    }

    pub(super) fn summaries() -> Vec<StoreRunSummary> {
        vec![
            summary(
                "run-1",
                "api-server",
                "implement",
                "Implement",
                "Add rate limiting to auth endpoints",
                "running",
                "2026-03-06T14:30:00Z",
                Some(420.0),
                None,
                None,
                None,
                &[("branch", "rate-limit"), ("team", "platform")],
            ),
            summary(
                "run-2",
                "web-dashboard",
                "implement",
                "Implement",
                "Migrate to React Router v7",
                "running",
                "2026-03-06T12:00:00Z",
                Some(8100.0),
                None,
                Some(RunControlAction::Pause),
                None,
                &[("owner", "frontend")],
            ),
            summary(
                "run-3",
                "shared-types",
                "expand",
                "Expand",
                "Update OpenAPI spec for v3",
                "starting",
                "2026-03-04T15:00:00Z",
                Some(4320.0),
                None,
                None,
                None,
                &[("priority", "high")],
            ),
            summary(
                "run-4",
                "shared-types",
                "implement",
                "Implement",
                "Add pipeline event types",
                "paused",
                "2026-03-04T10:00:00Z",
                Some(1680.0),
                None,
                None,
                None,
                &[("owner", "runtime")],
            ),
            summary(
                "run-5",
                "web-dashboard",
                "implement",
                "Implement",
                "Add dark mode toggle",
                "failed",
                "2026-03-03T16:45:00Z",
                Some(2100.0),
                Some("workflow_error"),
                None,
                None,
                &[("environment", "staging")],
            ),
            summary(
                "run-6",
                "api-server",
                "implement",
                "Implement",
                "Implement webhook retry logic",
                "succeeded",
                "2026-02-28T14:00:00Z",
                Some(259200.0),
                Some("completed"),
                None,
                Some(720000),
                &[("release", "preview")],
            ),
        ]
    }

    pub(super) fn board_items() -> Vec<RunListItem> {
        let mut summaries = summaries()
            .into_iter()
            .map(|summary| (summary.run_id.clone(), summary))
            .collect::<HashMap<_, _>>();

        vec![
            board_item(
                take_summary(&mut summaries, "run-1"),
                BoardColumn::Running,
                None,
                Some(sandbox("sb-a1b2c3d4", 4, 8)),
                None,
            ),
            board_item(
                take_summary(&mut summaries, "run-2"),
                BoardColumn::Running,
                None,
                Some(sandbox("sb-e5f6g7h8", 8, 16)),
                None,
            ),
            board_item(
                take_summary(&mut summaries, "run-3"),
                BoardColumn::Initializing,
                Some(pull_request(0, 567, 234, 0, vec![])),
                Some(sandbox("sb-q7r8s9t0", 4, 8)),
                Some(RunQuestion {
                    text: "Accept or push for another round?".into(),
                }),
            ),
            board_item(
                take_summary(&mut summaries, "run-4"),
                BoardColumn::Blocked,
                Some(pull_request(0, 145, 23, 0, vec![])),
                Some(sandbox("sb-u1v2w3x4", 4, 8)),
                Some(RunQuestion {
                    text: "Proceed from investigation to fix?".into(),
                }),
            ),
            board_item(
                take_summary(&mut summaries, "run-5"),
                BoardColumn::Failed,
                Some(pull_request(889, 234, 67, 4, vec![
                    check("lint", CheckRunStatus::Success, Some(23.0)),
                    check("typecheck", CheckRunStatus::Success, Some(72.0)),
                    check("unit-tests", CheckRunStatus::Success, Some(154.0)),
                    check("integration-tests", CheckRunStatus::Failure, Some(296.0)),
                    check("build", CheckRunStatus::Success, Some(105.0)),
                ])),
                None,
                None,
            ),
            board_item(
                take_summary(&mut summaries, "run-6"),
                BoardColumn::Succeeded,
                Some(pull_request(1249, 189, 45, 7, vec![
                    check("lint", CheckRunStatus::Success, Some(21.0)),
                    check("typecheck", CheckRunStatus::Success, Some(68.0)),
                    check("unit-tests", CheckRunStatus::Success, Some(192.0)),
                    check("integration-tests", CheckRunStatus::Success, Some(334.0)),
                    check("deploy-preview", CheckRunStatus::Success, Some(93.0)),
                ])),
                None,
                None,
            ),
        ]
    }

    pub(super) fn stages() -> Vec<RunStage> {
        vec![
            RunStage {
                id:            "detect-drift".into(),
                name:          "Detect Drift".into(),
                status:        StageStatus::Completed,
                duration_secs: Some(72.0),
                dot_id:        Some("detect".into()),
            },
            RunStage {
                id:            "propose-changes".into(),
                name:          "Propose Changes".into(),
                status:        StageStatus::Completed,
                duration_secs: Some(154.0),
                dot_id:        Some("propose".into()),
            },
            RunStage {
                id:            "review-changes".into(),
                name:          "Review Changes".into(),
                status:        StageStatus::Completed,
                duration_secs: Some(45.0),
                dot_id:        Some("review".into()),
            },
            RunStage {
                id:            "apply-changes".into(),
                name:          "Apply Changes".into(),
                status:        StageStatus::Running,
                duration_secs: Some(118.0),
                dot_id:        Some("apply".into()),
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
            stages:   vec![
                RunBillingStage {
                    stage:        BillingStageRef {
                        id:   "detect-drift".into(),
                        name: "Detect Drift".into(),
                    },
                    model:        ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    billing:      BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       12480,
                        output_tokens:      3210,
                        reasoning_tokens:   None,
                        total_tokens:       15690,
                        total_usd_micros:   Some(480_000),
                    },
                    runtime_secs: 72.0,
                },
                RunBillingStage {
                    stage:        BillingStageRef {
                        id:   "propose-changes".into(),
                        name: "Propose Changes".into(),
                    },
                    model:        ModelReference {
                        id: "Gemini 3.1".into(),
                    },
                    billing:      BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       28640,
                        output_tokens:      8750,
                        reasoning_tokens:   None,
                        total_tokens:       37390,
                        total_usd_micros:   Some(720_000),
                    },
                    runtime_secs: 154.0,
                },
                RunBillingStage {
                    stage:        BillingStageRef {
                        id:   "review-changes".into(),
                        name: "Review Changes".into(),
                    },
                    model:        ModelReference {
                        id: "Codex 5.3".into(),
                    },
                    billing:      BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       9120,
                        output_tokens:      2640,
                        reasoning_tokens:   None,
                        total_tokens:       11760,
                        total_usd_micros:   Some(190_000),
                    },
                    runtime_secs: 45.0,
                },
                RunBillingStage {
                    stage:        BillingStageRef {
                        id:   "apply-changes".into(),
                        name: "Apply Changes".into(),
                    },
                    model:        ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    billing:      BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       21300,
                        output_tokens:      6480,
                        reasoning_tokens:   None,
                        total_tokens:       27780,
                        total_usd_micros:   Some(870_000),
                    },
                    runtime_secs: 118.0,
                },
            ],
            totals:   RunBillingTotals {
                cache_read_tokens:  None,
                cache_write_tokens: None,
                runtime_secs:       389.0,
                input_tokens:       71540,
                output_tokens:      21080,
                reasoning_tokens:   None,
                total_tokens:       92620,
                total_usd_micros:   Some(2_260_000),
            },
            by_model: vec![
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       33780,
                        output_tokens:      9690,
                        reasoning_tokens:   None,
                        total_tokens:       43470,
                        total_usd_micros:   Some(1_350_000),
                    },
                    model:   ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    stages:  2,
                },
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       28640,
                        output_tokens:      8750,
                        reasoning_tokens:   None,
                        total_tokens:       37390,
                        total_usd_micros:   Some(720_000),
                    },
                    model:   ModelReference {
                        id: "Gemini 3.1".into(),
                    },
                    stages:  1,
                },
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       9120,
                        output_tokens:      2640,
                        reasoning_tokens:   None,
                        total_tokens:       11760,
                        total_usd_micros:   Some(190_000),
                    },
                    model:   ModelReference {
                        id: "Codex 5.3".into(),
                    },
                    stages:  1,
                },
            ],
        }
    }

    pub(super) fn questions() -> Vec<ApiQuestion> {
        vec![
            ApiQuestion {
                id:              "q-001".into(),
                text:            "Should we proceed with the proposed changes?".into(),
                stage:           "review".into(),
                question_type:   QuestionType::YesNo,
                options:         vec![
                    ApiQuestionOption {
                        key:   "yes".into(),
                        label: "Yes".into(),
                    },
                    ApiQuestionOption {
                        key:   "no".into(),
                        label: "No".into(),
                    },
                ],
                allow_freeform:  false,
                timeout_seconds: None,
                context_display: None,
            },
            ApiQuestion {
                id:              "q-002".into(),
                text:            "Which approach do you prefer for the migration?".into(),
                stage:           "migration".into(),
                question_type:   QuestionType::MultipleChoice,
                options:         vec![
                    ApiQuestionOption {
                        key:   "incremental".into(),
                        label: "Incremental migration".into(),
                    },
                    ApiQuestionOption {
                        key:   "big_bang".into(),
                        label: "Big-bang rewrite".into(),
                    },
                ],
                allow_freeform:  true,
                timeout_seconds: None,
                context_display: None,
            },
        ]
    }

    pub(super) fn settings() -> serde_json::Value {
        // v2 SettingsLayer shape — matches what /api/v1/runs/:id/settings
        // returns in production, so the demo renders identically.
        serde_json::json!({
            "_version": 1,
            "run": {
                "goal": "Add rate limiting to auth endpoints",
                "working_dir": "/workspace/api-server",
                "model": {
                    "provider": "anthropic",
                    "name": "claude-opus-4-6"
                },
                "prepare": {
                    "steps": [
                        { "command": ["bun", "install"] },
                        { "command": ["bun", "run", "typecheck"] }
                    ],
                    "timeout": "120s"
                },
                "sandbox": {
                    "provider": "daytona",
                    "daytona": {
                        "auto_stop_interval": 60,
                        "labels": { "project": "api-server" },
                        "snapshot": {
                            "name": "api-server-dev",
                            "cpu": 4,
                            "memory": "8GB",
                            "disk": "10GB"
                        }
                    }
                }
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn summary_parses_known_status_reason_values() {
            let summary = summary(
                "run-test",
                "demo-repo",
                "implement",
                "Implement",
                "Goal",
                "failed",
                "2026-03-06T14:30:00Z",
                Some(1.0),
                Some("cancelled"),
                None,
                None,
                &[],
            );

            assert_eq!(summary.status_reason, Some(StatusReason::Cancelled));
        }

        #[test]
        fn summary_ignores_unknown_status_reason() {
            let summary = summary(
                "run-test",
                "demo-repo",
                "implement",
                "Implement",
                "Goal",
                "failed",
                "2026-03-06T14:30:00Z",
                Some(1.0),
                Some("unexpected_reason"),
                None,
                None,
                &[],
            );

            assert_eq!(summary.status_reason, None);
        }

        #[test]
        fn summary_derives_title_like_server() {
            let goal = format!("## Plan: {}", "a".repeat(120));
            let summary = summary(
                "run-test",
                "demo-repo",
                "implement",
                "Implement",
                &goal,
                "running",
                "2026-03-06T14:30:00Z",
                Some(1.0),
                None,
                None,
                None,
                &[],
            );

            assert_eq!(summary.title, format!("{}...", "a".repeat(97)));
        }
    }
}

mod billing {
    use fabro_api::types::*;

    pub(super) fn aggregate() -> AggregateBilling {
        AggregateBilling {
            totals:   AggregateBillingTotals {
                cache_read_tokens:  None,
                cache_write_tokens: None,
                runs:               9,
                input_tokens:       643_860,
                output_tokens:      189_720,
                reasoning_tokens:   None,
                runtime_secs:       3_501.0,
                total_tokens:       833_580,
                total_usd_micros:   Some(20_340_000),
            },
            by_model: vec![
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       304_020,
                        output_tokens:      87_210,
                        reasoning_tokens:   None,
                        total_tokens:       391_230,
                        total_usd_micros:   Some(12_150_000),
                    },
                    model:   ModelReference {
                        id: "Opus 4.6".into(),
                    },
                    stages:  18,
                },
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       257_760,
                        output_tokens:      78_750,
                        reasoning_tokens:   None,
                        total_tokens:       336_510,
                        total_usd_micros:   Some(6_480_000),
                    },
                    model:   ModelReference {
                        id: "Gemini 3.1".into(),
                    },
                    stages:  9,
                },
                BillingByModel {
                    billing: BilledTokenCounts {
                        cache_read_tokens:  None,
                        cache_write_tokens: None,
                        input_tokens:       82_080,
                        output_tokens:      23_760,
                        reasoning_tokens:   None,
                        total_tokens:       105_840,
                        total_usd_micros:   Some(1_710_000),
                    },
                    model:   ModelReference {
                        id: "Codex 5.3".into(),
                    },
                    stages:  9,
                },
            ],
        }
    }
}

mod insights {
    use fabro_api::types::*;

    use super::ts;

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
                id:        "h1".into(),
                sql:       "SELECT workflow_name, COUNT(*) FROM runs GROUP BY 1".into(),
                timestamp: ts("2025-09-15T13:58:00Z"),
                elapsed:   0.342,
                row_count: 6,
            },
            HistoryEntry {
                id:        "h2".into(),
                sql:       "SELECT * FROM runs WHERE status = 'failed' LIMIT 100".into(),
                timestamp: ts("2025-09-15T13:52:00Z"),
                elapsed:   0.127,
                row_count: 23,
            },
            HistoryEntry {
                id:        "h3".into(),
                sql:
                    "SELECT date_trunc('day', created_at) as d, COUNT(*) FROM runs GROUP BY 1"
                        .into(),
                timestamp: ts("2025-09-15T13:45:00Z"),
                elapsed:   0.531,
                row_count: 30,
            },
        ]
    }
}

mod settings {
    pub(super) fn server_settings() -> serde_json::Value {
        // v2 SettingsLayer shape — matches what /api/v1/settings returns in
        // production, so the demo renders identically.
        serde_json::json!({
            "_version": 1,
            "server": {
                "storage": {
                    "root": "/home/fabro/.fabro"
                },
                "scheduler": {
                    "max_concurrent_runs": 10
                },
                "api": {
                    "url": "https://api.fabro.example.com"
                },
                "web": {
                    "enabled": true,
                    "url": "https://fabro.example.com"
                },
                "auth": {
                    "api": {
                        "jwt": { "enabled": true }
                    },
                    "web": {
                        "allowed_usernames": ["brynary", "alice"],
                        "providers": {
                            "github": {
                                "enabled": true,
                                "client_id": "Iv1.abc123"
                            }
                        }
                    }
                },
                "integrations": {
                    "github": {
                        "app_id": "12345",
                        "client_id": "Iv1.abc123",
                        "slug": "fabro-dev"
                    }
                }
            },
            "run": {
                "model": {
                    "provider": "anthropic",
                    "name": "claude-sonnet"
                },
                "sandbox": {
                    "provider": "daytona",
                    "daytona": {
                        "auto_stop_interval": 60,
                        "network": "block"
                    }
                }
            },
            "features": {
                "session_sandboxes": false,
                "retros": false
            }
        })
    }

    pub(super) fn resolved_server_settings() -> serde_json::Value {
        serde_json::json!({
            "project": {
                "directory": "."
            },
            "workflow": {
                "graph": "workflow.fabro"
            },
            "run": {
                "model": {
                    "provider": "anthropic",
                    "name": "claude-sonnet",
                    "fallbacks": []
                },
                "execution": {
                    "mode": "normal",
                    "approval": "prompt",
                    "retros": true
                },
                "sandbox": {
                    "provider": "daytona",
                    "preserve": false,
                    "devcontainer": false,
                    "env": {},
                    "local": {
                        "worktree_mode": "clean"
                    },
                    "daytona": {
                        "auto_stop_interval": 60,
                        "labels": {},
                        "network": "block",
                        "skip_clone": false
                    }
                },
                "notifications": {},
                "interviews": {},
                "agent": {
                    "mcps": {}
                },
                "hooks": [],
                "scm": {},
                "artifacts": {
                    "include": []
                },
                "inputs": {},
                "metadata": {},
                "git": {},
                "prepare": {
                    "commands": [],
                    "timeout_ms": 300000
                },
                "checkpoint": {
                    "exclude_globs": []
                }
            },
            "cli": {
                "auth": {},
                "exec": {
                    "prevent_idle_sleep": false,
                    "model": {},
                    "agent": {
                        "mcps": {}
                    }
                },
                "output": {
                    "format": "text",
                    "verbosity": "normal"
                },
                "updates": {
                    "check": true
                },
                "logging": {}
            },
            "server": {
                "api": {
                    "url": "https://api.fabro.example.com"
                },
                "web": {
                    "enabled": true,
                    "url": "https://fabro.example.com"
                },
                "auth": {
                    "api": {
                        "jwt": {
                            "enabled": true
                        }
                    },
                    "web": {
                        "allowed_usernames": ["brynary", "alice"],
                        "providers": {
                            "github": {
                                "enabled": true,
                                "client_id": "Iv1.abc123"
                            }
                        }
                    }
                },
                "storage": {
                    "root": "/home/fabro/.fabro"
                },
                "artifacts": {
                    "prefix": "",
                    "store": {
                        "type": "local",
                        "root": ""
                    }
                },
                "slatedb": {
                    "prefix": "",
                    "store": {
                        "type": "local",
                        "root": ""
                    },
                    "flush_interval": "0s"
                },
                "scheduler": {
                    "max_concurrent_runs": 10
                },
                "logging": {},
                "integrations": {
                    "github": {
                        "enabled": false,
                        "strategy": "token",
                        "app_id": "12345",
                        "client_id": "Iv1.abc123",
                        "slug": "fabro-dev",
                        "permissions": {}
                    },
                    "slack": {
                        "enabled": false
                    },
                    "discord": {
                        "enabled": false
                    },
                    "teams": {
                        "enabled": false
                    }
                }
            },
            "features": {
                "session_sandboxes": false
            }
        })
    }
}
