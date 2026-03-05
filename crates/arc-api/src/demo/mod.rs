//! Demo mode handlers that return static data for all API endpoints.
//! Used with `arc serve --demo` to showcase the UI without a real backend.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use serde_json::json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::error::ApiError;
use crate::jwt_auth::AuthenticatedService;
use crate::server::{AppState, PaginationParams};

fn paginated_response<T: serde::Serialize>(items: Vec<T>, pagination: &PaginationParams) -> Response {
    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset as usize;
    let mut data: Vec<_> = items.into_iter().skip(offset).take(limit + 1).collect();
    let has_more = data.len() > limit;
    data.truncate(limit);
    (StatusCode::OK, Json(json!({ "data": data, "meta": { "has_more": has_more } }))).into_response()
}

// ── Runs ───────────────────────────────────────────────────────────────

pub async fn list_runs(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(runs::list_items(), &pagination)
}

pub async fn start_run_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": "demo-run-new"})),
    )
        .into_response()
}

pub async fn get_run_stages(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(runs::stages())).into_response()
}

pub async fn get_stage_turns(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path((_id, _stage_id)): Path<(String, String)>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(runs::turns(), &pagination)
}

#[derive(serde::Deserialize)]
pub struct FilesQuery {
    #[allow(dead_code)]
    checkpoint: Option<String>,
}

pub async fn get_run_files(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
    Query(_q): Query<FilesQuery>,
) -> Response {
    (StatusCode::OK, Json(runs::files())).into_response()
}

pub async fn get_run_usage(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(runs::usage())).into_response()
}

pub async fn get_run_verifications(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(runs::verifications())).into_response()
}

pub async fn get_run_configuration(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/plain")],
        runs::configuration(),
    )
        .into_response()
}

pub async fn steer_run_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!({"accepted": true}))).into_response()
}

pub async fn generate_preview_url_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({"url": "https://google.com"})),
    )
        .into_response()
}

pub async fn get_run_status(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match runs::list_items().into_iter().find(|r| r.id == id) {
        Some(_) => (
            StatusCode::OK,
            Json(arc_types::RunStatusResponse {
                id: id.clone(),
                status: arc_types::RunStatus::Running,
                error: None,
                queue_position: None,
            }),
        )
            .into_response(),
        None => ApiError::not_found("Run not found.").into_response(),
    }
}

pub async fn get_questions_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    let empty: Vec<arc_types::ApiQuestion> = vec![];
    (StatusCode::OK, Json(empty)).into_response()
}

pub async fn answer_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path((_id, _qid)): Path<(String, String)>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!({"accepted": true}))).into_response()
}

pub async fn run_events_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    ApiError::new(StatusCode::GONE, "Event stream closed.").into_response()
}

pub async fn checkpoint_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!(null))).into_response()
}

pub async fn context_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!({}))).into_response()
}

pub async fn cancel_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!({"cancelled": true}))).into_response()
}

pub async fn get_run_graph(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    // Use graphviz to render the demo DOT source
    let dot_source = "digraph demo {\n  graph [goal=\"Demo\"]\n  rankdir=LR\n  start [shape=Mdiamond, label=\"Start\"]\n  detect [label=\"Detect\\nDrift\"]\n  exit [shape=Msquare, label=\"Exit\"]\n  propose [label=\"Propose\\nChanges\"]\n  review [label=\"Review\\nChanges\"]\n  apply [label=\"Apply\\nChanges\"]\n  start -> detect\n  detect -> exit [label=\"No drift\"]\n  detect -> propose [label=\"Drift found\"]\n  propose -> review\n  review -> propose [label=\"Revise\"]\n  review -> apply [label=\"Accept\"]\n  apply -> exit\n}";

    let mut child = match tokio::process::Command::new("dot")
        .arg("-Tsvg")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return ApiError::new(StatusCode::BAD_GATEWAY, "Graphviz dot command not available.").into_response();
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(dot_source.as_bytes()).await;
    }

    match child.wait_with_output().await {
        Ok(output) if output.status.success() => (
            StatusCode::OK,
            [("content-type", "image/svg+xml")],
            output.stdout,
        )
            .into_response(),
        _ => ApiError::new(StatusCode::BAD_GATEWAY, "Dot rendering failed.").into_response(),
    }
}

pub async fn get_run_retro(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let retro = match id.as_str() {
        "run-1" => serde_json::json!({
            "run_id": "run-1",
            "workflow_name": "implement",
            "goal": "Add rate limiting to auth endpoints",
            "timestamp": "2026-02-28T14:32:00Z",
            "smoothness": "smooth",
            "intent": "Implement token-bucket rate limiting on /auth/login and /auth/register to prevent brute-force attacks.",
            "outcome": "Rate limiter deployed with configurable per-IP limits. Integration tests added. Redis-backed counter with sliding window.",
            "stages": [
                {"stage_id": "detect-drift", "stage_label": "Detect Drift", "status": "completed", "duration_ms": 72000, "retries": 0, "cost": 0.48, "files_touched": ["src/middleware/rate-limit.ts"]},
                {"stage_id": "propose-changes", "stage_label": "Propose Changes", "status": "completed", "duration_ms": 154000, "retries": 0, "cost": 1.12, "files_touched": ["src/middleware/rate-limit.ts", "src/routes/auth.ts", "src/config.ts"]},
                {"stage_id": "review-changes", "stage_label": "Review Changes", "status": "completed", "duration_ms": 45000, "retries": 0, "cost": 0.31, "files_touched": []},
                {"stage_id": "apply-changes", "stage_label": "Apply Changes", "status": "completed", "duration_ms": 118000, "retries": 0, "cost": 0.87, "files_touched": ["src/middleware/rate-limit.ts", "src/routes/auth.ts", "src/config.ts", "tests/rate-limit.test.ts"]}
            ],
            "stats": {
                "total_duration_ms": 389000,
                "total_cost": 2.78,
                "total_retries": 0,
                "files_touched": ["src/middleware/rate-limit.ts", "src/routes/auth.ts", "src/config.ts", "tests/rate-limit.test.ts"],
                "stages_completed": 4,
                "stages_failed": 0
            },
            "learnings": [
                {"category": "repo", "text": "Redis client is initialized lazily in src/infra/redis.ts -- reuse existing connection pool."},
                {"category": "code", "text": "Auth middleware chain order matters: rate-limit must run before JWT validation."}
            ],
            "friction_points": [],
            "open_items": [
                {"kind": "follow_up", "description": "Add rate-limit headers (X-RateLimit-Remaining) to response."}
            ]
        }),
        "run-2" => serde_json::json!({
            "run_id": "run-2",
            "workflow_name": "implement",
            "goal": "Migrate to React Router v7",
            "timestamp": "2026-02-28T10:15:00Z",
            "smoothness": "bumpy",
            "intent": "Upgrade react-router from v6 to v7, updating all route definitions and loader/action patterns to the new API.",
            "outcome": "Migration completed but required 3 retries in the apply stage due to breaking changes in nested route handling. All routes now use the v7 data API.",
            "stages": [
                {"stage_id": "detect-drift", "stage_label": "Detect Drift", "status": "completed", "duration_ms": 95000, "retries": 0, "cost": 0.62, "files_touched": ["package.json"]},
                {"stage_id": "propose-changes", "stage_label": "Propose Changes", "status": "completed", "duration_ms": 312000, "retries": 1, "cost": 2.45, "notes": "First proposal missed nested outlet patterns. Retry produced correct migration.", "files_touched": ["src/routes.ts", "src/app.tsx", "src/routes/dashboard.tsx", "src/routes/settings.tsx"]},
                {"stage_id": "review-changes", "stage_label": "Review Changes", "status": "completed", "duration_ms": 88000, "retries": 0, "cost": 0.54, "files_touched": []},
                {"stage_id": "apply-changes", "stage_label": "Apply Changes", "status": "completed", "duration_ms": 480000, "retries": 3, "cost": 3.21, "notes": "Type errors in nested layouts required multiple correction passes.", "files_touched": ["src/routes.ts", "src/app.tsx", "src/routes/dashboard.tsx", "src/routes/settings.tsx", "src/routes/profile.tsx", "tests/routes.test.tsx"]}
            ],
            "stats": {
                "total_duration_ms": 975000,
                "total_cost": 6.82,
                "total_retries": 4,
                "files_touched": ["package.json", "src/routes.ts", "src/app.tsx", "src/routes/dashboard.tsx", "src/routes/settings.tsx", "src/routes/profile.tsx", "tests/routes.test.tsx"],
                "stages_completed": 4,
                "stages_failed": 0
            },
            "learnings": [
                {"category": "workflow", "text": "Framework migration tasks benefit from running type-check after each stage, not just at the end."},
                {"category": "code", "text": "React Router v7 outlets require explicit type annotations for loader data in nested routes."},
                {"category": "tool", "text": "The codemod tool missed JSX spread patterns -- manual fixup was needed."}
            ],
            "friction_points": [
                {"kind": "retry", "description": "Nested route outlet types were incorrect on first 3 attempts.", "stage_id": "apply-changes"},
                {"kind": "wrong_approach", "description": "Initially tried to keep v6 compat layer, which created more issues than a clean migration.", "stage_id": "propose-changes"}
            ],
            "open_items": [
                {"kind": "tech_debt", "description": "Leftover v6 compat shims in src/utils/router-compat.ts should be deleted."},
                {"kind": "test_gap", "description": "No E2E coverage for the new nested layout error boundaries."}
            ]
        }),
        "run-6" => serde_json::json!({
            "run_id": "run-6",
            "workflow_name": "implement",
            "goal": "Add dark mode toggle",
            "timestamp": "2026-02-27T16:45:00Z",
            "smoothness": "effortless",
            "intent": "Add a theme toggle component to the dashboard header with system/light/dark options, persisting preference to localStorage.",
            "outcome": "Dark mode toggle shipped with smooth CSS transitions. All existing components already used CSS variables, so no style refactoring was needed.",
            "stages": [
                {"stage_id": "detect-drift", "stage_label": "Detect Drift", "status": "completed", "duration_ms": 42000, "retries": 0, "cost": 0.28, "files_touched": []},
                {"stage_id": "propose-changes", "stage_label": "Propose Changes", "status": "completed", "duration_ms": 98000, "retries": 0, "cost": 0.71, "files_touched": ["src/components/ThemeToggle.tsx", "src/hooks/useTheme.ts"]},
                {"stage_id": "apply-changes", "stage_label": "Apply Changes", "status": "completed", "duration_ms": 76000, "retries": 0, "cost": 0.52, "files_touched": ["src/components/ThemeToggle.tsx", "src/hooks/useTheme.ts", "src/layouts/Header.tsx"]}
            ],
            "stats": {
                "total_duration_ms": 216000,
                "total_cost": 1.51,
                "total_retries": 0,
                "files_touched": ["src/components/ThemeToggle.tsx", "src/hooks/useTheme.ts", "src/layouts/Header.tsx"],
                "stages_completed": 3,
                "stages_failed": 0
            },
            "learnings": [
                {"category": "repo", "text": "CSS variables are defined in src/styles/tokens.css and already support dark values."}
            ],
            "friction_points": [],
            "open_items": []
        }),
        "run-3" => serde_json::json!({
            "run_id": "run-3",
            "workflow_name": "fix_build",
            "goal": "Fix config parsing for nested values",
            "timestamp": "2026-02-27T09:20:00Z",
            "smoothness": "struggled",
            "intent": "Fix TOML config parser to handle deeply nested table arrays, which was causing silent data loss on certain pipeline configs.",
            "outcome": "Root cause identified as incorrect recursion depth limit in the TOML walker. Fix applied but exposed a second bug in default value merging that required additional changes.",
            "stages": [
                {"stage_id": "investigate", "stage_label": "Investigate", "status": "completed", "duration_ms": 340000, "retries": 2, "cost": 1.85, "notes": "First investigation looked at wrong parser path. Second attempt found the actual recursion limit.", "files_touched": ["src/config/parser.ts", "src/config/defaults.ts"]},
                {"stage_id": "propose-fix", "stage_label": "Propose Fix", "status": "completed", "duration_ms": 210000, "retries": 1, "cost": 1.42, "files_touched": ["src/config/parser.ts", "src/config/defaults.ts", "src/config/merge.ts"]},
                {"stage_id": "apply-fix", "stage_label": "Apply Fix", "status": "completed", "duration_ms": 185000, "retries": 1, "cost": 1.15, "failure_reason": "Initial fix broke the default value merging path. Required a second pass.", "files_touched": ["src/config/parser.ts", "src/config/defaults.ts", "src/config/merge.ts", "tests/config-parser.test.ts"]},
                {"stage_id": "verify", "stage_label": "Verify", "status": "completed", "duration_ms": 95000, "retries": 0, "cost": 0.55, "files_touched": []}
            ],
            "stats": {
                "total_duration_ms": 830000,
                "total_cost": 4.97,
                "total_retries": 4,
                "files_touched": ["src/config/parser.ts", "src/config/defaults.ts", "src/config/merge.ts", "tests/config-parser.test.ts"],
                "stages_completed": 4,
                "stages_failed": 0
            },
            "learnings": [
                {"category": "code", "text": "TOML walker in parser.ts has a hardcoded depth limit of 8 -- needs to be configurable."},
                {"category": "code", "text": "Default merging in merge.ts uses shallow spread, which silently drops nested keys."},
                {"category": "workflow", "text": "Bug fix pipelines should include a regression test stage before verification."}
            ],
            "friction_points": [
                {"kind": "wrong_approach", "description": "Initial investigation focused on the YAML compatibility layer instead of the TOML parser.", "stage_id": "investigate"},
                {"kind": "retry", "description": "Fix introduced a regression in default value merging that required rework.", "stage_id": "apply-fix"},
                {"kind": "ambiguity", "description": "Config schema docs were outdated, making it unclear which nesting depth was intended."}
            ],
            "open_items": [
                {"kind": "tech_debt", "description": "Remove the hardcoded depth limit in src/config/parser.ts and make it configurable."},
                {"kind": "investigation", "description": "Audit other parsers for similar shallow-spread bugs in merging logic."},
                {"kind": "test_gap", "description": "No tests for configs nested deeper than 4 levels."}
            ]
        }),
        "run-8" => serde_json::json!({
            "run_id": "run-8",
            "workflow_name": "implement",
            "goal": "Implement webhook retry logic",
            "timestamp": "2026-02-26T11:00:00Z",
            "smoothness": "smooth",
            "intent": "Add exponential backoff retry logic for failed webhook deliveries with configurable max attempts and dead-letter queue.",
            "outcome": "Webhook retry system implemented with exponential backoff (base 2s, max 5 retries). Failed deliveries route to SQS dead-letter queue. Dashboard shows retry status.",
            "stages": [
                {"stage_id": "detect-drift", "stage_label": "Detect Drift", "status": "completed", "duration_ms": 55000, "retries": 0, "cost": 0.35, "files_touched": []},
                {"stage_id": "propose-changes", "stage_label": "Propose Changes", "status": "completed", "duration_ms": 178000, "retries": 0, "cost": 1.28, "files_touched": ["src/webhooks/retry.ts", "src/webhooks/dlq.ts", "src/webhooks/dispatcher.ts"]},
                {"stage_id": "review-changes", "stage_label": "Review Changes", "status": "completed", "duration_ms": 62000, "retries": 0, "cost": 0.41, "files_touched": []},
                {"stage_id": "apply-changes", "stage_label": "Apply Changes", "status": "completed", "duration_ms": 145000, "retries": 1, "cost": 1.05, "notes": "Minor type fix needed on retry delay calculation.", "files_touched": ["src/webhooks/retry.ts", "src/webhooks/dlq.ts", "src/webhooks/dispatcher.ts", "tests/webhook-retry.test.ts"]}
            ],
            "stats": {
                "total_duration_ms": 440000,
                "total_cost": 3.09,
                "total_retries": 1,
                "files_touched": ["src/webhooks/retry.ts", "src/webhooks/dlq.ts", "src/webhooks/dispatcher.ts", "tests/webhook-retry.test.ts"],
                "stages_completed": 4,
                "stages_failed": 0
            },
            "learnings": [
                {"category": "repo", "text": "SQS client wrapper is in src/infra/sqs.ts with pre-configured DLQ ARNs per environment."},
                {"category": "code", "text": "Webhook dispatcher already had a hook point for retry logic via the onFailure callback."}
            ],
            "friction_points": [
                {"kind": "retry", "description": "Retry delay formula had an off-by-one in the exponent calculation.", "stage_id": "apply-changes"}
            ],
            "open_items": [
                {"kind": "follow_up", "description": "Add webhook retry metrics to the Grafana dashboard."},
                {"kind": "follow_up", "description": "Document the DLQ reprocessing procedure in the runbook."}
            ]
        }),
        _ => serde_json::json!(null),
    };
    (StatusCode::OK, Json(retro)).into_response()
}

// ── Workflows ──────────────────────────────────────────────────────────

pub async fn list_workflows(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(workflows::list_items(), &pagination)
}

pub async fn get_workflow(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match workflows::detail(&name) {
        Some(detail) => (StatusCode::OK, Json(detail)).into_response(),
        None => ApiError::not_found("Workflow not found.").into_response(),
    }
}

pub async fn list_workflow_runs(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    let items: Vec<_> = runs::list_items()
        .into_iter()
        .filter(|r| r.workflow == name)
        .collect();
    paginated_response(items, &pagination)
}

pub async fn trigger_workflow_run_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_name): Path<String>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": "demo-workflow-run"})),
    )
        .into_response()
}

// ── Verifications ──────────────────────────────────────────────────────

pub async fn list_verifications(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (StatusCode::OK, Json(verifications::categories())).into_response()
}

pub async fn get_verification_detail(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Response {
    match verifications::detail(&slug) {
        Some(detail) => (StatusCode::OK, Json(detail)).into_response(),
        None => ApiError::not_found("Verification not found.").into_response(),
    }
}

// ── Retros ─────────────────────────────────────────────────────────────

pub async fn list_retros(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(retros::list_items(), &pagination)
}

// ── Sessions ───────────────────────────────────────────────────────────

pub async fn list_sessions(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(sessions::groups(), &pagination)
}

pub async fn create_session_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": "demo-session-new"})),
    )
        .into_response()
}

pub async fn get_session(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match sessions::detail(&id) {
        Some(detail) => (StatusCode::OK, Json(detail)).into_response(),
        None => ApiError::not_found("Session not found.").into_response(),
    }
}

pub async fn send_message_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!({"accepted": true}))).into_response()
}

pub async fn session_events_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    // Return an empty SSE-like response
    ApiError::new(StatusCode::GONE, "Event stream closed.").into_response()
}

// ── Insights ───────────────────────────────────────────────────────────

pub async fn list_saved_queries(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(insights::saved_queries(), &pagination)
}

pub async fn save_query_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": "new-q", "name": "New Query", "sql": "SELECT 1"})),
    )
        .into_response()
}

pub async fn update_query_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({"id": "1", "name": "Updated", "sql": "SELECT 1"})),
    )
        .into_response()
}

pub async fn delete_query_stub(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub async fn execute_query_stub(
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

pub async fn list_query_history(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(insights::history(), &pagination)
}

// ── Settings ───────────────────────────────────────────────────────────

pub async fn get_settings(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (StatusCode::OK, Json(settings::groups())).into_response()
}

// ── Projects ───────────────────────────────────────────────────────────

pub async fn list_projects(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(projects::list_items(), &pagination)
}

pub async fn list_branches(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<String>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    paginated_response(projects::branches(), &pagination)
}

// ── Usage ──────────────────────────────────────────────────────────────

pub async fn get_aggregate_usage(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
) -> Response {
    (StatusCode::OK, Json(usage::aggregate())).into_response()
}

// ── Data modules ───────────────────────────────────────────────────────

mod runs {
    use arc_types::*;

    pub fn list_items() -> Vec<RunListItem> {
        vec![
            RunListItem {
                id: "run-1".into(),
                repo: "api-server".into(),
                title: "Add rate limiting to auth endpoints".into(),
                workflow: "implement".into(),
                status: RunListItemStatus::Working,
                number: None,
                additions: None,
                deletions: None,
                checks: vec![],
                elapsed_secs: Some(420.0),
                elapsed_warning: Some(false),
                resources: Some("4 CPU / 8 GB".into()),
                comments: Some(0),
                question: None,
                sandbox_id: Some("sb-a1b2c3d4".into()),
            },
            RunListItem {
                id: "run-2".into(),
                repo: "web-dashboard".into(),
                title: "Migrate to React Router v7".into(),
                workflow: "implement".into(),
                status: RunListItemStatus::Working,
                number: None,
                additions: None,
                deletions: None,
                checks: vec![],
                elapsed_secs: Some(8100.0),
                elapsed_warning: Some(false),
                resources: Some("8 CPU / 16 GB".into()),
                comments: Some(0),
                question: None,
                sandbox_id: Some("sb-e5f6g7h8".into()),
            },
            RunListItem {
                id: "run-3".into(),
                repo: "cli-tools".into(),
                title: "Fix config parsing for nested values".into(),
                workflow: "fix_build".into(),
                status: RunListItemStatus::Working,
                number: None,
                additions: None,
                deletions: None,
                checks: vec![],
                elapsed_secs: Some(2700.0),
                elapsed_warning: Some(false),
                resources: Some("2 CPU / 4 GB".into()),
                comments: Some(0),
                question: None,
                sandbox_id: Some("sb-i9j0k1l2".into()),
            },
            RunListItem {
                id: "run-4".into(),
                repo: "api-server".into(),
                title: "Update OpenAPI spec for v3".into(),
                workflow: "expand".into(),
                status: RunListItemStatus::Pending,
                number: None,
                additions: Some(567),
                deletions: Some(234),
                checks: vec![],
                elapsed_secs: Some(4320.0),
                elapsed_warning: Some(false),
                resources: None,
                comments: Some(0),
                question: Some("Accept or push for another round?".into()),
                sandbox_id: Some("sb-q7r8s9t0".into()),
            },
            RunListItem {
                id: "run-5".into(),
                repo: "shared-types".into(),
                title: "Add pipeline event types".into(),
                workflow: "implement".into(),
                status: RunListItemStatus::Pending,
                number: None,
                additions: Some(145),
                deletions: Some(23),
                checks: vec![],
                elapsed_secs: Some(1680.0),
                elapsed_warning: Some(false),
                resources: None,
                comments: Some(0),
                question: Some("Proceed from investigation to fix?".into()),
                sandbox_id: Some("sb-u1v2w3x4".into()),
            },
            RunListItem {
                id: "run-6".into(),
                repo: "web-dashboard".into(),
                title: "Add dark mode toggle".into(),
                workflow: "implement".into(),
                status: RunListItemStatus::Review,
                number: Some(889),
                additions: Some(234),
                deletions: Some(67),
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
                elapsed_secs: Some(2100.0),
                elapsed_warning: Some(false),
                resources: None,
                comments: Some(4),
                question: None,
                sandbox_id: Some("sb-m3n4o5p6".into()),
            },
            RunListItem {
                id: "run-7".into(),
                repo: "infrastructure".into(),
                title: "Terraform module for Redis cluster".into(),
                workflow: "implement".into(),
                status: RunListItemStatus::Review,
                number: Some(156),
                additions: Some(412),
                deletions: Some(0),
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
                elapsed_secs: Some(720.0),
                elapsed_warning: Some(false),
                resources: None,
                comments: Some(1),
                question: None,
                sandbox_id: Some("sb-y5z6a7b8".into()),
            },
            RunListItem {
                id: "run-8".into(),
                repo: "api-server".into(),
                title: "Implement webhook retry logic".into(),
                workflow: "implement".into(),
                status: RunListItemStatus::Merge,
                number: Some(1249),
                additions: Some(189),
                deletions: Some(45),
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
                elapsed_secs: Some(259200.0),
                elapsed_warning: Some(true),
                resources: None,
                comments: Some(7),
                question: None,
                sandbox_id: Some("sb-c9d0e1f2".into()),
            },
            RunListItem {
                id: "run-9".into(),
                repo: "cli-tools".into(),
                title: "Add --verbose flag to run command".into(),
                workflow: "expand".into(),
                status: RunListItemStatus::Merge,
                number: Some(430),
                additions: Some(56),
                deletions: Some(12),
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
                elapsed_secs: Some(3900.0),
                elapsed_warning: Some(false),
                resources: None,
                comments: Some(2),
                question: None,
                sandbox_id: Some("sb-g3h4i5j6".into()),
            },
            RunListItem {
                id: "run-10".into(),
                repo: "shared-types".into(),
                title: "Export utility type helpers".into(),
                workflow: "sync_drift".into(),
                status: RunListItemStatus::Merge,
                number: Some(76),
                additions: Some(34),
                deletions: Some(8),
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
                elapsed_secs: Some(2880.0),
                elapsed_warning: Some(false),
                resources: None,
                comments: Some(0),
                question: None,
                sandbox_id: Some("sb-k7l8m9n0".into()),
            },
        ]
    }

    pub fn stages() -> Vec<RunStage> {
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

    pub fn turns() -> Vec<StageTurn> {
        vec![
            StageTurn { kind: StageTurnKind::System, content: Some("You are a drift detection agent. Compare the production and staging environments and identify any configuration or code drift.".into()), tools: vec![] },
            StageTurn { kind: StageTurnKind::Assistant, content: Some("I'll start by loading the environment configurations for both production and staging to compare them.".into()), tools: vec![] },
            StageTurn {
                kind: StageTurnKind::Tool, content: None,
                tools: vec![
                    ToolUse { tool_name: "read_file".into(), args: r#"{ "path": "environments/production/config.toml" }"#.into(), result: "[redis]\nhost = \"redis-prod.internal\"\nport = 6379".into() },
                    ToolUse { tool_name: "read_file".into(), args: r#"{ "path": "environments/staging/config.toml" }"#.into(), result: "[redis]\nhost = \"redis-staging.internal\"\nport = 6379".into() },
                ],
            },
            StageTurn { kind: StageTurnKind::Assistant, content: Some("I've detected drift in 3 resources between production and staging:\n\n1. **redis.max_connections** — production has 200, staging has 100\n2. **redis.tls** — enabled in production, disabled in staging\n3. **iam.session_duration** — production uses 3600s, staging uses 1800s".into()), tools: vec![] },
        ]
    }

    pub fn files() -> RunFiles {
        RunFiles {
            checkpoints: vec![
                FileCheckpoint { id: "all".into(), label: "All changes".into() },
                FileCheckpoint { id: "cp-4".into(), label: "Checkpoint 4 — Apply Changes".into() },
                FileCheckpoint { id: "cp-3".into(), label: "Checkpoint 3 — Review Changes".into() },
                FileCheckpoint { id: "cp-2".into(), label: "Checkpoint 2 — Propose Changes".into() },
                FileCheckpoint { id: "cp-1".into(), label: "Checkpoint 1 — Detect Drift".into() },
            ],
            files: vec![
                FileDiff {
                    old_file: DiffFile { name: "src/commands/run.ts".into(), contents: "import { parseArgs } from \"node:util\";\nimport { loadConfig } from \"../config.js\";\nimport { execute } from \"../executor.js\";\n\ninterface RunOptions {\n  config: string;\n  dryRun: boolean;\n}\n\nexport async function run(argv: string[]) {\n  const { values } = parseArgs({\n    args: argv,\n    options: {\n      config: { type: \"string\", short: \"c\", default: \"arc.toml\" },\n      \"dry-run\": { type: \"boolean\", default: false },\n    },\n  });\n\n  const opts: RunOptions = {\n    config: values.config ?? \"arc.toml\",\n    dryRun: values[\"dry-run\"] ?? false,\n  };\n\n  const config = await loadConfig(opts.config);\n  const result = await execute(config, { dryRun: opts.dryRun });\n\n  if (result.success) {\n    console.log(\"Run completed successfully.\");\n  } else {\n    console.error(\"Run failed:\", result.error);\n    process.exitCode = 1;\n  }\n}\n".into() },
                    new_file: DiffFile { name: "src/commands/run.ts".into(), contents: "import { parseArgs } from \"node:util\";\nimport { loadConfig } from \"../config.js\";\nimport { execute } from \"../executor.js\";\nimport { createLogger, type Logger } from \"../logger.js\";\n\ninterface RunOptions {\n  config: string;\n  dryRun: boolean;\n  verbose: boolean;\n}\n\nexport async function run(argv: string[]) {\n  const { values } = parseArgs({\n    args: argv,\n    options: {\n      config: { type: \"string\", short: \"c\", default: \"arc.toml\" },\n      \"dry-run\": { type: \"boolean\", default: false },\n      verbose: { type: \"boolean\", short: \"v\", default: false },\n    },\n  });\n\n  const opts: RunOptions = {\n    config: values.config ?? \"arc.toml\",\n    dryRun: values[\"dry-run\"] ?? false,\n    verbose: values.verbose ?? false,\n  };\n\n  const logger: Logger = createLogger({ verbose: opts.verbose });\n\n  const config = await loadConfig(opts.config);\n  logger.debug(\"Loaded config from %s\", opts.config);\n\n  const result = await execute(config, { dryRun: opts.dryRun, logger });\n  logger.debug(\"Execution finished in %dms\", result.elapsed);\n\n  if (result.success) {\n    console.log(\"Run completed successfully.\");\n  } else {\n    console.error(\"Run failed:\", result.error);\n    process.exitCode = 1;\n  }\n}\n".into() },
                },
                FileDiff {
                    old_file: DiffFile { name: "src/logger.ts".into(), contents: "".into() },
                    new_file: DiffFile { name: "src/logger.ts".into(), contents: "export interface Logger {\n  info(message: string, ...args: unknown[]): void;\n  debug(message: string, ...args: unknown[]): void;\n  error(message: string, ...args: unknown[]): void;\n}\n\ninterface LoggerOptions {\n  verbose: boolean;\n}\n\nexport function createLogger({ verbose }: LoggerOptions): Logger {\n  return {\n    info(message, ...args) {\n      console.log(message, ...args);\n    },\n    debug(message, ...args) {\n      if (verbose) {\n        console.log(\"[debug]\", message, ...args);\n      }\n    },\n    error(message, ...args) {\n      console.error(message, ...args);\n    },\n  };\n}\n".into() },
                },
                FileDiff {
                    old_file: DiffFile { name: "src/executor.ts".into(), contents: "import type { Config } from \"./config.js\";\n\ninterface ExecuteOptions {\n  dryRun: boolean;\n}\n\ninterface ExecuteResult {\n  success: boolean;\n  error?: string;\n}\n\nexport async function execute(\n  config: Config,\n  options: ExecuteOptions,\n): Promise<ExecuteResult> {\n  if (options.dryRun) {\n    console.log(\"Dry run — skipping execution.\");\n    return { success: true };\n  }\n\n  try {\n    for (const step of config.steps) {\n      await step.run();\n    }\n    return { success: true };\n  } catch (err) {\n    const message = err instanceof Error ? err.message : String(err);\n    return { success: false, error: message };\n  }\n}\n".into() },
                    new_file: DiffFile { name: "src/executor.ts".into(), contents: "import type { Config } from \"./config.js\";\nimport type { Logger } from \"./logger.js\";\n\ninterface ExecuteOptions {\n  dryRun: boolean;\n  logger: Logger;\n}\n\ninterface ExecuteResult {\n  success: boolean;\n  elapsed: number;\n  error?: string;\n}\n\nexport async function execute(\n  config: Config,\n  options: ExecuteOptions,\n): Promise<ExecuteResult> {\n  const start = performance.now();\n\n  if (options.dryRun) {\n    options.logger.info(\"Dry run — skipping execution.\");\n    return { success: true, elapsed: performance.now() - start };\n  }\n\n  try {\n    for (const step of config.steps) {\n      options.logger.debug(\"Running step: %s\", step.name);\n      await step.run();\n    }\n    return { success: true, elapsed: performance.now() - start };\n  } catch (err) {\n    const message = err instanceof Error ? err.message : String(err);\n    return { success: false, elapsed: performance.now() - start, error: message };\n  }\n}\n".into() },
                },
            ],
            stats: DiffStats { additions: 567, deletions: 234 },
        }
    }

    pub fn usage() -> RunUsage {
        RunUsage {
            stages: vec![
                UsageStage {
                    stage: "Detect Drift".into(),
                    model: "Opus 4.6".into(),
                    input_tokens: 12480,
                    output_tokens: 3210,
                    runtime_secs: 72.0,
                    cost: 0.48,
                },
                UsageStage {
                    stage: "Propose Changes".into(),
                    model: "Gemini 3.1".into(),
                    input_tokens: 28640,
                    output_tokens: 8750,
                    runtime_secs: 154.0,
                    cost: 0.72,
                },
                UsageStage {
                    stage: "Review Changes".into(),
                    model: "Codex 5.3".into(),
                    input_tokens: 9120,
                    output_tokens: 2640,
                    runtime_secs: 45.0,
                    cost: 0.19,
                },
                UsageStage {
                    stage: "Apply Changes".into(),
                    model: "Opus 4.6".into(),
                    input_tokens: 21300,
                    output_tokens: 6480,
                    runtime_secs: 118.0,
                    cost: 0.87,
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
                    model: "Opus 4.6".into(),
                    stages: 2,
                    input_tokens: 33780,
                    output_tokens: 9690,
                    cost: 1.35,
                },
                UsageByModel {
                    model: "Gemini 3.1".into(),
                    stages: 1,
                    input_tokens: 28640,
                    output_tokens: 8750,
                    cost: 0.72,
                },
                UsageByModel {
                    model: "Codex 5.3".into(),
                    stages: 1,
                    input_tokens: 9120,
                    output_tokens: 2640,
                    cost: 0.19,
                },
            ],
        }
    }

    pub fn verifications() -> Vec<arc_types::RunVerification> {
        super::verifications::run_verifications()
    }

    pub fn configuration() -> String {
        r#"version = 1

[task]
goal = "Add rate limiting to auth endpoints"
repo = "api-server"
branch = "feature/rate-limiting"

[agent]
model = "claude-opus-4-6"
max_retries = 3

[verification]
enabled = true
categories = ["traceability", "readability", "reliability", "coverage"]
"#
        .to_string()
    }
}

mod usage {
    use arc_types::*;

    pub fn aggregate() -> AggregateUsage {
        AggregateUsage {
            total_runs: 9,
            total_input_tokens: 643_860,
            total_output_tokens: 189_720,
            total_cost: 20.34,
            total_runtime_secs: 3_501.0,
            by_model: vec![
                UsageByModel {
                    model: "Opus 4.6".into(),
                    stages: 18,
                    input_tokens: 304_020,
                    output_tokens: 87_210,
                    cost: 12.15,
                },
                UsageByModel {
                    model: "Gemini 3.1".into(),
                    stages: 9,
                    input_tokens: 257_760,
                    output_tokens: 78_750,
                    cost: 6.48,
                },
                UsageByModel {
                    model: "Codex 5.3".into(),
                    stages: 9,
                    input_tokens: 82_080,
                    output_tokens: 23_760,
                    cost: 1.71,
                },
            ],
        }
    }
}

mod workflows {
    use arc_types::*;

    pub fn list_items() -> Vec<WorkflowListItem> {
        vec![
            WorkflowListItem {
                name: "Fix Build".into(),
                slug: "fix_build".into(),
                filename: "fix_build.dot".into(),
                last_run: Some("2 hours ago".into()),
                schedule: None,
                next_run: None,
            },
            WorkflowListItem {
                name: "Implement Feature".into(),
                slug: "implement".into(),
                filename: "implement.dot".into(),
                last_run: Some("4 days ago".into()),
                schedule: None,
                next_run: None,
            },
            WorkflowListItem {
                name: "Sync Drift".into(),
                slug: "sync_drift".into(),
                filename: "sync_drift.dot".into(),
                last_run: Some("1 day ago".into()),
                schedule: None,
                next_run: None,
            },
            WorkflowListItem {
                name: "Expand Product".into(),
                slug: "expand".into(),
                filename: "expand.dot".into(),
                last_run: Some("2 weeks ago".into()),
                schedule: None,
                next_run: None,
            },
        ]
    }

    pub fn detail(name: &str) -> Option<WorkflowDetail> {
        let items = [
            WorkflowDetail {
                title: "Fix Build".into(), slug: "fix_build".into(), filename: "fix_build.dot".into(),
                description: "Automatically diagnoses and fixes CI build failures by analyzing error logs, identifying root causes, and applying targeted code changes.".into(),
                config: r#"version = 1
goal = "Diagnose and fix CI build failures"
graph = "fix_build.dot"

[llm]
model = "claude-sonnet"

[vars]
repo_url = "https://github.com/org/service"
branch = "main"

[execution]
environment = "daytona"

[execution.daytona.sandbox]
auto_stop_interval = 60

[execution.daytona.sandbox.labels]
project = "fix-build"

[execution.daytona.snapshot]
name = "fix-build-dev"
cpu = 4
memory = 8
disk = 10
"#.into(),
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
                title: "Implement Feature".into(), slug: "implement".into(), filename: "implement.dot".into(),
                description: "Generates production-ready code from a technical blueprint, including tests, documentation, and a pull request ready for review.".into(),
                config: r#"version = 1
goal = "Implement feature from technical blueprint"
graph = "implement.dot"

[llm]
model = "claude-sonnet"

[vars]
spec_path = "specs/feature.md"
test_framework = "vitest"

[setup]
commands = ["bun install", "bun run typecheck"]
timeout_ms = 120000

[execution]
environment = "daytona"

[execution.daytona.sandbox]
auto_stop_interval = 120

[execution.daytona.sandbox.labels]
project = "implement"
team = "engineering"

[execution.daytona.snapshot]
name = "implement-dev"
cpu = 4
memory = 8
disk = 20
"#.into(),
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
                title: "Sync Drift".into(), slug: "sync_drift".into(), filename: "sync_drift.dot".into(),
                description: "Detects configuration and code drift between environments, then generates reconciliation patches to bring everything back in sync.".into(),
                config: r#"version = 1
goal = "Detect and reconcile configuration drift across environments"
graph = "sync_drift.dot"

[llm]
model = "claude-sonnet"

[vars]
source_env = "production"
target_env = "staging"
drift_threshold = "warn"

[execution]
environment = "daytona"

[execution.daytona.sandbox]
auto_stop_interval = 120

[execution.daytona.sandbox.labels]
project = "sync-drift"
team = "platform"

[execution.daytona.snapshot]
name = "sync-drift-dev"
cpu = 2
memory = 4
disk = 10
"#.into(),
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
                title: "Expand Product".into(), slug: "expand".into(), filename: "expand.dot".into(),
                description: "Evolves the product by analyzing usage patterns and specifications to propose and implement incremental improvements.".into(),
                config: r#"version = 1
goal = "Propose and implement incremental product improvements"
graph = "expand.dot"

[llm]
model = "claude-sonnet"

[vars]
analytics_window = "30d"
min_confidence = "0.8"

[execution]
environment = "daytona"

[execution.daytona.sandbox]
auto_stop_interval = 180

[execution.daytona.sandbox.labels]
project = "expand"
team = "product"

[execution.daytona.snapshot]
name = "expand-dev"
cpu = 2
memory = 4
disk = 10
"#.into(),
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
    use arc_types::*;

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
        type_: Option<VerificationType>,
        mode: VerificationMode,
        f1: Option<f64>,
        pass_at_1: Option<f64>,
        evaluations: &'static [EvaluationResult],
        // Run-level status
        run_status: VerificationStatus,
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
        result: VerificationStatus,
        timestamp: &'static str,
    }

    const DEFAULT_RECENT_RESULTS: &[RecentResultDef] = &[
        RecentResultDef {
            run_id: "run-047",
            run_title: "PR #312 \u{2014} Add OAuth2 PKCE flow",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "2h ago",
        },
        RecentResultDef {
            run_id: "run-046",
            run_title: "PR #311 \u{2014} Update rate limiter config",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "5h ago",
        },
        RecentResultDef {
            run_id: "run-044",
            run_title: "PR #309 \u{2014} Migrate to pnpm",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "1d ago",
        },
        RecentResultDef {
            run_id: "run-042",
            run_title: "PR #307 \u{2014} Fix session timeout",
            workflow: "fix_build",
            result: VerificationStatus::Pass,
            timestamp: "2d ago",
        },
        RecentResultDef {
            run_id: "run-040",
            run_title: "PR #305 \u{2014} Add webhook retries",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "3d ago",
        },
    ];

    const MOTIVATION_RESULTS: &[RecentResultDef] = &[
        RecentResultDef {
            run_id: "run-047",
            run_title: "PR #312 \u{2014} Add OAuth2 PKCE flow",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "2h ago",
        },
        RecentResultDef {
            run_id: "run-046",
            run_title: "PR #311 \u{2014} Update rate limiter config",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "5h ago",
        },
        RecentResultDef {
            run_id: "run-044",
            run_title: "PR #309 \u{2014} Migrate to pnpm",
            workflow: "code_review",
            result: VerificationStatus::Fail,
            timestamp: "1d ago",
        },
        RecentResultDef {
            run_id: "run-042",
            run_title: "PR #307 \u{2014} Fix session timeout",
            workflow: "fix_build",
            result: VerificationStatus::Pass,
            timestamp: "2d ago",
        },
        RecentResultDef {
            run_id: "run-040",
            run_title: "PR #305 \u{2014} Add webhook retries",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "3d ago",
        },
    ];

    const DOCUMENTATION_RESULTS: &[RecentResultDef] = &[
        RecentResultDef {
            run_id: "run-047",
            run_title: "PR #312 \u{2014} Add OAuth2 PKCE flow",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "2h ago",
        },
        RecentResultDef {
            run_id: "run-046",
            run_title: "PR #311 \u{2014} Update rate limiter config",
            workflow: "code_review",
            result: VerificationStatus::Fail,
            timestamp: "5h ago",
        },
        RecentResultDef {
            run_id: "run-044",
            run_title: "PR #309 \u{2014} Migrate to pnpm",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "1d ago",
        },
        RecentResultDef {
            run_id: "run-042",
            run_title: "PR #307 \u{2014} Fix session timeout",
            workflow: "fix_build",
            result: VerificationStatus::Pass,
            timestamp: "2d ago",
        },
        RecentResultDef {
            run_id: "run-040",
            run_title: "PR #305 \u{2014} Add webhook retries",
            workflow: "code_review",
            result: VerificationStatus::Fail,
            timestamp: "3d ago",
        },
    ];

    const ROLLOUT_ROLLBACK_RESULTS: &[RecentResultDef] = &[
        RecentResultDef {
            run_id: "run-047",
            run_title: "PR #312 \u{2014} Add OAuth2 PKCE flow",
            workflow: "code_review",
            result: VerificationStatus::Fail,
            timestamp: "2h ago",
        },
        RecentResultDef {
            run_id: "run-046",
            run_title: "PR #311 \u{2014} Update rate limiter config",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "5h ago",
        },
        RecentResultDef {
            run_id: "run-044",
            run_title: "PR #309 \u{2014} Migrate to pnpm",
            workflow: "code_review",
            result: VerificationStatus::Fail,
            timestamp: "1d ago",
        },
        RecentResultDef {
            run_id: "run-042",
            run_title: "PR #307 \u{2014} Fix session timeout",
            workflow: "fix_build",
            result: VerificationStatus::Fail,
            timestamp: "2d ago",
        },
        RecentResultDef {
            run_id: "run-040",
            run_title: "PR #305 \u{2014} Add webhook retries",
            workflow: "code_review",
            result: VerificationStatus::Pass,
            timestamp: "3d ago",
        },
    ];

    use EvaluationResult::{Fail as F, Pass as P};

    const ALL_CATEGORIES: &[CategoryDef] = &[
        CategoryDef {
            name: "Traceability",
            question: "Do we understand what this change is and why we're making it?",
            controls: &[
                ControlDef {
                    name: "Motivation", slug: "motivation",
                    description: "Origin of proposal identified",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Active,
                    f1: Some(0.87), pass_at_1: Some(0.82),
                    evaluations: &[P, P, F, P, P, P, P, F, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Verifies that every change traces back to a clear origin \u{2014} whether a ticket, RFC, customer request, or incident. Without documented motivation, reviewers lack context for evaluating whether the change is appropriate.",
                    checks: &["PR body or linked issue explains why the change is needed", "Commit messages reference a ticket or context", "No orphaned changes without traceable origin"],
                    pass_example: "PR links to JIRA-1234 and explains the user-facing pain point being resolved.",
                    fail_example: "PR description is empty or says only 'fix stuff'.",
                    recent_results: Some(MOTIVATION_RESULTS),
                },
                ControlDef {
                    name: "Specifications", slug: "specifications",
                    description: "Requirements written down",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Active,
                    f1: Some(0.83), pass_at_1: Some(0.78),
                    evaluations: &[P, F, P, P, P, F, P, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Checks that functional and non-functional requirements are written down before implementation begins. Specifications prevent scope creep and ensure everyone agrees on what done looks like.",
                    checks: &["Acceptance criteria listed in the issue or PR", "Edge cases documented", "Non-functional requirements (performance, security) stated when relevant"],
                    pass_example: "Issue includes acceptance criteria with three testable scenarios.",
                    fail_example: "Issue body says 'implement the feature' with no acceptance criteria.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Documentation", slug: "documentation",
                    description: "Developer and user docs added",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Active,
                    f1: Some(0.79), pass_at_1: Some(0.74),
                    evaluations: &[P, P, P, F, P, P, F, P, P, F],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Ensures developer-facing and user-facing documentation is added or updated alongside code changes. Stale docs degrade team velocity and increase onboarding cost.",
                    checks: &["README or docs updated for new features", "API documentation reflects endpoint changes", "Inline comments for non-obvious logic"],
                    pass_example: "New API endpoint has corresponding OpenAPI spec update and usage example in docs.",
                    fail_example: "New CLI flag added with no mention in README or --help text.",
                    recent_results: Some(DOCUMENTATION_RESULTS),
                },
                ControlDef {
                    name: "Minimization", slug: "minimization",
                    description: "No extraneous changes",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Evaluate,
                    f1: Some(0.72), pass_at_1: Some(0.68),
                    evaluations: &[P, F, P, F, P, P, F, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Flags extraneous changes that inflate the diff \u{2014} formatting-only edits, unrelated refactors, or drive-by fixes. Keeping PRs focused improves review quality and reduces revert risk.",
                    checks: &["No unrelated formatting or whitespace changes", "Refactors separated from feature work", "Each commit addresses a single concern"],
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
                    name: "Formatting", slug: "formatting",
                    description: "Code layout matches standard",
                    type_: Some(VerificationType::Automated), mode: VerificationMode::Active,
                    f1: Some(0.99), pass_at_1: Some(0.98),
                    evaluations: &[P, P, P, P, P, P, P, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Validates that code layout conforms to the project's formatting standard (e.g., Prettier, rustfmt). Automated formatting removes subjective style debates from code review.",
                    checks: &["All files pass the project formatter", "No manual formatting overrides without justification"],
                    pass_example: "All changed files pass `prettier --check` and `rustfmt --check`.",
                    fail_example: "Several files have inconsistent indentation that the formatter would fix.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Linting", slug: "linting",
                    description: "Linter issues resolved",
                    type_: Some(VerificationType::Automated), mode: VerificationMode::Active,
                    f1: Some(0.98), pass_at_1: Some(0.97),
                    evaluations: &[P, P, P, P, P, P, P, P, F, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Confirms that static analysis findings are resolved. Linter warnings left unaddressed accumulate into tech debt and mask real issues.",
                    checks: &["No new linter warnings introduced", "Existing warnings not suppressed without explanation", "Lint config not weakened"],
                    pass_example: "ESLint and Clippy pass with zero warnings on changed files.",
                    fail_example: "New `// eslint-disable-next-line` added to suppress a legitimate warning.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Style", slug: "style",
                    description: "House style applied",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Active,
                    f1: Some(0.81), pass_at_1: Some(0.76),
                    evaluations: &[P, F, P, P, P, P, F, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Evaluates whether the code follows the team's house style conventions beyond what automated formatters catch \u{2014} naming, file organization, import ordering, and idiomatic patterns.",
                    checks: &["Naming conventions followed (camelCase, snake_case as appropriate)", "Import ordering matches project convention", "Idiomatic patterns used for the language"],
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
                    name: "Completeness", slug: "completeness",
                    description: "Implementation covers requirements",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Active,
                    f1: Some(0.76), pass_at_1: Some(0.71),
                    evaluations: &[P, P, F, P, F, P, P, P, F, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Checks that the implementation fully covers the specified requirements. Partial implementations ship broken experiences and create follow-up tickets that could have been avoided.",
                    checks: &["All acceptance criteria addressed", "Edge cases handled", "Error states implemented"],
                    pass_example: "Feature handles all three specified user roles with appropriate permissions.",
                    fail_example: "Only the happy path is implemented; error and empty states are missing.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Defects", slug: "defects",
                    description: "Potential or likely bugs remediated",
                    type_: Some(VerificationType::AiAnalysis), mode: VerificationMode::Active,
                    f1: Some(0.84), pass_at_1: Some(0.79),
                    evaluations: &[P, P, P, F, P, P, P, P, P, F],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Identifies potential or likely bugs through static analysis and AI review. Catching defects before merge is orders of magnitude cheaper than finding them in production.",
                    checks: &["No off-by-one errors in loops or slices", "Null/undefined handled at boundaries", "Race conditions considered in async code"],
                    pass_example: "API handler validates input, handles missing fields gracefully, and returns appropriate HTTP status codes.",
                    fail_example: "Array index accessed without bounds check; crashes on empty input.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Performance", slug: "performance",
                    description: "Hot path impact identified",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Evaluate,
                    f1: Some(0.69), pass_at_1: Some(0.63),
                    evaluations: &[F, P, P, F, P, F, P, P, F, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Assesses whether the change impacts hot paths or introduces algorithmic regressions. Performance problems that ship to production are expensive to diagnose and fix.",
                    checks: &["No N+1 queries introduced", "Large collections not processed synchronously", "Caching considered for repeated expensive operations"],
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
                    name: "Test Coverage", slug: "test-coverage",
                    description: "Production code exercised by unit tests",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.95), pass_at_1: Some(0.93),
                    evaluations: &[P, P, P, P, P, P, F, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Measures whether production code is exercised by automated tests. Coverage gaps mean regressions can ship undetected.",
                    checks: &["New code has corresponding unit tests", "Coverage does not decrease", "Critical paths have integration tests"],
                    pass_example: "New service method has 6 unit tests covering happy path, error cases, and edge cases.",
                    fail_example: "New 200-line module has zero test files.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Test Quality", slug: "test-quality",
                    description: "Tests are robust and clear",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Evaluate,
                    f1: Some(0.71), pass_at_1: Some(0.65),
                    evaluations: &[P, F, F, P, P, F, P, F, P, P],
                    run_status: VerificationStatus::Fail,
                    detail_description: "Evaluates whether tests are robust, readable, and actually verify behavior rather than implementation details. Low-quality tests give false confidence.",
                    checks: &["Tests verify behavior, not implementation", "Assertions are specific and meaningful", "Tests are independent and deterministic"],
                    pass_example: "Tests assert on API response shape and status codes, not on internal method call counts.",
                    fail_example: "Tests mock every dependency and only verify that mocks were called.",
                    recent_results: None,
                },
                ControlDef {
                    name: "E2E Coverage", slug: "e2e-coverage",
                    description: "Browser automation exercises UX",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.91), pass_at_1: Some(0.88),
                    evaluations: &[P, P, P, F, P, P, P, P, P, P],
                    run_status: VerificationStatus::Na,
                    detail_description: "Checks that user-facing workflows are exercised by end-to-end browser automation. E2E tests catch integration issues that unit tests miss.",
                    checks: &["Critical user flows have Playwright/Cypress tests", "E2E tests run in CI", "No flaky E2E tests introduced"],
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
                    name: "Architecture", slug: "architecture",
                    description: "Layering and dependency graph meets design",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.88), pass_at_1: Some(0.84),
                    evaluations: &[P, P, P, P, F, P, P, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Validates that layering and dependency directions conform to the project's architectural design. Architectural violations compound over time and make systems harder to evolve.",
                    checks: &["Dependencies point inward (domain doesn't depend on infra)", "No circular dependencies introduced", "Module boundaries respected"],
                    pass_example: "New repository implementation depends on domain interfaces, not the other way around.",
                    fail_example: "Domain model imports directly from the HTTP framework package.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Interfaces", slug: "interfaces",
                    description: "",
                    type_: None, mode: VerificationMode::Disabled,
                    f1: None, pass_at_1: None,
                    evaluations: &[],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Reviews public API surfaces for clarity, consistency, and backward compatibility. Interfaces are contracts \u{2014} once published, they're expensive to change.",
                    checks: &["Public API types are well-defined", "Breaking changes documented", "Consistent naming across endpoints"],
                    pass_example: "New endpoint follows existing naming and error format conventions.",
                    fail_example: "New endpoint uses different error format than all other endpoints.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Duplication", slug: "duplication",
                    description: "Similar and identical code blocks identified",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.96), pass_at_1: Some(0.94),
                    evaluations: &[P, P, P, P, P, P, P, F, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Detects similar or identical code blocks that could be consolidated. Duplication increases maintenance burden and creates inconsistency risk.",
                    checks: &["No copy-pasted logic across files", "Shared utilities used for common patterns", "Similar test setup consolidated"],
                    pass_example: "Date formatting logic extracted into a shared utility used by 4 components.",
                    fail_example: "Same 15-line validation function copy-pasted into three different handlers.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Simplicity", slug: "simplicity",
                    description: "Extra review for reducing complexity",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Active,
                    f1: Some(0.74), pass_at_1: Some(0.69),
                    evaluations: &[P, F, P, P, F, P, P, F, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Flags unnecessarily complex code that could be simplified without changing behavior. Simpler code is easier to review, debug, and extend.",
                    checks: &["No premature abstractions", "Control flow is straightforward", "Functions are focused and short"],
                    pass_example: "Conditional logic uses early returns instead of deeply nested if-else chains.",
                    fail_example: "Three-level generic abstraction for a function called in one place.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Dead Code", slug: "dead-code",
                    description: "Unexecuted code and dependencies removed",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.93), pass_at_1: Some(0.90),
                    evaluations: &[P, P, P, P, P, F, P, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Identifies unexecuted code paths and unused dependencies. Dead code misleads readers and bloats bundles.",
                    checks: &["No unreachable code paths", "Unused imports and variables removed", "Deprecated functions removed if no longer called"],
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
                    name: "Vulnerabilities", slug: "vulnerabilities",
                    description: "Security issues are remediated",
                    type_: Some(VerificationType::AiAnalysis), mode: VerificationMode::Active,
                    f1: Some(0.86), pass_at_1: Some(0.81),
                    evaluations: &[P, P, F, P, P, P, P, P, F, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Scans for known security vulnerabilities using both AI analysis and static scanning tools. Shipping known vulnerabilities exposes users and the organization to risk.",
                    checks: &["No SQL injection or XSS vectors", "User input sanitized at boundaries", "Authentication/authorization checks present"],
                    pass_example: "User input passed through parameterized queries; HTML output escaped.",
                    fail_example: "Raw SQL string concatenation with user-supplied values.",
                    recent_results: None,
                },
                ControlDef {
                    name: "IaC Scanning", slug: "iac-scanning",
                    description: "",
                    type_: None, mode: VerificationMode::Disabled,
                    f1: None, pass_at_1: None,
                    evaluations: &[],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Validates infrastructure-as-code definitions against security best practices. Misconfigured infrastructure is a leading cause of data breaches.",
                    checks: &["No publicly accessible storage buckets", "Encryption at rest enabled", "Least-privilege IAM policies"],
                    pass_example: "Terraform module creates S3 bucket with encryption, versioning, and private ACL.",
                    fail_example: "CloudFormation template creates an RDS instance with no encryption and public accessibility.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Dependency Alerts", slug: "dependency-alerts",
                    description: "Known CVEs are patched",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.97), pass_at_1: Some(0.95),
                    evaluations: &[P, P, P, P, P, P, P, P, P, F],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Checks that third-party dependencies are free from known CVEs. Vulnerable dependencies are an easy attack vector that automated tools can detect.",
                    checks: &["No dependencies with known critical CVEs", "Lock file updated to patched versions", "Unused dependencies removed"],
                    pass_example: "Dependabot alert resolved by updating lodash from 4.17.20 to 4.17.21.",
                    fail_example: "Package.json pins a version of axios with a known SSRF vulnerability.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Security Controls", slug: "security-controls",
                    description: "Organization standards applied",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Active,
                    f1: Some(0.80), pass_at_1: Some(0.75),
                    evaluations: &[P, P, F, P, P, F, P, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Verifies that organization-specific security standards are applied \u{2014} rate limiting, audit logging, CORS policies, and secret management.",
                    checks: &["Secrets not hardcoded in source", "Rate limiting on public endpoints", "Audit logging for sensitive operations"],
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
                    name: "Compatibility", slug: "compatibility",
                    description: "Breaking changes are avoided",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.89), pass_at_1: Some(0.85),
                    evaluations: &[P, P, P, P, F, P, P, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Detects breaking changes in APIs, database schemas, or wire formats that could disrupt consumers. Breaking changes require coordination that surprises prevent.",
                    checks: &["No removed or renamed public API fields", "Database migrations are backward-compatible", "Wire format changes are additive"],
                    pass_example: "New field added to API response; no existing fields removed or renamed.",
                    fail_example: "Column renamed in migration while old code is still deployed.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Rollout / Rollback", slug: "rollout-rollback",
                    description: "Known rollback plan if deploy fails",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Evaluate,
                    f1: Some(0.66), pass_at_1: Some(0.60),
                    evaluations: &[F, P, F, P, F, P, P, F, P, F],
                    run_status: VerificationStatus::Fail,
                    detail_description: "Confirms that the change has a clear deployment plan and can be safely rolled back if issues arise. Every production deploy should be reversible.",
                    checks: &["Feature flag available for gradual rollout", "Database migration is reversible", "Rollback procedure documented"],
                    pass_example: "Feature behind a LaunchDarkly flag with 10% initial rollout and documented rollback steps.",
                    fail_example: "Irreversible database migration with no rollback plan.",
                    recent_results: Some(ROLLOUT_ROLLBACK_RESULTS),
                },
                ControlDef {
                    name: "Observability", slug: "observability",
                    description: "Logging, metrics, tracing instrumented",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Evaluate,
                    f1: Some(0.73), pass_at_1: Some(0.67),
                    evaluations: &[P, F, P, F, P, P, F, P, F, P],
                    run_status: VerificationStatus::Fail,
                    detail_description: "Ensures that logging, metrics, and tracing are instrumented for new code paths. Without observability, production issues are invisible until users report them.",
                    checks: &["Structured logging for new operations", "Metrics emitted for key business events", "Distributed tracing propagated"],
                    pass_example: "New payment endpoint logs transaction IDs, emits latency metrics, and propagates trace context.",
                    fail_example: "New background job has no logging or metrics; failures are silent.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Cost", slug: "cost",
                    description: "Tech ops costs estimated",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Evaluate,
                    f1: Some(0.78), pass_at_1: Some(0.72),
                    evaluations: &[P, P, F, P, F, P, P, F, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Estimates the infrastructure and operational cost impact of the change. Unchecked cost growth erodes margins and can cause budget surprises.",
                    checks: &["New infrastructure resources sized appropriately", "No unbounded resource consumption", "Cost estimate provided for significant changes"],
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
                    name: "Change Control", slug: "change-control",
                    description: "Separation of Duties policy met",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.94), pass_at_1: Some(0.91),
                    evaluations: &[P, P, P, P, P, P, P, P, F, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Validates that separation-of-duties policies are met \u{2014} the author is not the sole reviewer, approvals are obtained, and the change went through the proper process.",
                    checks: &["PR has at least one approval from non-author", "Required reviewers have signed off", "No self-merging without policy exception"],
                    pass_example: "PR approved by two team members before merge; CI checks all green.",
                    fail_example: "Author approved and merged their own PR with no other reviewers.",
                    recent_results: None,
                },
                ControlDef {
                    name: "AI Governance", slug: "ai-governance",
                    description: "AI involvement was acceptable",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.85), pass_at_1: Some(0.80),
                    evaluations: &[P, P, P, F, P, P, P, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Checks that AI-generated or AI-assisted code meets the organization's governance requirements \u{2014} attribution, review depth, and acceptable use.",
                    checks: &["AI-generated code clearly attributed", "Human review of AI suggestions documented", "AI usage within acceptable-use policy"],
                    pass_example: "PR notes that implementation was AI-assisted; human reviewer verified logic and tests.",
                    fail_example: "Entire module generated by AI with no human review or attribution.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Privacy", slug: "privacy",
                    description: "PII is identified and handled to standards",
                    type_: Some(VerificationType::Ai), mode: VerificationMode::Active,
                    f1: Some(0.77), pass_at_1: Some(0.72),
                    evaluations: &[P, F, P, P, P, F, P, P, P, F],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Ensures that personally identifiable information (PII) is identified, classified, and handled according to privacy standards (GDPR, CCPA).",
                    checks: &["PII fields identified and documented", "Data retention policies applied", "Consent mechanisms in place for data collection"],
                    pass_example: "New user profile endpoint masks email in logs and respects data deletion requests.",
                    fail_example: "User email addresses logged in plaintext to application logs.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Accessibility", slug: "accessibility",
                    description: "Software meets accessibility requirements",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.90), pass_at_1: Some(0.87),
                    evaluations: &[P, P, P, P, P, F, P, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Verifies that UI changes meet accessibility requirements (WCAG 2.1 AA). Inaccessible software excludes users and creates legal risk.",
                    checks: &["Semantic HTML elements used", "ARIA labels present on interactive elements", "Color contrast meets WCAG AA standards"],
                    pass_example: "New modal uses <dialog>, has aria-labelledby, and focus is trapped within.",
                    fail_example: "Custom dropdown built with <div> elements, no keyboard navigation, no ARIA roles.",
                    recent_results: None,
                },
                ControlDef {
                    name: "Licensing", slug: "licensing",
                    description: "Supply chain meets IP policy",
                    type_: Some(VerificationType::Analysis), mode: VerificationMode::Active,
                    f1: Some(0.96), pass_at_1: Some(0.93),
                    evaluations: &[P, P, P, P, P, P, P, P, P, P],
                    run_status: VerificationStatus::Pass,
                    detail_description: "Ensures that all third-party dependencies comply with the organization's intellectual property policy. License violations can have severe legal consequences.",
                    checks: &["No GPL-licensed dependencies in proprietary code", "License file present for new dependencies", "Supply chain attestation where required"],
                    pass_example: "New dependency uses MIT license; added to approved dependency list.",
                    fail_example: "AGPL-licensed library added to a closed-source commercial product.",
                    recent_results: None,
                },
            ],
        },
    ];

    // ── Helpers ──────────────────────────────────────────────────────────

    fn recent_results_from_def(defs: &[RecentResultDef]) -> Vec<RecentControlResult> {
        defs.iter()
            .map(|r| RecentControlResult {
                run_id: r.run_id.into(),
                run_title: r.run_title.into(),
                workflow: r.workflow.into(),
                result: r.result,
                timestamp: r.timestamp.into(),
            })
            .collect()
    }

    fn category_run_status(cat: &CategoryDef) -> VerificationStatus {
        if cat
            .controls
            .iter()
            .any(|c| c.run_status == VerificationStatus::Fail)
        {
            VerificationStatus::Fail
        } else if cat
            .controls
            .iter()
            .all(|c| c.run_status == VerificationStatus::Na)
        {
            VerificationStatus::Na
        } else {
            VerificationStatus::Pass
        }
    }

    // ── Public API ──────────────────────────────────────────────────────

    pub fn categories() -> Vec<VerificationCategory> {
        ALL_CATEGORIES
            .iter()
            .map(|cat| VerificationCategory {
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

    pub fn detail(slug: &str) -> Option<VerificationDetailResponse> {
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
                            type_: s.type_,
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
                            type_: ctrl.type_,
                            category: cat.name.into(),
                        },
                        performance: ControlPerformance {
                            mode: ctrl.mode,
                            f1: ctrl.f1,
                            pass_at_1: ctrl.pass_at_1,
                            evaluations: ctrl.evaluations.to_vec(),
                        },
                        control_detail: ControlDetail {
                            description: ctrl.detail_description.into(),
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

    pub fn run_verifications() -> Vec<RunVerification> {
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
                        description: c.description.into(),
                        type_: c.type_,
                        status: c.run_status,
                    })
                    .collect(),
            })
            .collect()
    }
}

mod retros {
    use arc_types::*;

    pub fn list_items() -> Vec<RetroListItem> {
        vec![
            RetroListItem {
                run_id: "run-1".into(),
                workflow_name: "implement".into(),
                goal: "Add rate limiting to auth endpoints".into(),
                timestamp: "2026-02-28T14:32:00Z".into(),
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
                run_id: "run-2".into(),
                workflow_name: "implement".into(),
                goal: "Migrate to React Router v7".into(),
                timestamp: "2026-02-28T10:15:00Z".into(),
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
                run_id: "run-6".into(),
                workflow_name: "implement".into(),
                goal: "Add dark mode toggle".into(),
                timestamp: "2026-02-27T16:45:00Z".into(),
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
                run_id: "run-3".into(),
                workflow_name: "fix_build".into(),
                goal: "Fix config parsing for nested values".into(),
                timestamp: "2026-02-27T09:20:00Z".into(),
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
                run_id: "run-8".into(),
                workflow_name: "implement".into(),
                goal: "Implement webhook retry logic".into(),
                timestamp: "2026-02-26T11:00:00Z".into(),
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
    use arc_types::*;

    pub fn groups() -> Vec<SessionGroup> {
        vec![
            SessionGroup {
                label: "Today".into(),
                sessions: vec![
                    SessionListItem {
                        id: "s1".into(),
                        title: "Add rate limiting to auth endpoints".into(),
                        repo: "api-server".into(),
                        time: "2h ago".into(),
                    },
                    SessionListItem {
                        id: "s2".into(),
                        title: "Fix config parsing for nested values".into(),
                        repo: "cli-tools".into(),
                        time: "4h ago".into(),
                    },
                ],
            },
            SessionGroup {
                label: "Yesterday".into(),
                sessions: vec![
                    SessionListItem {
                        id: "s3".into(),
                        title: "Migrate to React Router v7".into(),
                        repo: "web-dashboard".into(),
                        time: "1d ago".into(),
                    },
                    SessionListItem {
                        id: "s4".into(),
                        title: "Add dark mode toggle".into(),
                        repo: "web-dashboard".into(),
                        time: "1d ago".into(),
                    },
                    SessionListItem {
                        id: "s5".into(),
                        title: "Update OpenAPI spec for v3".into(),
                        repo: "api-server".into(),
                        time: "1d ago".into(),
                    },
                ],
            },
            SessionGroup {
                label: "Previous 7 days".into(),
                sessions: vec![
                    SessionListItem {
                        id: "s6".into(),
                        title: "Terraform module for Redis cluster".into(),
                        repo: "infrastructure".into(),
                        time: "3d ago".into(),
                    },
                    SessionListItem {
                        id: "s7".into(),
                        title: "Add pipeline event types".into(),
                        repo: "shared-types".into(),
                        time: "5d ago".into(),
                    },
                    SessionListItem {
                        id: "s8".into(),
                        title: "Implement webhook retry logic".into(),
                        repo: "api-server".into(),
                        time: "6d ago".into(),
                    },
                ],
            },
        ]
    }

    pub fn detail(id: &str) -> Option<SessionDetail> {
        match id {
            "s1" => Some(SessionDetail {
                id: "s1".into(), title: "Add rate limiting to auth endpoints".into(), repo: "api-server".into(), model: "Opus 4.6".into(),
                turns: vec![
                    SessionTurn { kind: SessionTurnKind::User, content: Some("Add rate limiting to the auth endpoints. We're getting hit with brute force attempts on /api/auth/login and /api/auth/register. Use a sliding window approach with Redis, 10 requests per minute per IP.".into()), date: Some("Feb 28".into()), tools: vec![] },
                    SessionTurn { kind: SessionTurnKind::Assistant, content: Some("I'll implement sliding window rate limiting using Redis. Let me first look at the existing auth routes and middleware setup.".into()), date: None, tools: vec![] },
                    SessionTurn { kind: SessionTurnKind::Tool, content: None, date: None, tools: vec![
                        ToolUse { tool_name: "read_file".into(), args: r#"{ "path": "src/routes/auth.ts" }"#.into(), result: "import { Router } from \"express\";\nimport { loginHandler } from \"../handlers/auth\";\nimport { registerHandler } from \"../handlers/auth\";\n\nconst router = Router();\nrouter.post(\"/login\", loginHandler);\nrouter.post(\"/register\", registerHandler);\n\nexport default router;".into() },
                        ToolUse { tool_name: "read_file".into(), args: r#"{ "path": "src/middleware/index.ts" }"#.into(), result: "export { authMiddleware } from \"./auth\";\nexport { corsMiddleware } from \"./cors\";\nexport { loggingMiddleware } from \"./logging\";".into() },
                        ToolUse { tool_name: "search_files".into(), args: r#"{ "pattern": "redis", "path": "src/" }"#.into(), result: "src/lib/redis.ts:1: import Redis from \"ioredis\";\nsrc/lib/redis.ts:3: export const redis = new Redis(process.env.REDIS_URL);".into() },
                    ] },
                    SessionTurn { kind: SessionTurnKind::Assistant, content: Some("Good \u{2014} there's already a Redis client at `src/lib/redis.ts`. I'll create a rate limiting middleware using the sliding window algorithm and apply it to the auth routes.\n\nHere's the plan:\n1. Create `src/middleware/rate-limit.ts` with sliding window logic\n2. Apply it to `/login` and `/register` routes\n3. Return `429 Too Many Requests` when the limit is exceeded".into()), date: None, tools: vec![] },
                    SessionTurn { kind: SessionTurnKind::Tool, content: None, date: None, tools: vec![
                        ToolUse { tool_name: "write_file".into(), args: r#"{ "path": "src/middleware/rate-limit.ts" }"#.into(), result: "File written: src/middleware/rate-limit.ts (47 lines)".into() },
                        ToolUse { tool_name: "edit_file".into(), args: r#"{ "path": "src/routes/auth.ts", "operations": [{ "type": "insert", "line": 3 }] }"#.into(), result: "Applied 1 edit to src/routes/auth.ts".into() },
                        ToolUse { tool_name: "edit_file".into(), args: r#"{ "path": "src/middleware/index.ts", "operations": [{ "type": "append" }] }"#.into(), result: "Applied 1 edit to src/middleware/index.ts".into() },
                    ] },
                    SessionTurn { kind: SessionTurnKind::Assistant, content: Some("Done. I've created the rate limiter and wired it up:\n\n- **`src/middleware/rate-limit.ts`** \u{2014} Sliding window rate limiter using Redis sorted sets. Each request adds a timestamped entry, expired entries are pruned, and the count is checked against the limit.\n- **`src/routes/auth.ts`** \u{2014} Applied `rateLimit({ windowMs: 60_000, max: 10 })` to both `/login` and `/register`.\n- **`src/middleware/index.ts`** \u{2014} Re-exported the new middleware.\n\nWhen the limit is exceeded, clients get a `429` response with a `Retry-After` header indicating seconds until the window resets.".into()), date: None, tools: vec![] },
                ],
            }),
            "s2" => Some(SessionDetail {
                id: "s2".into(), title: "Fix config parsing for nested values".into(), repo: "cli-tools".into(), model: "Sonnet 4.6".into(),
                turns: vec![
                    SessionTurn { kind: SessionTurnKind::User, content: Some("The CLI crashes when parsing nested TOML config values like [database.connection]. Can you debug and fix this?".into()), date: Some("Feb 28".into()), tools: vec![] },
                    SessionTurn { kind: SessionTurnKind::Assistant, content: Some("Let me look at the config parser to understand how nested keys are handled.".into()), date: None, tools: vec![] },
                    SessionTurn { kind: SessionTurnKind::Tool, content: None, date: None, tools: vec![
                        ToolUse { tool_name: "read_file".into(), args: r#"{ "path": "src/config/parser.ts" }"#.into(), result: "export function parseConfig(raw: string): Record<string, string> {\n  const result: Record<string, string> = {};\n  for (const line of raw.split(\"\\n\")) {\n    const match = line.match(/^(\\w+)\\s*=\\s*(.+)$/);\n    if (match) result[match[1]] = match[2].trim();\n  }\n  return result;\n}".into() },
                    ] },
                    SessionTurn { kind: SessionTurnKind::Assistant, content: Some("Found the issue. The parser uses a flat `Record<string, string>` and the regex `^(\\w+)` only matches simple keys \u{2014} it can't handle TOML section headers like `[database.connection]` or dotted keys like `database.host`.\n\nI'll rewrite the parser to support nested sections and dotted keys, producing a properly nested object.".into()), date: None, tools: vec![] },
                    SessionTurn { kind: SessionTurnKind::Tool, content: None, date: None, tools: vec![
                        ToolUse { tool_name: "write_file".into(), args: r#"{ "path": "src/config/parser.ts" }"#.into(), result: "File written: src/config/parser.ts (62 lines)".into() },
                        ToolUse { tool_name: "run_command".into(), args: r#"{ "command": "npm test -- --grep 'config parser'" }"#.into(), result: "PASS src/config/__tests__/parser.test.ts\n  config parser\n    \u{2713} parses flat keys (2ms)\n    \u{2713} parses section headers (1ms)\n    \u{2713} parses dotted keys (1ms)\n    \u{2713} handles nested sections (1ms)\n\nTest Suites: 1 passed\nTests: 4 passed".into() },
                    ] },
                    SessionTurn { kind: SessionTurnKind::Assistant, content: Some("Fixed. The parser now:\n\n1. Tracks the current section header (e.g. `[database.connection]`)\n2. Constructs nested objects from dotted paths\n3. Handles both `key = value` within sections and bare `dotted.key = value` syntax\n\nAll 4 tests pass, including the new nested section case.".into()), date: None, tools: vec![] },
                ],
            }),
            "s3" => Some(SessionDetail {
                id: "s3".into(), title: "Migrate to React Router v7".into(), repo: "web-dashboard".into(), model: "Opus 4.6".into(),
                turns: vec![
                    SessionTurn { kind: SessionTurnKind::User, content: Some("Help me migrate our app from React Router v6 to v7. We're using createBrowserRouter with data loaders.".into()), date: Some("Feb 26".into()), tools: vec![] },
                    SessionTurn { kind: SessionTurnKind::Assistant, content: Some("I'll audit your current router setup and identify what needs to change for v7. Let me scan the codebase.".into()), date: None, tools: vec![] },
                    SessionTurn { kind: SessionTurnKind::Tool, content: None, date: None, tools: vec![
                        ToolUse { tool_name: "search_files".into(), args: r#"{ "pattern": "createBrowserRouter" }"#.into(), result: "src/main.tsx:5: import { createBrowserRouter } from \"react-router-dom\";\nsrc/main.tsx:8: const router = createBrowserRouter([".into() },
                        ToolUse { tool_name: "read_file".into(), args: r#"{ "path": "package.json" }"#.into(), result: "\"react-router-dom\": \"^6.22.0\"".into() },
                    ] },
                    SessionTurn { kind: SessionTurnKind::Assistant, content: Some("You're on React Router 6.22. The migration to v7 involves:\n\n1. Replace `react-router-dom` with `react-router` (single package)\n2. Update `createBrowserRouter` import path\n3. Migrate data loaders to the new `Route.LoaderArgs` type\n4. Update any `useLoaderData` calls to be type-safe\n\nLet me make these changes.".into()), date: None, tools: vec![] },
                ],
            }),
            _ => None,
        }
    }
}

mod insights {
    use arc_types::*;

    pub fn saved_queries() -> Vec<SavedQuery> {
        vec![
            SavedQuery { id: "1".into(), name: "Run duration by workflow".into(), sql: "SELECT workflow_name, AVG(duration_seconds) as avg_duration,\n       COUNT(*) as run_count\nFROM runs\nGROUP BY workflow_name\nORDER BY avg_duration DESC\nLIMIT 20".into() },
            SavedQuery { id: "2".into(), name: "Daily failure rate".into(), sql: "SELECT date_trunc('day', created_at) as day,\n       COUNT(*) FILTER (WHERE status = 'failed') as failures,\n       COUNT(*) as total\nFROM runs\nGROUP BY 1\nORDER BY 1 DESC\nLIMIT 30".into() },
            SavedQuery { id: "3".into(), name: "Top repos by activity".into(), sql: "SELECT repo, COUNT(*) as runs\nFROM runs\nGROUP BY repo\nORDER BY runs DESC".into() },
        ]
    }

    pub fn history() -> Vec<HistoryEntry> {
        vec![
            HistoryEntry {
                id: "h1".into(),
                sql: "SELECT workflow_name, COUNT(*) FROM runs GROUP BY 1".into(),
                timestamp: "2 min ago".into(),
                elapsed: 0.342,
                row_count: 6,
            },
            HistoryEntry {
                id: "h2".into(),
                sql: "SELECT * FROM runs WHERE status = 'failed' LIMIT 100".into(),
                timestamp: "8 min ago".into(),
                elapsed: 0.127,
                row_count: 23,
            },
            HistoryEntry {
                id: "h3".into(),
                sql: "SELECT date_trunc('day', created_at) as d, COUNT(*) FROM runs GROUP BY 1"
                    .into(),
                timestamp: "15 min ago".into(),
                elapsed: 0.531,
                row_count: 30,
            },
        ]
    }
}

mod settings {
    use arc_types::*;

    pub fn groups() -> Vec<SettingGroup> {
        vec![
            SettingGroup {
                id: "general".into(),
                name: "General".into(),
                description: "Core platform settings and defaults.".into(),
                fields: vec![
                    SettingField {
                        key: "org_name".into(),
                        label: "Organization name".into(),
                        value: "Acme Corp".into(),
                        type_: SettingFieldType::Text,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "default_branch".into(),
                        label: "Default branch".into(),
                        value: "main".into(),
                        type_: SettingFieldType::Text,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "timezone".into(),
                        label: "Timezone".into(),
                        value: "America/New_York".into(),
                        type_: SettingFieldType::Select,
                        options: vec![
                            "America/New_York".into(),
                            "America/Chicago".into(),
                            "America/Denver".into(),
                            "America/Los_Angeles".into(),
                            "UTC".into(),
                            "Europe/London".into(),
                            "Europe/Berlin".into(),
                            "Asia/Tokyo".into(),
                        ],
                        description: None,
                    },
                    SettingField {
                        key: "auto_cancel".into(),
                        label: "Auto-cancel superseded runs".into(),
                        value: "true".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                ],
            },
            SettingGroup {
                id: "git".into(),
                name: "Git & VCS".into(),
                description: "Version control integration and repository settings.".into(),
                fields: vec![
                    SettingField {
                        key: "github_org".into(),
                        label: "GitHub organization".into(),
                        value: "acme-corp".into(),
                        type_: SettingFieldType::Text,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "clone_protocol".into(),
                        label: "Clone protocol".into(),
                        value: "SSH".into(),
                        type_: SettingFieldType::Select,
                        options: vec!["SSH".into(), "HTTPS".into()],
                        description: None,
                    },
                    SettingField {
                        key: "auto_merge".into(),
                        label: "Auto-merge when checks pass".into(),
                        value: "false".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "delete_branch".into(),
                        label: "Delete branch after merge".into(),
                        value: "true".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "commit_signing".into(),
                        label: "Require commit signing".into(),
                        value: "false".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                ],
            },
            SettingGroup {
                id: "compute".into(),
                name: "Compute".into(),
                description: "Resource allocation and execution environment.".into(),
                fields: vec![
                    SettingField {
                        key: "default_cpu".into(),
                        label: "Default CPU".into(),
                        value: "4".into(),
                        type_: SettingFieldType::Select,
                        options: vec!["2".into(), "4".into(), "8".into(), "16".into()],
                        description: None,
                    },
                    SettingField {
                        key: "default_memory".into(),
                        label: "Default memory".into(),
                        value: "8 GB".into(),
                        type_: SettingFieldType::Select,
                        options: vec!["4 GB".into(), "8 GB".into(), "16 GB".into(), "32 GB".into()],
                        description: None,
                    },
                    SettingField {
                        key: "max_parallel".into(),
                        label: "Max parallel runs".into(),
                        value: "10".into(),
                        type_: SettingFieldType::Text,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "timeout_minutes".into(),
                        label: "Run timeout (minutes)".into(),
                        value: "120".into(),
                        type_: SettingFieldType::Text,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "gpu_enabled".into(),
                        label: "GPU acceleration".into(),
                        value: "false".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                ],
            },
            SettingGroup {
                id: "notifications".into(),
                name: "Notifications".into(),
                description: "Alerts and notification delivery preferences.".into(),
                fields: vec![
                    SettingField {
                        key: "slack_webhook".into(),
                        label: "Slack webhook URL".into(),
                        value: "https://hooks.slack.com/services/T00/B00/xxxx".into(),
                        type_: SettingFieldType::Text,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "notify_on_failure".into(),
                        label: "Notify on failure".into(),
                        value: "true".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "notify_on_success".into(),
                        label: "Notify on success".into(),
                        value: "false".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "notify_on_approval".into(),
                        label: "Notify on approval needed".into(),
                        value: "true".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "email_digest".into(),
                        label: "Daily email digest".into(),
                        value: "false".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                ],
            },
            SettingGroup {
                id: "security".into(),
                name: "Security".into(),
                description: "Access control and security policies.".into(),
                fields: vec![
                    SettingField {
                        key: "sso_provider".into(),
                        label: "SSO provider".into(),
                        value: "Okta".into(),
                        type_: SettingFieldType::Select,
                        options: vec![
                            "None".into(),
                            "Okta".into(),
                            "Azure AD".into(),
                            "Google Workspace".into(),
                            "OneLogin".into(),
                        ],
                        description: None,
                    },
                    SettingField {
                        key: "mfa_required".into(),
                        label: "Require MFA".into(),
                        value: "true".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "session_timeout".into(),
                        label: "Session timeout".into(),
                        value: "8 hours".into(),
                        type_: SettingFieldType::Select,
                        options: vec![
                            "1 hour".into(),
                            "4 hours".into(),
                            "8 hours".into(),
                            "24 hours".into(),
                            "7 days".into(),
                        ],
                        description: None,
                    },
                    SettingField {
                        key: "audit_log".into(),
                        label: "Audit logging".into(),
                        value: "true".into(),
                        type_: SettingFieldType::Toggle,
                        options: vec![],
                        description: None,
                    },
                    SettingField {
                        key: "ip_allowlist".into(),
                        label: "IP allowlist".into(),
                        value: "".into(),
                        type_: SettingFieldType::Text,
                        options: vec![],
                        description: Some(
                            "Comma-separated CIDRs. Leave empty to allow all.".into(),
                        ),
                    },
                ],
            },
        ]
    }
}

mod projects {
    use arc_types::*;

    pub fn list_items() -> Vec<Project> {
        vec![
            Project {
                id: "arc-web".into(),
                name: "arc-web".into(),
            },
            Project {
                id: "arc-workflows".into(),
                name: "arc-workflows".into(),
            },
            Project {
                id: "arc-cli".into(),
                name: "arc-cli".into(),
            },
        ]
    }

    pub fn branches() -> Vec<Branch> {
        vec![
            Branch {
                id: "main".into(),
                name: "main".into(),
            },
            Branch {
                id: "develop".into(),
                name: "develop".into(),
            },
            Branch {
                id: "feature/start-page".into(),
                name: "feature/start-page".into(),
            },
        ]
    }
}
