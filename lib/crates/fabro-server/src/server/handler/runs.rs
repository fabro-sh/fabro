use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use axum_extra::extract::Query as ExtraQuery;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use fabro_api::types::{
    BoardColumn, RunManifest, SubmitAnswerRequest, UpdateRunParentRequest, UpdateRunRequest,
};
use fabro_config::Storage;
use fabro_interview::AnswerSubmission;
use fabro_llm::client::Client as LlmClient;
use fabro_types::{
    Principal, RunClientProvenance, RunId, RunProvenance, RunServerProvenance, SystemActorKind,
    parse_blob_ref,
};
use fabro_util::version::FABRO_VERSION;
use fabro_workflow::command_log::{command_log_path, read_json_string_blob, read_log_slice};
use fabro_workflow::run_status::RunStatus;
use fabro_workflow::{Error as WorkflowError, operations};
use tokio::fs;
use tracing::info;

use super::super::{
    AppState, ListResponse, PaginationParams, RunExecutionMode, answer_from_request,
    api_question_from_pending_interview, default_page_limit, delete_run_internal,
    load_pending_interview, managed_run, paginate_items, parse_run_id_path, reject_if_archived,
    resolve_interp_string, submit_pending_interview_answer, workflow_event,
};
use crate::error::ApiError;
use crate::principal_middleware::{
    RequireCommandLog, RequireRunScoped, RequireRunScopedOrRunTools, RequiredRunToolActor,
    RequiredUser,
};
use crate::run_files::{list_run_commits, list_run_files};
use crate::run_manifest;
use crate::run_selector::{ResolveRunError, resolve_run_by_selector};
use crate::run_title_generation::{self, GenerateTitleInput, TitlePromptInput, WorkflowSummary};
use crate::server_secrets::LlmClientResult;

pub(super) fn manifest_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/preflight", post(run_preflight))
        .route("/validate", post(validate_run_manifest))
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs", get(list_runs).post(create_run))
        .route("/runs/resolve", get(resolve_run))
        .route(
            "/runs/{id}",
            get(get_run_status).patch(update_run).delete(delete_run),
        )
        .route(
            "/runs/{id}/parent",
            put(link_run_parent).delete(unlink_run_parent),
        )
        .route("/runs/{id}/questions", get(get_questions))
        .route("/runs/{id}/questions/{qid}/answer", post(submit_answer))
        .route("/runs/{id}/state", get(get_run_state))
        .route("/runs/{id}/logs", get(get_run_logs))
        .route(
            "/runs/{id}/stages/{stageId}/logs/output",
            get(get_run_stage_command_log),
        )
        .route("/runs/{id}/settings", get(get_run_settings))
        .route("/runs/{id}/files", get(list_run_files))
        .route("/runs/{id}/commits", get(list_run_commits))
        .merge(manifest_routes())
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum RunsSortKey {
    #[default]
    CreatedAt,
    UpdatedAt,
    Status,
    Elapsed,
    Repo,
    Title,
    Workflow,
    Changes,
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum RunsSortDirection {
    Asc,
    #[default]
    Desc,
}

#[derive(serde::Deserialize)]
struct ListRunsParams {
    #[serde(rename = "page[limit]", default = "default_page_limit")]
    limit:            u32,
    #[serde(rename = "page[offset]", default)]
    offset:           u32,
    #[serde(default)]
    include_archived: bool,
    #[serde(default)]
    parent_id:        Option<RunId>,
    #[serde(default)]
    status:           Vec<BoardColumn>,
    #[serde(default)]
    sort:             RunsSortKey,
    #[serde(default)]
    direction:        RunsSortDirection,
}

impl ListRunsParams {
    fn pagination(&self) -> PaginationParams {
        PaginationParams {
            limit:  self.limit,
            offset: self.offset,
        }
    }

    fn status_filter(&self) -> Option<HashSet<BoardColumn>> {
        if self.status.is_empty() {
            None
        } else {
            Some(self.status.iter().copied().collect())
        }
    }
}

pub(crate) fn board_column(status: RunStatus, archived: bool) -> BoardColumn {
    if archived {
        return BoardColumn::Archived;
    }
    match status {
        RunStatus::Submitted | RunStatus::Pending { .. } => BoardColumn::Pending,
        RunStatus::Runnable => BoardColumn::Runnable,
        RunStatus::Starting => BoardColumn::Initializing,
        RunStatus::Running | RunStatus::Paused { .. } => BoardColumn::Running,
        RunStatus::Blocked { .. } => BoardColumn::Blocked,
        RunStatus::Succeeded { .. } => BoardColumn::Succeeded,
        RunStatus::Failed { .. } | RunStatus::Dead => BoardColumn::Failed,
        RunStatus::Removing => BoardColumn::Removing,
    }
}

fn run_elapsed_ms(run: &fabro_types::Run, now: DateTime<Utc>) -> i64 {
    let start = run
        .timestamps
        .started_at
        .unwrap_or(run.timestamps.created_at);
    let end = run.timestamps.completed_at.unwrap_or(now);
    (end - start).num_milliseconds().max(0)
}

fn sort_runs(runs: &mut [fabro_types::Run], key: RunsSortKey, direction: RunsSortDirection) {
    let now = Utc::now();
    let asc = matches!(direction, RunsSortDirection::Asc);
    runs.sort_by(|a, b| {
        let primary = match key {
            RunsSortKey::CreatedAt => a.timestamps.created_at.cmp(&b.timestamps.created_at),
            RunsSortKey::UpdatedAt => {
                let av = a
                    .timestamps
                    .last_event_at
                    .unwrap_or(a.timestamps.created_at);
                let bv = b
                    .timestamps
                    .last_event_at
                    .unwrap_or(b.timestamps.created_at);
                av.cmp(&bv)
            }
            RunsSortKey::Status => {
                let ac = board_column(a.lifecycle.status, a.lifecycle.archived);
                let bc = board_column(b.lifecycle.status, b.lifecycle.archived);
                ac.cmp(&bc)
            }
            RunsSortKey::Elapsed => run_elapsed_ms(a, now).cmp(&run_elapsed_ms(b, now)),
            RunsSortKey::Repo => run_repo_key(a).cmp(&run_repo_key(b)),
            RunsSortKey::Title => run_title_key(a).cmp(&run_title_key(b)),
            RunsSortKey::Workflow => run_workflow_key(a).cmp(&run_workflow_key(b)),
            RunsSortKey::Changes => run_changes_total(a).cmp(&run_changes_total(b)),
        };
        let primary = if asc { primary } else { primary.reverse() };
        // Stable tiebreak: newer ULIDs (and thus newer runs) first.
        primary.then_with(|| b.id.cmp(&a.id))
    });
}

fn run_repo_key(run: &fabro_types::Run) -> String {
    run.repository
        .as_ref()
        .map(|repo| repo.name.to_lowercase())
        .unwrap_or_default()
}

fn run_title_key(run: &fabro_types::Run) -> String {
    run.title.trim().to_lowercase()
}

fn run_workflow_key(run: &fabro_types::Run) -> String {
    let wf = &run.workflow;
    wf.name
        .as_deref()
        .or(wf.graph_name.as_deref())
        .or(wf.slug.as_deref())
        .map(str::to_lowercase)
        .unwrap_or_default()
}

fn run_changes_total(run: &fabro_types::Run) -> i64 {
    run.diff
        .as_ref()
        .map_or(0, |diff| diff.additions + diff.deletions)
}

async fn link_run_parent(
    RequireRunScopedOrRunTools(child_id, actor): RequireRunScopedOrRunTools,
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateRunParentRequest>,
) -> Response {
    let parent_id = match req.parent_id.parse::<RunId>() {
        Ok(parent_id) => parent_id,
        Err(err) => {
            return ApiError::bad_request(format!("invalid parent run ID: {err}")).into_response();
        }
    };
    let _parent_link_guard = state.parent_link_lock.lock().await;
    let child = match state.store.get_cached_summary(&child_id, Utc::now()).await {
        Ok(Some(summary)) => summary,
        Ok(None) => return ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if parent_id == child_id {
        return ApiError::bad_request("A run cannot be its own parent.").into_response();
    }
    if let Err(err) = validate_parent_link(&state, child_id, parent_id).await {
        return err.into_response();
    }
    if child.parent_id == Some(parent_id) {
        return (
            StatusCode::OK,
            Json(state.decorate_run_summary(child).await),
        )
            .into_response();
    }

    let Ok(run_store) = state.store.open_run(&child_id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    if let Err(err) = workflow_event::append_event(
        &run_store,
        &child_id,
        &workflow_event::Event::RunParentLinked {
            previous_parent_id: child.parent_id,
            parent_id,
            actor: Some(actor),
        },
    )
    .await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    updated_run_response(&state, &child_id).await
}

async fn unlink_run_parent(
    RequireRunScopedOrRunTools(child_id, actor): RequireRunScopedOrRunTools,
    State(state): State<Arc<AppState>>,
) -> Response {
    let _parent_link_guard = state.parent_link_lock.lock().await;
    let child = match state.store.get_cached_summary(&child_id, Utc::now()).await {
        Ok(Some(summary)) => summary,
        Ok(None) => return ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let Some(previous_parent_id) = child.parent_id else {
        return (
            StatusCode::OK,
            Json(state.decorate_run_summary(child).await),
        )
            .into_response();
    };

    let Ok(run_store) = state.store.open_run(&child_id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    if let Err(err) = workflow_event::append_event(
        &run_store,
        &child_id,
        &workflow_event::Event::RunParentUnlinked {
            previous_parent_id,
            actor: Some(actor),
        },
    )
    .await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    updated_run_response(&state, &child_id).await
}

async fn validate_parent_link(
    state: &AppState,
    child_id: RunId,
    parent_id: RunId,
) -> Result<(), ApiError> {
    let mut cursor = Some(parent_id);
    let mut visited = HashSet::new();
    while let Some(current_id) = cursor {
        if current_id == child_id {
            return Err(ApiError::bad_request("Parent link would create a cycle."));
        }
        if !visited.insert(current_id) {
            return Err(ApiError::bad_request("Parent link would create a cycle."));
        }
        let summary = state
            .store
            .get_cached_summary(&current_id, Utc::now())
            .await
            .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
        let Some(summary) = summary else {
            if current_id == parent_id {
                return Err(ApiError::not_found("Parent run not found."));
            }
            return Ok(());
        };
        cursor = summary.parent_id;
    }
    Ok(())
}

async fn updated_run_response(state: &AppState, run_id: &RunId) -> Response {
    match state.store.get_cached_summary(run_id, Utc::now()).await {
        Ok(Some(summary)) => (
            StatusCode::OK,
            Json(state.decorate_run_summary(summary).await),
        )
            .into_response(),
        Ok(None) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn list_runs(
    _auth: RequiredRunToolActor,
    State(state): State<Arc<AppState>>,
    ExtraQuery(params): ExtraQuery<ListRunsParams>,
) -> Response {
    let entries = match state
        .store
        .list_cached_runs(
            &fabro_store::ListRunsQuery {
                parent_id: params.parent_id,
                ..fabro_store::ListRunsQuery::default()
            },
            Utc::now(),
        )
        .await
    {
        Ok(entries) => entries,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let status_filter = params.status_filter();
    let include_archived = params.include_archived;

    let filtered: Vec<fabro_types::Run> = entries
        .into_iter()
        .map(|entry| entry.summary)
        .filter(|run| {
            let column = board_column(run.lifecycle.status, run.lifecycle.archived);
            match &status_filter {
                Some(set) => set.contains(&column),
                None => {
                    column != BoardColumn::Removing
                        && (include_archived || column != BoardColumn::Archived)
                }
            }
        })
        .collect();

    let mut decorated = state.decorate_run_summaries(filtered).await;
    sort_runs(&mut decorated, params.sort, params.direction);
    let total = decorated.len() as u64;
    let (data, has_more) = paginate_items(decorated, &params.pagination());

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": data,
            "meta": { "has_more": has_more, "total": total }
        })),
    )
        .into_response()
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
    offset:         u64,
    next_offset:    u64,
    total_bytes:    u64,
    bytes_base64:   String,
    eof:            bool,
    cas_ref:        Option<String>,
    live_streaming: bool,
}

async fn resolve_run(
    _auth: RequiredRunToolActor,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ResolveRunQuery>,
) -> Response {
    let runs = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default(), Utc::now())
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
        |run| run.id.to_string(),
        |run| run.workflow.slug.clone(),
        |run| run.workflow.name.clone(),
        |run| run.id.created_at(),
        |run| run.id.created_at().to_rfc3339(),
        |run| {
            run.repository
                .as_ref()
                .and_then(|repository| repository.origin_url.clone())
        },
    ) {
        Ok(run) => {
            let run = state.decorate_run_summary(run.clone()).await;
            (StatusCode::OK, Json(run)).into_response()
        }
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
        Ok(super::super::DeleteRunOutcome::NoContent) => StatusCode::NO_CONTENT.into_response(),
        Ok(super::super::DeleteRunOutcome::Preserved(response)) => {
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(response) => response,
    }
}

async fn update_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let request = match serde_json::from_slice::<UpdateRunRequest>(&body) {
        Ok(request) => request,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let title = match fabro_types::normalize_explicit_run_title(request.title.as_str()) {
        Ok(title) => title,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let current = match state.store.get_cached_summary(&id, Utc::now()).await {
        Ok(Some(summary)) => summary,
        Ok(None) => return ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if current.title == title {
        return (
            StatusCode::OK,
            Json(state.decorate_run_summary(current).await),
        )
            .into_response();
    }

    let run_store = match state.store.open_run(&id).await {
        Ok(run_store) => run_store,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Err(err) =
        workflow_event::append_event(&run_store, &id, &workflow_event::Event::RunTitleUpdated {
            title,
            actor: Some(Principal::User(subject.0)),
        })
        .await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    match state.store.get_cached_summary(&id, Utc::now()).await {
        Ok(Some(summary)) => (
            StatusCode::OK,
            Json(state.decorate_run_summary(summary).await),
        )
            .into_response(),
        Ok(None) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn create_run(
    RequiredRunToolActor(actor): RequiredRunToolActor,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let req = match serde_json::from_slice::<RunManifest>(&body) {
        Ok(req) => req,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let explicit_title_supplied = req.title.is_some();
    let manifest_run_defaults = state.manifest_run_defaults();
    let manifest_environment_defaults = state.manifest_environment_defaults();
    let prepared = match run_manifest::prepare_manifest_with_environment_defaults(
        manifest_run_defaults.as_ref(),
        manifest_environment_defaults.as_ref(),
        &req,
    ) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let run_id = prepared.run_id.unwrap_or_else(RunId::new);
    if let Some(parent_id) = prepared.parent_id {
        if parent_id == run_id {
            return ApiError::bad_request("A run cannot be its own parent.").into_response();
        }
        if let Err(err) = validate_parent_link(&state, run_id, parent_id).await {
            return err.into_response();
        }
    }
    info!(run_id = %run_id, "Run created");

    let web_url = state.run_web_url(&run_id);
    let catalog = state.catalog();
    // Resolve once: we need both the provider IDs (for the run create input
    // and ask-fabro-readiness) and the LLM client itself (for the spawned
    // title-generation task). `ready_llm_provider_ids` would otherwise call
    // `resolve_llm_client` a second time and discard the client.
    let llm_client_for_title = match state.resolve_llm_client().await {
        Ok(result) => Some(result),
        Err(err) => {
            tracing::warn!(error = ?err, "Failed to resolve LLM client while creating run");
            None
        }
    };
    let ready_provider_ids = llm_client_for_title
        .as_ref()
        .map(LlmClientResult::provider_ids)
        .unwrap_or_default();
    let mut create_input = run_manifest::create_run_input(
        prepared.clone(),
        ready_provider_ids.clone(),
        web_url.clone(),
    );
    create_input.run_id = Some(run_id);
    create_input.provenance = Some(run_provenance(&headers, &actor));
    create_input.submitted_manifest_bytes = Some(body.to_vec());

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
        catalog,
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
    let summary = match state
        .store
        .get_cached_summary(&created.run_id, Utc::now())
        .await
    {
        Ok(Some(summary)) => summary,
        Ok(None) => return ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let deterministic_title = summary.title.clone();

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

    if !explicit_title_supplied && !ready_provider_ids.is_empty() {
        if let Some(llm_result) = llm_client_for_title {
            let run_spec = created.persisted.run_spec();
            let workflow = run_title_generation::workflow_summary(&run_spec.graph);
            let run_inputs = run_spec.settings.run.inputs.clone();
            let workflow_target = prepared.target_path.to_string();
            let title_catalog = state.catalog();
            let title_model = title_catalog.small_default_for_configured_ids(&ready_provider_ids);
            let title_model_id = title_model.id.clone();
            let title_provider_id = title_model.provider.clone();
            spawn_generated_title_task(GeneratedTitleTask {
                state: Arc::clone(&state),
                run_id: created.run_id,
                deterministic_title,
                workflow_target,
                workflow,
                run_inputs,
                client: llm_result.client,
                model_id: title_model_id,
                provider_id: title_provider_id,
            });
        }
    }

    (
        StatusCode::CREATED,
        Json(state.decorate_run_summary(summary).await),
    )
        .into_response()
}

struct GeneratedTitleTask {
    state:               Arc<AppState>,
    run_id:              RunId,
    deterministic_title: String,
    workflow_target:     String,
    workflow:            WorkflowSummary,
    run_inputs:          std::collections::HashMap<String, toml::Value>,
    client:              LlmClient,
    model_id:            String,
    provider_id:         fabro_model::ProviderId,
}

fn spawn_generated_title_task(task: GeneratedTitleTask) {
    tokio::spawn(async move {
        let generated_title = run_title_generation::generate_title_or_current(GenerateTitleInput {
            client:      Arc::new(task.client),
            model_id:    task.model_id,
            provider_id: task.provider_id,
            prompt:      TitlePromptInput {
                run_id:          &task.run_id,
                current_title:   &task.deterministic_title,
                workflow_target: Some(task.workflow_target.as_str()),
                run_inputs:      &task.run_inputs,
                workflow:        &task.workflow,
            },
        })
        .await;
        if generated_title == task.deterministic_title {
            return;
        }

        let current = match task
            .state
            .store
            .get_cached_summary(&task.run_id, Utc::now())
            .await
        {
            Ok(Some(summary)) => summary,
            Ok(None) => return,
            Err(err) => {
                tracing::debug!(run_id = %task.run_id, error = %err, "Failed to re-read run summary for title update");
                return;
            }
        };
        if current.title != task.deterministic_title {
            return;
        }
        let run_store = match task.state.store.open_run(&task.run_id).await {
            Ok(store) => store,
            Err(err) => {
                tracing::debug!(run_id = %task.run_id, error = %err, "Failed to open run store for title update");
                return;
            }
        };
        if let Err(err) = workflow_event::append_event(
            &run_store,
            &task.run_id,
            &workflow_event::Event::RunTitleUpdated {
                title: generated_title,
                actor: Some(Principal::System {
                    system_kind: SystemActorKind::Engine,
                }),
            },
        )
        .await
        {
            tracing::debug!(run_id = %task.run_id, error = %err, "Failed to append generated run title event");
        }
    });
}

pub(super) fn run_provenance(headers: &HeaderMap, subject: &Principal) -> RunProvenance {
    RunProvenance {
        server:  Some(RunServerProvenance {
            version: FABRO_VERSION.to_string(),
        }),
        client:  run_client_provenance(headers),
        subject: Some(subject.clone()),
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
    let manifest_environment_defaults = state.manifest_environment_defaults();
    let prepared = match run_manifest::prepare_manifest_with_environment_defaults(
        manifest_run_defaults.as_ref(),
        manifest_environment_defaults.as_ref(),
        &req,
    ) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let mut validated = match run_manifest::validate_prepared_manifest(&prepared, state.catalog()) {
        Ok(validated) => validated,
        Err(WorkflowError::Parse(_)) => {
            return ApiError::bad_request("Validation failed").into_response();
        }
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    validated.promote_template_undefined_variables_to_errors();
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
    let manifest_environment_defaults = state.manifest_environment_defaults();
    let prepared = match run_manifest::prepare_manifest_with_environment_defaults(
        manifest_run_defaults.as_ref(),
        manifest_environment_defaults.as_ref(),
        &req,
    ) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let validated = match run_manifest::validate_prepared_manifest(&prepared, state.catalog()) {
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
    RequireRunScopedOrRunTools(id, _actor): RequireRunScopedOrRunTools,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.store.get_cached_summary(&id, Utc::now()).await {
        Ok(Some(run)) => {
            (StatusCode::OK, Json(state.decorate_run_summary(run).await)).into_response()
        }
        Ok(None) => ApiError::not_found("Run not found.").into_response(),
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
    let cached = match state.store.get_cached_run(&id).await {
        Ok(Some(cached)) => cached,
        Ok(None) => return ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    (
        StatusCode::OK,
        Json(cached.projection.spec.settings.clone()),
    )
        .into_response()
}

async fn get_questions(
    RequireRunScopedOrRunTools(id, _actor): RequireRunScopedOrRunTools,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.store.get_cached_run(&id).await {
        Ok(Some(cached)) => {
            let questions = cached
                .projection
                .pending_interviews
                .values()
                .map(api_question_from_pending_interview)
                .collect::<Vec<_>>();
            (StatusCode::OK, Json(ListResponse::new(questions))).into_response()
        }
        Ok(None) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn submit_answer(
    RequireRunScopedOrRunTools(id, actor): RequireRunScopedOrRunTools,
    State(state): State<Arc<AppState>>,
    Path((_id, qid)): Path<(String, String)>,
    Json(req): Json<SubmitAnswerRequest>,
) -> Response {
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
    let submission = AnswerSubmission::new(answer, actor);
    match submit_pending_interview_answer(state.as_ref(), &pending, submission).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(response) => response,
    }
}

async fn get_run_state(
    RequireRunScopedOrRunTools(id, _actor): RequireRunScopedOrRunTools,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.store.get_cached_run(&id).await {
        Ok(Some(cached)) => Json((*cached.projection).clone()).into_response(),
        Ok(None) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
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
    RequireCommandLog(id, stage_id): RequireCommandLog,
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

    let stream_value = node.output.as_deref();
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
    let scratch_path = command_log_path(&run_dir, &stage_id);

    match read_log_slice(&scratch_path, query.offset, limit).await {
        Ok((bytes, total_bytes)) => {
            return build_command_log_response(
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
            query.offset,
            limit,
            LogSource::Full(inline_text.as_bytes()),
            true,
            None,
            live_streaming,
        );
    }

    build_command_log_response(
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
