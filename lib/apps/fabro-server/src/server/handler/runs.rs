use std::collections::{HashMap, HashSet};
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
    BoardColumn, RunIntent, RunManifest, SubmitAnswerRequest, UpdateRunParentRequest,
    UpdateRunRequest,
};
use fabro_config::{Storage, project};
use fabro_environment::{DEFAULT_ENVIRONMENT_ID, EnvironmentId};
use fabro_interview::AnswerSubmission;
use fabro_llm::Client as LlmClient;
use fabro_manifest::RunOverrideInput;
use fabro_static::EnvVars;
use fabro_store::platform_records::{PlatformRecord, RunParentRecord, RunTitleRecord};
use fabro_store::{
    RunSummaryListQuery, RunSummarySort, RunSummarySortDirection, RunSummaryVisibility,
};
use fabro_types::diagnostic::Severity;
use fabro_types::settings::run::RunMode;
use fabro_types::{
    AutomationRef, ContextWindowStaleness, ManifestPath, Principal, Run, RunClientProvenance,
    RunId, RunProvenance, RunServerProvenance, RunStatusKind, RunTarget, SandboxProviderKind,
    StageContextWindow, StageContextWindowUnavailableReason, StageHandler, StageModelUsage,
    StageProjection, ValidatedRunTarget, json_scalar_to_toml_value, parse_blob_ref,
};
use fabro_util::error as error_util;
use fabro_util::version::FABRO_VERSION;
use fabro_workflow::pipeline::Validated;
use fabro_workflow::run_status::RunStatus;
use fabro_workflow::{Error as WorkflowError, operations};
use lithos_llm::catalog::ProviderId;
use serde::de::IgnoredAny;
use strum::VariantArray as _;
use tokio::{fs, task};
use tracing::info;

use super::super::{
    AppState, DeleteRunOutcome, ListResponse, RunExecutionMode, VariableError, answer_from_request,
    api_question_from_pending_interview, clamp_page_limit, clamp_page_offset, default_page_limit,
    delete_run_internal, load_pending_interview, managed_run, parse_run_id_path,
    parse_stage_id_path, petri_runs, reject_if_archived, run_records,
    submit_pending_interview_answer,
};
use crate::error::ApiError;
use crate::principal_middleware::{
    RequireCommandLog, RequireRunManagementTarget, RequireRunScoped, RequireRunStageScoped,
    RequiredRunManagementActor, RequiredUser,
};
use crate::run_compiler::{self, RawRunCompilerInput};
use crate::run_files::{list_run_commits, list_run_files};
use crate::run_intent::{
    EnvironmentSelectionError, PreparedIntentTarget, RunIntentAdmissionError,
    lower_workflow_closure, pin_workflow_environment_authority, prepare_intent_target,
};
use crate::run_selector::{ResolveRunError, resolve_run_by_selector};
use crate::run_title_generation::{self, GenerateTitleInput, TitlePromptInput, WorkflowSummary};
#[cfg(any(test, feature = "test-support"))]
use crate::test_support as server_test_support;
use crate::{petri_check, run_manifest};

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
    let child = match state.stores.run_summaries.get(&child_id, Utc::now()).await {
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

    let _ = actor;
    let record = PlatformRecord::RunParent(RunParentRecord {
        parent_id:          Some(parent_id),
        previous_parent_id: child.parent_id,
    });
    if let Err(err) = run_records::append(&state, child_id, record).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    updated_run_response(&state, &child_id).await
}

async fn unlink_run_parent(
    RequireRunManagementTarget(child_id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    let _parent_link_guard = state.parent_link_lock.lock().await;
    let child = match state.stores.run_summaries.get(&child_id, Utc::now()).await {
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

    let _ = actor;
    let record = PlatformRecord::RunParent(RunParentRecord {
        parent_id:          None,
        previous_parent_id: Some(previous_parent_id),
    });
    if let Err(err) = run_records::append(&state, child_id, record).await {
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
            .run_summaries
            .get(&current_id, Utc::now())
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
        let projection = state.stores.runs.load_run_projection(run_id).await?;
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
    let current = match state.stores.run_summaries.get(&id, Utc::now()).await {
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

    let _ = subject;
    if let Err(err) = run_records::append(
        &state,
        id,
        PlatformRecord::RunTitle(RunTitleRecord { title }),
    )
    .await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    match state.stores.run_summaries.get(&id, Utc::now()).await {
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
    // Decode the original bytes so duplicate fields and error locations survive.
    let intent = match serde_json::from_slice::<RunIntent>(&body) {
        Ok(intent) => intent,
        Err(error) => return create_run_parse_error(&body, &error),
    };
    Box::pin(create_run_from_intent(state, CreateRunFromIntentRequest {
        intent,
        explicit_run_id: None,
        actor,
        headers,
        automation: None,
    }))
    .await
}

fn create_run_parse_error(body: &[u8], intent_error: &serde_json::Error) -> Response {
    if let Err(error) = serde_json::from_slice::<IgnoredAny>(body) {
        return ApiError::with_code(StatusCode::BAD_REQUEST, error.to_string(), "invalid_json")
            .into_response();
    }
    ApiError::with_code(
        StatusCode::UNPROCESSABLE_ENTITY,
        intent_error.to_string(),
        "run_intent_invalid",
    )
    .into_response()
}

pub(crate) struct CreateRunFromIntentRequest {
    pub(crate) intent:          RunIntent,
    /// Run ID preallocated by server-side automation code, never supplied by
    /// an HTTP create body.
    pub(crate) explicit_run_id: Option<RunId>,
    pub(crate) actor:           Principal,
    pub(crate) headers:         HeaderMap,
    pub(crate) automation:      Option<AutomationRef>,
}

pub(crate) async fn create_run_from_intent(
    state: Arc<AppState>,
    request: CreateRunFromIntentRequest,
) -> Response {
    let CreateRunFromIntentRequest {
        intent,
        explicit_run_id,
        actor,
        headers,
        automation,
    } = request;
    let explicit_title_supplied = intent.title.is_some();
    // Validate the pure, in-memory request facts before paying for
    // blob-store reads and closure lowering.
    let ValidatedRunTarget { target, git } = match intent.target.validate() {
        Ok(validated) => validated,
        Err(error) => return run_intent_admission_error(error.into()),
    };
    let environment_id = match select_intent_environment_id(
        &state,
        intent
            .environment_id
            .as_deref()
            .unwrap_or(DEFAULT_ENVIRONMENT_ID),
    ) {
        Ok(id) => id,
        Err(error) => return run_intent_admission_error(error.into()),
    };
    if let Err(error) = validate_intent_actor_target(&state, &actor, &target).await {
        return run_intent_admission_error(error);
    }
    let blobs = state.store_ref().blobs();
    let version_store = fabro_workflow_version::WorkflowVersionStore::new(blobs);
    let closure = match version_store.get_closure(&intent.workflow_version_id).await {
        Ok(Some(closure)) => closure,
        Ok(None) => {
            return intent_error(
                StatusCode::NOT_FOUND,
                "workflow version not found",
                "workflow_version_not_found",
            );
        }
        Err(source) => {
            return run_intent_admission_error(RunIntentAdmissionError::VersionStore { source });
        }
    };
    let mut lowered = match lower_workflow_closure(&closure) {
        Ok(lowered) => lowered,
        Err(error) => return run_intent_admission_error(error.into()),
    };
    if let Some(layer) = lowered.workflow_layer.as_mut() {
        pin_workflow_environment_authority(layer, environment_id.as_str());
    }

    let title = match intent
        .title
        .as_deref()
        .map(fabro_types::normalize_explicit_run_title)
        .transpose()
    {
        Ok(title) => title,
        Err(err) => {
            return intent_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                err.to_string(),
                "run_intent_invalid",
            );
        }
    };
    let mut input_overrides = HashMap::new();
    for (name, value) in &intent.args.inputs {
        let value = match json_scalar_to_toml_value(value) {
            Ok(value) => value,
            Err(err) => {
                return intent_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("args.inputs.{name}: {err}"),
                    "run_intent_invalid",
                );
            }
        };
        input_overrides.insert(name.clone(), value);
    }
    let run_overrides = fabro_manifest::build_run_overrides(RunOverrideInput {
        goal:             None,
        model:            intent.args.model.as_deref(),
        provider:         intent.args.provider.as_deref(),
        environment:      Some(environment_id.as_str()),
        preserve_sandbox: intent.args.preserve_sandbox,
        dry_run:          intent.args.dry_run,
        auto_approve:     intent.args.auto_approve,
        labels:           intent.args.labels,
    });

    let entrypoint = lowered.entrypoint.clone();
    let workflow_slug = project::workflow_slug_from_path(entrypoint.as_path());
    let raw_compiler_input = RawRunCompilerInput {
        workflow_bundle: lowered.workflow_bundle,
        entrypoint: lowered.entrypoint,
        // Intent compilation is isolated from target-project content. Folder
        // identity is projected to `source_directory` during persistence and
        // must never become a compiler lookup root.
        cwd: PathBuf::from("/workspace"),
        server_run_defaults: state.manifest_run_defaults().as_ref().clone(),
        server_environment_defaults: state.environment_store().catalog_layer().as_ref().clone(),
        server_mcp_catalog: state.mcp_server_store().catalog_settings(),
        workflow_layer: lowered.workflow_layer,
        run_overrides: Some(run_overrides),
        input_overrides,
        inline_goal_override: intent.goal,
        run_id: explicit_run_id,
        title,
        parent_id: intent.parent_id,
        // Target identity and its Git projection are attached after provider
        // admission via `with_target_and_git`; the compiler never reads them.
        git: None,
        storage_root: state.server_storage_dir(),
        workflow_slug,
        workflow_version_id: Some(intent.workflow_version_id),
        target: None,
        provenance: run_provenance(&headers, &actor),
        web_url: None,
        automation,
    };
    let normalized = match run_compiler::normalize_source(raw_compiler_input) {
        Ok(normalized) => normalized,
        Err(error) => return run_intent_admission_error(error.into()),
    };
    let layered = match run_compiler::layer_settings(normalized) {
        Ok(layered) => layered,
        Err(error) => return run_intent_admission_error(error.into()),
    };
    let vars = match snapshot_run_variables(&state).await {
        Ok(vars) => vars,
        Err(source) => {
            return run_intent_admission_error(RunIntentAdmissionError::VariableSnapshot {
                source,
            });
        }
    };
    let mut prepared = match run_compiler::apply_run_variables(layered, vars) {
        Ok(prepared) => prepared,
        Err(error) => return run_intent_admission_error(error.into()),
    };
    if let Err(error) = validate_intent_environment(&state, prepared.settings(), &target).await {
        return run_intent_admission_error(error.into());
    }
    let PreparedIntentTarget { target, git } = match prepare_intent_target(target, git).await {
        Ok(prepared) => prepared,
        Err(error) => return run_intent_admission_error(error.into()),
    };
    prepared = prepared.with_target_and_git(target, git);
    let (prepared, run_id) = prepared.resolve_run_id();
    if let Err(response) = validate_optional_parent(&state, run_id, prepared.parent_id()).await {
        return response;
    }
    let prepared = prepared.with_web_url(state.run_web_url(&run_id));
    finalize_created_run(state, prepared, explicit_title_supplied, entrypoint).await
}

/// A run must not be its own parent; an explicit parent must pass
/// [`validate_parent_link`].
async fn validate_optional_parent(
    state: &AppState,
    run_id: RunId,
    parent_id: Option<RunId>,
) -> Result<(), Response> {
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    if parent_id == run_id {
        return Err(ApiError::bad_request("A run cannot be its own parent.").into_response());
    }
    validate_parent_link(state, run_id, parent_id)
        .await
        .map_err(IntoResponse::into_response)
}

/// Compile, persist, register, and render the admitted run.
async fn finalize_created_run(
    state: Arc<AppState>,
    prepared: run_compiler::PreparedRun,
    explicit_title_supplied: bool,
    title_generation_target: ManifestPath,
) -> Response {
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
                state.catalog().as_ref(),
                &ready_provider_ids,
            )
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            ready_provider_ids.clone()
        }
    };
    // Petri compiles the run: the bundle goes to `Runtime::check`, its
    // diagnostics come back in Fabro's shape, and the admitted graph is what
    // the run executes. Fabro's own settings resolution ran above.
    let pinned = match petri_runs::admit(&state, &prepared, &run_materialization_provider_ids).await
    {
        Ok(admission) => run_compiler::compile_admitted(prepared, admission).await,
        Err(error) => Err(error),
    };
    let pinned = match pinned {
        Ok(pinned) => pinned,
        Err(error) => return run_intent_admission_error(error.into()),
    };
    let persistence_input = run_compiler::assemble_run(pinned);
    let created = match Box::pin(operations::persist_create_run(
        state.stores.runs.as_ref(),
        persistence_input,
    ))
    .await
    {
        Ok(created) => created,
        Err(error) => {
            tracing::error!(error = %error, error_chain = ?error_util::collect_chain(&error), "Failed to persist admitted run intent");
            return intent_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to persist run",
                "run_persistence_failed",
            );
        }
    };
    let created_at = created.run_id.created_at();
    // The run's summary row is the projector's: wait for the pass that
    // folds the run's first records before reading the run back.
    state.petri_projector.settle(created.run_id).await;
    let summary = match state
        .stores
        .run_summaries
        .get(&created.run_id, Utc::now())
        .await
    {
        Ok(Some(summary)) => summary,
        Ok(None) => {
            return intent_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "created run summary is unavailable",
                "run_persistence_failed",
            );
        }
        Err(error) => {
            tracing::error!(error = %error, "Failed to read admitted run summary");
            return intent_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to read created run",
                "run_persistence_failed",
            );
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
            let title_catalog = state.catalog();
            if let Some(title_model) = title_catalog.small_default_for(&ready_provider_ids) {
                spawn_generated_title_task(GeneratedTitleTask {
                    state: Arc::clone(&state),
                    run_id: created.run_id,
                    deterministic_title,
                    workflow_target: title_generation_target.to_string(),
                    workflow,
                    run_inputs,
                    client: llm_result.client,
                    model_id: title_model.model.id().to_string(),
                    provider_id: title_model.provider.id().clone(),
                });
            }
        }
    }
    info!(run_id = %created.run_id, "Run created from intent");
    (
        StatusCode::CREATED,
        Json(state.decorate_run_summary(summary).await),
    )
        .into_response()
}

fn intent_error(status: StatusCode, detail: impl Into<String>, code: &'static str) -> Response {
    ApiError::with_code(status, detail, code).into_response()
}

fn run_intent_admission_error(error: RunIntentAdmissionError) -> Response {
    match &error {
        RunIntentAdmissionError::VersionStore { .. }
        | RunIntentAdmissionError::VariableSnapshot { .. }
        | RunIntentAdmissionError::WorkerRun { .. }
        | RunIntentAdmissionError::Environment(EnvironmentSelectionError::CredentialStore {
            ..
        }) => {
            tracing::error!(
                error = %error,
                error_chain = ?error_util::collect_chain(&error),
                "Run intent admission failed"
            );
        }
        RunIntentAdmissionError::Lowering(_) | RunIntentAdmissionError::Compiler(_) => {
            tracing::warn!(
                error = %error,
                error_chain = ?error_util::collect_chain(&error),
                "Run intent admission rejected"
            );
        }
        RunIntentAdmissionError::Target(_)
        | RunIntentAdmissionError::FolderTarget(_)
        | RunIntentAdmissionError::WorkerRunNotFound { .. }
        | RunIntentAdmissionError::Environment(_) => {}
    }

    match error {
        RunIntentAdmissionError::VersionStore { .. } => intent_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "workflow version store operation failed",
            "workflow_version_store_error",
        ),
        RunIntentAdmissionError::Lowering(error) => intent_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("workflow version cannot be used for run creation: {error}"),
            "workflow_version_unusable",
        ),
        RunIntentAdmissionError::Target(error) => intent_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            error.to_string(),
            "target_invalid",
        ),
        RunIntentAdmissionError::FolderTarget(error) => intent_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            error.to_string(),
            "target_invalid",
        ),
        RunIntentAdmissionError::Environment(error) => match error {
            EnvironmentSelectionError::InvalidId { source, .. } => intent_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                source.to_string(),
                "run_intent_invalid",
            ),
            EnvironmentSelectionError::NotFound { .. } => intent_error(
                StatusCode::NOT_FOUND,
                error.to_string(),
                "environment_not_found",
            ),
            EnvironmentSelectionError::TargetUnsupported { .. } => intent_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                error.to_string(),
                "target_environment_unsupported",
            ),
            EnvironmentSelectionError::AutomaticPullRequestUnsupported => intent_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                error.to_string(),
                "pull_request_environment_unsupported",
            ),
            EnvironmentSelectionError::ProviderDisabled { .. }
            | EnvironmentSelectionError::MissingCredential { .. } => intent_error(
                StatusCode::SERVICE_UNAVAILABLE,
                error.to_string(),
                "integration_unavailable",
            ),
            // Nothing has been persisted yet on this path, so the code names
            // the failing subsystem instead of claiming a persistence
            // failure.
            EnvironmentSelectionError::CredentialStore { .. } => intent_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to read sandbox credentials",
                "credential_store_error",
            ),
        },
        // Return the curated compiler detail; retain its source chain in the log.
        // A validation failure names its diagnostics, since the message alone
        // ("Validation failed") tells the caller nothing to fix.
        RunIntentAdmissionError::Compiler(error) => intent_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "run intent could not be compiled: {}",
                compiler_error_detail(&error)
            ),
            "run_compile_invalid",
        ),
        RunIntentAdmissionError::VariableSnapshot { .. } => intent_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to load run variables",
            "variable_store_error",
        ),
        RunIntentAdmissionError::WorkerRun { .. } => intent_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to inspect originating worker run",
            "worker_run_store_error",
        ),
        RunIntentAdmissionError::WorkerRunNotFound { .. } => intent_error(
            StatusCode::NOT_FOUND,
            "originating worker run not found",
            "worker_run_not_found",
        ),
    }
}

/// The compiler error's text, with every error diagnostic of a validation
/// failure listed as `rule: message`.
fn compiler_error_detail(error: &run_compiler::RunCompilerError) -> String {
    let run_compiler::RunCompilerError::Workflow(WorkflowError::ValidationFailed { diagnostics }) =
        error
    else {
        return error.to_string();
    };
    let listed = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| format!("{}: {}", diagnostic.rule, diagnostic.message))
        .collect::<Vec<_>>();
    if listed.is_empty() {
        error.to_string()
    } else {
        format!("{error}: {}", listed.join("; "))
    }
}

async fn validate_intent_actor_target(
    state: &AppState,
    actor: &Principal,
    target: &RunTarget,
) -> Result<(), RunIntentAdmissionError> {
    let (Principal::Worker { run_id }, RunTarget::Folder { .. }) = (actor, target) else {
        return Ok(());
    };
    let projection = state
        .stores
        .runs
        .load_run_projection(run_id)
        .await
        .map_err(|source| RunIntentAdmissionError::WorkerRun {
            run_id: *run_id,
            source,
        })?
        .ok_or(RunIntentAdmissionError::WorkerRunNotFound { run_id: *run_id })?;
    if !projection.spec.settings.run.environment.provider.is_local() {
        return Err(EnvironmentSelectionError::TargetUnsupported {
            detail: "folder targets created by a worker require a Local parent environment",
        }
        .into());
    }
    Ok(())
}

fn select_intent_environment_id(
    state: &AppState,
    value: &str,
) -> Result<EnvironmentId, EnvironmentSelectionError> {
    let id =
        value
            .parse::<EnvironmentId>()
            .map_err(|source| EnvironmentSelectionError::InvalidId {
                value: value.to_string(),
                source,
            })?;
    if state.environment_store().get(&id).is_none() {
        return Err(EnvironmentSelectionError::NotFound { id });
    }
    Ok(id)
}

async fn validate_intent_environment(
    state: &AppState,
    settings: &fabro_types::WorkflowSettings,
    target: &RunTarget,
) -> Result<(), EnvironmentSelectionError> {
    let configured_provider = run_manifest::configured_sandbox_provider(&settings.run);
    let effective_provider = run_manifest::effective_sandbox_provider(&settings.run);
    let image = &settings.run.environment.image;
    let image_incompatible = effective_provider == SandboxProviderKind::DOCKER
        && image.docker.is_none()
        && image.dockerfile.is_some();
    let (target_incompatible, detail) = match target {
        RunTarget::Git(_) => (
            configured_provider == SandboxProviderKind::LOCAL || !settings.run.clone.enabled,
            "Git targets require a compatible clone-enabled Docker or Daytona environment",
        ),
        RunTarget::None {} => (
            configured_provider == SandboxProviderKind::LOCAL,
            "none targets require a compatible Docker or Daytona environment",
        ),
        RunTarget::Folder { .. } => (
            configured_provider != SandboxProviderKind::LOCAL,
            "folder targets require a Local environment",
        ),
    };
    if image_incompatible || target_incompatible {
        return Err(EnvironmentSelectionError::TargetUnsupported { detail });
    }
    // Settings resolution drops `run.pull_request` unless it is enabled, so
    // `Some` means automatic pull requests were requested.
    if !configured_provider.clones_workspace() && settings.run.pull_request.is_some() {
        return Err(EnvironmentSelectionError::AutomaticPullRequestUnsupported);
    }
    if let Some(detail) =
        run_manifest::sandbox_provider_policy_error(&state.server_settings(), &effective_provider)
    {
        return Err(EnvironmentSelectionError::ProviderDisabled {
            provider: effective_provider,
            detail,
        });
    }
    if effective_provider == SandboxProviderKind::DAYTONA {
        match state.vault_secret(EnvVars::DAYTONA_API_KEY).await {
            Ok(Some(key)) if !key.trim().is_empty() => {}
            Ok(_) => {
                return Err(EnvironmentSelectionError::MissingCredential {
                    provider: effective_provider,
                    name:     EnvVars::DAYTONA_API_KEY,
                });
            }
            Err(source) => {
                return Err(EnvironmentSelectionError::CredentialStore {
                    name: EnvVars::DAYTONA_API_KEY,
                    source,
                });
            }
        }
    }
    Ok(())
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
    provider_id:         ProviderId,
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

        // The generated title replaces the deterministic one only while the
        // run still carries it: a title someone set meanwhile stays.
        let current = match run_records::projection(&task.state, task.run_id).await {
            Ok(Some(projection)) => projection.title().to_string(),
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(run_id = %task.run_id, error = %err, "Failed to load the run for its title update");
                return;
            }
        };
        if current != task.deterministic_title {
            return;
        }
        if let Err(err) = run_records::append(
            &task.state,
            task.run_id,
            PlatformRecord::RunTitle(RunTitleRecord {
                title: generated_title,
            }),
        )
        .await
        {
            tracing::warn!(run_id = %task.run_id, error = %err, "Failed to record the generated run title");
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
    let mut validated =
        match validate_manifest_on_petri(&state, &prepared, vars, &ready_providers).await {
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
    let (_, ready_providers) = state.resolve_llm_client_with_ready_ids().await;
    let validated =
        match validate_manifest_on_petri(&state, &prepared, vars, &ready_providers).await {
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

/// Validate a prepared manifest as a run would be admitted: Fabro's
/// structural pass, then Petri's check with the model client over the ready
/// providers, on the blocking pool.
async fn validate_manifest_on_petri(
    state: &Arc<AppState>,
    prepared: &run_manifest::PreparedManifest,
    vars: HashMap<String, String>,
    ready_providers: &[ProviderId],
) -> Result<Validated, WorkflowError> {
    let launch = petri_check::launch(
        &state.catalog(),
        &prepared.settings,
        ready_providers,
        None,
        None,
    );
    let dry_run = prepared.settings.run.execution.mode == RunMode::DryRun;
    let runtime = petri_runs::runtime_spec(state, ready_providers, dry_run);
    let has_ready_provider = !ready_providers.is_empty();
    let prepared = prepared.clone();
    task::spawn_blocking(move || {
        run_manifest::validate_prepared_manifest(
            &prepared,
            &vars,
            launch,
            runtime,
            has_ready_provider,
            false,
        )
    })
    .await
    .map_err(|source| WorkflowError::engine_with_source("manifest check task failed", source))?
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
    let projection = match state.load_run_projection(&id).await {
        Ok(projection) => projection,
        Err(err) => return err.into_response(),
    };
    (StatusCode::OK, Json(projection.spec.settings.clone())).into_response()
}

async fn get_questions(
    RequireRunManagementTarget(id, _actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.load_run_projection(&id).await {
        Ok(projection) => {
            let questions = projection
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
    match state.load_run_projection(&id).await {
        Ok(projection) => Json(&*projection).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn get_run_logs(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    if let Err(err) = state.load_run_projection(&id).await {
        return err.into_response();
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
    let projection = match state.load_run_projection(&id).await {
        Ok(projection) => projection,
        Err(err) => return err.into_response(),
    };
    let Some(stage) = projection.stage(&stage_id) else {
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

    let Some(snapshot) = stage
        .agent
        .as_ref()
        .and_then(|agent| agent.context_window.as_ref())
    else {
        return Json(StageContextWindow::unavailable(
            stage_id,
            StageContextWindowUnavailableReason::NotObserved,
            "No context-window snapshot has been observed for this stage.",
        ))
        .into_response();
    };

    let mut response = StageContextWindow::available(stage_id, snapshot);
    if stage.state.is_terminal() {
        response.staleness = ContextWindowStaleness::Stored;
    }
    Json(response).into_response()
}

fn is_agent_context_window_stage(stage: &StageProjection) -> bool {
    if stage
        .agent
        .as_ref()
        .is_some_and(|agent| agent.context_window.is_some())
    {
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
    let projection = match state.load_run_projection(&id).await {
        Ok(projection) => projection,
        Err(err) => return err.into_response(),
    };
    let Some(node) = projection.stage(&stage_id) else {
        return ApiError::not_found("Stage not found.").into_response();
    };

    let stream_value = node.output.as_deref();
    let cas_ref = stream_value
        .filter(|value| parse_blob_ref(value).is_some())
        .map(str::to_string);
    let live_streaming = node
        .live_streaming
        .unwrap_or_else(|| cas_ref.is_none() && node.completion.is_none());

    // A stage's output is on its record: inline, or in the blob table when
    // Petri offloaded it. The blob holds the output value as JSON (a string
    // for a command's output), so a string decodes and anything else is
    // served as written.
    if let Some(cas_ref) = cas_ref {
        let Some(hash) = parse_blob_ref(&cas_ref) else {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "invalid output blob ref")
                .into_response();
        };
        let text = match state.store_ref().blobs().read(&hash).await {
            Ok(Some(bytes)) => serde_json::from_slice::<String>(&bytes)
                .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned()),
            Ok(None) => String::new(),
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        };
        return build_command_log_response(
            query.offset,
            limit,
            text.as_bytes(),
            true,
            Some(cas_ref),
            live_streaming,
        );
    }

    if let Some(inline_text) = stream_value {
        return build_command_log_response(
            query.offset,
            limit,
            inline_text.as_bytes(),
            true,
            None,
            live_streaming,
        );
    }

    build_command_log_response(
        query.offset,
        limit,
        &[],
        node.completion.is_some(),
        None,
        live_streaming,
    )
}

fn build_command_log_response(
    requested_offset: u64,
    limit: u64,
    bytes: &[u8],
    eof: bool,
    cas_ref: Option<String>,
    live_streaming: bool,
) -> Response {
    let total_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let offset = requested_offset.min(total_bytes);
    let start = usize::try_from(offset).unwrap_or(bytes.len());
    let end = start
        .saturating_add(usize::try_from(limit).unwrap_or(usize::MAX))
        .min(bytes.len());
    let body_bytes = bytes[start..end].to_vec();
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
