use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::body::Body;
#[cfg(test)]
use axum::body::to_bytes;
use axum::extract::{self as axum_extract, DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::Key;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use chrono::{DateTime, Utc};
pub use fabro_api::types::{
    AggregateUsage, AggregateUsageTotals, ApiQuestion, ArtifactEntry, ArtifactListResponse,
    BatchDeleteRunsRequest, BatchDeleteRunsResponse, BatchDeleteRunsResult,
    BatchDeleteRunsResultOutcome, BatchDeleteRunsSummary, BatchRunLifecycleRequest,
    BatchRunLifecycleResponse, BatchRunLifecycleResult, BatchRunLifecycleResultOutcome,
    BatchRunLifecycleSummary, CloseRunPullRequestResponse, CompletionResponse,
    CreateCompletionRequest, CreateRunPullRequestRequest, CreateSecretRequest,
    CreateVariableRequest, DeleteRunResponse, DeleteRunSandbox, DeleteSecretRequest,
    DenyRunRequest, DiskUsageResponse, DiskUsageRunRow, DiskUsageSummaryRow, ErrorResponseEntry,
    IntegrationConnectionKind, IntegrationConnectionState, IntegrationConnectionStatus,
    IntegrationProvider, IntegrationStatus, LinkRunPullRequestRequest, MergeRunPullRequestRequest,
    MergeRunPullRequestResponse, ModelReference, PaginatedRunList, PaginationMeta,
    PreflightResponse, PreviewUrlRequest, PreviewUrlResponse, Provider,
    ProviderCredentialTestRequest, ProviderCredentialTestResponse, ProviderList, PruneRunEntry,
    PruneRunsRequest, PruneRunsResponse, RenderWorkflowGraphDirection, RenderWorkflowGraphRequest,
    Run, RunArtifactEntry, RunArtifactListResponse, RunError, RunManifest, RunStage, RunUsage,
    RunUsageStage, RunUsageTotals, SandboxDetails, SandboxFileEntry, SandboxFileListResponse,
    SandboxService, SandboxServiceListResponse, SshAccessRequest, SshAccessResponse, StageHandler,
    StageState, StartRunRequest, SubmitAnswerRequest, SystemCpuResourceScope, SystemCpuResources,
    SystemDiskResourceScope, SystemDiskResources, SystemInfoResponse, SystemIntegrationStatus,
    SystemIntegrationsResponse, SystemMemoryResourceScope, SystemMemoryResources,
    SystemRepairRunIssue, SystemRepairRunsResponse, SystemResourcesResponse, SystemRunCounts,
    UpdateVariableRequest, UsageByModel, UsageStageRef, VariableListResponse, VncPreviewResponse,
    WriteBlobResponse,
};
use fabro_auth::SqlVaultCredentialSource;
use fabro_automation::{self, AutomationStore};
use fabro_config::daemon::ServerDaemon;
use fabro_config::{LlmLayer, RunLayer, Storage, WorkflowSettingsBuilder};
use fabro_db::DbPool;
use fabro_environment::EnvironmentStore;
use fabro_interview::{
    Answer, AnswerSubmission, ControlInterviewer, Question, WorkerControlEnvelope,
    WorkerControlOutcome,
};
use fabro_llm::credentials::CredentialProvider;
use fabro_llm::lithos_catalog::Catalog;
use fabro_llm::{ClientOptions, FabroClient};
use fabro_mcp_store::McpServerStore;
use fabro_petri::controls::{RunControls, SteerError};
use fabro_petri::projector::Projector;
use fabro_redact::redact_jsonl_line;
use fabro_sandbox::details::sandbox_details;
use fabro_sandbox::driver::{DaytonaCredentials, ProviderAccess, ProviderConnectOptions};
use fabro_sandbox::reconnect::reconnect_for_run;
use fabro_sandbox::{SandboxInventory, daytona};
use fabro_slack::client::{PostedMessage as SlackPostedMessage, SlackClient};
use fabro_slack::config::{
    SlackCredentialResolution,
    resolve_credentials_status_with_lookup as resolve_slack_credentials_status_with_lookup,
};
use fabro_slack::payload::SlackAnswerSubmission;
use fabro_slack::threads::ThreadRegistry;
use fabro_slack::{blocks as slack_blocks, connection as slack_connection};
use fabro_static::EnvVars;
use fabro_store::platform_records::{
    InterviewAnsweredRecord, NotificationSentRecord, PlatformRecord, PlatformRecordKind,
    RunLifecycleKind, RunLifecycleRecord,
};
use fabro_store::{
    ArtifactKey, ArtifactStore, AuthCodeStore, AuthSessionStore, Database, KeyedMutex,
    NodeArtifact, PendingInterviewRecord, RunSessionEventStore, RunSessionRecordStore,
    RunSummaryStore, StageArtifactEntry, StageId,
};
#[cfg(test)]
use fabro_types::BlockedReason;
use fabro_types::settings::RunNamespace;
use fabro_types::settings::run::NotificationRouteSettings;
use fabro_types::settings::server::{
    GithubIntegrationSettings, GithubIntegrationStrategy, LogDestination,
};
use fabro_types::{
    AskFabro, AskFabroUnavailableReason, BlobHash, InterviewQuestionRecord, ModelRef,
    ModelTestMode, PendingReason, Principal, PullRequestLink, QuestionType, RunControlAction,
    RunId, RunRunnableSource, RunStatusKind, RunStreamItem, RunStreamItemKind, SandboxProviderKind,
    ServerSettings,
};
use fabro_util::error::{
    SharedError, collect_causes, render_compact_with_causes, render_with_causes,
};
use fabro_util::version::FABRO_VERSION;
use fabro_variable::{Error as VariableError, VariableStore};
use fabro_vault::{SecretStore, SecretStoreError, SecretType, Vault};
use fabro_workflow::run_lookup::{
    RunInfo, StatusFilter, filter_runs, scan_runs_with_summaries, scratch_base,
};
use fabro_workflow::run_status::{FailureReason, RunStatus, SuccessReason};
use fabro_workflow::{Error as WorkflowError, operations, pull_request};
use futures_util::future::join_all;
use lithos_llm::catalog::ProviderId;
use lithos_llm::types::Usage;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore, broadcast, mpsc, oneshot};
use tokio::task::spawn_blocking;
use tokio::time::{sleep, timeout};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::{BroadcastStream, UnboundedReceiverStream};
use tokio_util::sync::CancellationToken;
use tower::{ServiceExt, service_fn};
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::compression::{CompressionLayer, CompressionLevel};
use tracing::{Instrument, debug, error, info, warn};

use crate::auth::{self, GithubEndpoints, auth_translation_middleware, demo_routing_middleware};
use crate::automation_materializer::{
    AutomationRunMaterializeInput, AutomationRunMaterialized, AutomationRunMaterializer,
    ProductionAutomationRunMaterializer, RunMaterializeError,
};
use crate::canonical_origin::{canonical_origin_from_effective_web_url, effective_web_url};
use crate::error::ApiError;
use crate::git_checkout::GitRepoCache;
use crate::github_webhooks::{
    WEBHOOK_ROUTE, WEBHOOK_SECRET_ENV, parse_event_metadata, verify_signature,
};
use crate::jwt_auth::{self, AuthMode};
use crate::petri_runs::PetriRuns;
use crate::principal_middleware::{
    AuthContextSlot, RequestAuth, RequestAuthContext, RequireRunBlob, RequireRunManagementTarget,
    RequireRunScoped, RequireStageArtifact, RequireWorkerRunScoped, RequireWorkerRunSegment,
    RequiredUser, principal_middleware,
};
use crate::request_id::{self, RequestId};
use crate::run_files::{FilesInFlight, new_files_in_flight};
use crate::server_secrets::ServerSecrets;
use crate::spawn_env::apply_render_graph_env;
use crate::worker_control::{
    LocalWorkerControlBus, WORKER_CONTROL_ACK_WAIT, WorkerControlAcks, WorkerControlBus,
    WorkerControlBusError,
};
use crate::worker_runtime::{
    LocalWorkerRuntime, WorkerExit, WorkerLaunchSpec, WorkerRef, WorkerRuntime,
};
use crate::worker_token::{WorkerScopeSet, WorkerTokenKeys, issue_worker_token_with_scopes};
use crate::{
    canonical_host, demo, diagnostics, run_manifest, security_headers, static_files, web_auth,
};

mod automation_scheduler;
mod handler;
pub(crate) mod petri_runs;
mod pull_request_supervisor;
pub(crate) mod resource_sampler;
pub(crate) mod run_records;
mod session_runtime;
pub(crate) mod stream_follower;

pub(crate) use automation_scheduler::spawn_automation_scheduler;
pub(crate) use handler::graph::render_graph_bytes;
#[cfg(test)]
pub(in crate::server) use handler::graph::{
    RenderSubprocessError, render_dot_subprocess, render_graph_bytes_with_exe_override,
};
#[cfg(test)]
pub(in crate::server) use handler::system::validate_github_slug;
pub(crate) use pull_request_supervisor::spawn_pull_request_creation_supervisor;
use session_runtime::SessionRuntimeManager;

pub(crate) type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub fn default_page_limit() -> u32 {
    20
}

#[derive(serde::Deserialize)]
pub struct PaginationParams {
    #[serde(rename = "page[limit]", default = "default_page_limit")]
    pub limit:  u32,
    #[serde(rename = "page[offset]", default)]
    pub offset: u32,
}

pub(crate) fn clamp_page_limit(limit: u32) -> u32 {
    limit.clamp(1, 100)
}

pub(crate) fn clamp_page_offset(offset: u32) -> u32 {
    offset.min(MAX_PAGE_OFFSET)
}

pub(crate) fn paginate_items<T>(items: Vec<T>, pagination: &PaginationParams) -> (Vec<T>, bool) {
    let limit = clamp_page_limit(pagination.limit) as usize;
    let offset = clamp_page_offset(pagination.offset) as usize;
    let mut data: Vec<_> = items.into_iter().skip(offset).take(limit + 1).collect();
    let has_more = data.len() > limit;
    data.truncate(limit);
    (data, has_more)
}

#[derive(serde::Deserialize)]
pub(crate) struct DfParams {
    #[serde(default)]
    pub(crate) verbose: bool,
}

/// List response envelope with pagination metadata.
#[derive(serde::Serialize)]
pub struct ListResponse<T: serde::Serialize> {
    data: T,
    meta: PaginationMeta,
}

impl<T: serde::Serialize> ListResponse<T> {
    /// Non-paginated response with `has_more: false`.
    pub fn new(data: T) -> Self {
        Self {
            data,
            meta: PaginationMeta {
                has_more: false,
                total:    None,
            },
        }
    }

    pub fn paginated(data: T, has_more: bool, total: u64) -> Self {
        Self {
            data,
            meta: PaginationMeta {
                has_more,
                total: i64::try_from(total).ok(),
            },
        }
    }
}

/// Snapshot of a managed run.
struct ManagedRun {
    dot_source: String,
    status: RunStatus,
    error: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    // Populated when running:
    answer_transport: Option<RunAnswerTransport>,
    accepted_questions: HashSet<String>,
    /// Stage IDs of currently steerable live agent sessions, keyed to the
    /// session id that owns the active lease. Used by the steerability
    /// predicate for steer/interrupt controls.
    active_steerable_stages: HashMap<StageId, String>,
    /// API-mode session targets eligible for live pair control. ACP sessions
    /// can be steerable but are intentionally excluded from pairing.
    /// Stage IDs of currently running agent sessions that have no live
    /// steering capability, keyed to the session id that owns the marker.
    active_non_steerable_stages: HashMap<StageId, String>,
    cancel_tx: Option<oneshot::Sender<()>>,
    cancel_token: Option<CancellationToken>,
    worker_ref: Option<WorkerRef>,
    /// Exact worker currently covered by a cancellation escalation task.
    /// Prevents repeated cancel requests from arming duplicate watchdogs.
    cancel_escalation_worker: Option<WorkerRef>,
    run_dir: Option<std::path::PathBuf>,
    execution_mode: RunExecutionMode,
}

impl ManagedRun {
    /// True if cancellation should still escalate to `worker_ref`; clears a
    /// stale escalation marker as a side effect.
    fn escalation_still_current(&mut self, worker_ref: &WorkerRef) -> bool {
        let matches_watchdog = self.cancel_escalation_worker.as_ref() == Some(worker_ref);
        let still_current = matches_watchdog
            && !self.status.is_terminal()
            && self.worker_ref.as_ref() == Some(worker_ref);
        if matches_watchdog && !still_current {
            self.cancel_escalation_worker = None;
        }
        still_current
    }

    /// Clears the escalation marker if it is still owned by `worker_ref`.
    fn clear_escalation_for(&mut self, worker_ref: &WorkerRef) {
        if self.cancel_escalation_worker.as_ref() == Some(worker_ref) {
            self.cancel_escalation_worker = None;
        }
    }
}

#[derive(Clone, Copy)]
enum RunExecutionMode {
    Start,
    Resume,
}

const WORKER_CANCEL_GRACE: Duration = Duration::from_secs(5);
const TERMINAL_DELETE_WORKER_GRACE: Duration = Duration::from_millis(50);
const WORKER_CONTROL_ENQUEUE_TIMEOUT: Duration = Duration::from_secs(1);
/// Per-model usage totals.
#[derive(Default)]
pub(crate) struct ModelUsageTotals {
    pub(crate) stages: i64,
    pub(crate) usage:  Usage,
}

/// In-memory aggregate usage counters, reset on server restart.
#[derive(Default)]
pub(crate) struct UsageAccumulator {
    pub(crate) total_runs:   i64,
    pub(crate) total_timing: fabro_types::RunTiming,
    pub(crate) by_model:     HashMap<ModelRef, ModelUsageTotals>,
}

#[derive(Clone)]
enum RunAnswerTransport {
    Worker {
        run_id: RunId,
        bus:    Arc<dyn WorkerControlBus>,
        /// Where the worker's answers to steers and interrupts arrive.
        acks:   Arc<WorkerControlAcks>,
    },
    InProcess {
        interviewer: Arc<ControlInterviewer>,
        /// The run's controls, answered in place: the in-process run has
        /// no worker to forward a steer or an interrupt to.
        controls:    RunControls,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerTransportError {
    Closed,
    Timeout,
}

/// What a steer or an interrupt came to, as far as the caller is told.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RunControlAnswer {
    /// The worker delivered it, to the stage named when it had one.
    Delivered { stage: Option<String> },
    /// The worker (or Petri) refused it: the code and the reason.
    Refused { code: String, message: String },
    /// The control was forwarded but no answer arrived within
    /// [`WORKER_CONTROL_ACK_WAIT`]; the run's stream says what became of it.
    Pending,
}

impl From<WorkerControlOutcome> for RunControlAnswer {
    fn from(outcome: WorkerControlOutcome) -> Self {
        match outcome {
            WorkerControlOutcome::Delivered { stage } => Self::Delivered { stage },
            WorkerControlOutcome::Refused { code, message } => Self::Refused { code, message },
        }
    }
}

impl RunControlAnswer {
    /// The answer to a control the run's own controls settled in place:
    /// `control` names it in the refusal's message, `refused_code` is the
    /// code of a refusal whose reason has none of its own.
    fn from_controls(
        control: &str,
        refused_code: &str,
        result: Result<String, SteerError>,
    ) -> Self {
        match result {
            Ok(stage) => Self::Delivered { stage: Some(stage) },
            Err(error) => Self::Refused {
                code:    error.code().unwrap_or(refused_code).to_string(),
                message: format!("{control} refused: {error}"),
            },
        }
    }
}

impl RunAnswerTransport {
    async fn publish_worker_control(
        run_id: RunId,
        bus: &Arc<dyn WorkerControlBus>,
        message: WorkerControlEnvelope,
    ) -> Result<(), WorkerControlBusError> {
        timeout(WORKER_CONTROL_ENQUEUE_TIMEOUT, bus.publish(run_id, message))
            .await
            .map_err(|_| WorkerControlBusError::PublishTimeout)?
            .map(|_| ())
    }

    fn answer_error_from_bus(error: &WorkerControlBusError) -> AnswerTransportError {
        match error {
            WorkerControlBusError::PublishTimeout => AnswerTransportError::Timeout,
            WorkerControlBusError::Closed
            | WorkerControlBusError::Unavailable
            | WorkerControlBusError::InvalidCursor { .. } => AnswerTransportError::Closed,
        }
    }

    /// Publish a control the worker answers: registered with the run's
    /// acknowledgements first, so the answer has a waiter, then published
    /// with the request id, then waited for. `Pending` when no answer
    /// arrives within [`WORKER_CONTROL_ACK_WAIT`].
    async fn publish_answered_control(
        run_id: RunId,
        bus: &Arc<dyn WorkerControlBus>,
        acks: &WorkerControlAcks,
        message: WorkerControlEnvelope,
    ) -> Result<RunControlAnswer, AnswerTransportError> {
        let pending = acks.register(run_id);
        let message = message.with_request_id(pending.request_id.clone());
        Self::publish_worker_control(run_id, bus, message)
            .await
            .map_err(|err| Self::answer_error_from_bus(&err))?;
        Ok(acks
            .wait(pending)
            .await
            .map_or(RunControlAnswer::Pending, RunControlAnswer::from))
    }

    async fn submit(
        &self,
        qid: &str,
        submission: AnswerSubmission,
    ) -> Result<(), AnswerTransportError> {
        match self {
            Self::Worker { run_id, bus, .. } => {
                let message = WorkerControlEnvelope::interview_answer(qid.to_string(), submission);
                Self::publish_worker_control(*run_id, bus, message)
                    .await
                    .map_err(|err| Self::answer_error_from_bus(&err))
            }
            Self::InProcess { interviewer, .. } => interviewer
                .submit(qid, submission)
                .await
                .map_err(|_| AnswerTransportError::Closed),
        }
    }

    async fn cancel_run(&self) -> Result<(), AnswerTransportError> {
        match self {
            Self::Worker { run_id, bus, .. } => {
                let message = WorkerControlEnvelope::cancel_run();
                Self::publish_worker_control(*run_id, bus, message)
                    .await
                    .map_err(|err| Self::answer_error_from_bus(&err))
            }
            Self::InProcess { interviewer, .. } => {
                interviewer.cancel_all().await;
                Ok(())
            }
        }
    }

    /// Forward a steer to the worker, for the stage it names or the run's
    /// one live agent stage, and wait for its answer. The in-process path
    /// answers from the run's own controls at once.
    async fn steer(
        &self,
        text: String,
        stage: Option<String>,
        actor: Principal,
    ) -> Result<RunControlAnswer, AnswerTransportError> {
        match self {
            Self::Worker { run_id, bus, acks } => {
                let message = WorkerControlEnvelope::steer(text, stage, actor);
                Self::publish_answered_control(*run_id, bus, acks, message).await
            }
            Self::InProcess { controls, .. } => Ok(RunControlAnswer::from_controls(
                "Steer",
                "steer_refused",
                controls.steer(stage.as_deref(), &text).await,
            )),
        }
    }

    /// Forward an interrupt to the worker, for the stage it names or the
    /// run's one live agent stage, and wait for its answer; `text`, when
    /// given, is the stage's next input.
    async fn interrupt(
        &self,
        stage: Option<String>,
        text: Option<String>,
        actor: Principal,
    ) -> Result<RunControlAnswer, AnswerTransportError> {
        match self {
            Self::Worker { run_id, bus, acks } => {
                let message = match text {
                    Some(text) => WorkerControlEnvelope::interrupt_then_steer(text, stage, actor),
                    None => WorkerControlEnvelope::interrupt(stage, actor),
                };
                Self::publish_answered_control(*run_id, bus, acks, message).await
            }
            Self::InProcess { controls, .. } => Ok(RunControlAnswer::from_controls(
                "Interrupt",
                "interrupt_refused",
                controls.interrupt(stage.as_deref(), text.as_deref()).await,
            )),
        }
    }

    async fn pause_run(&self) -> Result<(), AnswerTransportError> {
        match self {
            Self::Worker { run_id, bus, .. } => {
                let message = WorkerControlEnvelope::pause_run();
                Self::publish_worker_control(*run_id, bus, message)
                    .await
                    .map_err(|err| Self::answer_error_from_bus(&err))
            }
            Self::InProcess { .. } => Err(AnswerTransportError::Closed),
        }
    }

    async fn unpause_run(&self) -> Result<(), AnswerTransportError> {
        match self {
            Self::Worker { run_id, bus, .. } => {
                let message = WorkerControlEnvelope::unpause_run();
                Self::publish_worker_control(*run_id, bus, message)
                    .await
                    .map_err(|err| Self::answer_error_from_bus(&err))
            }
            Self::InProcess { .. } => Err(AnswerTransportError::Closed),
        }
    }
}

#[derive(Debug, Clone)]
struct LoadedPendingInterview {
    run_id:   RunId,
    qid:      String,
    question: InterviewQuestionRecord,
}

#[derive(Debug, Clone)]
struct SlackLifecycleDetails {
    kind:        slack_blocks::RunLifecycleKind,
    /// The legacy name of the lifecycle event, which the notification
    /// routes in the run's settings name: `run.started`, `run.completed`,
    /// `run.failed`.
    event_name:  &'static str,
    result:      Option<String>,
    duration_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct SlackLifecyclePullRequest {
    number: u64,
    title:  Option<String>,
    url:    Option<String>,
}

/// A question posted to Slack: the message, and the question's text for
/// the update that closes it.
#[derive(Debug, Clone)]
struct SlackPostedQuestion {
    message: SlackPostedMessage,
    text:    String,
}

#[derive(Debug, Clone)]
struct SlackConnectionRuntimeState {
    status:            IntegrationConnectionState,
    last_connected_at: Option<DateTime<Utc>>,
    last_error:        Option<String>,
}

impl Default for SlackConnectionRuntimeState {
    fn default() -> Self {
        Self {
            status:            IntegrationConnectionState::Connecting,
            last_connected_at: None,
            last_error:        None,
        }
    }
}

fn sanitize_integration_error(error: &str) -> String {
    const MAX_ERROR_CHARS: usize = 240;
    let sanitized = error.replace(['\r', '\n'], " ");
    sanitized.chars().take(MAX_ERROR_CHARS).collect()
}

#[derive(Clone)]
struct SlackService {
    client:          SlackClient,
    app_token:       String,
    default_channel: Option<String>,
    posted_messages: Arc<Mutex<HashMap<(RunId, String), SlackPostedQuestion>>>,
    thread_registry: Arc<ThreadRegistry>,
    connection:      Arc<Mutex<SlackConnectionRuntimeState>>,
}

impl SlackService {
    fn new(bot_token: String, app_token: String, default_channel: Option<String>) -> Self {
        Self {
            client: SlackClient::new(bot_token),
            app_token,
            default_channel,
            posted_messages: Arc::new(Mutex::new(HashMap::new())),
            thread_registry: Arc::new(ThreadRegistry::new()),
            connection: Arc::new(Mutex::new(SlackConnectionRuntimeState::default())),
        }
    }

    fn connection_status(&self) -> IntegrationConnectionStatus {
        let state = self
            .connection
            .lock()
            .expect("slack connection state lock poisoned")
            .clone();
        IntegrationConnectionStatus {
            kind:              IntegrationConnectionKind::SocketMode,
            status:            state.status,
            last_connected_at: state.last_connected_at,
            last_error:        state.last_error,
        }
    }

    fn status_sink(&self) -> slack_connection::ConnectionStatusSink {
        let connection = Arc::clone(&self.connection);
        Arc::new(move |update| {
            let mut state = connection
                .lock()
                .expect("slack connection state lock poisoned");
            match update {
                slack_connection::ConnectionStatusUpdate::Connecting => {
                    state.status = IntegrationConnectionState::Connecting;
                    state.last_error = None;
                }
                slack_connection::ConnectionStatusUpdate::Connected => {
                    state.status = IntegrationConnectionState::Connected;
                    state.last_connected_at = Some(Utc::now());
                    state.last_error = None;
                }
                slack_connection::ConnectionStatusUpdate::Error(error) => {
                    state.status = IntegrationConnectionState::Error;
                    state.last_error = Some(sanitize_integration_error(&error));
                }
            }
        })
    }

    /// What the run's stream says since the last look: a question asked,
    /// answered or expired, and the lifecycle transitions the notification
    /// routes name.
    async fn observe(&self, state: &AppState, run_id: RunId, items: &[RunStreamItem]) {
        let run_web_url = state.run_web_url(&run_id);
        for item in items {
            match item.kind {
                RunStreamItemKind::Platform => {
                    let Some(record) = platform_record_of(item) else {
                        continue;
                    };
                    match record {
                        PlatformRecord::InterviewAnswered(answered) => {
                            self.finish_interview(
                                run_id,
                                &answered.question,
                                answered.text.as_deref().unwrap_or_default(),
                                answered.answer.as_deref().unwrap_or("Answered"),
                            )
                            .await;
                        }
                        PlatformRecord::RunLifecycle(lifecycle) => {
                            if let Some(details) = slack_lifecycle_details(&lifecycle) {
                                self.handle_lifecycle(
                                    state,
                                    run_id,
                                    &details,
                                    run_web_url.as_deref(),
                                )
                                .await;
                            }
                        }
                        _ => {}
                    }
                }
                RunStreamItemKind::Petri => {
                    let Some(parsed) = petri_parsed(item) else {
                        continue;
                    };
                    match parsed.get("kind").and_then(serde_json::Value::as_str) {
                        Some("question") => {
                            if let Some(question_id) = parsed
                                .get("question")
                                .and_then(|question| question.get("id"))
                                .and_then(serde_json::Value::as_str)
                            {
                                self.post_question(
                                    state,
                                    run_id,
                                    question_id,
                                    run_web_url.as_deref(),
                                )
                                .await;
                            }
                        }
                        Some("question_expired") => {
                            if let Some(question_id) =
                                parsed.get("question").and_then(serde_json::Value::as_str)
                            {
                                let text = self.posted_question_text(run_id, question_id);
                                self.finish_interview(run_id, question_id, &text, "Timed out")
                                    .await;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Post a pending question to the default channel, once.
    async fn post_question(
        &self,
        state: &AppState,
        run_id: RunId,
        question_id: &str,
        run_web_url: Option<&str>,
    ) {
        let Some(default_channel) = self.default_channel.as_deref() else {
            return;
        };
        let key = (run_id, question_id.to_string());
        if self
            .posted_messages
            .lock()
            .expect("slack posted messages lock poisoned")
            .contains_key(&key)
        {
            return;
        }
        let projection = match run_records::projection(state, run_id).await {
            Ok(Some(projection)) => projection,
            Ok(None) => return,
            Err(err) => {
                warn!(run_id = %run_id, error = %err, "Skipping Slack question: the run's projection could not be loaded");
                return;
            }
        };
        let Some(pending) = projection.pending_interviews.get(question_id) else {
            return;
        };
        let question = runtime_question_from_interview_record(&pending.question);
        let blocks = slack_blocks::question_to_blocks(
            &run_id.to_string(),
            question_id,
            &question,
            run_web_url,
        );
        if let Ok(posted) = self
            .client
            .post_message(default_channel, &blocks, None)
            .await
        {
            if question.allow_freeform || question.question_type == QuestionType::Freeform {
                self.thread_registry
                    .register(&posted.ts, &run_id.to_string(), question_id);
            }
            self.posted_messages
                .lock()
                .expect("slack posted messages lock poisoned")
                .insert(key, SlackPostedQuestion {
                    message: posted,
                    text:    pending.question.text.clone(),
                });
        }
    }

    fn posted_question_text(&self, run_id: RunId, question_id: &str) -> String {
        self.posted_messages
            .lock()
            .expect("slack posted messages lock poisoned")
            .get(&(run_id, question_id.to_string()))
            .map(|posted| posted.text.clone())
            .unwrap_or_default()
    }

    async fn handle_lifecycle(
        &self,
        state: &AppState,
        run_id: RunId,
        details: &SlackLifecycleDetails,
        run_web_url: Option<&str>,
    ) {
        let event_name = details.event_name;
        let projection = match run_records::projection(state, run_id).await {
            Ok(Some(projection)) => projection,
            Ok(None) => {
                warn!(
                    run_id = %run_id,
                    event = event_name,
                    "Skipping Slack lifecycle notification because run projection is missing"
                );
                return;
            }
            Err(err) => {
                warn!(
                    run_id = %run_id,
                    event = event_name,
                    error = %err,
                    "Skipping Slack lifecycle notification because run projection could not be loaded"
                );
                return;
            }
        };

        // Filter routes first; bail out before any further work if none match.
        let mut routes: Vec<_> = projection
            .spec
            .settings
            .run
            .notifications
            .iter()
            .filter(|(_, route)| {
                route.enabled
                    && route.provider.as_deref() == Some("slack")
                    && route.events.iter().any(|event| event == event_name)
            })
            .collect();
        if routes.is_empty() {
            return;
        }
        routes.sort_by_key(|(route_name, _)| *route_name);

        // A notification is sent once per route and event: the `notification.sent`
        // record is the memory that survives a restart.
        let sent = match state
            .stores
            .run_summaries
            .platform_records()
            .read_kind(&run_id, PlatformRecordKind::NotificationSent)
            .await
        {
            Ok(records) => records
                .into_iter()
                .filter_map(|stored| match stored.record {
                    PlatformRecord::NotificationSent(record) => Some((record.route, record.event)),
                    _ => None,
                })
                .collect::<HashSet<_>>(),
            Err(err) => {
                warn!(run_id = %run_id, error = %err, "Skipping Slack lifecycle notification: sent notifications could not be read");
                return;
            }
        };

        let workflow_label = slack_lifecycle_workflow_label(projection.as_ref(), None, event_name);
        let pull_request = projection
            .pull_request
            .as_ref()
            .map(slack_lifecycle_pull_request_from_link);
        let run_id_text = run_id.to_string();
        let run_url = run_web_url.or(projection.web_url.as_deref());
        let pull_request_blocks =
            pull_request
                .as_ref()
                .map(|pull_request| slack_blocks::RunLifecyclePullRequest {
                    number: pull_request.number,
                    title:  pull_request.title.as_deref(),
                    url:    pull_request.url.as_deref(),
                });
        let blocks =
            slack_blocks::run_lifecycle_blocks(details.kind, &slack_blocks::RunLifecycleBlocks {
                run_id: &run_id_text,
                run_url,
                workflow_label: &workflow_label,
                result: details.result.as_deref(),
                duration_ms: details.duration_ms,
                pull_request: pull_request_blocks,
            });

        let blocks = &blocks;
        let posts = routes.into_iter().filter_map(|(route_name, route)| {
            if sent.contains(&(route_name.clone(), event_name.to_string())) {
                return None;
            }
            let channel =
                resolve_slack_lifecycle_route_channel(run_id, route_name, route, event_name)?;
            Some(async move {
                match self.client.post_message(&channel, blocks, None).await {
                    Ok(posted) => {
                        let record = PlatformRecord::NotificationSent(NotificationSentRecord {
                            route:      route_name.clone(),
                            event:      event_name.to_string(),
                            channel:    Some(posted.channel_id.clone()),
                            thread:     None,
                            message_id: Some(posted.ts.clone()),
                            question:   None,
                            operation:  None,
                        });
                        if let Err(err) = run_records::append(state, run_id, record).await {
                            warn!(run_id = %run_id, error = %err, "the Slack notification was sent but not recorded");
                        }
                    }
                    Err(err) => {
                        warn!(
                            run_id = %run_id,
                            event = event_name,
                            notification_route = route_name.as_str(),
                            error = %err,
                            "Failed to post Slack lifecycle notification"
                        );
                    }
                }
            })
        });
        join_all(posts).await;
    }

    async fn finish_interview(
        &self,
        run_id: RunId,
        qid: &str,
        question_text: &str,
        answer_text: &str,
    ) {
        let key = (run_id, qid.to_string());
        let posted = self
            .posted_messages
            .lock()
            .expect("slack posted messages lock poisoned")
            .remove(&key);
        let Some(posted) = posted else {
            return;
        };
        let question_text = if question_text.is_empty() {
            posted.text.as_str()
        } else {
            question_text
        };

        self.thread_registry.remove(&posted.message.ts);
        let blocks = slack_blocks::answered_blocks(question_text, answer_text);
        let _ = self
            .client
            .update_message(&posted.message.channel_id, &posted.message.ts, &blocks)
            .await;
    }

    async fn submit_answer(&self, state: Arc<AppState>, submission: SlackAnswerSubmission) {
        let Ok(run_id) = RunId::from_str(&submission.run_id) else {
            return;
        };

        let Ok(pending) = load_pending_interview(state.as_ref(), run_id, &submission.qid).await
        else {
            return;
        };
        let answer_submission = AnswerSubmission::new(submission.answer, submission.actor);
        let _ = submit_pending_interview_answer(state.as_ref(), &pending, answer_submission).await;
    }
}

fn slack_lifecycle_details(record: &RunLifecycleRecord) -> Option<SlackLifecycleDetails> {
    match record.transition {
        RunLifecycleKind::Running => Some(SlackLifecycleDetails {
            kind:        slack_blocks::RunLifecycleKind::Started,
            event_name:  "run.started",
            result:      None,
            duration_ms: None,
        }),
        RunLifecycleKind::Succeeded => Some(SlackLifecycleDetails {
            kind:        slack_blocks::RunLifecycleKind::Completed,
            event_name:  "run.completed",
            result:      Some(match record.status {
                Some(RunStatus::Succeeded { reason }) => reason.to_string(),
                _ => "completed".to_string(),
            }),
            duration_ms: None,
        }),
        RunLifecycleKind::Failed | RunLifecycleKind::Dead => Some(SlackLifecycleDetails {
            kind:        slack_blocks::RunLifecycleKind::Failed,
            event_name:  "run.failed",
            result:      Some(slack_lifecycle_failed_result(record)),
            duration_ms: None,
        }),
        _ => None,
    }
}

fn slack_lifecycle_failed_result(record: &RunLifecycleRecord) -> String {
    let reason = match record.status {
        Some(RunStatus::Failed { reason }) => reason.to_string(),
        Some(RunStatus::Dead) => "dead".to_string(),
        _ => "failed".to_string(),
    };
    match record.reason.as_deref().map(str::trim) {
        Some(message) if !message.is_empty() => format!("{reason} — {message}"),
        _ => reason,
    }
}

/// The platform record a stream item carries, when it carries one.
fn platform_record_of(item: &RunStreamItem) -> Option<PlatformRecord> {
    if item.kind != RunStreamItemKind::Platform {
        return None;
    }
    serde_json::from_value(item.item.get("record")?.clone()).ok()
}

/// What a Petri event of the stream parsed out of a step's progress: a
/// question, an expiry, a note.
fn petri_parsed(item: &RunStreamItem) -> Option<&serde_json::Value> {
    if item.kind != RunStreamItemKind::Petri {
        return None;
    }
    item.item.get("derived")?.get("parsed")
}

fn slack_lifecycle_workflow_label(
    projection: &fabro_store::RunProjection,
    started_event_name: Option<&str>,
    event_name: &str,
) -> String {
    [
        projection.spec.workflow_name(),
        projection.spec.workflow_slug(),
        projection.spec.graph_name(),
        started_event_name,
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|value| !value.is_empty())
    .unwrap_or(event_name)
    .to_string()
}

fn slack_lifecycle_pull_request_from_link(link: &PullRequestLink) -> SlackLifecyclePullRequest {
    SlackLifecyclePullRequest {
        number: link.number,
        title:  None,
        url:    Some(link.html_url()),
    }
}

fn resolve_slack_lifecycle_route_channel(
    run_id: RunId,
    route_name: &str,
    route: &NotificationRouteSettings,
    event_name: &str,
) -> Option<String> {
    let Some(channel) = route
        .slack
        .as_ref()
        .and_then(|slack| slack.channel.as_ref())
    else {
        warn!(
            run_id = %run_id,
            notification_route = route_name,
            event = event_name,
            "Skipping Slack lifecycle notification route without channel"
        );
        return None;
    };

    // `{{ vars.* }}` is substituted at run creation, so the channel is literal
    // here; anything still unresolved skips the route rather than sending to a
    // half-rendered channel name.
    let resolved = match channel.resolve_with(&mut fabro_types::settings::ResolveCtx::new()) {
        Ok(resolved) => resolved,
        Err(err) => {
            warn!(
                run_id = %run_id,
                notification_route = route_name,
                event = event_name,
                error = %err,
                "Skipping Slack lifecycle notification route with unresolved channel"
            );
            return None;
        }
    };
    if resolved.trim().is_empty() {
        warn!(
            run_id = %run_id,
            notification_route = route_name,
            event = event_name,
            "Skipping Slack lifecycle notification route with empty channel"
        );
        return None;
    }
    Some(resolved)
}

/// Shared application state for the server.
pub struct AppState {
    runs: Mutex<HashMap<RunId, ManagedRun>>,
    aggregate_usage: Mutex<UsageAccumulator>,
    pub(crate) stores: AppStores,
    session_runtimes: SessionRuntimeManager,
    artifact_store: ArtifactStore,
    automation_repo_cache: Arc<GitRepoCache>,
    #[cfg(any(test, feature = "test-support"))]
    automation_materializer_override: Option<Arc<dyn AutomationRunMaterializer>>,
    worker_tokens: WorkerTokenKeys,
    started_at: Instant,
    resource_sampler: resource_sampler::ResourceSampler,
    max_concurrent_runs: usize,
    pub(crate) worker_control_bus: Arc<dyn WorkerControlBus>,
    /// The steers and interrupts awaiting their worker's answer.
    pub(crate) worker_control_acks: Arc<WorkerControlAcks>,
    pub(crate) worker_runtime: Arc<dyn WorkerRuntime>,
    /// The Petri runs held open for workers over the API.
    pub(crate) petri_runs: PetriRuns,
    /// The projector of Petri runs: signalled after each committed record.
    pub(crate) petri_projector: Arc<Projector>,
    /// The server's reader of every run's stream, into the live state.
    pub(crate) stream_follower: Arc<stream_follower::StreamFollower>,
    scheduler_notify: Notify,
    automation_scheduler_notify: Notify,
    pull_request_scheduler_notify: Notify,
    pull_request_creation_queue: Mutex<pull_request_supervisor::PendingPullRequestCreationQueue>,
    global_event_tx: broadcast::Sender<RunStreamItem>,
    /// Per-run coalescing registry for `GET /runs/{id}/files`. Concurrent
    /// callers for the same run share one materialization; different runs
    /// proceed in parallel. See `crate::run_files` for semantics.
    pub(crate) files_in_flight: FilesInFlight,
    pull_request_create_locks: KeyedMutex<RunId>,
    /// One lock per run around a control request's check-and-append.
    control_request_locks: KeyedMutex<RunId>,
    parent_link_lock: AsyncMutex<()>,

    pub(super) server_secrets: ServerSecrets,
    pub(crate) llm_source: Arc<dyn CredentialProvider>,
    /// The database pool the stores share, for the Petri run store a
    /// server-process run writes its records through.
    pub(crate) db_pool: DbPool,
    manifest_run_defaults: RwLock<Arc<RunLayer>>,
    manifest_run_settings: RwLock<std::result::Result<RunNamespace, SharedError>>,
    pub(crate) server_settings: RwLock<Arc<ServerSettings>>,
    effective_web_url: RwLock<String>,
    catalog: RwLock<Arc<Catalog>>,
    pub(crate) env_lookup: EnvLookup,
    pub(crate) github_api_base_url: String,
    active_config_path: PathBuf,
    http_client: Option<fabro_http::HttpClient>,
    sandbox_inventory: SandboxInventory,
    shutdown: CancellationToken,
    shutting_down: AtomicBool,
    /// Test switch: execute runs in this process instead of a worker.
    execute_in_process: bool,
    slack_service: Option<Arc<SlackService>>,
    slack_started: AtomicBool,
    github_webhook_secret: Option<String>,
}

pub(crate) struct AppStores {
    pub(crate) runs:            Arc<Database>,
    pub(crate) run_summaries:   Arc<RunSummaryStore>,
    /// Ask Fabro conversations, keyed by session id.
    pub(crate) session_records: Arc<RunSessionRecordStore>,
    /// The events of Ask Fabro sessions, numbered per session.
    pub(crate) session_events:  Arc<RunSessionEventStore>,
    pub(crate) auth_codes:      Arc<AuthCodeStore>,
    pub(crate) auth_sessions:   Arc<AuthSessionStore>,
    pub(crate) automations:     Arc<AutomationStore>,
    pub(crate) environments:    Arc<EnvironmentStore>,
    pub(crate) mcp_servers:     Arc<McpServerStore>,
    pub(crate) vault:           Arc<SecretStore>,
    pub(crate) variables:       Arc<VariableStore>,
}

#[cfg(any(test, feature = "test-support"))]
impl AppState {
    /// Access the auth session store so tests can seed CLI sessions against
    /// the same SQLite pool the router reads from.
    #[must_use]
    pub fn test_auth_session_store(&self) -> &Arc<AuthSessionStore> {
        &self.stores.auth_sessions
    }

    /// Access the auth-code store used by this router.
    #[must_use]
    pub fn test_auth_code_store(&self) -> &Arc<AuthCodeStore> {
        &self.stores.auth_codes
    }

    /// The Petri run store the worker endpoints answer from, so a test can
    /// release a lease as an operator would and read who holds one.
    #[must_use]
    pub fn test_petri_run_store(&self) -> &fabro_petri::SqliteRunStore {
        self.petri_runs.store()
    }

    /// The projector of Petri runs, so a test can wait for a run's view to
    /// settle before it reads it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_petri_projector(&self) -> &Arc<Projector> {
        &self.petri_projector
    }

    /// The pool the Petri view tables live in, so a test can read them.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_petri_view_pool(&self) -> DbPool {
        self.stores.runs.run_summary_store().pool()
    }

    /// A worker token for `run_id` with the plain `run:worker` scope, as the
    /// server mints for the worker it launches.
    pub fn test_issue_worker_token(&self, run_id: &RunId) -> String {
        issue_worker_token_with_scopes(&self.worker_tokens, run_id, WorkerScopeSet::run_worker())
            .expect("a test worker token signs")
    }
}

impl AppState {
    pub(crate) fn automation_store(&self) -> &AutomationStore {
        &self.stores.automations
    }

    pub(crate) fn environment_store(&self) -> &EnvironmentStore {
        &self.stores.environments
    }

    pub(crate) fn mcp_server_store(&self) -> &McpServerStore {
        &self.stores.mcp_servers
    }

    pub(crate) async fn materialize_automation_run(
        &self,
        input: AutomationRunMaterializeInput,
    ) -> Result<AutomationRunMaterialized, RunMaterializeError> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(materializer) = self.automation_materializer_override.as_ref() {
            return materializer.materialize(input).await;
        }

        let settings = self.server_settings();
        let credentials = self
            .github_credentials(&settings.server.integrations.github)
            .await
            .map_err(|source| RunMaterializeError::LoadCredentials { source })?;
        ProductionAutomationRunMaterializer::new(
            credentials,
            self.github_api_base_url.clone(),
            self.http_client.clone(),
            Arc::clone(&self.automation_repo_cache),
            fabro_workflow_version::WorkflowVersionStore::new(self.store_ref().blobs()),
        )
        .materialize(input)
        .await
    }

    pub(crate) fn notify_automation_scheduler(&self) {
        self.automation_scheduler_notify.notify_one();
    }

    pub(crate) fn automation_scheduler_notified(
        &self,
    ) -> impl std::future::Future<Output = ()> + '_ {
        self.automation_scheduler_notify.notified()
    }

    pub(crate) fn notify_pull_request_scheduler(&self) {
        self.pull_request_scheduler_notify.notify_one();
    }

    pub(crate) fn pull_request_scheduler_notified(
        &self,
    ) -> impl std::future::Future<Output = ()> + '_ {
        self.pull_request_scheduler_notify.notified()
    }
}

pub(crate) struct AskFabroReadiness {
    default_model: Option<String>,
}

impl AskFabroReadiness {
    pub(crate) fn decorate(&self, mut run: fabro_types::Run) -> fabro_types::Run {
        run.ask_fabro = self.ask_fabro_for(&run);
        run
    }

    fn ask_fabro_for(&self, run: &fabro_types::Run) -> AskFabro {
        let unavailable_reason = if run.sandbox.is_none() {
            Some(AskFabroUnavailableReason::NoSandbox)
        } else if run
            .sandbox
            .as_ref()
            .and_then(fabro_types::RunSandbox::instance)
            .is_none()
        {
            Some(AskFabroUnavailableReason::SandboxNotReady)
        } else if self.default_model.is_none() {
            Some(AskFabroUnavailableReason::LlmUnconfigured)
        } else {
            None
        };

        AskFabro {
            available: unavailable_reason.is_none(),
            unavailable_reason,
            default_model: self.default_model.clone(),
        }
    }
}

pub(crate) struct AppStateConfig {
    pub(crate) resolved_settings: ResolvedAppStateSettings,
    /// Execute runs in this process instead of a worker (tests only).
    pub(crate) execute_in_process: bool,
    pub(crate) max_concurrent_runs: usize,
    pub(crate) store: Arc<Database>,
    pub(crate) artifact_store: ArtifactStore,
    pub(crate) db_pool: DbPool,
    pub(crate) preloaded_vault: Vault,
    pub(crate) server_secrets: ServerSecrets,
    pub(crate) env_lookup: EnvLookup,
    pub(crate) github_api_base_url: Option<String>,
    pub(crate) active_config_path: PathBuf,
    pub(crate) http_client: Option<fabro_http::HttpClient>,
    pub(crate) sandbox_inventory: Option<SandboxInventory>,
    pub(crate) shutdown: CancellationToken,
    #[cfg(test)]
    pub(crate) worker_control_bus: Option<Arc<dyn WorkerControlBus>>,
    #[cfg(test)]
    pub(crate) worker_runtime: Option<Arc<dyn WorkerRuntime>>,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) automation_materializer_override: Option<Arc<dyn AutomationRunMaterializer>>,
}

#[derive(Clone)]
pub(crate) struct ResolvedAppStateSettings {
    pub(crate) server_settings:       ServerSettings,
    pub(crate) manifest_run_defaults: RunLayer,
    pub(crate) llm_overlay:           LlmLayer,
}

/// Add a concluded run's usage to the server's aggregate; a run that
/// recorded no conclusion adds nothing.
pub(crate) fn accumulate_concluded_run_usage(
    state: &AppState,
    final_state: &fabro_store::RunProjection,
) {
    if final_state.conclusion.is_none() {
        return;
    }
    let mut agg = state
        .aggregate_usage
        .lock()
        .expect("aggregate_usage lock poisoned");
    accumulate_usage_rollup(
        &mut agg,
        &fabro_workflow::usage_rollup_from_projection(final_state),
    );
}

fn accumulate_usage_rollup(
    accumulator: &mut UsageAccumulator,
    rollup: &fabro_workflow::ProjectionUsageRollup,
) {
    accumulator.total_runs += 1;
    accumulator.total_timing = accumulator.total_timing.saturating_add(&rollup.timing);
    for model in &rollup.by_model {
        let entry = accumulator.by_model.entry(model.model.clone()).or_default();
        entry.stages += model.stages;
        entry.usage = entry.usage.saturating_add(model.usage);
    }
}

impl AppState {
    pub(crate) fn manifest_run_defaults(&self) -> Arc<RunLayer> {
        Arc::clone(
            &self
                .manifest_run_defaults
                .read()
                .expect("manifest run defaults lock poisoned"),
        )
    }

    pub(crate) fn server_settings(&self) -> Arc<ServerSettings> {
        Arc::clone(
            &self
                .server_settings
                .read()
                .expect("server settings lock poisoned"),
        )
    }

    pub(crate) fn catalog(&self) -> Arc<Catalog> {
        Arc::clone(&self.catalog.read().expect("catalog lock poisoned"))
    }

    pub(crate) fn active_config_path(&self) -> &std::path::Path {
        &self.active_config_path
    }

    pub(crate) fn manifest_run_settings(&self) -> std::result::Result<RunNamespace, SharedError> {
        self.manifest_run_settings
            .read()
            .expect("manifest run settings lock poisoned")
            .clone()
    }

    pub(crate) fn refresh_manifest_run_settings_from_catalogs(&self) {
        let manifest_run_defaults = self.manifest_run_defaults();
        let manifest_run_settings = resolve_manifest_run_settings_with_catalog(
            manifest_run_defaults.as_ref(),
            &self.stores.environments,
            &self.stores.mcp_servers,
        );
        *self
            .manifest_run_settings
            .write()
            .expect("manifest run settings lock poisoned") = manifest_run_settings;
    }

    pub(crate) fn refresh_manifest_run_settings_from_environment_catalog(&self) {
        self.refresh_manifest_run_settings_from_catalogs();
    }

    fn http_client(&self) -> Result<fabro_http::HttpClient, fabro_http::HttpClientBuildError> {
        match &self.http_client {
            Some(client) => Ok(client.clone()),
            None => fabro_http::http_client(),
        }
    }

    pub(crate) fn server_storage_dir(&self) -> PathBuf {
        PathBuf::from(&self.server_settings().server.storage.root)
    }

    /// Scratch directory used by the automation materializer when staging
    /// per-run manifests. Shared by API-triggered and scheduled fires.
    pub(crate) fn automation_temp_root(&self) -> PathBuf {
        Storage::new(self.server_storage_dir())
            .scratch_dir()
            .join("automations")
    }

    /// Snapshotted at create-time so attach replays surface the same link
    /// even if `server.web.url` is later changed. `None` when the UI is
    /// turned off or `server.web.url` is unset/invalid.
    pub(crate) fn run_web_url(&self, run_id: &fabro_types::RunId) -> Option<String> {
        if !self.server_settings().server.web.enabled {
            return None;
        }
        let base = self.canonical_origin().ok()?;
        Some(format!("{}/runs/{run_id}", base.trim_end_matches('/')))
    }

    pub(crate) async fn resolve_llm_client(&self) -> anyhow::Result<FabroClient> {
        resolve_llm_client_from_source(
            Arc::clone(&self.llm_source),
            self.catalog(),
            self.http_client.clone(),
        )
        .await
    }

    pub(crate) async fn configured_llm_provider_ids(&self) -> Vec<ProviderId> {
        let catalog = self.catalog();
        fabro_llm::configured_providers(catalog.as_ref(), self.llm_source.as_ref()).await
    }

    /// Resolve the LLM client once and derive the ready provider IDs from it,
    /// logging a warning when resolution fails. Callers that need both values
    /// must use this instead of `ready_llm_provider_ids` so the client is not
    /// resolved twice.
    pub(crate) async fn resolve_llm_client_with_ready_ids(
        &self,
    ) -> (anyhow::Result<FabroClient>, Vec<ProviderId>) {
        let llm_result = self.resolve_llm_client().await;
        if let Err(err) = &llm_result {
            warn!(error = ?err, "Failed to resolve LLM client while checking ready providers");
        }
        let ready_provider_ids = llm_result
            .as_ref()
            .map(FabroClient::provider_ids)
            .unwrap_or_default();
        (llm_result, ready_provider_ids)
    }

    pub(crate) async fn ready_llm_provider_ids(&self) -> Vec<ProviderId> {
        self.resolve_llm_client_with_ready_ids().await.1
    }

    pub(crate) async fn decorate_run_summary(&self, run: fabro_types::Run) -> fabro_types::Run {
        self.ask_fabro_readiness().await.decorate(run)
    }

    pub(crate) async fn decorate_run_summaries(
        &self,
        runs: Vec<fabro_types::Run>,
    ) -> Vec<fabro_types::Run> {
        let readiness = self.ask_fabro_readiness().await;
        runs.into_iter()
            .map(|run| readiness.decorate(run))
            .collect()
    }

    pub(crate) async fn ask_fabro_readiness(&self) -> AskFabroReadiness {
        let provider_ids = self.ready_llm_provider_ids().await;
        let default_model = if provider_ids.is_empty() {
            None
        } else {
            self.catalog()
                .default_offering_for(&provider_ids)
                .map(|entry| entry.model.id().to_string())
        };
        AskFabroReadiness { default_model }
    }

    pub(crate) async fn vault_secret(
        &self,
        name: &str,
    ) -> Result<Option<String>, SecretStoreError> {
        self.stores
            .vault
            .get(name)
            .await
            .map(|entry| entry.map(|entry| entry.value))
    }

    pub(crate) fn config_env_lookup(&self, name: &str) -> Option<String> {
        (self.env_lookup)(name)
    }

    /// Daytona credentials for `api_key`: the key from the vault, the
    /// control-plane URL and organization from server configuration, and
    /// the server's HTTP client. The process environment is consulted only
    /// through the configured lookup.
    pub(crate) fn daytona_credentials(&self, api_key: String) -> DaytonaCredentials {
        DaytonaCredentials::from_api_key(api_key, |name| self.config_env_lookup(name))
            .with_http_client(self.http_client().ok())
    }

    /// Everything a reconnect needs to reach a run's provider: the server's
    /// provider settings and the Daytona credentials from the vault (`None`
    /// when no key is stored).
    pub(crate) async fn provider_access(&self) -> Result<ProviderAccess, SecretStoreError> {
        Ok(ProviderAccess {
            providers: self.server_settings().server.sandbox.providers.clone(),
            daytona:   self
                .vault_secret(EnvVars::DAYTONA_API_KEY)
                .await?
                .map(|api_key| self.daytona_credentials(api_key)),
        })
    }

    pub(crate) async fn check_daytona_api_key(
        &self,
        api_key: String,
    ) -> anyhow::Result<daytona::DaytonaKeyCheck> {
        self.check_daytona_api_key_with_timeout(api_key, daytona::DAYTONA_CREDENTIAL_PROBE_TIMEOUT)
            .await
    }

    pub(crate) async fn check_daytona_api_key_with_timeout(
        &self,
        api_key: String,
        probe_timeout: Duration,
    ) -> anyhow::Result<daytona::DaytonaKeyCheck> {
        daytona::check_daytona_api_key(&self.daytona_credentials(api_key), probe_timeout).await
    }

    /// Borrow the persistent store so sibling modules can open run readers
    /// without cross-module state coupling on the `AppState` field layout.
    pub(crate) fn store_ref(&self) -> &Arc<Database> {
        &self.stores.runs
    }

    /// Loads the current projection for `run_id`, with the standard HTTP error
    /// mapping: storage failures become 500s and a missing run becomes the
    /// canonical 404.
    pub(crate) async fn load_run_projection(
        &self,
        run_id: &RunId,
    ) -> Result<Arc<fabro_store::RunProjection>, ApiError> {
        run_records::require_projection(self, *run_id).await
    }

    pub(crate) fn session_runtimes(&self) -> &SessionRuntimeManager {
        &self.session_runtimes
    }

    pub(crate) fn sandbox_inventory(&self) -> &SandboxInventory {
        &self.sandbox_inventory
    }

    pub(crate) fn server_secret(&self, name: &str) -> Option<String> {
        self.server_secrets.get(name)
    }

    pub(crate) fn worker_token_keys(&self) -> &WorkerTokenKeys {
        &self.worker_tokens
    }

    /// Loopback target this server is bound to, derived from the runtime
    /// daemon record. Used by in-process Ask Fabro sessions to call the local
    /// API over the normal HTTP path (authed with a same-run worker token).
    pub(crate) fn self_server_target(&self) -> anyhow::Result<fabro_client::ServerTarget> {
        let storage_dir = self.server_storage_dir();
        let runtime_directory = Storage::new(&storage_dir).runtime_directory();
        let daemon = ServerDaemon::read(&runtime_directory)?.with_context(|| {
            format!(
                "server record {} is missing",
                runtime_directory.record_path().display()
            )
        })?;
        // `Bind::to_target()` already produces the http(s)-URL-or-absolute-
        // socket-path form that `ServerTarget`'s FromStr understands.
        daemon.bind.to_target().parse()
    }

    pub(crate) fn effective_web_url(&self) -> String {
        self.effective_web_url
            .read()
            .expect("effective web url lock poisoned")
            .clone()
    }

    pub(crate) fn canonical_origin(&self) -> Result<String, String> {
        canonical_origin_from_effective_web_url(&self.effective_web_url())
    }

    pub(crate) fn session_key(&self) -> Option<Key> {
        self.server_secret(EnvVars::SESSION_SECRET)
            .and_then(|value| auth::derive_cookie_key(value.as_bytes()).ok())
    }

    pub(crate) async fn github_credentials(
        &self,
        settings: &GithubIntegrationSettings,
    ) -> anyhow::Result<Option<fabro_github::GitHubCredentials>> {
        match settings.strategy {
            GithubIntegrationStrategy::App => {
                let Some(app_id) = settings.app_id.clone() else {
                    return Ok(None);
                };
                let raw = self
                    .vault_secret(EnvVars::GITHUB_APP_PRIVATE_KEY)
                    .await
                    .map_err(anyhow::Error::new)?;
                let Some(raw) = raw else {
                    return Ok(None);
                };
                let private_key_pem = decode_secret_pem(EnvVars::GITHUB_APP_PRIVATE_KEY, &raw)
                    .map_err(anyhow::Error::msg)?;
                Ok(Some(fabro_github::GitHubCredentials::App(
                    fabro_github::GitHubAppCredentials {
                        app_id,
                        private_key_pem,
                        slug: settings.slug.clone(),
                    },
                )))
            }
            GithubIntegrationStrategy::Token => {
                let token = self
                    .vault_secret(EnvVars::GITHUB_TOKEN)
                    .await
                    .map_err(anyhow::Error::new)?
                    .as_deref()
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_string);
                match token {
                    Some(token) => {
                        fabro_github::validate_static_github_token(&token)
                            .map_err(anyhow::Error::msg)?;
                        Ok(Some(fabro_github::GitHubCredentials::Pat(token)))
                    }
                    None => anyhow::bail!(
                        "GITHUB_TOKEN not configured -- run fabro install or run fabro secret set GITHUB_TOKEN"
                    ),
                }
            }
        }
    }

    fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Relaxed);
        self.scheduler_notify.notify_waiters();
        self.automation_scheduler_notify.notify_waiters();
        self.pull_request_scheduler_notify.notify_waiters();
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Relaxed)
    }

    pub(crate) fn replace_runtime_settings(
        &self,
        resolved_settings: ResolvedAppStateSettings,
    ) -> anyhow::Result<()> {
        let ResolvedAppStateSettings {
            server_settings,
            manifest_run_defaults,
            llm_overlay,
        } = resolved_settings;
        let server_settings = Arc::new(server_settings);
        let manifest_run_defaults = Arc::new(manifest_run_defaults);
        let effective_web_url =
            effective_web_url(&server_settings.server, |name| (self.env_lookup)(name));
        let manifest_run_settings = resolve_manifest_run_settings_with_catalog(
            manifest_run_defaults.as_ref(),
            &self.stores.environments,
            &self.stores.mcp_servers,
        );
        let catalog = Arc::new(
            fabro_llm::build_catalog(&llm_overlay, &|name| (self.env_lookup)(name))
                .context("building LLM model catalog")?,
        );
        canonical_origin_from_effective_web_url(&effective_web_url).map_err(anyhow::Error::msg)?;

        *self
            .manifest_run_defaults
            .write()
            .expect("manifest run defaults lock poisoned") = manifest_run_defaults;
        *self
            .manifest_run_settings
            .write()
            .expect("manifest run settings lock poisoned") = manifest_run_settings;
        *self
            .server_settings
            .write()
            .expect("server settings lock poisoned") = server_settings;
        *self
            .effective_web_url
            .write()
            .expect("effective web url lock poisoned") = effective_web_url;
        *self.catalog.write().expect("catalog lock poisoned") = catalog;
        Ok(())
    }
}

/// Builds the server's LLM client: retries and attachment inlining on, the
/// server's HTTP client for provider requests when one is configured.
async fn resolve_llm_client_from_source(
    source: Arc<dyn CredentialProvider>,
    catalog: Arc<Catalog>,
    http_client: Option<fabro_http::HttpClient>,
) -> anyhow::Result<FabroClient> {
    let mut options = ClientOptions::standard();
    options.http = http_client;
    fabro_llm::build_client(Catalog::clone(&catalog), source, options)
        .await
        .context("building the LLM client")
}

fn decode_secret_pem(name: &str, raw: &str) -> Result<String, String> {
    if raw.starts_with("-----") {
        return Ok(raw.to_string());
    }
    let pem_bytes = BASE64_STANDARD
        .decode(raw)
        .map_err(|err| format!("{name} is not valid PEM or base64: {err}"))?;
    String::from_utf8(pem_bytes)
        .map_err(|err| format!("{name} base64 decoded to invalid UTF-8: {err}"))
}

fn start_optional_slack_service(state: &Arc<AppState>) {
    let Some(service) = state.slack_service.clone() else {
        return;
    };
    if state.slack_started.swap(true, Ordering::SeqCst) {
        return;
    }

    let event_state = Arc::clone(state);
    let event_service = Arc::clone(&service);
    tokio::spawn(async move {
        let mut rx = event_state.global_event_tx.subscribe();
        loop {
            match rx.recv().await {
                Ok(item) => {
                    event_service
                        .observe(
                            event_state.as_ref(),
                            item.run_id,
                            std::slice::from_ref(&item),
                        )
                        .await;
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });

    let socket_state = Arc::clone(state);
    tokio::spawn(async move {
        let submit_service = Arc::clone(&service);
        let on_submit: Arc<dyn Fn(SlackAnswerSubmission) + Send + Sync> =
            Arc::new(move |submission| {
                let state = Arc::clone(&socket_state);
                let service = Arc::clone(&submit_service);
                tokio::spawn(async move {
                    service.submit_answer(state, submission).await;
                });
            });
        slack_connection::run_with_status(
            &service.client,
            &service.app_token,
            &service.thread_registry,
            on_submit,
            service.status_sink(),
        )
        .await;
    });
}

/// Build the axum Router with all run endpoints and embedded static assets.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Public router helper keeps the existing ergonomic API and forwards by reference."
)]
pub fn build_router(state: Arc<AppState>, auth_mode: AuthMode) -> Router {
    build_router_with_options(state, &auth_mode, RouterOptions::default())
}

#[derive(Clone, Debug)]
pub struct RouterOptions {
    pub web_enabled:       bool,
    pub static_asset_root: Option<PathBuf>,
    pub github_endpoints:  Option<Arc<GithubEndpoints>>,
    /// Set when serving with the `--watch-web` dev flag. The static-file
    /// handler then refuses to fall back to the embedded SPA snapshot and
    /// returns a 503 "build in progress" page on miss, so developers see
    /// their edits or a clear signal — never stale embedded bytes.
    pub watch_web:         bool,
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            web_enabled:       true,
            static_asset_root: None,
            github_endpoints:  None,
            watch_web:         false,
        }
    }
}

fn removed_web_route(path: &str) -> bool {
    matches!(path, "/setup/complete") || path.starts_with("/install")
}

/// Build the axum Router with configurable web surface routing.
pub fn build_router_with_options(
    state: Arc<AppState>,
    auth_mode: &AuthMode,
    options: RouterOptions,
) -> Router {
    start_optional_slack_service(&state);
    let RouterOptions {
        web_enabled,
        static_asset_root,
        github_endpoints,
        watch_web,
    } = options;
    let translation_state = Arc::clone(&state);
    let state_for_canonical_host = Arc::clone(&state);
    let github_endpoints =
        github_endpoints.unwrap_or_else(|| Arc::new(GithubEndpoints::production_defaults()));
    let webhook_secret = state.github_webhook_secret.clone();
    let principal_layer = middleware::from_fn_with_state(Arc::clone(&state), principal_middleware);
    let api_common = if web_enabled {
        Router::new()
            .route("/openapi.json", get(handler::openapi_spec))
            .merge(web_auth::api_routes())
    } else {
        Router::new().route("/openapi.json", get(handler::openapi_spec))
    };

    let demo_router = Router::new()
        .nest(
            "/api/v1",
            api_common
                .clone()
                .merge(handler::demo_routes())
                .layer(principal_layer.clone()),
        )
        .layer(axum::Extension(auth_mode.clone()))
        .layer(axum::Extension(Arc::clone(&github_endpoints)))
        .with_state(state.clone());

    let mut real_router = Router::new().nest(
        "/api/v1",
        api_common
            .merge(handler::real_routes())
            .layer(principal_layer),
    );
    if web_enabled {
        real_router = real_router.nest("/auth", web_auth::routes().merge(auth::web_routes()));
    }
    let real_router = real_router
        .layer(axum::Extension(github_endpoints))
        .with_state(state);

    let dispatch = service_fn(move |req: axum_extract::Request| {
        let demo = demo_router.clone();
        let real = real_router.clone();
        async move {
            let demo_active = web_enabled
                && req.uri().path().starts_with("/api/")
                && req.headers().get("x-fabro-demo").is_some_and(|v| v == "1");
            if demo_active {
                demo.oneshot(req).await
            } else {
                real.oneshot(req).await
            }
        }
    });

    let mut app_router = Router::new()
        .route("/health", get(handler::health))
        .fallback_service(service_fn(move |req: axum_extract::Request| {
            let dispatch = dispatch.clone();
            let static_asset_root = static_asset_root.clone();
            async move {
                let path = req.uri().path().to_string();
                let dispatch_path = path.starts_with("/api/")
                    || path == "/health"
                    || (web_enabled && path.starts_with("/auth/"));
                if dispatch_path {
                    dispatch.oneshot(req).await
                } else if web_enabled && removed_web_route(&path) {
                    Ok::<_, std::convert::Infallible>(StatusCode::NOT_FOUND.into_response())
                } else if web_enabled && matches!(req.method(), &Method::GET | &Method::HEAD) {
                    let headers = req.headers().clone();
                    Ok::<_, std::convert::Infallible>(
                        static_files::serve_with_asset_root(
                            &path,
                            &headers,
                            static_asset_root.as_deref(),
                            watch_web,
                        )
                        .await,
                    )
                } else {
                    Ok::<_, std::convert::Infallible>(StatusCode::NOT_FOUND.into_response())
                }
            }
        }));

    app_router = app_router.layer(middleware::from_fn_with_state(
        translation_state,
        auth_translation_middleware,
    ));
    app_router = app_router.layer(middleware::from_fn(demo_routing_middleware));
    app_router = app_router.layer(axum::Extension(auth_mode.clone()));

    let mut router = app_router;
    if let Some(secret) = webhook_secret {
        let secret: Arc<[u8]> = Arc::from(secret.into_bytes().into_boxed_slice());
        router = github_webhook_routes(secret).merge(router);
    }

    router
        // Innermost of the outer layers so every response body — static SPA
        // assets and JSON API alike — is compressed before the header/log
        // middlewares see it.
        .layer(compression_layer())
        .layer(middleware::from_fn_with_state(
            canonical_host::Config {
                state: state_for_canonical_host,
                web_enabled,
            },
            canonical_host::redirect_middleware,
        ))
        .layer(middleware::from_fn(security_headers::layer))
        .layer(middleware::from_fn(http_log_middleware))
        .layer(middleware::from_fn(request_id::layer))
}

/// Response-compression layer shared by the main and install-mode routers.
///
/// The default predicate skips streaming SSE (`text/event-stream`), gRPC,
/// images, ZIP archives, and tiny bodies. The quality is pinned because
/// tower-http's default defers to each codec's own default, and brotli's is
/// quality 11 — seconds of CPU on a multi-megabyte asset. Level 4 keeps both
/// codecs fast at a near-optimal ratio.
pub(crate) fn compression_layer() -> CompressionLayer<impl Predicate> {
    CompressionLayer::new()
        .quality(CompressionLevel::Precise(4))
        .compress_when(DefaultPredicate::new().and(NotForContentType::const_new("application/zip")))
}

async fn http_log_middleware(mut req: axum_extract::Request, next: Next) -> Response {
    let path = req.uri().path();
    if path.starts_with("/assets/") || path.starts_with("/images/") {
        return next.run(req).await;
    }
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .copied()
        .map(RequestId::render)
        .unwrap_or_default();
    let auth_slot = AuthContextSlot::initial();
    req.extensions_mut().insert(auth_slot.clone());
    let start = std::time::Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16();
    let latency_ms = start.elapsed().as_millis();
    let auth_context = auth_slot.log_snapshot();
    let principal_kind = auth_context
        .principal
        .as_ref()
        .map_or("none", Principal::kind);
    let auth_status = auth_context.auth_status.as_str();

    macro_rules! emit_http_log {
        ($level:ident $(, $field:ident = $value:expr)* $(,)?) => {{
            if let Some(auth_error_code) = auth_context.auth_error_code {
                let auth_error_code = auth_error_code.as_str();
                $level!(
                    %method,
                    %path,
                    status,
                    latency_ms,
                    request_id = %request_id,
                    principal_kind,
                    auth_status,
                    auth_error_code,
                    $($field = $value,)*
                    "HTTP response"
                );
            } else {
                $level!(
                    %method,
                    %path,
                    status,
                    latency_ms,
                    request_id = %request_id,
                    principal_kind,
                    auth_status,
                    $($field = $value,)*
                    "HTTP response"
                );
            }
        }};
    }

    macro_rules! emit_principal_http_log {
        ($level:ident) => {{
            match &auth_context.principal {
                Some(Principal::User(user)) => emit_http_log!(
                    $level,
                    user_auth_method = user.auth_method.as_str(),
                    idp_issuer = user.identity.issuer(),
                    idp_subject = user.identity.subject(),
                    login = user.login.as_str(),
                ),
                Some(Principal::Worker { run_id }) => {
                    emit_http_log!($level, run_id = run_id.to_string().as_str(),)
                }
                Some(Principal::Webhook { delivery_id }) => {
                    emit_http_log!($level, delivery_id = delivery_id.as_str(),)
                }
                Some(Principal::Slack {
                    team_id, user_id, ..
                }) => emit_http_log!(
                    $level,
                    team_id = team_id.as_str(),
                    user_id = user_id.as_str(),
                ),
                None | Some(Principal::Agent { .. } | Principal::System { .. }) => {
                    emit_http_log!($level)
                }
            }
        }};
    }

    if status >= 500 {
        emit_principal_http_log!(error);
    } else {
        emit_principal_http_log!(info);
    }
    response
}

fn github_webhook_routes(secret: Arc<[u8]>) -> Router {
    Router::new()
        .route(WEBHOOK_ROUTE, post(github_webhook))
        .with_state(secret)
}

async fn github_webhook(
    State(secret): State<Arc<[u8]>>,
    RequestAuth(auth_slot): RequestAuth,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let delivery_id = headers
        .get("x-github-delivery")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");

    let Some(signature) = headers
        .get("x-hub-signature-256")
        .and_then(|value| value.to_str().ok())
    else {
        auth_slot.replace(RequestAuthContext::invalid());
        warn!(delivery = %delivery_id, "Webhook missing X-Hub-Signature-256 header");
        return StatusCode::UNAUTHORIZED;
    };

    if !verify_signature(&secret, &body, signature) {
        auth_slot.replace(RequestAuthContext::invalid());
        warn!(delivery = %delivery_id, "Webhook HMAC signature mismatch");
        return StatusCode::UNAUTHORIZED;
    }

    auth_slot.replace(RequestAuthContext::authenticated(
        Principal::Webhook {
            delivery_id: delivery_id.to_string(),
        },
        None,
    ));

    let event_type = headers
        .get("x-github-event")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");

    if tracing::enabled!(tracing::Level::DEBUG) {
        let (repo, action) = parse_event_metadata(&body);
        debug!(
            event = %event_type,
            delivery = %delivery_id,
            repo = %repo,
            action = %action,
            "Webhook received"
        );
    } else {
        info!(
            event = %event_type,
            delivery = %delivery_id,
            "Webhook received"
        );
    }

    StatusCode::OK
}

struct PrunePlan {
    run_ids:          Vec<RunId>,
    rows:             Vec<PruneRunEntry>,
    total_size_bytes: u64,
}

#[expect(
    clippy::disallowed_methods,
    reason = "sync helper invoked from async handler via spawn_blocking (see callers at :1301 / :1341)"
)]
fn build_disk_usage_response(
    summaries: &[fabro_types::Run],
    storage_dir: &std::path::Path,
    verbose: bool,
) -> anyhow::Result<DiskUsageResponse> {
    let scratch_base_dir = scratch_base(storage_dir);
    let logs_base_dir = Storage::new(storage_dir).runtime_directory().logs_dir();
    let runs = scan_runs_with_summaries(summaries, &scratch_base_dir)?;

    let mut active_count = 0u64;
    let mut total_run_size = 0u64;
    let mut reclaimable_run_size = 0u64;
    let mut run_rows = Vec::new();

    for run in &runs {
        let size = dir_size(&run.path);
        total_run_size += size;
        if run.status().is_active() {
            active_count += 1;
        } else {
            reclaimable_run_size += size;
        }
        if verbose {
            run_rows.push(DiskUsageRunRow {
                run_id:        Some(run.run_id().to_string()),
                workflow_name: Some(run.workflow_display_name()),
                status:        Some(run.status().to_string()),
                start_time:    Some(run.start_time()),
                size_bytes:    Some(to_i64(size)),
                reclaimable:   Some(!run.status().is_active()),
            });
        }
    }

    let mut log_count = 0u64;
    let mut total_log_size = 0u64;
    if let Ok(entries) = std::fs::read_dir(logs_base_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|ext| ext != "log") {
                continue;
            }
            if let Ok(metadata) = path.metadata() {
                log_count += 1;
                total_log_size += metadata.len();
            }
        }
    }

    // Measure the whole storage tree so the managed total can't drift as new
    // subdirectories are added. "other" is the residual (database, artifacts,
    // sessions, vaults) — everything that isn't an enumerated run or log file.
    let managed_size = dir_size(storage_dir);
    let other_size = managed_size.saturating_sub(total_run_size + total_log_size);

    Ok(DiskUsageResponse {
        summary:                 vec![
            DiskUsageSummaryRow {
                type_:             Some("runs".to_string()),
                count:             Some(to_i64(runs.len())),
                active:            Some(to_i64(active_count)),
                size_bytes:        Some(to_i64(total_run_size)),
                reclaimable_bytes: Some(to_i64(reclaimable_run_size)),
            },
            DiskUsageSummaryRow {
                type_:             Some("logs".to_string()),
                count:             Some(to_i64(log_count)),
                active:            None,
                size_bytes:        Some(to_i64(total_log_size)),
                reclaimable_bytes: Some(to_i64(total_log_size)),
            },
            DiskUsageSummaryRow {
                type_:             Some("other".to_string()),
                count:             None,
                active:            None,
                size_bytes:        Some(to_i64(other_size)),
                reclaimable_bytes: Some(0),
            },
        ],
        total_size_bytes:        Some(to_i64(managed_size)),
        total_reclaimable_bytes: Some(to_i64(reclaimable_run_size + total_log_size)),
        runs:                    verbose.then_some(run_rows),
    })
}

fn build_prune_plan(
    request: &PruneRunsRequest,
    summaries: &[fabro_types::Run],
    storage_dir: &std::path::Path,
) -> anyhow::Result<PrunePlan> {
    let scratch_base_dir = scratch_base(storage_dir);
    let runs = scan_runs_with_summaries(summaries, &scratch_base_dir)?;
    let label_filters = request
        .labels
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();

    let mut filtered = filter_runs(
        &runs,
        request.before.as_deref(),
        request.workflow.as_deref(),
        &label_filters,
        request.orphans,
        StatusFilter::All,
    );

    let has_explicit_filters =
        request.before.is_some() || request.workflow.is_some() || !label_filters.is_empty();
    let staleness_threshold = if let Some(duration) = request.older_than.as_deref() {
        Some(parse_system_duration(duration)?)
    } else if !has_explicit_filters {
        Some(chrono::Duration::hours(24))
    } else {
        None
    };

    if let Some(threshold) = staleness_threshold {
        let cutoff = chrono::Utc::now() - threshold;
        filtered.retain(|run| {
            run.end_time
                .or(run.start_time_dt)
                .is_some_and(|time| time < cutoff)
        });
    }

    filtered.retain(|run| !run.status().is_active());

    let rows = filtered
        .iter()
        .map(|run| PruneRunEntry {
            run_id:        Some(run.run_id().to_string()),
            dir_name:      Some(run.dir_name.clone()),
            workflow_name: Some(run.workflow_display_name()),
            size_bytes:    Some(to_i64(dir_size(&run.path))),
        })
        .collect::<Vec<_>>();
    let total_size_bytes = rows
        .iter()
        .map(|row| row.size_bytes.unwrap_or_default())
        .sum::<i64>()
        .max(0)
        .try_into()
        .unwrap_or_default();

    Ok(PrunePlan {
        run_ids: filtered.iter().map(RunInfo::run_id).collect(),
        rows,
        total_size_bytes,
    })
}

fn resolve_manifest_run_settings_with_catalog(
    manifest_run_defaults: &RunLayer,
    environment_store: &EnvironmentStore,
    mcp_server_store: &McpServerStore,
) -> std::result::Result<RunNamespace, SharedError> {
    WorkflowSettingsBuilder::new()
        .server_manifest_defaults(
            manifest_run_defaults.clone(),
            (*environment_store.catalog_layer()).clone(),
        )
        .server_mcp_catalog(mcp_server_store.catalog_settings())
        .build()
        .map(|settings| settings.run)
        .map_err(|err| SharedError::new(anyhow::Error::msg(err.to_string())))
}

fn system_sandbox_provider(
    manifest_run_settings: &std::result::Result<RunNamespace, SharedError>,
) -> String {
    manifest_run_settings.as_ref().map_or_else(
        |_| SandboxProviderKind::default().to_string(),
        |settings| settings.environment.provider.to_string(),
    )
}

fn parse_system_duration(raw: &str) -> anyhow::Result<chrono::Duration> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "empty duration string");
    let (num_str, unit) = raw.split_at(raw.len().saturating_sub(1));
    let amount = num_str.parse::<u64>()?;
    match unit {
        "h" => Ok(chrono::Duration::hours(
            i64::try_from(amount).unwrap_or(i64::MAX),
        )),
        "d" => Ok(chrono::Duration::days(
            i64::try_from(amount).unwrap_or(i64::MAX),
        )),
        _ => anyhow::bail!("invalid duration unit '{unit}' in '{raw}' (expected 'h' or 'd')"),
    }
}

fn dir_size(path: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|metadata| metadata.len())
        .sum()
}

fn to_i64<T>(value: T) -> i64
where
    i64: TryFrom<T>,
{
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn worker_token_keys_from_server_secrets(
    server_secrets: &ServerSecrets,
) -> anyhow::Result<WorkerTokenKeys> {
    let session_secret = server_secrets
        .get(EnvVars::SESSION_SECRET)
        .ok_or_else(|| jwt_auth::session_secret_key_error(&auth::KeyDeriveError::Empty))?;
    WorkerTokenKeys::from_master_secret(session_secret.as_bytes())
        .map_err(|err| jwt_auth::session_secret_key_error(&err))
}

fn build_sandbox_inventory(
    server_settings: &ServerSettings,
    daytona_api_key: Option<String>,
    env_lookup: &EnvLookup,
    http_client: Option<fabro_http::HttpClient>,
) -> SandboxInventory {
    let provider_settings = &server_settings.server.sandbox.providers;
    let mut inventory = SandboxInventory::empty();

    if provider_settings.is_enabled(&SandboxProviderKind::LOCAL) {
        inventory = inventory.with_host_directories(SandboxProviderKind::LOCAL);
    }

    if let Some(docker) = provider_settings.get(&SandboxProviderKind::DOCKER) {
        if docker.enabled {
            inventory = inventory.with_lazy(
                SandboxProviderKind::DOCKER,
                docker.clone(),
                ProviderConnectOptions::default(),
            );
        }
    }

    if let Some(daytona) = provider_settings.get(&SandboxProviderKind::DAYTONA) {
        if let Some(api_key) = daytona_api_key.filter(|_| daytona.enabled) {
            let credentials = DaytonaCredentials::from_api_key(api_key, |name| env_lookup(name))
                .with_http_client(http_client);
            inventory = inventory.with_lazy(
                SandboxProviderKind::DAYTONA,
                daytona.clone(),
                ProviderConnectOptions {
                    host_registry_root: None,
                    daytona:            Some(credentials),
                },
            );
        }
    }

    inventory
}

pub(crate) fn automation_dir_for_active_config(active_config_path: &std::path::Path) -> PathBuf {
    active_config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("automations")
}

fn mcp_server_dir_for_active_config(active_config_path: &std::path::Path) -> PathBuf {
    active_config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("mcps")
}

#[expect(
    clippy::disallowed_methods,
    reason = "synchronous app-state assembly may run inside an async runtime; a short-lived OS \
              thread avoids nested Tokio runtimes"
)]
fn load_store_blocking<T, F, Fut>(description: &'static str, load: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    std::thread::spawn(move || {
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .with_context(|| format!("build {description} runtime"))?;
        runtime.block_on(load())
    })
    .join()
    .expect("store load thread should not panic")
}

pub(crate) fn build_app_state(config: AppStateConfig) -> anyhow::Result<Arc<AppState>> {
    let AppStateConfig {
        resolved_settings,
        execute_in_process,
        max_concurrent_runs,
        store,
        artifact_store,
        db_pool,
        preloaded_vault,
        server_secrets,
        env_lookup,
        github_api_base_url,
        active_config_path,
        http_client,
        sandbox_inventory,
        shutdown,
        #[cfg(test)]
        worker_control_bus,
        #[cfg(test)]
        worker_runtime,
        #[cfg(any(test, feature = "test-support"))]
        automation_materializer_override,
    } = config;

    let store_pool = db_pool.clone();
    let automation_migration_pool = db_pool.clone();
    load_store_blocking("automation environment migration", move || async move {
        fabro_automation::backfill_environment_selectors(&automation_migration_pool)
            .await
            .map_err(anyhow::Error::new)
    })
    .context("backfill automation environment selectors")?;
    let automation_store = Arc::new(AutomationStore::new(db_pool.clone()));
    let local_provider_enabled = resolved_settings
        .server_settings
        .server
        .sandbox
        .providers
        .is_enabled(&SandboxProviderKind::LOCAL);
    let environment_pool = db_pool.clone();
    let environment_store = Arc::new(
        load_store_blocking("environment store", move || async move {
            EnvironmentStore::load(environment_pool, local_provider_enabled)
                .await
                .map_err(anyhow::Error::new)
        })
        .context("load environments")?,
    );
    let run_summaries = store.run_summary_store();
    let auth_codes = Arc::new(AuthCodeStore::new(db_pool.clone()));
    let auth_sessions = Arc::new(AuthSessionStore::new(db_pool.clone()));
    let mcp_server_dir = mcp_server_dir_for_active_config(&active_config_path);
    let mcp_server_pool = db_pool.clone();
    let mcp_server_store = Arc::new(
        load_store_blocking("MCP server store", move || async move {
            McpServerStore::open(mcp_server_pool, mcp_server_dir)
                .await
                .map_err(anyhow::Error::new)
        })
        .context("load mcp servers")?,
    );
    let variables = Arc::new(VariableStore::new(db_pool.clone()));
    let petri_runs = PetriRuns::new(db_pool.clone());
    // Petri's records live on the shared pool; the view tables live where the
    // run summary store keeps the `runs` row (the same database in the
    // server, a fixture of its own in a test).
    let petri_projector = Projector::new(db_pool.clone(), store.run_summary_store().pool());
    {
        let projector = Arc::clone(&petri_projector);
        store.set_platform_record_hook(Arc::new(move |run_id| projector.signal(run_id)));
    }
    let session_records = Arc::new(RunSessionRecordStore::new(db_pool.clone()));
    let session_events = Arc::new(RunSessionEventStore::new(db_pool.clone()));
    let secret_store = Arc::new(SecretStore::new(db_pool));
    let vault = preloaded_vault;
    // Read vault secrets needed for synchronous setup before we wrap the vault in
    // an async lock for the rest of AppState.
    let daytona_api_key = vault.get(EnvVars::DAYTONA_API_KEY).map(str::to_string);
    let llm_source: Arc<dyn CredentialProvider> = Arc::new(SqlVaultCredentialSource::vault_only(
        Arc::clone(&secret_store),
    ));
    let (global_event_tx, _) = broadcast::channel(4096);
    let current_server_settings = Arc::new(resolved_settings.server_settings);
    let current_effective_web_url =
        effective_web_url(&current_server_settings.server, |name| env_lookup(name));
    let current_manifest_run_defaults = Arc::new(resolved_settings.manifest_run_defaults);
    let current_manifest_run_settings = resolve_manifest_run_settings_with_catalog(
        current_manifest_run_defaults.as_ref(),
        &environment_store,
        &mcp_server_store,
    );
    let current_catalog = Arc::new(
        fabro_llm::build_catalog(&resolved_settings.llm_overlay, &|name| env_lookup(name))
            .context("building LLM model catalog")?,
    );
    let sandbox_inventory = sandbox_inventory.unwrap_or_else(|| {
        build_sandbox_inventory(
            current_server_settings.as_ref(),
            daytona_api_key,
            &env_lookup,
            http_client.clone(),
        )
    });
    let slack_service = {
        let slack_settings = &current_server_settings.server.integrations.slack;
        if slack_settings.enabled {
            let default_channel = slack_settings.default_channel.clone();
            match resolve_slack_credentials_status_with_lookup(|name| {
                vault.get(name).map(str::to_string)
            }) {
                SlackCredentialResolution::Configured(credentials) => {
                    info!(
                        default_channel_configured = default_channel.is_some(),
                        "Slack integration enabled"
                    );
                    Some(Arc::new(SlackService::new(
                        credentials.bot_token,
                        credentials.app_token,
                        default_channel,
                    )))
                }
                SlackCredentialResolution::Missing { env_vars } => {
                    info!(
                        missing_env_vars = %env_vars.join(","),
                        "Slack integration disabled; missing credentials"
                    );
                    None
                }
            }
        } else {
            info!("Slack integration disabled by server configuration");
            None
        }
    };
    let worker_tokens = worker_token_keys_from_server_secrets(&server_secrets)?;
    let github_api_base_url = github_api_base_url.unwrap_or_else(fabro_github::github_api_base_url);
    let storage_root = PathBuf::from(&current_server_settings.server.storage.root);
    let automation_repo_cache = Arc::new(GitRepoCache::new(
        Storage::new(&storage_root)
            .cache_dir()
            .join("automation-repos"),
    ));
    let worker_control_bus: Arc<dyn WorkerControlBus> = {
        #[cfg(test)]
        {
            worker_control_bus.unwrap_or_else(|| Arc::new(LocalWorkerControlBus::new()))
        }
        #[cfg(not(test))]
        {
            Arc::new(LocalWorkerControlBus::new())
        }
    };
    let worker_runtime: Arc<dyn WorkerRuntime> = {
        #[cfg(test)]
        {
            worker_runtime.unwrap_or_else(|| Arc::new(LocalWorkerRuntime::new()))
        }
        #[cfg(not(test))]
        {
            Arc::new(LocalWorkerRuntime::new())
        }
    };
    Ok(Arc::new(AppState {
        runs: Mutex::new(HashMap::new()),
        aggregate_usage: Mutex::new(UsageAccumulator::default()),
        stores: AppStores {
            runs: store,
            run_summaries,
            session_records,
            session_events,
            auth_codes,
            auth_sessions,
            automations: automation_store,
            environments: environment_store,
            mcp_servers: mcp_server_store,
            vault: secret_store,
            variables,
        },
        session_runtimes: SessionRuntimeManager::new(),
        artifact_store,
        automation_repo_cache,
        #[cfg(any(test, feature = "test-support"))]
        automation_materializer_override,
        worker_tokens,
        started_at: Instant::now(),
        resource_sampler: resource_sampler::ResourceSampler::new(),
        max_concurrent_runs,
        worker_control_bus,
        worker_control_acks: Arc::new(WorkerControlAcks::new(WORKER_CONTROL_ACK_WAIT)),
        worker_runtime,
        petri_runs,
        petri_projector,
        stream_follower: Arc::new(stream_follower::StreamFollower::default()),
        scheduler_notify: Notify::new(),
        automation_scheduler_notify: Notify::new(),
        pull_request_scheduler_notify: Notify::new(),
        pull_request_creation_queue: Mutex::new(
            pull_request_supervisor::PendingPullRequestCreationQueue::default(),
        ),
        global_event_tx,
        files_in_flight: new_files_in_flight(),
        pull_request_create_locks: KeyedMutex::new(),
        control_request_locks: KeyedMutex::new(),
        parent_link_lock: AsyncMutex::new(()),
        server_secrets,
        llm_source,
        db_pool: store_pool,
        manifest_run_defaults: RwLock::new(current_manifest_run_defaults),
        manifest_run_settings: RwLock::new(current_manifest_run_settings),
        server_settings: RwLock::new(current_server_settings),
        effective_web_url: RwLock::new(current_effective_web_url),
        catalog: RwLock::new(current_catalog),
        env_lookup: Arc::clone(&env_lookup),
        github_api_base_url,
        active_config_path,
        http_client,
        sandbox_inventory,
        shutdown,
        shutting_down: AtomicBool::new(false),
        execute_in_process,
        slack_service,
        slack_started: AtomicBool::new(false),
        // Startup snapshot for the sync router build; rotating the webhook
        // secret requires a server restart.
        github_webhook_secret: vault.get(WEBHOOK_SECRET_ENV).map(str::to_string),
    }))
}

const MAX_PAGE_OFFSET: u32 = 1_000_000;

enum DeleteRunOutcome {
    Deleted,
    AlreadyAbsent,
    Preserved(DeleteRunResponse),
}

enum SandboxDeleteOutcome {
    /// The durable run store did not exist; nothing to delete.
    Absent,
    /// The sandbox resource was cleaned up (or there was none to clean).
    Cleaned,
    /// Sandbox is being handed off to the operator instead of deleted.
    Preserved(DeleteRunResponse),
}

async fn delete_run_internal(
    state: &AppState,
    id: RunId,
    force: bool,
) -> Result<DeleteRunOutcome, ApiError> {
    if !force {
        reject_active_delete_without_force(state, &id).await?;
    }

    let mut managed_run = if let Ok(mut runs) = state.runs.lock() {
        runs.remove(&id)
    } else {
        None
    };
    let had_managed_run = managed_run.is_some();
    let durable_status = if managed_run.is_some() {
        durable_run_status(state, id).await.ok().flatten()
    } else {
        None
    };
    let should_signal_cancel = !durable_status.is_some_and(RunStatus::is_terminal);

    if let Some(managed_run) = managed_run.as_mut() {
        if should_signal_cancel {
            if let Some(token) = &managed_run.cancel_token {
                token.cancel();
            }
            if let Some(answer_transport) = managed_run.answer_transport.clone() {
                let _ = answer_transport.cancel_run().await;
            }
            if let Some(cancel_tx) = managed_run.cancel_tx.take() {
                let _ = cancel_tx.send(());
            }
        }
        // Terminal runs can still carry a stale worker ref briefly after their
        // completion events land, so avoid paying the full cancellation grace.
        let delete_grace = if should_signal_cancel && managed_run.status.requires_force_to_delete()
        {
            WORKER_CANCEL_GRACE
        } else {
            TERMINAL_DELETE_WORKER_GRACE
        };
        terminate_worker_for_deletion(
            &state.worker_runtime,
            managed_run.worker_ref.clone(),
            delete_grace,
        )
        .await;
    }

    let delete_outcome = delete_run_sandbox_resource(state, id, force).await?;

    if let Some(mut managed_run) = managed_run {
        if let Some(run_dir) = managed_run.run_dir.take() {
            remove_run_dir(&run_dir)
                .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
        }
    } else {
        let storage = Storage::new(state.server_storage_dir());
        let run_dir = storage.run_scratch(&id).root().to_path_buf();
        remove_run_dir(&run_dir)
            .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    }

    state
        .stores
        .runs
        .delete_run(&id)
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    state.petri_runs.worker_exited(id);
    state
        .petri_projector
        .delete_run(id)
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    state
        .artifact_store
        .delete_for_run(&id)
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    state
        .stores
        .session_events
        .delete_for_run(id)
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    match delete_outcome {
        SandboxDeleteOutcome::Preserved(response) => Ok(DeleteRunOutcome::Preserved(response)),
        SandboxDeleteOutcome::Cleaned => Ok(DeleteRunOutcome::Deleted),
        SandboxDeleteOutcome::Absent if had_managed_run => Ok(DeleteRunOutcome::Deleted),
        SandboxDeleteOutcome::Absent => Ok(DeleteRunOutcome::AlreadyAbsent),
    }
}

async fn delete_run_sandbox_resource(
    state: &AppState,
    id: RunId,
    force: bool,
) -> Result<SandboxDeleteOutcome, ApiError> {
    let projection = match run_records::projection(state, id).await {
        Ok(Some(projection)) => projection,
        Ok(None) => return Ok(SandboxDeleteOutcome::Absent),
        Err(err) if force => {
            tracing::warn!(
                run_id = %id,
                error = %format!("{err:#}"),
                "Skipping sandbox provider delete because run projection cannot be loaded"
            );
            return Ok(SandboxDeleteOutcome::Cleaned);
        }
        Err(err) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                err.to_string(),
            ));
        }
    };
    let delete_started = matches!(projection.status, RunStatus::Removing);
    let can_mark_removing = projection.status.can_transition_to(RunStatus::Removing);
    if !delete_started && can_mark_removing {
        run_records::lifecycle(
            state,
            id,
            run_records::transition(RunLifecycleKind::Removing, RunStatus::Removing),
        )
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    }

    let preserve = projection
        .spec()
        .settings
        .run
        .environment
        .lifecycle
        .preserve;
    let Some(record) = projection
        .sandbox
        .as_ref()
        .and_then(fabro_types::RunSandbox::instance)
        .cloned()
    else {
        return Ok(SandboxDeleteOutcome::Cleaned);
    };
    let runtime = &record.runtime;
    if preserve {
        return Ok(SandboxDeleteOutcome::Preserved(DeleteRunResponse {
            deleted:           true,
            sandbox_preserved: true,
            sandbox:           DeleteRunSandbox {
                provider: record.provider,
                id:       runtime.id.clone(),
            },
        }));
    }

    let access = state
        .provider_access()
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let sandbox = match reconnect_for_run(&record, &access, Some(id), None).await {
        Ok(sandbox) => sandbox,
        Err(err) if force || delete_started => {
            tracing::warn!(
                run_id = %id,
                error = %render_with_causes(&err.to_string(), &collect_causes(err.as_ref())),
                "Skipping sandbox provider delete during run deletion"
            );
            return Ok(SandboxDeleteOutcome::Cleaned);
        }
        Err(err) => {
            let detail = render_with_causes(&err.to_string(), &collect_causes(err.as_ref()));
            return Err(ApiError::new(StatusCode::CONFLICT, detail));
        }
    };
    if let Err(err) = sandbox.delete().await {
        if force || delete_started {
            tracing::warn!(
                run_id = %id,
                error = %err.display_with_causes(),
                "Skipping failed sandbox provider delete during run deletion"
            );
            return Ok(SandboxDeleteOutcome::Cleaned);
        }
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            err.display_with_causes(),
        ));
    }

    Ok(SandboxDeleteOutcome::Cleaned)
}

async fn reject_active_delete_without_force(
    state: &AppState,
    run_id: &RunId,
) -> Result<(), ApiError> {
    let managed_status = state
        .runs
        .lock()
        .ok()
        .and_then(|runs| runs.get(run_id).map(|managed_run| managed_run.status));
    if let Some(status) = managed_status {
        if status.requires_force_to_delete() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                active_run_delete_message(*run_id, status),
            ));
        }
        return Ok(());
    }

    match state.stores.run_summaries.get(run_id, Utc::now()).await {
        Ok(Some(summary)) if summary.lifecycle.status.requires_force_to_delete() => {
            Err(ApiError::new(
                StatusCode::CONFLICT,
                active_run_delete_message(*run_id, summary.lifecycle.status),
            ))
        }
        Ok(_) => Ok(()),
        Err(err) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        )),
    }
}

fn active_run_delete_message(run_id: RunId, status: impl std::fmt::Display) -> String {
    let run_id = run_id.to_string();
    let short_run_id = &run_id[..12.min(run_id.len())];
    format!(
        "cannot remove active run {short_run_id} (status: {status}, use force=true or --force to force)"
    )
}

async fn terminate_worker_for_deletion(
    worker_runtime: &Arc<dyn WorkerRuntime>,
    worker_ref: Option<WorkerRef>,
    grace: Duration,
) {
    let Some(worker_ref) = worker_ref else {
        return;
    };

    worker_runtime.request_stop(&worker_ref).await;

    let deadline = Instant::now() + grace;
    while Instant::now() < deadline && worker_runtime.is_alive(&worker_ref).await {
        sleep(Duration::from_millis(50)).await;
    }

    if worker_runtime.is_alive(&worker_ref).await {
        worker_runtime.force_stop(&worker_ref).await;
        let kill_deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < kill_deadline && worker_runtime.is_alive(&worker_ref).await {
            sleep(Duration::from_millis(50)).await;
        }
    }
}

fn remove_run_dir(run_dir: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(run_dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
fn compute_queue_positions(runs: &HashMap<RunId, ManagedRun>) -> HashMap<RunId, i64> {
    let mut runnable: Vec<(&RunId, &ManagedRun)> = runs
        .iter()
        .filter(|(_, r)| r.status == RunStatus::Runnable)
        .collect();
    runnable.sort_by_key(|(_, r)| r.created_at);
    runnable
        .into_iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i64::try_from(i + 1).unwrap()))
        .collect()
}

pub(in crate::server) fn counts_toward_scheduler_capacity(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Starting
            | RunStatus::Running
            | RunStatus::Blocked { .. }
            | RunStatus::Paused { .. }
    )
}

#[allow(
    clippy::result_large_err,
    reason = "Run ID parsing returns HTTP 400 responses directly."
)]
pub(crate) fn parse_run_id_path(id: &str) -> Result<RunId, Response> {
    id.parse::<RunId>()
        .map_err(|_| ApiError::bad_request("Invalid run ID.").into_response())
}

#[allow(
    clippy::result_large_err,
    reason = "Stage ID parsing returns HTTP 400 responses directly."
)]
pub(crate) fn parse_stage_id_path(stage_id: &str) -> Result<StageId, Response> {
    StageId::from_str(stage_id)
        .map_err(|_| ApiError::bad_request("Invalid stage ID.").into_response())
}

#[allow(
    clippy::result_large_err,
    reason = "Blob hash parsing returns HTTP 400 responses directly."
)]
pub(crate) fn parse_blob_hash_path(blob_hash: &str) -> Result<BlobHash, Response> {
    BlobHash::from_str(blob_hash)
        .map_err(|_| ApiError::bad_request("Invalid blob hash.").into_response())
}

#[allow(
    clippy::result_large_err,
    reason = "Missing query parameter validation returns HTTP 400 responses directly."
)]
fn required_query_param<T: Clone>(value: Option<&T>, name: &str) -> Result<T, Response> {
    value.cloned().ok_or_else(|| {
        ApiError::bad_request(format!("Missing {name} query parameter.")).into_response()
    })
}

#[allow(
    clippy::result_large_err,
    reason = "Artifact path validation returns HTTP 400 responses directly."
)]
fn validate_relative_artifact_path(kind: &str, value: &str) -> Result<String, Response> {
    if value.is_empty() {
        return Err(ApiError::bad_request(format!("{kind} must not be empty")).into_response());
    }

    if value.contains('\\') {
        return Err(
            ApiError::bad_request(format!("{kind} must not contain backslashes")).into_response(),
        );
    }

    let segments = value.split('/').collect::<Vec<_>>();
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err(
            ApiError::bad_request(format!("{kind} must not contain empty path segments"))
                .into_response(),
        );
    }
    if segments
        .iter()
        .any(|segment| matches!(*segment, "." | ".."))
    {
        return Err(ApiError::bad_request(format!(
            "{kind} must be a relative path without '.' or '..' segments"
        ))
        .into_response());
    }

    Ok(segments.join("/"))
}

fn bad_request_response(detail: impl Into<String>) -> Response {
    ApiError::bad_request(detail.into()).into_response()
}

fn payload_too_large_response(detail: impl Into<String>) -> Response {
    ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, detail.into()).into_response()
}

fn octet_stream_response(bytes: Bytes) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/octet-stream")],
        bytes,
    )
        .into_response()
}

fn clear_live_run_state(run: &mut ManagedRun) {
    run.answer_transport = None;
    run.accepted_questions.clear();
    run.active_steerable_stages.clear();
    run.active_non_steerable_stages.clear();
    run.cancel_tx = None;
    run.cancel_token = None;
    run.worker_ref = None;
    run.cancel_escalation_worker = None;
}

fn cleanup_worker_control_bus_for_run(state: &AppState, run_id: RunId) {
    let bus = Arc::clone(&state.worker_control_bus);
    state.worker_control_acks.forget_run(run_id);
    tokio::spawn(async move {
        bus.cleanup_run(run_id).await;
    });
}

/// A question the run's stream closed (answered, or expired) no longer holds
/// an accepted-answer claim; a terminal run holds none.
fn reconcile_live_interview_state(run: &mut ManagedRun, item: &RunStreamItem) {
    match platform_record_of(item) {
        Some(PlatformRecord::InterviewAnswered(answered)) => {
            run.accepted_questions.remove(&answered.question);
        }
        Some(PlatformRecord::RunLifecycle(record))
            if matches!(
                record.transition,
                RunLifecycleKind::Succeeded | RunLifecycleKind::Failed | RunLifecycleKind::Dead
            ) =>
        {
            run.accepted_questions.clear();
        }
        _ => {}
    }
    if let Some(question_id) = petri_parsed(item)
        .filter(|parsed| {
            parsed.get("kind").and_then(serde_json::Value::as_str) == Some("question_expired")
        })
        .and_then(|parsed| parsed.get("question"))
        .and_then(serde_json::Value::as_str)
    {
        run.accepted_questions.remove(question_id);
    }
}

fn claim_run_answer_transport(
    state: &AppState,
    run_id: RunId,
    qid: &str,
) -> Result<RunAnswerTransport, StatusCode> {
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    let managed_run = runs.get_mut(&run_id).ok_or(StatusCode::NOT_FOUND)?;
    let transport = managed_run
        .answer_transport
        .clone()
        .ok_or(StatusCode::CONFLICT)?;

    if !managed_run.accepted_questions.insert(qid.to_string()) {
        return Err(StatusCode::CONFLICT);
    }

    Ok(transport)
}

fn release_run_answer_claim(state: &AppState, run_id: RunId, qid: &str) {
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        managed_run.accepted_questions.remove(qid);
    }
}

#[derive(Clone)]
struct LiveWorkerProcess {
    run_id:     RunId,
    worker_ref: WorkerRef,
}

/// Pick the terminal failure for a run that never produced its own terminal
/// event. A pending cancel wins over whatever failure the caller observed, so a
/// run that was cancelled while its worker was launching or dying is recorded
/// as cancelled rather than as broken.
fn failure_honoring_pending_cancel(
    pending_control: Option<RunControlAction>,
    otherwise: impl FnOnce() -> (WorkflowError, FailureReason),
) -> (WorkflowError, FailureReason) {
    if pending_control == Some(RunControlAction::Cancel) {
        (WorkflowError::Cancelled, FailureReason::Cancelled)
    } else {
        otherwise()
    }
}

fn failure_for_incomplete_run(
    pending_control: Option<RunControlAction>,
    terminated_message: String,
) -> (WorkflowError, FailureReason) {
    failure_honoring_pending_cancel(pending_control, || {
        (
            WorkflowError::engine(terminated_message),
            FailureReason::Terminated,
        )
    })
}

pub(crate) async fn reconcile_incomplete_runs_on_startup(
    state: &Arc<AppState>,
) -> anyhow::Result<usize> {
    const RECONCILABLE_STATUSES: &[RunStatusKind] = &[
        RunStatusKind::Runnable,
        RunStatusKind::Starting,
        RunStatusKind::Running,
        RunStatusKind::Blocked,
        RunStatusKind::Paused,
        RunStatusKind::Removing,
    ];
    let summaries = state
        .stores
        .run_summaries
        .list_by_statuses(RECONCILABLE_STATUSES, chrono::Utc::now())
        .await?;
    let mut reconciled = 0usize;

    for summary in summaries {
        let Some(run_state) = run_records::projection(state, summary.id).await? else {
            continue;
        };
        // A run continues from its records in a new worker, unless a cancel
        // was pending or the run was being removed: those end failed.
        if petri_run_resumes_on_restart(&summary) {
            petri_runs::reconcile_on_startup(state, summary.id, &run_state).await?;
            reconciled += 1;
            continue;
        }
        let (error, reason) = failure_for_incomplete_run(
            summary.lifecycle.pending_control,
            "Fabro server restarted before the run reached a terminal state.".to_string(),
        );
        run_records::lifecycle(
            state,
            summary.id,
            run_records::failed(reason, error.to_string()),
        )
        .await?;
        reconciled += 1;
    }

    Ok(reconciled)
}

/// Whether a run the server finds in flight at startup is one a Petri
/// worker can continue: it was runnable or running (blocked or paused
/// count), no cancel was pending, and it was not being removed.
fn petri_run_resumes_on_restart(summary: &fabro_types::Run) -> bool {
    summary.lifecycle.pending_control != Some(RunControlAction::Cancel)
        && matches!(
            summary.lifecycle.status,
            RunStatus::Runnable
                | RunStatus::Starting
                | RunStatus::Running
                | RunStatus::Blocked { .. }
                | RunStatus::Paused { .. }
        )
}

fn live_worker_processes(state: &AppState) -> Vec<LiveWorkerProcess> {
    let runs = state.runs.lock().expect("runs lock poisoned");
    runs.iter()
        .filter_map(|(run_id, managed_run)| {
            managed_run
                .worker_ref
                .clone()
                .map(|worker_ref| LiveWorkerProcess {
                    run_id: *run_id,
                    worker_ref,
                })
        })
        .collect()
}

async fn persist_shutdown_run_failures(
    state: &Arc<AppState>,
    workers: &[LiveWorkerProcess],
) -> anyhow::Result<()> {
    let run_ids = workers
        .iter()
        .map(|worker| worker.run_id)
        .collect::<HashSet<_>>();

    for run_id in run_ids {
        let Some(run_state) = run_records::projection(state, run_id).await? else {
            continue;
        };
        if run_state.status.is_terminal() {
            continue;
        }

        let (error, reason) = failure_for_incomplete_run(
            run_state.pending_control,
            "Fabro server shut down before the run reached a terminal state.".to_string(),
        );
        run_records::lifecycle(
            state,
            run_id,
            run_records::failed(reason, error.to_string()),
        )
        .await?;
    }

    Ok(())
}

pub(crate) async fn shutdown_active_workers(state: &Arc<AppState>) -> anyhow::Result<usize> {
    shutdown_active_workers_with_grace(state, WORKER_CANCEL_GRACE, Duration::from_millis(50)).await
}

async fn shutdown_active_workers_with_grace(
    state: &Arc<AppState>,
    grace: Duration,
    poll_interval: Duration,
) -> anyhow::Result<usize> {
    state.begin_shutdown();
    let workers = live_worker_processes(state.as_ref());

    join_all(
        workers
            .iter()
            .map(|worker| state.worker_runtime.request_stop(&worker.worker_ref)),
    )
    .await;

    let survivors = poll_until_dead(state.as_ref(), &workers, grace, poll_interval).await;

    if !survivors.is_empty() {
        join_all(
            survivors
                .iter()
                .map(|worker_ref| state.worker_runtime.force_stop(worker_ref)),
        )
        .await;
        // Wait for the kernel to reap the killed workers so callers can
        // assume the processes are actually gone when shutdown returns.
        let kill_deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < kill_deadline
            && !alive_refs(state.as_ref(), &survivors).await.is_empty()
        {
            sleep(poll_interval).await;
        }
    }

    persist_shutdown_run_failures(state, &workers).await?;
    Ok(workers.len())
}

/// Poll until either the deadline expires or every worker is dead, returning
/// the set of workers still alive when polling stopped.
async fn poll_until_dead(
    state: &AppState,
    workers: &[LiveWorkerProcess],
    grace: Duration,
    poll_interval: Duration,
) -> Vec<WorkerRef> {
    let refs: Vec<WorkerRef> = workers.iter().map(|w| w.worker_ref.clone()).collect();
    let deadline = Instant::now() + grace;
    loop {
        let alive = alive_refs(state, &refs).await;
        if alive.is_empty() || Instant::now() >= deadline {
            return alive;
        }
        sleep(poll_interval).await;
    }
}

async fn alive_refs(state: &AppState, refs: &[WorkerRef]) -> Vec<WorkerRef> {
    let liveness = join_all(refs.iter().map(|r| state.worker_runtime.is_alive(r))).await;
    refs.iter()
        .zip(liveness)
        .filter(|(_, alive)| *alive)
        .map(|(r, _)| r.clone())
        .collect()
}

async fn persist_cancelled_run_status(state: &AppState, run_id: RunId) -> anyhow::Result<()> {
    let Some(run_state) = run_records::projection(state, run_id).await? else {
        anyhow::bail!("run {run_id} not found");
    };
    if run_state.status.is_terminal() {
        return Ok(());
    }
    run_records::lifecycle(
        state,
        run_id,
        run_records::failed(
            FailureReason::Cancelled,
            WorkflowError::Cancelled.to_string(),
        ),
    )
    .await
    .map(|_| ())
}

/// Reject the run before execution if its effective sandbox provider is
/// disabled by server policy. Returns `true` when the run was rejected.
async fn reject_run_if_sandbox_provider_disabled(
    state: &Arc<AppState>,
    server_settings: &ServerSettings,
    run_id: RunId,
    settings: &RunNamespace,
) -> bool {
    let provider = run_manifest::effective_sandbox_provider(settings);
    let Some(error) = run_manifest::sandbox_provider_policy_error(server_settings, &provider)
    else {
        return false;
    };
    tracing::warn!(run_id = %run_id, error = %error, "Sandbox provider disabled by server policy");
    fail_run_before_execution(state, run_id, FailureReason::LaunchFailed, error).await;
    true
}

async fn fail_run_before_execution(
    state: &Arc<AppState>,
    run_id: RunId,
    reason: FailureReason,
    message: String,
) {
    if let Err(err) =
        run_records::lifecycle(state, run_id, run_records::failed(reason, message.clone())).await
    {
        error!(run_id = %run_id, error = %err, "Failed to persist run failure status");
    }

    fail_managed_run(state, run_id, reason, message);
    state.scheduler_notify.notify_one();
}

fn managed_run(
    dot_source: String,
    status: RunStatus,
    created_at: chrono::DateTime<chrono::Utc>,
    run_dir: std::path::PathBuf,
    execution_mode: RunExecutionMode,
) -> ManagedRun {
    ManagedRun {
        dot_source,
        status,
        error: None,
        created_at,
        answer_transport: None,
        accepted_questions: HashSet::new(),
        active_steerable_stages: HashMap::new(),
        active_non_steerable_stages: HashMap::new(),
        cancel_tx: None,
        cancel_token: None,
        worker_ref: None,
        cancel_escalation_worker: None,
        run_dir: Some(run_dir),
        execution_mode,
    }
}

fn worker_mode_arg(mode: RunExecutionMode) -> &'static str {
    match mode {
        RunExecutionMode::Start => "start",
        RunExecutionMode::Resume => "resume",
    }
}

async fn load_pending_control(
    state: &AppState,
    run_id: RunId,
) -> anyhow::Result<Option<RunControlAction>> {
    Ok(state
        .stores
        .run_summaries
        .get(&run_id, Utc::now())
        .await?
        .and_then(|summary| summary.lifecycle.pending_control))
}

async fn durable_run_status(state: &AppState, run_id: RunId) -> anyhow::Result<Option<RunStatus>> {
    Ok(state
        .stores
        .run_summaries
        .get(&run_id, Utc::now())
        .await?
        .map(|summary| summary.lifecycle.status))
}

fn fail_managed_run(state: &Arc<AppState>, run_id: RunId, reason: FailureReason, message: String) {
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        managed_run.status = RunStatus::Failed { reason };
        managed_run.error = Some(message);
        clear_live_run_state(managed_run);
    }
    cleanup_worker_control_bus_for_run(state.as_ref(), run_id);
}

/// Fold one lifecycle record of the run's stream into the in-memory run:
/// the status the scheduler and the control handlers read. A `runnable`
/// record is not folded: scheduling is owned by the start and approve
/// handlers, which set the live status and notify the scheduler themselves.
fn apply_lifecycle_to_managed_run(state: &AppState, run_id: RunId, record: &RunLifecycleRecord) {
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    let Some(managed_run) = runs.get_mut(&run_id) else {
        return;
    };
    match record.transition {
        RunLifecycleKind::Submitted => managed_run.status = RunStatus::Submitted,
        RunLifecycleKind::Pending => {
            if let Some(status) = record.status {
                managed_run.status = status;
            }
        }
        RunLifecycleKind::Starting => managed_run.status = RunStatus::Starting,
        RunLifecycleKind::Running => managed_run.status = RunStatus::Running,
        RunLifecycleKind::Blocked => {
            let Some(RunStatus::Blocked { blocked_reason }) = record.status else {
                return;
            };
            managed_run.status = match managed_run.status {
                RunStatus::Paused { .. } => RunStatus::Paused {
                    prior_block: Some(blocked_reason),
                },
                _ => RunStatus::Blocked { blocked_reason },
            };
        }
        RunLifecycleKind::Unblocked => {
            managed_run.status = match managed_run.status {
                RunStatus::Paused { .. } => RunStatus::Paused { prior_block: None },
                _ => RunStatus::Running,
            };
        }
        RunLifecycleKind::Paused => {
            let prior_block = match managed_run.status {
                RunStatus::Blocked { blocked_reason } => Some(blocked_reason),
                RunStatus::Paused { prior_block } => prior_block,
                _ => None,
            };
            managed_run.status = RunStatus::Paused { prior_block };
        }
        RunLifecycleKind::Unpaused => {
            managed_run.status = match managed_run.status {
                RunStatus::Paused {
                    prior_block: Some(blocked_reason),
                } => RunStatus::Blocked { blocked_reason },
                _ => RunStatus::Running,
            };
        }
        RunLifecycleKind::Removing => managed_run.status = RunStatus::Removing,
        RunLifecycleKind::Succeeded => {
            managed_run.status = record.status.unwrap_or(RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            });
            managed_run.error = None;
            managed_run.active_steerable_stages.clear();
            managed_run.active_non_steerable_stages.clear();
            cleanup_worker_control_bus_for_run(state, run_id);
        }
        RunLifecycleKind::Failed | RunLifecycleKind::Dead => {
            managed_run.status = record.status.unwrap_or(RunStatus::Failed {
                reason: FailureReason::WorkflowError,
            });
            managed_run.error.clone_from(&record.reason);
            managed_run.active_steerable_stages.clear();
            managed_run.active_non_steerable_stages.clear();
            cleanup_worker_control_bus_for_run(state, run_id);
        }
        RunLifecycleKind::Runnable
        | RunLifecycleKind::StartRequested
        | RunLifecycleKind::Approved
        | RunLifecycleKind::Denied
        | RunLifecycleKind::CancelRequested
        | RunLifecycleKind::PauseRequested
        | RunLifecycleKind::UnpauseRequested => {}
    }
}

async fn drain_worker_stderr(
    run_id: RunId,
    stderr: std::pin::Pin<Box<dyn AsyncRead + Send + 'static>>,
) -> anyhow::Result<()> {
    let mut lines = BufReader::new(stderr).lines();

    while let Some(line) = lines.next_line().await? {
        tracing::warn!(run_id = %run_id, "Worker stderr: {line}");
    }

    Ok(())
}

async fn fail_worker_launch(state: &Arc<AppState>, run_id: RunId, err: anyhow::Error) {
    tracing::error!(run_id = %run_id, error = %err, "Failed to spawn worker");
    let pending_control = match run_records::projection(state, run_id).await {
        Ok(run_state) => run_state.and_then(|run_state| run_state.pending_control),
        Err(state_err) => {
            tracing::warn!(
                run_id = %run_id,
                error = %state_err,
                "Failed to load run state after worker launch failure"
            );
            None
        }
    };
    let launch_message = format!("Failed to spawn worker: {err}");
    let (error, reason) = failure_honoring_pending_cancel(pending_control, || {
        (
            WorkflowError::engine_with_anyhow("Failed to spawn worker", err),
            FailureReason::LaunchFailed,
        )
    });
    let message = if reason == FailureReason::Cancelled {
        "Run cancelled before worker launch completed".to_string()
    } else {
        launch_message
    };
    let _ = run_records::lifecycle(
        state,
        run_id,
        run_records::failed(reason, error.to_string()),
    )
    .await;
    fail_managed_run(state, run_id, reason, message);
    state.scheduler_notify.notify_one();
}

/// A worker that exited without recording the run's end left it failed.
async fn append_worker_exit_failure(state: &AppState, run_id: RunId, worker_exit: &WorkerExit) {
    let run_state = match run_records::projection(state, run_id).await {
        Ok(Some(run_state)) => run_state,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Failed to load run state after worker exit");
            return;
        }
    };
    if run_state.status.is_terminal() {
        return;
    }

    let (error, reason) = failure_for_incomplete_run(
        run_state.pending_control,
        format!(
            "Worker exited before emitting a terminal run event: {}",
            worker_exit.detail
        ),
    );
    if let Err(err) = run_records::lifecycle(
        state,
        run_id,
        run_records::failed(reason, error.to_string()),
    )
    .await
    {
        tracing::warn!(run_id = %run_id, error = %err, "Failed to record the worker exit failure");
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Worker subprocess startup resolves Cargo's test binary env override when present."
)]
fn worker_launch_spec(
    state: &AppState,
    run_id: RunId,
    mode: RunExecutionMode,
    run_dir: &std::path::Path,
    agent_fabro_tools_enabled: bool,
    github_app_private_key: Option<String>,
    daytona_api_key: Option<String>,
) -> anyhow::Result<WorkerLaunchSpec> {
    let current_exe = std::env::current_exe().context("reading current executable path")?;
    let executable =
        std::env::var_os(EnvVars::CARGO_BIN_EXE_FABRO).map_or(current_exe, PathBuf::from);
    let storage_dir = state.server_storage_dir();
    let runtime_directory = Storage::new(&storage_dir).runtime_directory();
    let daemon = ServerDaemon::read(&runtime_directory)?.with_context(|| {
        format!(
            "server record {} is missing",
            runtime_directory.record_path().display()
        )
    })?;
    let scopes = if agent_fabro_tools_enabled {
        WorkerScopeSet::run_worker_with_agent_run_tools()
    } else {
        WorkerScopeSet::run_worker()
    };
    let worker_token = issue_worker_token_with_scopes(state.worker_token_keys(), &run_id, scopes)
        .map_err(|_| anyhow::anyhow!("failed to sign worker token"))?;
    let log_destination = resolved_log_destination(state)?;
    let fabro_log = if (state.env_lookup)(EnvVars::FABRO_LOG).is_none() {
        state.server_settings().server.logging.level.clone()
    } else {
        None
    };

    Ok(WorkerLaunchSpec {
        executable,
        server_target: daemon.bind.to_target(),
        storage_dir,
        run_dir: run_dir.to_path_buf(),
        run_id,
        mode: worker_mode_arg(mode),
        worker_token,
        log_destination,
        fabro_log,
        active_config_path: state.active_config_path().to_path_buf(),
        github_app_private_key,
        daytona_api_key,
        fabro_home: fabro_config::Home::from_env().root().to_path_buf(),
    })
}

fn resolved_log_destination(state: &AppState) -> anyhow::Result<LogDestination> {
    let env_value = (state.env_lookup)(EnvVars::FABRO_LOG_DESTINATION);
    fabro_config::resolve_log_destination_with_env(
        state.server_settings().server.logging.destination,
        env_value.as_deref(),
    )
}

fn runtime_question_from_interview_record(question: &InterviewQuestionRecord) -> Question {
    Question {
        id:              question.id.clone(),
        text:            question.text.clone(),
        question_type:   question.question_type,
        options:         question.options.clone(),
        allow_freeform:  question.allow_freeform,
        default:         None,
        timeout_seconds: question.timeout_seconds,
        stage:           question.stage.clone(),
        metadata:        HashMap::new(),
        context_display: question.context_display.clone(),
        review_target:   question.review_target.clone(),
    }
}

fn api_question_from_interview_record(question: &InterviewQuestionRecord) -> ApiQuestion {
    ApiQuestion {
        id:              question.id.clone(),
        text:            question.text.clone(),
        stage:           question.stage.clone(),
        question_type:   question.question_type,
        options:         question.options.clone(),
        allow_freeform:  question.allow_freeform,
        timeout_seconds: question.timeout_seconds,
        context_display: question.context_display.clone(),
        review_target:   question.review_target.clone(),
    }
}

fn api_question_from_pending_interview(record: &PendingInterviewRecord) -> ApiQuestion {
    api_question_from_interview_record(&record.question)
}

#[allow(
    clippy::result_large_err,
    reason = "Pending-interview lookup maps storage failures to HTTP responses."
)]
async fn load_pending_interview(
    state: &AppState,
    run_id: RunId,
    qid: &str,
) -> Result<LoadedPendingInterview, Response> {
    let projection = state
        .load_run_projection(&run_id)
        .await
        .map_err(IntoResponse::into_response)?;
    let Some(record) = projection.pending_interviews.get(qid) else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Question no longer exists or was already answered.",
        )
        .into_response());
    };

    Ok(LoadedPendingInterview {
        run_id,
        qid: qid.to_string(),
        question: record.question.clone(),
    })
}

#[allow(
    clippy::result_large_err,
    reason = "Interview answer validation returns HTTP 400 responses directly."
)]
fn validate_answer_for_question(
    question: &InterviewQuestionRecord,
    answer: &Answer,
) -> Result<(), Response> {
    match (&question.question_type, &answer.value) {
        (
            QuestionType::YesNo | QuestionType::Confirmation,
            fabro_interview::AnswerValue::Yes | fabro_interview::AnswerValue::No,
        )
        | (
            _,
            fabro_interview::AnswerValue::Interrupted
            | fabro_interview::AnswerValue::Skipped
            | fabro_interview::AnswerValue::Timeout,
        ) => Ok(()),
        (QuestionType::MultipleChoice, fabro_interview::AnswerValue::Selected(key)) => {
            if question.options.iter().any(|option| option.key == *key) {
                Ok(())
            } else {
                Err(ApiError::bad_request("Invalid option key.").into_response())
            }
        }
        (QuestionType::MultiSelect, fabro_interview::AnswerValue::MultiSelected(keys)) => {
            if keys
                .iter()
                .all(|key| question.options.iter().any(|option| option.key == *key))
            {
                Ok(())
            } else {
                Err(ApiError::bad_request("Invalid option key.").into_response())
            }
        }
        (QuestionType::Freeform, fabro_interview::AnswerValue::Text(text))
            if !text.trim().is_empty() =>
        {
            Ok(())
        }
        (_, fabro_interview::AnswerValue::Text(text))
            if question.allow_freeform && !text.trim().is_empty() =>
        {
            Ok(())
        }
        _ => Err(ApiError::bad_request("Answer does not match question type.").into_response()),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "Interview submission maps validation failures to HTTP responses."
)]
async fn submit_pending_interview_answer(
    state: &AppState,
    pending: &LoadedPendingInterview,
    submission: AnswerSubmission,
) -> Result<(), Response> {
    validate_answer_for_question(&pending.question, &submission.answer)?;
    deliver_answer_to_run(state, pending.run_id, &pending.qid, submission).await
}

#[allow(
    clippy::result_large_err,
    reason = "Interview delivery maps run-state failures to HTTP responses."
)]
async fn deliver_answer_to_run(
    state: &AppState,
    run_id: RunId,
    qid: &str,
    submission: AnswerSubmission,
) -> Result<(), Response> {
    let transport = match claim_run_answer_transport(state, run_id, qid) {
        Ok(transport) => transport,
        Err(StatusCode::NOT_FOUND) => {
            return Err(ApiError::not_found("Run not found.").into_response());
        }
        Err(StatusCode::CONFLICT) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "Question no longer exists or was already answered.",
            )
            .into_response());
        }
        Err(status) => {
            return Err(
                ApiError::new(status, "Run is not ready to accept answers.").into_response()
            );
        }
    };

    let answered = InterviewAnsweredRecord {
        question:  qid.to_string(),
        principal: Some(submission.actor.clone()),
        channel:   None,
        text:      None,
        answer:    Some(answer_text(&submission.answer)),
    };
    if let Ok(()) = transport.submit(qid, submission).await {
        // The answer reached the run; who gave it, and what, is Fabro's
        // record beside the answer Petri records.
        if let Err(err) =
            run_records::append(state, run_id, PlatformRecord::InterviewAnswered(answered)).await
        {
            warn!(run_id = %run_id, question = qid, error = %err, "the answer was delivered but not recorded");
        }
        Ok(())
    } else {
        release_run_answer_claim(state, run_id, qid);
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Failed to deliver answer to the active run.",
        )
        .into_response())
    }
}

/// An answer as text, for the record and the readers that show it.
fn answer_text(answer: &fabro_interview::Answer) -> String {
    use fabro_interview::AnswerValue;
    match &answer.value {
        AnswerValue::Yes => "yes".to_string(),
        AnswerValue::No => "no".to_string(),
        AnswerValue::Cancelled => "cancelled".to_string(),
        AnswerValue::Interrupted => "interrupted".to_string(),
        AnswerValue::Skipped => "skipped".to_string(),
        AnswerValue::Timeout => "timed out".to_string(),
        AnswerValue::Selected(key) => key.clone(),
        AnswerValue::MultiSelected(keys) => keys.join(", "),
        AnswerValue::Text(text) => text.clone(),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "Answer request parsing returns HTTP 400 responses directly."
)]
fn answer_from_request(
    req: SubmitAnswerRequest,
    question: &InterviewQuestionRecord,
) -> Result<Answer, Response> {
    match req {
        SubmitAnswerRequest::YesRequest(_) => Ok(Answer::yes()),
        SubmitAnswerRequest::NoRequest(_) => Ok(Answer::no()),
        SubmitAnswerRequest::SelectedRequest(req) => {
            let key = req.option_key;
            let option = question
                .options
                .iter()
                .find(|option| option.key == key)
                .cloned();
            match option {
                Some(option) => Ok(Answer::selected(key, option)),
                None => Err(ApiError::bad_request("Invalid option key.").into_response()),
            }
        }
        SubmitAnswerRequest::MultiSelectedRequest(req) => {
            for key in &req.option_keys {
                let valid = question.options.iter().any(|option| option.key == *key);
                if !valid {
                    return Err(ApiError::bad_request("Invalid option key.").into_response());
                }
            }
            Ok(Answer::multi_selected(req.option_keys))
        }
        SubmitAnswerRequest::TextRequest(req) => Ok(Answer::text(req.text)),
    }
}

/// Execute a single run: transitions runnable → starting → running →
/// completed/failed/cancelled.
async fn execute_run(state: Arc<AppState>, run_id: RunId) {
    if state.is_shutting_down() {
        return;
    }

    // A run executes in its worker process. Under the test override it
    // executes in this process instead, so the scenario tests need no worker
    // binary.
    if state.execute_in_process {
        Box::pin(petri_runs::execute(state, run_id)).await;
        return;
    }

    Box::pin(execute_run_subprocess(state, run_id)).await;
}

async fn execute_run_subprocess(state: Arc<AppState>, run_id: RunId) {
    let (run_dir, execution_mode) = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if state.is_shutting_down() {
            return;
        }
        let managed_run = match runs.get_mut(&run_id) {
            Some(run) if run.status == RunStatus::Runnable => run,
            _ => return,
        };
        let Some(run_dir) = managed_run.run_dir.clone() else {
            return;
        };
        managed_run.status = RunStatus::Starting;
        (run_dir, managed_run.execution_mode)
    };

    stream_follower::follow_run(&state, run_id).await;
    let run_state = match run_records::projection(&state, run_id).await {
        Ok(Some(run_state)) => run_state,
        Ok(None) => {
            tracing::error!(run_id = %run_id, "Run not found at launch");
            fail_managed_run(
                &state,
                run_id,
                FailureReason::WorkflowError,
                "Run not found at launch".to_string(),
            );
            state.scheduler_notify.notify_one();
            return;
        }
        Err(err) => {
            tracing::error!(run_id = %run_id, error = %err, "Failed to load run state");
            fail_managed_run(
                &state,
                run_id,
                FailureReason::WorkflowError,
                format!("Failed to load run state: {err}"),
            );
            state.scheduler_notify.notify_one();
            return;
        }
    };
    let agent_fabro_tools_enabled = run_state.spec.settings.run.agent.fabro_tools;
    if reject_run_if_sandbox_provider_disabled(
        &state,
        &state.server_settings(),
        run_id,
        &run_state.spec.settings.run,
    )
    .await
    {
        return;
    }

    // A Daytona run's worker hands the vault's key to Petri's Daytona
    // plugin through its own environment; any other run's worker never
    // sees it.
    let wants_daytona =
        run_state.spec.settings.run.environment.provider == SandboxProviderKind::DAYTONA;
    let secrets = async {
        let github_app_private_key = state.vault_secret(EnvVars::GITHUB_APP_PRIVATE_KEY).await?;
        let daytona_api_key = if wants_daytona {
            state.vault_secret(EnvVars::DAYTONA_API_KEY).await?
        } else {
            None
        };
        Ok::<_, SecretStoreError>((github_app_private_key, daytona_api_key))
    };
    let (github_app_private_key, daytona_api_key) = match secrets.await {
        Ok(value) => value,
        Err(err) => {
            fail_run_before_execution(
                &state,
                run_id,
                FailureReason::WorkflowError,
                "Loading worker secrets failed".to_string(),
            )
            .await;
            tracing::error!(run_id = %run_id, error = ?err, "Loading worker secrets failed");
            return;
        }
    };
    let state_for_build = Arc::clone(&state);
    let run_dir_for_build = run_dir.clone();
    let start_result = spawn_blocking(move || {
        worker_launch_spec(
            state_for_build.as_ref(),
            run_id,
            execution_mode,
            &run_dir_for_build,
            agent_fabro_tools_enabled,
            github_app_private_key,
            daytona_api_key,
        )
    })
    .await
    .context("worker_launch_spec task failed")
    .and_then(|inner| inner);

    let launch_result = match start_result {
        Ok(spec) => state.worker_runtime.start(spec).await,
        Err(err) => Err(err),
    };
    let started_worker = match launch_result {
        Ok(worker) => worker,
        Err(err) => {
            fail_worker_launch(&state, run_id, err).await;
            return;
        }
    };
    let worker_ref = started_worker.worker_ref.clone();

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&run_id) {
            managed_run.worker_ref = Some(worker_ref.clone());
            managed_run.run_dir = Some(run_dir.clone());
            managed_run.answer_transport = Some(RunAnswerTransport::Worker {
                run_id,
                bus: Arc::clone(&state.worker_control_bus),
                acks: Arc::clone(&state.worker_control_acks),
            });
        }
    }

    let stderr_task = tokio::spawn(drain_worker_stderr(run_id, started_worker.stderr));

    let worker_exit = match started_worker.wait.await {
        Ok(exit) => exit,
        Err(err) => {
            tracing::error!(run_id = %run_id, error = %err, "Failed while waiting on worker");
            let message = format!("Worker wait failed: {err}");
            state.worker_runtime.force_stop(&worker_ref).await;
            let _ = run_records::lifecycle(
                &state,
                run_id,
                run_records::failed(FailureReason::Terminated, message.clone()),
            )
            .await;
            fail_managed_run(&state, run_id, FailureReason::Terminated, message);
            state.scheduler_notify.notify_one();
            return;
        }
    };

    match stderr_task.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!(run_id = %run_id, error = %err, "Worker stderr drain failed");
        }
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Worker stderr task panicked");
        }
    }

    let superseded = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        runs.get(&run_id)
            .is_some_and(|managed_run| managed_run.worker_ref.as_ref() != Some(&worker_ref))
    };
    if superseded {
        tracing::info!(
            run_id = %run_id,
            worker_ref = ?worker_ref,
            "Skipping stale worker cleanup for superseded run execution"
        );
        return;
    }

    // The worker is gone: whatever Petri run handles it held open over the
    // API drop here, so its lease never outlives it.
    state.petri_runs.worker_exited(run_id);
    state.petri_projector.signal(run_id);
    append_worker_exit_failure(&state, run_id, &worker_exit).await;

    let final_state = match run_records::projection(&state, run_id).await {
        Ok(Some(final_state)) => final_state,
        Ok(None) => {
            tracing::warn!(run_id = %run_id, "The run's final state is missing from the store");
            fail_managed_run(
                &state,
                run_id,
                FailureReason::WorkflowError,
                "The run's final state is missing from the store".to_string(),
            );
            state.scheduler_notify.notify_one();
            return;
        }
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Failed to load final run state from store");
            fail_managed_run(
                &state,
                run_id,
                FailureReason::WorkflowError,
                format!("Failed to load final run state: {err}"),
            );
            state.scheduler_notify.notify_one();
            return;
        }
    };

    accumulate_concluded_run_usage(&state, &final_state);

    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        if final_state.status != managed_run.status {
            managed_run.status = final_state.status;
        } else if !worker_exit.success {
            managed_run.status = RunStatus::Failed {
                reason: FailureReason::Terminated,
            };
        }
        managed_run.error = final_state
            .conclusion
            .as_ref()
            .and_then(|conclusion| {
                conclusion.failure.as_ref().map(|failure| {
                    render_compact_with_causes(&failure.detail.message, &failure.detail.causes)
                })
            })
            .or_else(|| managed_run.error.clone());
        managed_run.run_dir = Some(run_dir);
        clear_live_run_state(managed_run);
    }
    drop(runs);
    state.scheduler_notify.notify_one();
}

/// Background task that promotes runnable runs when capacity is available.
pub fn spawn_scheduler(state: Arc<AppState>) {
    stream_follower::spawn_stream_follower(Arc::clone(&state));
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = state.scheduler_notify.notified() => {},
                () = sleep(std::time::Duration::from_secs(1)) => {},
            }
            if state.is_shutting_down() {
                break;
            }
            let runs_to_start = {
                let runs = state.runs.lock().expect("runs lock poisoned");
                let active = runs
                    .values()
                    .filter(|r| counts_toward_scheduler_capacity(r.status))
                    .count();
                let available = state.max_concurrent_runs.saturating_sub(active);
                if available == 0 {
                    Vec::new()
                } else {
                    let mut runnable: Vec<_> = runs
                        .iter()
                        .filter(|(_, r)| r.status == RunStatus::Runnable)
                        .map(|(id, r)| (*id, r.created_at))
                        .collect();
                    runnable.sort_by_key(|(_, created_at)| *created_at);
                    runnable
                        .into_iter()
                        .take(available)
                        .map(|(id, _)| id)
                        .collect::<Vec<_>>()
                }
            };
            for id in runs_to_start {
                if state.is_shutting_down() {
                    break;
                }
                let state_clone = Arc::clone(&state);
                tokio::spawn(
                    execute_run(state_clone, id).instrument(tracing::info_span!("run", id = %id)),
                );
            }
        }
    });
}

async fn append_control_request(
    state: &AppState,
    run_id: RunId,
    action: RunControlAction,
    actor: Option<Principal>,
) -> anyhow::Result<()> {
    let _ = actor;
    let kind = match action {
        RunControlAction::Cancel => RunLifecycleKind::CancelRequested,
        RunControlAction::Pause => RunLifecycleKind::PauseRequested,
        RunControlAction::Unpause => RunLifecycleKind::UnpauseRequested,
    };
    // The check and the append are one step under the run's control lock,
    // so two concurrent requests for the same control record it once.
    let _guard = state.control_request_locks.lock(run_id).await;
    if action == RunControlAction::Cancel {
        // A cancel already pending is not asked for twice.
        let pending = run_records::projection(state, run_id)
            .await?
            .and_then(|projection| projection.pending_control);
        if pending == Some(RunControlAction::Cancel) {
            return Ok(());
        }
    }
    let mut record = RunLifecycleRecord::new(kind);
    record.action = Some(action);
    run_records::lifecycle(state, run_id, record)
        .await
        .map(|_| ())
}

/// Returns a 409 response with an actionable "unarchive first" message if the
/// run is currently archived. Returns `None` otherwise (including when the run
/// doesn't exist — the caller's own not-found handling will surface that).
async fn reject_if_archived(state: &AppState, run_id: &RunId) -> Option<Response> {
    let summary = state
        .stores
        .run_summaries
        .get(run_id, Utc::now())
        .await
        .ok()
        .flatten()?;
    summary.lifecycle.archived_at.is_some().then(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            operations::archived_rejection_message(run_id),
        )
        .into_response()
    })
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "server unit tests stage fixtures with sync std::fs writes"
)]
mod tests;
