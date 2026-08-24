use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
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
    BoardColumn, ManifestConfigType, ManifestGoalType, RunManifest, SubmitAnswerRequest,
    UpdateRunParentRequest, UpdateRunRequest,
};
use fabro_config::{CliLayer, RunLayer, Storage};
use fabro_interview::AnswerSubmission;
use fabro_llm::client::Client as LlmClient;
use fabro_store::{
    RunSummaryListQuery, RunSummarySort, RunSummarySortDirection, RunSummaryVisibility,
};
use fabro_types::{
    AutomationRef, ManifestPath, Principal, Run, RunClientProvenance, RunId, RunProvenance,
    RunServerProvenance, RunStatusKind, StageContextWindow, StageContextWindowStaleness,
    StageContextWindowUnavailableReason, StageHandler, StageModelUsage, StageProjection,
    SystemActorKind, parse_blob_ref,
};
use fabro_util::version::FABRO_VERSION;
use fabro_workflow::command_log::{command_log_path, read_json_string_blob, read_log_slice};
use fabro_workflow::run_status::RunStatus;
use fabro_workflow::workflow_bundle::WorkflowBundle;
use fabro_workflow::{Error as WorkflowError, operations};
use strum::VariantArray as _;
use tokio::fs;
use tracing::info;

use super::super::{
    AppState, DeleteRunOutcome, ListResponse, RunExecutionMode, VariableError, answer_from_request,
    api_question_from_pending_interview, clamp_page_limit, clamp_page_offset, default_page_limit,
    delete_run_internal, load_pending_interview, managed_run, mirror_persisted_run_created_to_main,
    parse_run_id_path, parse_stage_id_path, reject_if_archived, submit_pending_interview_answer,
    workflow_event,
};
use crate::error::ApiError;
use crate::principal_middleware::{
    RequireCommandLog, RequireRunManagementTarget, RequireRunScoped, RequireRunStageScoped,
    RequiredRunManagementActor, RequiredUser,
};
use crate::run_compiler::{
    self, ProjectSettingsPathError, ProjectSettingsSource, RawRunCompilerInput, RunCompilerError,
};
use crate::run_files::{list_run_commits, list_run_files};
use crate::run_manifest;
use crate::run_selector::{ResolveRunError, resolve_run_by_selector};
use crate::run_title_generation::{self, GenerateTitleInput, TitlePromptInput, WorkflowSummary};
#[cfg(any(test, feature = "test-support"))]
use crate::test_support as server_test_support;

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
        .route(
            "/runs/{id}/stages/{stageId}/context-window",
            get(get_run_stage_context_window),
        )
        .route("/runs/{id}/settings", get(get_run_settings))
        .route("/runs/{id}/files", get(list_run_files))
        .route("/runs/{id}/commits", get(list_run_commits))
        .merge(manifest_routes())
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
    sort:             RunSummarySort,
    #[serde(default)]
    direction:        RunSummarySortDirection,
}

impl ListRunsParams {
    fn summary_query(&self) -> RunSummaryListQuery {
        RunSummaryListQuery {
            parent_id: self.parent_id,
            visibility: summary_visibility(&self.status, self.include_archived),
            sort: self.sort,
            direction: self.direction,
            limit: clamp_page_limit(self.limit),
            offset: clamp_page_offset(self.offset),
            ..RunSummaryListQuery::default()
        }
    }
}

fn summary_visibility(selected: &[BoardColumn], include_archived: bool) -> RunSummaryVisibility {
    if selected.is_empty() {
        return RunSummaryVisibility::Default { include_archived };
    }

    let mut statuses = HashSet::new();
    let mut archived = false;
    for column in selected {
        match board_column_rank(*column) {
            None => archived = true,
            Some(rank) => statuses.extend(
                RunStatusKind::VARIANTS
                    .iter()
                    .copied()
                    .filter(|kind| kind.board_rank() == rank),
            ),
        }
    }
    RunSummaryVisibility::Selected {
        statuses: statuses.into_iter().collect(),
        archived,
    }
}

/// Rank of each board column, mirroring the `BoardColumn` enum order.
/// Statuses map to columns through [`RunStatusKind::board_rank`]; `archived`
/// has no rank because it selects on the archival overlay, not a status.
fn board_column_rank(column: BoardColumn) -> Option<u8> {
    match column {
        BoardColumn::Pending => Some(0),
        BoardColumn::Runnable => Some(1),
        BoardColumn::Initializing => Some(2),
        BoardColumn::Running => Some(3),
        BoardColumn::Blocked => Some(4),
        BoardColumn::Succeeded => Some(5),
        BoardColumn::Failed => Some(6),
        BoardColumn::Archived => None,
        BoardColumn::Removing => Some(8),
    }
}

async fn link_run_parent(
    RequireRunManagementTarget(child_id, actor): RequireRunManagementTarget,
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
    let child = match state
        .stores
        .runs
        .get_cached_summary(&child_id, Utc::now())
        .await
    {
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

    let Ok(run_store) = state.stores.runs.open_run(&child_id).await else {
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
    RequireRunManagementTarget(child_id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    let _parent_link_guard = state.parent_link_lock.lock().await;
    let child = match state
        .stores
        .runs
        .get_cached_summary(&child_id, Utc::now())
        .await
    {
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

    let Ok(run_store) = state.stores.runs.open_run(&child_id).await else {
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
            .stores
            .runs
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
    match run_summary_at(state, run_id, Utc::now()).await {
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

/// Read the durable summary and overlay its timing from the live projection.
///
/// The SQLite read model stores active timing as of the most recent event.
/// An open inference or tool bracket keeps accruing between events, so detail
/// reads need the projection's current estimate while the run is non-terminal.
async fn run_summary_at(
    state: &AppState,
    run_id: &RunId,
    now: DateTime<Utc>,
) -> fabro_store::Result<Option<Run>> {
    let Some(mut summary) = state.stores.run_summaries.get(run_id, now).await? else {
        return Ok(None);
    };
    if summary.timestamps.completed_at.is_none() {
        let projection = state.stores.runs.get_cached_projection(run_id).await?;
        if let Some(timing) = projection.and_then(|projection| projection.live_run_timing(now)) {
            summary.timing = Some(timing);
        }
    }
    Ok(Some(summary))
}

async fn list_runs(
    _auth: RequiredRunManagementActor,
    State(state): State<Arc<AppState>>,
    ExtraQuery(params): ExtraQuery<ListRunsParams>,
) -> Response {
    run_summary_page_response(&state, &params.summary_query()).await
}

/// List run summaries matching `query`, decorate them, and wrap them in the
/// paginated list envelope. Shared by the runs and automation-runs lists.
pub(super) async fn run_summary_page_response(
    state: &AppState,
    query: &RunSummaryListQuery,
) -> Response {
    match state.stores.run_summaries.list(query, Utc::now()).await {
        Ok(page) => {
            let data = state.decorate_run_summaries(page.data).await;
            (
                StatusCode::OK,
                Json(ListResponse::paginated(data, page.has_more, page.total)),
            )
                .into_response()
        }
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
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
    _auth: RequiredRunManagementActor,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ResolveRunQuery>,
) -> Response {
    let identities = match state.stores.run_summaries.list_identities().await {
        Ok(identities) => identities,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let resolved_id = match resolve_run_by_selector(
        &identities,
        &query.selector,
        |run| run.id.to_string(),
        |run| run.workflow_slug.clone(),
        |run| run.workflow_name.clone(),
        |run| run.id.created_at(),
        |run| run.id.created_at().to_rfc3339(),
        |run| run.repository_origin_url.clone(),
    ) {
        Ok(identity) => identity.id,
        Err(err @ (ResolveRunError::InvalidSelector | ResolveRunError::AmbiguousPrefix { .. })) => {
            return ApiError::bad_request(err.to_string()).into_response();
        }
        Err(err @ ResolveRunError::NotFound { .. }) => {
            return ApiError::not_found(err.to_string()).into_response();
        }
    };
    updated_run_response(&state, &resolved_id).await
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

    match delete_run_internal(state.as_ref(), id, query.force).await {
        Ok(DeleteRunOutcome::Deleted | DeleteRunOutcome::AlreadyAbsent) => {
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(DeleteRunOutcome::Preserved(response)) => {
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(error) => error.into_response(),
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
    let current = match state.stores.runs.get_cached_summary(&id, Utc::now()).await {
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

    let run_store = match state.stores.runs.open_run(&id).await {
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

    match state.stores.runs.get_cached_summary(&id, Utc::now()).await {
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
    RequiredRunManagementActor(actor): RequiredRunManagementActor,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let req = match serde_json::from_slice::<RunManifest>(&body) {
        Ok(req) => req,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let explicit_title_supplied = req.title.is_some();
    Box::pin(create_run_from_manifest(
        state,
        CreateRunFromManifestRequest {
            manifest: req,
            submitted_manifest_bytes: body.to_vec(),
            explicit_run_id: None,
            explicit_title_supplied,
            actor,
            headers,
            automation: None,
        },
    ))
    .await
}

pub(crate) struct CreateRunFromManifestRequest {
    pub(crate) manifest:                 RunManifest,
    pub(crate) submitted_manifest_bytes: Vec<u8>,
    /// Run ID preallocated by server-side automation code, never supplied by
    /// an HTTP create body.
    pub(crate) explicit_run_id:          Option<RunId>,
    pub(crate) explicit_title_supplied:  bool,
    pub(crate) actor:                    Principal,
    pub(crate) headers:                  HeaderMap,
    pub(crate) automation:               Option<AutomationRef>,
}

struct ManifestRunCompilerAdapter {
    workflow_bundle:      WorkflowBundle,
    entrypoint:           ManifestPath,
    cwd:                  PathBuf,
    project_settings:     Vec<ProjectSettingsSource>,
    user_toml:            Vec<String>,
    run_overrides:        Option<RunLayer>,
    cli_overrides:        Option<CliLayer>,
    input_overrides:      HashMap<String, toml::Value>,
    inline_goal_override: Option<String>,
}

fn adapt_manifest_source_for_run_compiler(
    manifest: &RunManifest,
) -> anyhow::Result<ManifestRunCompilerAdapter> {
    if manifest.version != 1 {
        anyhow::bail!("unsupported manifest version {}", manifest.version);
    }
    let cwd = PathBuf::from(&manifest.cwd);
    let entrypoint = ManifestPath::from_wire(&manifest.target.path)
        .ok_or_else(|| anyhow::anyhow!("invalid manifest target path: {}", manifest.target.path))?;
    let workflow_bundle = run_manifest::workflow_bundle_from_manifest(&manifest.workflows)?;
    if workflow_bundle.workflow(&entrypoint).is_none() {
        anyhow::bail!("manifest target path is missing from workflows map");
    }
    let overrides = run_manifest::manifest_args_overrides(manifest.args.as_ref())
        .context("failed to parse manifest args")?;
    let project_settings = manifest
        .configs
        .iter()
        .filter(|config| config.type_ == ManifestConfigType::Project)
        .filter_map(|config| config.source.as_ref().map(|source| (config, source)))
        .map(|(config, source)| ProjectSettingsSource {
            path: normalize_project_settings_path(config.path.as_deref(), &cwd),
            toml: source.clone(),
        })
        .collect();
    let user_toml = manifest
        .configs
        .iter()
        .filter(|config| config.type_ == ManifestConfigType::User)
        .filter_map(|config| config.source.clone())
        .collect();
    let inline_goal_override = manifest
        .goal
        .as_ref()
        .filter(|goal| goal.type_ != ManifestGoalType::Graph)
        .map(|goal| goal.text.clone());

    Ok(ManifestRunCompilerAdapter {
        workflow_bundle,
        entrypoint,
        cwd,
        project_settings,
        user_toml,
        run_overrides: overrides.run,
        cli_overrides: overrides.cli,
        input_overrides: overrides.input_overrides,
        inline_goal_override,
    })
}

fn normalize_project_settings_path(
    path: Option<&str>,
    cwd: &std::path::Path,
) -> Result<ManifestPath, ProjectSettingsPathError> {
    let path = path.ok_or(ProjectSettingsPathError::Missing)?;
    let path_ref = std::path::Path::new(path);
    let manifest_path = if path_ref.is_absolute() {
        ManifestPath::from_absolute(path_ref, cwd)
    } else {
        ManifestPath::from_wire(path)
    };
    manifest_path.ok_or_else(|| ProjectSettingsPathError::Invalid {
        path: path.to_string(),
    })
}

struct ManifestRunIdentity {
    run_id:    Option<RunId>,
    parent_id: Option<RunId>,
    title:     Option<String>,
}

fn manifest_run_identity(
    manifest: &RunManifest,
    explicit_run_id: Option<RunId>,
) -> anyhow::Result<ManifestRunIdentity> {
    let title = manifest
        .title
        .as_ref()
        .map(|title| fabro_types::normalize_explicit_run_title(title.as_str()))
        .transpose()?;
    let parent_id = manifest
        .parent_id
        .as_deref()
        .map(str::parse::<RunId>)
        .transpose()
        .context("invalid parent run ID")?;
    Ok(ManifestRunIdentity {
        run_id: explicit_run_id,
        parent_id,
        title,
    })
}

/// Map a [`RunCompilerError`] onto the create endpoint's pre-extraction wire
/// contract. The 400 details for source, settings, and interpolation errors
/// are the error types' own `Display` strings, which are pinned to the
/// legacy messages.
fn run_compiler_error_response(error: RunCompilerError) -> Response {
    match error {
        RunCompilerError::InvalidSource(_)
        | RunCompilerError::InvalidSettings(_)
        | RunCompilerError::VariableInterpolation(_) => {
            ApiError::bad_request(error.to_string()).into_response()
        }
        RunCompilerError::Workflow(
            WorkflowError::ValidationFailed { .. } | WorkflowError::Parse(_),
        ) => ApiError::bad_request("Validation failed").into_response(),
        RunCompilerError::Workflow(
            err @ (WorkflowError::ModelSelection(_) | WorkflowError::ModelReference(_)),
        ) => ApiError::bad_request(err.to_string()).into_response(),
        RunCompilerError::Workflow(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to persist run state: {err}"),
        )
        .into_response(),
    }
}

pub(crate) async fn create_run_from_manifest(
    state: Arc<AppState>,
    request: CreateRunFromManifestRequest,
) -> Response {
    let CreateRunFromManifestRequest {
        manifest,
        submitted_manifest_bytes,
        explicit_run_id,
        explicit_title_supplied,
        actor,
        headers,
        automation,
    } = request;
    let manifest_run_defaults = state.manifest_run_defaults();
    let manifest_environment_defaults = state.environment_store().catalog_layer();
    let manifest_mcp_server_catalog = state.mcp_server_store().catalog_settings();
    let manifest_adapter = match adapt_manifest_source_for_run_compiler(&manifest) {
        Ok(adapter) => adapter,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let title_generation_target = manifest_adapter.entrypoint.clone();
    let raw_compiler_input = RawRunCompilerInput {
        workflow_bundle: manifest_adapter.workflow_bundle,
        entrypoint: manifest_adapter.entrypoint,
        cwd: manifest_adapter.cwd,
        server_run_defaults: manifest_run_defaults.as_ref().clone(),
        server_environment_defaults: manifest_environment_defaults.as_ref().clone(),
        server_mcp_catalog: manifest_mcp_server_catalog,
        project_settings: manifest_adapter.project_settings,
        user_toml: manifest_adapter.user_toml,
        run_overrides: manifest_adapter.run_overrides,
        cli_overrides: manifest_adapter.cli_overrides,
        input_overrides: manifest_adapter.input_overrides,
        inline_goal_override: manifest_adapter.inline_goal_override,
        run_id: None,
        title: None,
        parent_id: None,
        git: manifest.git.clone(),
        storage_root: state.server_storage_dir(),
        workflow_slug: None,
        workflow_version_id: None,
        provenance: run_provenance(&headers, &actor),
        web_url: None,
        submitted_manifest_bytes: Some(submitted_manifest_bytes),
        automation,
    };
    let normalized = match run_compiler::normalize_source(raw_compiler_input) {
        Ok(normalized) => normalized,
        Err(err) => return run_compiler_error_response(err),
    };
    let layered = match run_compiler::layer_settings(normalized) {
        Ok(layered) => layered,
        Err(err) => return run_compiler_error_response(err),
    };
    let vars = match snapshot_run_variables(&state).await {
        Ok(vars) => vars,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let prepared = match run_compiler::apply_run_variables(layered, vars) {
        Ok(prepared) => prepared,
        Err(err) => return run_compiler_error_response(err),
    };
    let identity = match manifest_run_identity(&manifest, explicit_run_id) {
        Ok(identity) => identity,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let prepared = prepared.with_identity(identity.run_id, identity.parent_id, identity.title);
    let (prepared, run_id) = prepared.resolve_run_id();
    let prepared = prepared.with_web_url(state.run_web_url(&run_id));
    let provider = run_manifest::effective_sandbox_provider(&prepared.settings().run);
    if let Some(error) =
        run_manifest::sandbox_provider_policy_error(&state.server_settings(), provider)
    {
        return ApiError::bad_request(error).into_response();
    }
    if let Some(parent_id) = prepared.parent_id() {
        if parent_id == run_id {
            return ApiError::bad_request("A run cannot be its own parent.").into_response();
        }
        if let Err(err) = validate_parent_link(&state, run_id, parent_id).await {
            return err.into_response();
        }
    }
    info!(run_id = %run_id, "Run created");

    let catalog = state.catalog();
    // Resolve once: we need both the provider IDs (for the run create input
    // and ask-fabro-readiness) and the LLM client itself (for the spawned
    // title-generation task). `ready_llm_provider_ids` would otherwise call
    // `resolve_llm_client` a second time and discard the client.
    let (llm_result, ready_provider_ids) = state.resolve_llm_client_with_ready_ids().await;
    let llm_client_for_title = llm_result.ok();
    let run_materialization_provider_ids = {
        #[cfg(any(test, feature = "test-support"))]
        {
            server_test_support::test_run_materialization_provider_ids(
                catalog.as_ref(),
                &ready_provider_ids,
            )
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            ready_provider_ids.clone()
        }
    };
    let pinned =
        match run_compiler::compile_and_pin(prepared, run_materialization_provider_ids, catalog)
            .await
        {
            Ok(pinned) => pinned,
            Err(err) => return run_compiler_error_response(err),
        };
    let persistence_input = run_compiler::assemble_run(pinned);
    let created = match Box::pin(operations::persist_create_run(
        state.stores.runs.as_ref(),
        persistence_input,
    ))
    .await
    {
        Ok(created) => created,
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to persist run state: {err}"),
            )
            .into_response();
        }
    };
    if let Err(err) = mirror_persisted_run_created_to_main(state.as_ref(), created.run_id).await {
        tracing::error!(
            run_id = %created.run_id,
            error = %err,
            "Failed to mirror persisted run creation to main event stream"
        );
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to record run creation.",
        )
        .into_response();
    }
    let created_at = created.run_id.created_at();
    let summary = match state
        .stores
        .runs
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
            let workflow_target = title_generation_target.to_string();
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
                model_id: title_model_id.to_string(),
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

        let run_store = match task.state.stores.runs.open_run(&task.run_id).await {
            Ok(store) => store,
            Err(err) => {
                tracing::debug!(run_id = %task.run_id, error = %err, "Failed to open run store for title update");
                return;
            }
        };
        let expected_title = task.deterministic_title;
        if let Err(err) = workflow_event::append_event_if(
            &run_store,
            &task.run_id,
            &workflow_event::Event::RunTitleUpdated {
                title: generated_title,
                actor: Some(Principal::System {
                    system_kind: SystemActorKind::Engine,
                }),
            },
            move |projection| projection.title().as_ref() == expected_title,
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
        subject: subject.clone(),
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
    let manifest_environment_defaults = state.environment_store().catalog_layer();
    let manifest_mcp_server_catalog = state.mcp_server_store().catalog_settings();
    let mut prepared = match run_manifest::prepare_manifest_with_environment_defaults(
        manifest_run_defaults.as_ref(),
        manifest_environment_defaults.as_ref(),
        &manifest_mcp_server_catalog,
        &req,
    ) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let vars = match snapshot_run_variables(&state).await {
        Ok(vars) => vars,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Err(err) = run_compiler::substitute_run_variables(&vars, &mut prepared.settings) {
        return ApiError::bad_request(format!("Run config variable interpolation failed: {err}"))
            .into_response();
    }
    let (llm_result, ready_providers) = state.resolve_llm_client_with_ready_ids().await;
    let mut validated = match run_manifest::validate_prepared_manifest_for_preflight(
        &prepared,
        state.catalog(),
        vars,
        &ready_providers,
    ) {
        Ok(validated) => validated,
        Err(WorkflowError::Parse(_)) => {
            return ApiError::bad_request("Validation failed").into_response();
        }
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    validated.promote_template_undefined_variables_to_errors();
    let response =
        match run_manifest::run_preflight(&state, &prepared, &validated, llm_result).await {
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
    let manifest_environment_defaults = state.environment_store().catalog_layer();
    let manifest_mcp_server_catalog = state.mcp_server_store().catalog_settings();
    let mut prepared = match run_manifest::prepare_manifest_with_environment_defaults(
        manifest_run_defaults.as_ref(),
        manifest_environment_defaults.as_ref(),
        &manifest_mcp_server_catalog,
        &req,
    ) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let vars = match snapshot_run_variables(&state).await {
        Ok(vars) => vars,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Err(err) = run_compiler::substitute_run_variables(&vars, &mut prepared.settings) {
        return ApiError::bad_request(format!("Run config variable interpolation failed: {err}"))
            .into_response();
    }
    let validated = match run_manifest::validate_prepared_manifest_with_vars(
        &prepared,
        state.catalog(),
        vars,
    ) {
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

async fn snapshot_run_variables(
    state: &AppState,
) -> Result<HashMap<String, String>, VariableError> {
    state.stores.variables.value_map().await
}

async fn get_run_status(
    RequireRunManagementTarget(id, _actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    match run_summary_at(&state, &id, Utc::now()).await {
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
    let cached = match state.cached_run(&id).await {
        Ok(cached) => cached,
        Err(err) => return err.into_response(),
    };
    (
        StatusCode::OK,
        Json(cached.projection.spec.settings.clone()),
    )
        .into_response()
}

async fn get_questions(
    RequireRunManagementTarget(id, _actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.cached_run(&id).await {
        Ok(cached) => {
            let questions = cached
                .projection
                .pending_interviews
                .values()
                .map(api_question_from_pending_interview)
                .collect::<Vec<_>>();
            (StatusCode::OK, Json(ListResponse::new(questions))).into_response()
        }
        Err(err) => err.into_response(),
    }
}

async fn submit_answer(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
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
    RequireRunManagementTarget(id, _actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.cached_run(&id).await {
        Ok(cached) => Json(&*cached.projection).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn get_run_logs(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    if state.stores.runs.open_run_reader(&id).await.is_err() {
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

async fn get_run_stage_context_window(
    RequireRunStageScoped(id, raw_stage_id): RequireRunStageScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    let stage_id = match parse_stage_id_path(&raw_stage_id) {
        Ok(stage_id) => stage_id,
        Err(response) => return response,
    };
    let cached = match state.cached_run(&id).await {
        Ok(cached) => cached,
        Err(err) => return err.into_response(),
    };
    let Some(stage) = cached.projection.stage(&stage_id) else {
        return ApiError::not_found("Stage not found.").into_response();
    };

    if !is_agent_context_window_stage(stage) {
        return Json(StageContextWindow::unavailable(
            stage_id,
            StageContextWindowUnavailableReason::NotAgentStage,
            "Context-window data is only available for agent stages.",
        ))
        .into_response();
    }

    let Some(snapshot) = stage.context_window.as_ref() else {
        return Json(StageContextWindow::unavailable(
            stage_id,
            StageContextWindowUnavailableReason::NotObserved,
            "No context-window snapshot has been observed for this stage.",
        ))
        .into_response();
    };

    let mut response = StageContextWindow::available(stage_id, snapshot);
    if stage.state.is_terminal() {
        response.staleness = StageContextWindowStaleness::Stored;
    }
    Json(response).into_response()
}

fn is_agent_context_window_stage(stage: &StageProjection) -> bool {
    if stage.context_window.is_some() {
        return true;
    }
    if stage.handler == Some(StageHandler::Agent) {
        return true;
    }
    stage.provider_used.as_ref().is_some_and(|usage| {
        usage.mode == StageModelUsage::MODE_AGENT || usage.mode == StageModelUsage::MODE_ACP
    })
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
    let cached = match state.cached_run(&id).await {
        Ok(cached) => cached,
        Err(err) => return err.into_response(),
    };
    let Some(node) = cached.projection.stage(&stage_id) else {
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
        let run_store = match state.stores.runs.open_run_reader(&id).await {
            Ok(run_store) => run_store,
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        };
        let text = match read_json_string_blob(&run_store.into(), &cas_ref).await {
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
