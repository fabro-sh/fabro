use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::routing::{get, post};
use serde::Serialize;

use super::{ApiError, AppState, IntoResponse, Json, Response, StatusCode, demo};

mod artifacts;
mod automations;
mod billing;
mod completions;
mod environments;
pub(in crate::server) mod events;
pub(in crate::server) mod graph;
mod lifecycle;
mod models;
mod pair;
mod pull_requests;
pub(in crate::server) mod runs;
mod sandbox;
mod sandboxes;
mod secrets;
mod sessions;
mod steer;
pub(in crate::server) mod system;
mod variables;
mod worker_control;

pub(super) use system::{health, openapi_spec};

async fn not_implemented() -> Response {
    ApiError::new(StatusCode::NOT_IMPLEMENTED, "Not implemented.").into_response()
}

fn parse_required_if_match<R>(
    headers: &HeaderMap,
    resource: &str,
    id: &impl std::fmt::Display,
) -> Result<R, ApiError>
where
    R: std::str::FromStr,
    R::Err: std::fmt::Display,
{
    let Some(value) = headers.get(header::IF_MATCH) else {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            format!("If-Match header is required for {resource}: {id}"),
        ));
    };
    let value = value
        .to_str()
        .map_err(|_| ApiError::bad_request("If-Match header must be visible ASCII"))?;
    let value = unquote_etag(value.trim());
    value.parse::<R>().map_err(|err| {
        ApiError::bad_request(format!("invalid If-Match {resource} revision: {err}"))
    })
}

fn unquote_etag(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|unquoted| unquoted.strip_suffix('"'))
        .unwrap_or(value)
}

fn json_with_etag_response<T>(
    status: StatusCode,
    resource: &str,
    revision: &impl std::fmt::Display,
    body: T,
) -> Response
where
    T: Serialize,
{
    let etag = HeaderValue::from_str(&format!("\"{revision}\""))
        .unwrap_or_else(|_| panic!("{resource} revisions are valid ETag header values"));
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(header::ETAG, etag);
    response
}

pub(super) fn demo_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs", get(demo::list_runs).post(demo::create_run_stub))
        .route("/runs/resolve", get(demo::resolve_run))
        .route("/attach", get(demo::attach_events_stub))
        .route("/runs/{id}", get(demo::get_run_status))
        .route("/runs/{id}/questions", get(demo::get_questions_stub))
        .route("/runs/{id}/questions/{qid}/answer", post(demo::answer_stub))
        .route("/runs/{id}/state", get(not_implemented))
        .route("/runs/{id}/logs", get(not_implemented))
        .route(
            "/runs/{id}/events",
            get(not_implemented).post(not_implemented),
        )
        .route("/runs/{id}/attach", get(demo::run_events_stub))
        .route("/runs/{id}/blobs", post(not_implemented))
        .route("/runs/{id}/blobs/{blobId}", get(not_implemented))
        .route(
            "/runs/{id}/stages/{stageId}/logs/output",
            get(not_implemented),
        )
        .route("/runs/{id}/checkpoint", get(demo::checkpoint_stub))
        .route("/runs/{id}/cancel", post(demo::cancel_stub))
        .route("/runs/{id}/start", post(demo::start_run_stub))
        .route("/runs/{id}/approve", post(demo::start_run_stub))
        .route("/runs/{id}/deny", post(demo::deny_run_stub))
        .route("/runs/{id}/pause", post(demo::pause_stub))
        .route("/runs/{id}/unpause", post(demo::unpause_stub))
        .route("/runs/{id}/graph", get(demo::get_run_graph))
        .route("/runs/{id}/graph/source", get(demo::get_run_graph_source))
        .route("/runs/{id}/stages", get(demo::get_run_stages))
        .route("/runs/{id}/artifacts", get(demo::list_run_artifacts_stub))
        .route("/runs/{id}/files", get(demo::list_run_files_stub))
        .route("/runs/{id}/commits", get(demo::list_run_commits_stub))
        .route(
            "/runs/{id}/stages/{stageId}/events",
            get(demo::get_stage_events),
        )
        .route(
            "/runs/{id}/stages/{stageId}/context-window",
            get(not_implemented),
        )
        .route(
            "/runs/{id}/stages/{stageId}/artifacts",
            get(not_implemented).post(not_implemented),
        )
        .route(
            "/runs/{id}/stages/{stageId}/artifacts/download",
            get(not_implemented),
        )
        .route("/runs/{id}/billing", get(demo::get_run_billing))
        .route("/runs/{id}/settings", get(demo::get_run_settings))
        .route("/runs/{id}/preview", post(demo::generate_preview_url_stub))
        .route("/runs/{id}/ssh", post(demo::create_ssh_access_stub))
        .route(
            "/runs/{id}/sandbox/files",
            get(demo::list_sandbox_files_stub),
        )
        .route(
            "/runs/{id}/sandbox/services",
            get(demo::list_sandbox_services_stub),
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
        .route(
            "/secrets",
            get(demo::list_secrets)
                .post(demo::create_secret)
                .delete(demo::delete_secret_by_name),
        )
        .route("/repos/github/{owner}/{name}", get(demo::get_github_repo))
        .route("/health/diagnostics", post(demo::run_diagnostics))
        .route("/settings", get(demo::get_server_settings))
        .route("/system/info", get(demo::get_system_info))
        .route("/system/integrations", get(demo::get_system_integrations))
        .route("/system/resources", get(demo::get_system_resources))
        .route("/system/df", get(demo::get_system_disk_usage))
        .route("/system/repair/runs", get(demo::get_system_repair_runs))
        .route("/system/prune/runs", post(demo::prune_runs))
        .route("/billing", get(demo::get_aggregate_billing))
        .route("/workflows", get(demo::list_workflows))
        .route("/workflows/{name}", get(demo::get_workflow))
        .route("/workflows/{name}/runs", get(demo::list_workflow_runs))
        .merge(runs::manifest_routes())
        .merge(graph::manifest_routes())
        .merge(models::routes())
        .merge(completions::routes())
}

pub(super) fn real_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/stages/{stageId}/turns", get(not_implemented))
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
        .merge(runs::routes())
        .merge(events::routes())
        .merge(billing::routes())
        .merge(pull_requests::routes())
        .merge(artifacts::routes())
        .merge(automations::routes())
        .merge(environments::routes())
        .merge(sandbox::routes())
        .merge(sandboxes::routes())
        .merge(lifecycle::routes())
        .merge(steer::routes())
        .merge(pair::routes())
        .merge(graph::manifest_routes())
        .merge(graph::run_routes())
        .merge(models::routes())
        .merge(secrets::routes())
        .merge(variables::routes())
        .merge(worker_control::routes())
        .merge(sessions::routes())
        .merge(system::routes())
        .merge(completions::routes())
}
