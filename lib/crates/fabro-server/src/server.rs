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
pub use fabro_api::types::{
    AggregateBilling, AggregateBillingTotals, ApiQuestion, ApiQuestionOption, AppendEventResponse,
    ArtifactEntry, ArtifactListResponse, BillingByModel, BillingStageRef,
    CloseRunPullRequestResponse, CompletionContentPart, CompletionMessage, CompletionMessageRole,
    CompletionResponse, CompletionToolChoiceMode, CompletionUsage, CreateCompletionRequest,
    CreateRunPullRequestRequest, CreateSecretRequest, DeleteRunResponse, DeleteRunSandbox,
    DeleteSecretRequest, DiskUsageResponse, DiskUsageRunRow, DiskUsageSummaryRow, ForkRequest,
    ForkResponse, LinkRunPullRequestRequest, MergeRunPullRequestRequest,
    MergeRunPullRequestResponse, ModelReference, PaginatedEventList, PaginatedRunList,
    PaginationMeta, PreflightResponse, PreviewUrlRequest, PreviewUrlResponse, Provider,
    ProviderList, PruneRunEntry, PruneRunsRequest, PruneRunsResponse, RenderWorkflowGraphDirection,
    RenderWorkflowGraphRequest, RewindRequest, RewindResponse, RunArtifactEntry,
    RunArtifactListResponse, RunBilling, RunBillingStage, RunBillingTotals, RunError, RunManifest,
    RunStage, SandboxDetails, SandboxFileEntry, SandboxFileListResponse, SandboxService,
    SandboxServiceListResponse, SshAccessRequest, SshAccessResponse, StageHandler, StageState,
    StartRunRequest, SubmitAnswerRequest, SystemCpuResourceScope, SystemCpuResources,
    SystemDiskResourceScope, SystemDiskResources, SystemFeatures, SystemInfoResponse,
    SystemMemoryResourceScope, SystemMemoryResources, SystemRepairRunIssue,
    SystemRepairRunsResponse, SystemResourcesResponse, SystemRunCounts, TimelineEntryResponse,
    VncPreviewResponse, WriteBlobResponse,
};
use fabro_auth::{CredentialSource, VaultCredentialSource, auth_issue_message};
#[cfg(test)]
use fabro_config::RunSettingsBuilder;
use fabro_config::daemon::ServerDaemon;
use fabro_config::{RunLayer, Storage};
use fabro_interview::{
    Answer, AnswerSubmission, ControlInterviewer, Interviewer, Question, WorkerControlEnvelope,
};
use fabro_llm::client::Client as LlmClient;
use fabro_llm::generate::{GenerateParams, generate_object};
use fabro_llm::model_test::run_model_test;
use fabro_llm::types::{
    ContentPart, FinishReason, Message as LlmMessage, Request as LlmRequest, Role, ToolChoice,
    ToolDefinition,
};
use fabro_model::catalog::LlmCatalogSettings;
use fabro_model::{BilledTokenCounts, Catalog, ModelRef, ModelTestMode, ProviderId};
use fabro_redact::redact_jsonl_line;
use fabro_sandbox::daytona::{self, DaytonaSandbox};
use fabro_sandbox::details::sandbox_details;
use fabro_sandbox::reconnect::reconnect_for_run;
use fabro_sandbox::{Sandbox, SandboxProvider};
use fabro_slack::client::{PostedMessage as SlackPostedMessage, SlackClient};
use fabro_slack::config::resolve_credentials as resolve_slack_credentials;
use fabro_slack::payload::SlackAnswerSubmission;
use fabro_slack::threads::ThreadRegistry;
use fabro_slack::{blocks as slack_blocks, connection as slack_connection};
use fabro_static::EnvVars;
use fabro_store::{
    ArtifactKey, ArtifactStore, Database, EventEnvelope, EventPayload, NodeArtifact,
    PendingInterviewRecord, StageArtifactEntry, StageId,
};
#[cfg(test)]
use fabro_types::BlockedReason;
use fabro_types::settings::run::RunMode;
use fabro_types::settings::server::{
    GithubIntegrationSettings, GithubIntegrationStrategy, LogDestination,
};
use fabro_types::settings::{InterpString, RunNamespace};
use fabro_types::{
    AgentBackend, AskFabro, AskFabroUnavailableReason, EventBody, InterviewQuestionRecord, PairId,
    PairMessageId, PairTarget, Principal, PullRequestLink, QuestionType, RunBlobId,
    RunControlAction, RunEvent, RunId, ServerSettings, SessionCapability,
};
use fabro_util::error::{
    SharedError, collect_causes, render_compact_with_causes, render_with_causes,
};
use fabro_util::version::FABRO_VERSION;
use fabro_vault::{Error as VaultError, SecretType, Vault};
use fabro_workflow::artifact_upload::ArtifactSink;
#[cfg(test)]
use fabro_workflow::command_log::command_log_path;
use fabro_workflow::event::{self as workflow_event, Emitter};
use fabro_workflow::handler::HandlerRegistry;
use fabro_workflow::pipeline::Persisted;
use fabro_workflow::records::Checkpoint;
use fabro_workflow::run_lookup::{
    RunInfo, StatusFilter, filter_runs, scan_runs_with_summaries, scratch_base,
};
use fabro_workflow::run_status::{FailureReason, RunStatus, SuccessReason};
use fabro_workflow::{Error as WorkflowError, operations, pull_request};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdin, Command};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{
    Mutex as AsyncMutex, Notify, OwnedMutexGuard, RwLock as AsyncRwLock, Semaphore, broadcast,
    mpsc, oneshot,
};
use tokio::task::spawn_blocking;
use tokio::time::{sleep, timeout};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::{BroadcastStream, UnboundedReceiverStream};
use tokio_util::sync::CancellationToken;
use tower::{ServiceExt, service_fn};
use tracing::{Instrument, debug, error, info, warn};
use ulid::Ulid;

use crate::auth::{self, GithubEndpoints, auth_translation_middleware, demo_routing_middleware};
use crate::canonical_origin::resolve_canonical_origin;
use crate::error::ApiError;
use crate::github_webhooks::{
    WEBHOOK_ROUTE, WEBHOOK_SECRET_ENV, parse_event_metadata, verify_signature,
};
use crate::ip_allowlist::{IpAllowlistConfig, ip_allowlist_middleware};
use crate::jwt_auth::{self, AuthMode};
use crate::principal_middleware::{
    AuthContextSlot, RequestAuth, RequestAuthContext, RequireRunBlob, RequireRunScoped,
    RequireRunScopedOrRunTools, RequireRunStageScoped, RequireStageArtifact, RequiredUser,
    principal_middleware,
};
use crate::request_id::{self, RequestId};
use crate::run_files::{FilesInFlight, new_files_in_flight};
use crate::server_secrets::{LlmClientResult, ServerSecrets};
use crate::spawn_env::{apply_render_graph_env, apply_worker_env};
use crate::worker_token::{WorkerScopeSet, WorkerTokenKeys, issue_worker_token_with_scopes};
use crate::{
    canonical_host, demo, diagnostics, run_manifest, security_headers, static_files,
    vault_legacy_migration, web_auth,
};

mod handler;
mod resource_sampler;
mod session_runtime;

pub(crate) use handler::events::EventListParams;
#[cfg(test)]
pub(in crate::server) use handler::events::filtered_global_events;
pub(crate) use handler::graph::render_graph_bytes;
#[cfg(test)]
pub(in crate::server) use handler::graph::{
    RenderSubprocessError, render_dot_subprocess, render_graph_bytes_with_exe_override,
};
#[cfg(test)]
pub(in crate::server) use handler::system::validate_github_slug;
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

pub(crate) fn paginate_items<T>(items: Vec<T>, pagination: &PaginationParams) -> (Vec<T>, bool) {
    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset.min(MAX_PAGE_OFFSET) as usize;
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

/// Non-paginated list response wrapper with `has_more: false`.
#[derive(serde::Serialize)]
pub struct ListResponse<T: serde::Serialize> {
    data: T,
    meta: PaginationMeta,
}

impl<T: serde::Serialize> ListResponse<T> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            meta: PaginationMeta { has_more: false },
        }
    }
}

/// Snapshot of a managed run.
struct ManagedRun {
    dot_source: String,
    status: RunStatus,
    error: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    enqueued_at: Instant,
    // Populated when running:
    answer_transport: Option<RunAnswerTransport>,
    accepted_questions: HashSet<String>,
    /// Stage IDs of currently steerable live agent sessions, keyed to the
    /// session id that owns the active lease. Used by the steerability
    /// predicate for steer/interrupt controls.
    active_steerable_stages: HashMap<StageId, String>,
    /// API-mode session targets eligible for live pair control. ACP sessions
    /// can be steerable but are intentionally excluded from pairing.
    active_api_targets: HashMap<StageId, PairTarget>,
    /// Stage IDs of currently running agent sessions that have no live
    /// steering capability, keyed to the session id that owns the marker.
    active_non_steerable_stages: HashMap<StageId, String>,
    event_tx: Option<broadcast::Sender<RunEvent>>,
    checkpoint: Option<Checkpoint>,
    cancel_tx: Option<oneshot::Sender<()>>,
    cancel_token: Option<CancellationToken>,
    worker_pid: Option<u32>,
    worker_pgid: Option<u32>,
    run_dir: Option<std::path::PathBuf>,
    execution_mode: RunExecutionMode,
}

#[derive(Clone, Copy)]
enum RunExecutionMode {
    Start,
    Resume,
}

enum ExecutionResult {
    Completed(Box<Result<operations::Started, WorkflowError>>),
    CancelledBySignal,
}

const WORKER_CANCEL_GRACE: Duration = Duration::from_secs(5);
const TERMINAL_DELETE_WORKER_GRACE: Duration = Duration::from_millis(50);
const WORKER_CONTROL_QUEUE_CAPACITY: usize = 8;
const WORKER_CONTROL_ENQUEUE_TIMEOUT: Duration = Duration::from_secs(1);
/// Per-model billing totals.
#[derive(Default)]
struct ModelBillingTotals {
    stages:  i64,
    billing: BilledTokenCounts,
}

/// In-memory aggregate billing counters, reset on server restart.
#[derive(Default)]
struct BillingAccumulator {
    total_runs:   i64,
    total_timing: fabro_types::RunTiming,
    by_model:     HashMap<ModelRef, ModelBillingTotals>,
}

pub(crate) type RegistryFactoryOverride =
    dyn Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync;

#[derive(Clone)]
enum RunAnswerTransport {
    Subprocess {
        control_tx: mpsc::Sender<WorkerControlEnvelope>,
    },
    InProcess {
        interviewer:  Arc<ControlInterviewer>,
        steering_hub: Arc<fabro_workflow::SteeringHub>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerTransportError {
    Closed,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairTransportError {
    Closed,
    Timeout,
    Control(fabro_workflow::PairControlError),
}

impl RunAnswerTransport {
    async fn submit(
        &self,
        qid: &str,
        submission: AnswerSubmission,
    ) -> Result<(), AnswerTransportError> {
        match self {
            Self::Subprocess { control_tx } => {
                let message = WorkerControlEnvelope::interview_answer(qid.to_string(), submission);
                timeout(WORKER_CONTROL_ENQUEUE_TIMEOUT, control_tx.send(message))
                    .await
                    .map_err(|_| AnswerTransportError::Timeout)?
                    .map_err(|_| AnswerTransportError::Closed)
            }
            Self::InProcess { interviewer, .. } => interviewer
                .submit(qid, submission)
                .await
                .map_err(|_| AnswerTransportError::Closed),
        }
    }

    async fn cancel_run(&self) -> Result<(), AnswerTransportError> {
        match self {
            Self::Subprocess { control_tx } => {
                let message = WorkerControlEnvelope::cancel_run();
                timeout(WORKER_CONTROL_ENQUEUE_TIMEOUT, control_tx.send(message))
                    .await
                    .map_err(|_| AnswerTransportError::Timeout)?
                    .map_err(|_| AnswerTransportError::Closed)
            }
            Self::InProcess { interviewer, .. } => {
                interviewer.cancel_all().await;
                Ok(())
            }
        }
    }

    /// Forward a steer to the worker (subprocess) or directly into the
    /// in-process steering hub.
    async fn steer(&self, text: String, actor: Principal) -> Result<(), AnswerTransportError> {
        match self {
            Self::Subprocess { control_tx } => {
                let message = WorkerControlEnvelope::steer(text, actor);
                timeout(WORKER_CONTROL_ENQUEUE_TIMEOUT, control_tx.send(message))
                    .await
                    .map_err(|_| AnswerTransportError::Timeout)?
                    .map_err(|_| AnswerTransportError::Closed)
            }
            Self::InProcess { steering_hub, .. } => {
                steering_hub.deliver_steer(text, Some(actor));
                Ok(())
            }
        }
    }

    async fn interrupt(&self, actor: Principal) -> Result<(), AnswerTransportError> {
        match self {
            Self::Subprocess { control_tx } => {
                let message = WorkerControlEnvelope::interrupt(actor);
                timeout(WORKER_CONTROL_ENQUEUE_TIMEOUT, control_tx.send(message))
                    .await
                    .map_err(|_| AnswerTransportError::Timeout)?
                    .map_err(|_| AnswerTransportError::Closed)
            }
            Self::InProcess { steering_hub, .. } => {
                steering_hub.interrupt(Some(&actor));
                Ok(())
            }
        }
    }

    async fn interrupt_then_steer(
        &self,
        text: String,
        actor: Principal,
    ) -> Result<(), AnswerTransportError> {
        match self {
            Self::Subprocess { control_tx } => {
                let message = WorkerControlEnvelope::interrupt_then_steer(text, actor);
                timeout(WORKER_CONTROL_ENQUEUE_TIMEOUT, control_tx.send(message))
                    .await
                    .map_err(|_| AnswerTransportError::Timeout)?
                    .map_err(|_| AnswerTransportError::Closed)
            }
            Self::InProcess { steering_hub, .. } => {
                steering_hub.interrupt_then_steer(&text, Some(&actor));
                Ok(())
            }
        }
    }

    async fn start_pair(
        &self,
        run_id: RunId,
        pair_id: PairId,
        target: PairTarget,
        actor: Principal,
    ) -> Result<(), PairTransportError> {
        match self {
            Self::Subprocess { control_tx } => {
                let message = WorkerControlEnvelope::start_pair(run_id, pair_id, target, actor);
                timeout(WORKER_CONTROL_ENQUEUE_TIMEOUT, control_tx.send(message))
                    .await
                    .map_err(|_| PairTransportError::Timeout)?
                    .map_err(|_| PairTransportError::Closed)
            }
            Self::InProcess { steering_hub, .. } => steering_hub
                .start_pair(run_id, pair_id, target, Some(actor))
                .map(|_| ())
                .map_err(PairTransportError::Control),
        }
    }

    async fn send_pair_message(
        &self,
        pair_id: PairId,
        message_id: PairMessageId,
        text: String,
        client_message_id: Option<String>,
        actor: Principal,
    ) -> Result<(), PairTransportError> {
        match self {
            Self::Subprocess { control_tx } => {
                let message = WorkerControlEnvelope::pair_message(
                    pair_id,
                    message_id,
                    text.clone(),
                    client_message_id.clone(),
                    actor,
                );
                timeout(WORKER_CONTROL_ENQUEUE_TIMEOUT, control_tx.send(message))
                    .await
                    .map_err(|_| PairTransportError::Timeout)?
                    .map_err(|_| PairTransportError::Closed)
            }
            Self::InProcess { steering_hub, .. } => steering_hub
                .send_pair_message(pair_id, message_id, text, client_message_id, Some(actor))
                .map(|_| ())
                .map_err(PairTransportError::Control),
        }
    }

    async fn end_pair(&self, pair_id: PairId, actor: Principal) -> Result<(), PairTransportError> {
        match self {
            Self::Subprocess { control_tx } => {
                let message = WorkerControlEnvelope::end_pair(pair_id, actor);
                timeout(WORKER_CONTROL_ENQUEUE_TIMEOUT, control_tx.send(message))
                    .await
                    .map_err(|_| PairTransportError::Timeout)?
                    .map_err(|_| PairTransportError::Closed)
            }
            Self::InProcess { steering_hub, .. } => steering_hub
                .end_pair(pair_id, Some(actor))
                .map(|_| ())
                .map_err(PairTransportError::Control),
        }
    }
}

#[derive(Debug, Clone)]
struct LoadedPendingInterview {
    run_id:   RunId,
    qid:      String,
    question: InterviewQuestionRecord,
}

#[derive(Clone)]
struct SlackService {
    client:          SlackClient,
    app_token:       String,
    default_channel: String,
    posted_messages: Arc<Mutex<HashMap<(RunId, String), SlackPostedMessage>>>,
    thread_registry: Arc<ThreadRegistry>,
}

impl SlackService {
    fn new(bot_token: String, app_token: String, default_channel: String) -> Self {
        Self {
            client: SlackClient::new(bot_token),
            app_token,
            default_channel,
            posted_messages: Arc::new(Mutex::new(HashMap::new())),
            thread_registry: Arc::new(ThreadRegistry::new()),
        }
    }

    async fn handle_event(&self, event: &RunEvent, run_web_url: Option<&str>) {
        match &event.body {
            EventBody::InterviewStarted(props) => {
                if props.question_id.is_empty() {
                    return;
                }
                let key = (event.run_id, props.question_id.clone());
                if self
                    .posted_messages
                    .lock()
                    .expect("slack posted messages lock poisoned")
                    .contains_key(&key)
                {
                    return;
                }

                let question = runtime_question_from_interview_record(&InterviewQuestionRecord {
                    id:              props.question_id.clone(),
                    text:            props.question.clone(),
                    stage:           props.stage.clone(),
                    question_type:   props.question_type.parse().unwrap_or_default(),
                    options:         props.options.clone(),
                    allow_freeform:  props.allow_freeform,
                    timeout_seconds: props.timeout_seconds,
                    context_display: props.context_display.clone(),
                });
                let blocks = slack_blocks::question_to_blocks(
                    &event.run_id.to_string(),
                    &props.question_id,
                    &question,
                    run_web_url,
                );

                if let Ok(posted) = self
                    .client
                    .post_message(&self.default_channel, &blocks, None)
                    .await
                {
                    if question.allow_freeform || question.question_type == QuestionType::Freeform {
                        self.thread_registry.register(
                            &posted.ts,
                            &event.run_id.to_string(),
                            &props.question_id,
                        );
                    }
                    self.posted_messages
                        .lock()
                        .expect("slack posted messages lock poisoned")
                        .insert(key, posted);
                }
            }
            EventBody::InterviewCompleted(props) => {
                self.finish_interview(
                    event.run_id,
                    &props.question_id,
                    &props.question,
                    &props.answer,
                )
                .await;
            }
            EventBody::InterviewTimeout(props) => {
                self.finish_interview(
                    event.run_id,
                    &props.question_id,
                    &props.question,
                    "Timed out",
                )
                .await;
            }
            EventBody::InterviewInterrupted(props) => {
                self.finish_interview(
                    event.run_id,
                    &props.question_id,
                    &props.question,
                    "Interrupted",
                )
                .await;
            }
            _ => {}
        }
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

        self.thread_registry.remove(&posted.ts);
        let blocks = slack_blocks::answered_blocks(question_text, answer_text);
        let _ = self
            .client
            .update_message(&posted.channel_id, &posted.ts, &blocks)
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

/// Shared application state for the server.
pub struct AppState {
    runs: Mutex<HashMap<RunId, ManagedRun>>,
    aggregate_billing: Mutex<BillingAccumulator>,
    store: Arc<Database>,
    session_runtimes: SessionRuntimeManager,
    artifact_store: ArtifactStore,
    worker_tokens: WorkerTokenKeys,
    started_at: Instant,
    resource_sampler: resource_sampler::ResourceSampler,
    max_concurrent_runs: usize,
    scheduler_notify: Notify,
    global_event_tx: broadcast::Sender<EventEnvelope>,
    /// Per-run coalescing registry for `GET /runs/{id}/files`. Concurrent
    /// callers for the same run share one materialization; different runs
    /// proceed in parallel. See `crate::run_files` for semantics.
    pub(crate) files_in_flight: FilesInFlight,
    pull_request_create_locks: PullRequestCreateLocks,
    parent_link_lock: AsyncMutex<()>,

    pub(crate) vault: Arc<AsyncRwLock<Vault>>,
    pub(super) server_secrets: ServerSecrets,
    pub(crate) llm_source: Arc<dyn CredentialSource>,
    manifest_run_defaults: RwLock<Arc<RunLayer>>,
    manifest_run_settings: RwLock<std::result::Result<RunNamespace, SharedError>>,
    pub(crate) server_settings: RwLock<Arc<ServerSettings>>,
    catalog: RwLock<Arc<Catalog>>,
    pub(crate) env_lookup: EnvLookup,
    pub(crate) github_api_base_url: String,
    active_config_path: PathBuf,
    http_client: Option<fabro_http::HttpClient>,
    shutdown: CancellationToken,
    shutting_down: AtomicBool,
    registry_factory_override: Option<Box<RegistryFactoryOverride>>,
    slack_service: Option<Arc<SlackService>>,
    slack_started: AtomicBool,
}

type PullRequestCreateLocks = Arc<Mutex<HashMap<RunId, Arc<AsyncMutex<()>>>>>;

struct AskFabroReadiness {
    feature_enabled: bool,
    default_model:   Option<String>,
}

impl AskFabroReadiness {
    fn decorate(&self, mut run: fabro_types::Run) -> fabro_types::Run {
        run.ask_fabro = self.ask_fabro_for(&run);
        run
    }

    fn ask_fabro_for(&self, run: &fabro_types::Run) -> AskFabro {
        let unavailable_reason = if !self.feature_enabled {
            Some(AskFabroUnavailableReason::FeatureDisabled)
        } else if run.sandbox.is_none() {
            Some(AskFabroUnavailableReason::NoSandbox)
        } else if run
            .sandbox
            .as_ref()
            .and_then(|sandbox| sandbox.runtime.as_ref())
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

struct PullRequestCreateGuard {
    locks:  PullRequestCreateLocks,
    run_id: RunId,
    mutex:  Arc<AsyncMutex<()>>,
    guard:  Option<OwnedMutexGuard<()>>,
}

impl Drop for PullRequestCreateGuard {
    fn drop(&mut self) {
        self.guard.take();

        let mut locks = self
            .locks
            .lock()
            .expect("pull request create locks poisoned");
        if locks.get(&self.run_id).is_some_and(|mutex| {
            Arc::ptr_eq(mutex, &self.mutex) && Arc::strong_count(&self.mutex) == 2
        }) {
            locks.remove(&self.run_id);
        }
    }
}

async fn lock_pull_request_create(
    locks: &PullRequestCreateLocks,
    run_id: &RunId,
) -> PullRequestCreateGuard {
    let mutex = {
        let mut locks = locks.lock().expect("pull request create locks poisoned");
        Arc::clone(
            locks
                .entry(*run_id)
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    };
    let guard = mutex.clone().lock_owned().await;
    PullRequestCreateGuard {
        locks: Arc::clone(locks),
        run_id: *run_id,
        mutex,
        guard: Some(guard),
    }
}

pub(crate) struct AppStateConfig {
    pub(crate) resolved_settings:         ResolvedAppStateSettings,
    pub(crate) registry_factory_override: Option<Box<RegistryFactoryOverride>>,
    pub(crate) max_concurrent_runs:       usize,
    pub(crate) store:                     Arc<Database>,
    pub(crate) artifact_store:            ArtifactStore,
    pub(crate) vault_path:                PathBuf,
    pub(crate) server_secrets:            ServerSecrets,
    pub(crate) env_lookup:                EnvLookup,
    pub(crate) github_api_base_url:       Option<String>,
    pub(crate) active_config_path:        PathBuf,
    pub(crate) http_client:               Option<fabro_http::HttpClient>,
    pub(crate) shutdown:                  CancellationToken,
}

#[derive(Clone)]
pub(crate) struct ResolvedAppStateSettings {
    pub(crate) server_settings:       ServerSettings,
    pub(crate) manifest_run_defaults: RunLayer,
    pub(crate) manifest_run_settings: std::result::Result<RunNamespace, SharedError>,
    pub(crate) llm_catalog_settings:  LlmCatalogSettings,
}

fn accumulate_billing_rollup(
    accumulator: &mut BillingAccumulator,
    rollup: &fabro_workflow::ProjectionBillingRollup,
) {
    accumulator.total_runs += 1;
    accumulator.total_timing = accumulator.total_timing.saturating_add(&rollup.timing);
    for model in &rollup.by_model {
        let entry = accumulator.by_model.entry(model.model.clone()).or_default();
        entry.stages += model.stages;
        entry.billing.add_counts(&model.billing);
    }
}

pub(crate) fn run_stage_from_stage_id(
    stage_id: &StageId,
    name: impl Into<String>,
    status: StageState,
    wall_time_ms: Option<u64>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    handler: StageHandler,
) -> RunStage {
    RunStage {
        id: stage_id.to_string(),
        name: name.into(),
        handler,
        status,
        wall_time_ms,
        node_id: stage_id.node_id().to_string(),
        visit: std::num::NonZeroU32::new(stage_id.visit())
            .expect("StageId stores a non-zero visit"),
        started_at,
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

    fn http_client(&self) -> Result<fabro_http::HttpClient, fabro_http::HttpClientBuildError> {
        match &self.http_client {
            Some(client) => Ok(client.clone()),
            None => fabro_http::http_client(),
        }
    }

    pub(crate) fn server_storage_dir(&self) -> PathBuf {
        PathBuf::from(
            resolve_interp_string(&self.server_settings().server.storage.root)
                .expect("server storage root should be resolved at startup"),
        )
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

    pub(crate) async fn resolve_llm_client(&self) -> anyhow::Result<LlmClientResult> {
        resolve_llm_client_from_source(self.llm_source.as_ref(), self.catalog()).await
    }

    pub(crate) async fn configured_llm_provider_ids(&self) -> Vec<ProviderId> {
        let catalog = self.catalog();
        self.llm_source.configured_providers(catalog.as_ref()).await
    }

    pub(crate) async fn ready_llm_provider_ids(&self) -> Vec<ProviderId> {
        match self.resolve_llm_client().await {
            Ok(result) => result.provider_ids(),
            Err(err) => {
                warn!(error = ?err, "Failed to resolve LLM client while checking ready providers");
                Vec::new()
            }
        }
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

    async fn ask_fabro_readiness(&self) -> AskFabroReadiness {
        let feature_enabled = self.server_settings().features.session_sandboxes;
        if !feature_enabled {
            return AskFabroReadiness {
                feature_enabled,
                default_model: None,
            };
        }

        let provider_ids = self.ready_llm_provider_ids().await;
        let default_model = if provider_ids.is_empty() {
            None
        } else {
            Some(
                self.catalog()
                    .default_for_configured_ids(&provider_ids)
                    .id
                    .clone(),
            )
        };
        AskFabroReadiness {
            feature_enabled,
            default_model,
        }
    }

    pub(crate) fn vault_or_env(&self, name: &str) -> Option<String> {
        process_env_var(name).or_else(|| {
            self.vault
                .try_read()
                .ok()
                .and_then(|vault| vault.get(name).map(str::to_string))
        })
    }

    fn env_lookup_or_vault_or_env(&self, name: &str) -> Option<String> {
        (self.env_lookup)(name).or_else(|| self.vault_or_env(name))
    }

    pub(crate) async fn check_daytona_api_key(
        &self,
        api_key: String,
    ) -> anyhow::Result<daytona::DaytonaKeyCheck> {
        let base_url = self
            .env_lookup_or_vault_or_env(EnvVars::DAYTONA_API_URL)
            .or_else(|| self.env_lookup_or_vault_or_env(EnvVars::DAYTONA_SERVER_URL))
            .unwrap_or_else(|| daytona::DEFAULT_DAYTONA_API_URL.to_string());
        let org_id = self.env_lookup_or_vault_or_env(EnvVars::DAYTONA_ORGANIZATION_ID);

        let http_client = fabro_http::http_client().context("failed to build HTTP client")?;
        daytona::check_daytona_api_key_with(&base_url, org_id.as_deref(), api_key, http_client)
            .await
    }

    /// Public accessor used by `run_files` — mirrors `vault_or_env` without
    /// changing its visibility semantics.
    pub(crate) fn vault_or_env_pub(&self, name: &str) -> Option<String> {
        self.vault_or_env(name)
    }

    /// Borrow the persistent store so sibling modules can open run readers
    /// without cross-module state coupling on the `AppState` field layout.
    pub(crate) fn store_ref(&self) -> &Arc<Database> {
        &self.store
    }

    pub(crate) fn session_runtimes(&self) -> &SessionRuntimeManager {
        &self.session_runtimes
    }

    pub(crate) fn server_secret(&self, name: &str) -> Option<String> {
        self.server_secrets.get(name)
    }

    pub(crate) fn worker_token_keys(&self) -> &WorkerTokenKeys {
        &self.worker_tokens
    }

    pub(crate) fn resolve_interp(&self, value: &InterpString) -> anyhow::Result<String> {
        value
            .resolve(|name| (self.env_lookup)(name))
            .map(|resolved| resolved.value)
            .map_err(anyhow::Error::from)
    }

    pub(crate) fn canonical_origin(&self) -> Result<String, String> {
        resolve_canonical_origin(&self.server_settings().server, &self.env_lookup)
    }

    pub(crate) fn session_key(&self) -> Option<Key> {
        self.server_secret(EnvVars::SESSION_SECRET)
            .and_then(|value| auth::derive_cookie_key(value.as_bytes()).ok())
    }

    pub(crate) fn github_credentials(
        &self,
        settings: &GithubIntegrationSettings,
    ) -> Result<Option<fabro_github::GitHubCredentials>, String> {
        match settings.strategy {
            GithubIntegrationStrategy::App => {
                let Some(app_id) = settings.app_id.as_ref().map(InterpString::as_source) else {
                    return Ok(None);
                };
                let raw = self.server_secret(EnvVars::GITHUB_APP_PRIVATE_KEY);
                let Some(raw) = raw else {
                    return Ok(None);
                };
                let private_key_pem = decode_secret_pem(EnvVars::GITHUB_APP_PRIVATE_KEY, &raw)?;
                Ok(Some(fabro_github::GitHubCredentials::App(
                    fabro_github::GitHubAppCredentials {
                        app_id,
                        private_key_pem,
                        slug: settings.slug.as_ref().map(InterpString::as_source),
                    },
                )))
            }
            GithubIntegrationStrategy::Token => {
                let token = self
                    .vault_or_env(EnvVars::GITHUB_TOKEN)
                    .or_else(|| self.vault_or_env(EnvVars::GH_TOKEN))
                    .as_deref()
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_string);
                match token {
                    Some(token) => {
                        fabro_github::validate_static_github_token(&token)
                            .map_err(|err| err.to_string())?;
                        Ok(Some(fabro_github::GitHubCredentials::Pat(token)))
                    }
                    None => Err(
                        "GITHUB_TOKEN not configured — run fabro install or set GITHUB_TOKEN"
                            .to_string(),
                    ),
                }
            }
        }
    }

    fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Relaxed);
        self.scheduler_notify.notify_waiters();
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
            manifest_run_settings,
            llm_catalog_settings,
        } = resolved_settings;
        let server_settings = Arc::new(server_settings);
        let manifest_run_defaults = Arc::new(manifest_run_defaults);
        let catalog = Arc::new(
            Catalog::from_builtin_with_overrides(&llm_catalog_settings)
                .context("building LLM model catalog")?,
        );
        resolve_canonical_origin(&server_settings.server, &self.env_lookup)
            .map_err(anyhow::Error::msg)?;

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
        *self.catalog.write().expect("catalog lock poisoned") = catalog;
        Ok(())
    }
}

async fn resolve_llm_client_from_source(
    source: &dyn CredentialSource,
    catalog: Arc<Catalog>,
) -> anyhow::Result<LlmClientResult> {
    let resolved = source
        .resolve(catalog.as_ref())
        .await
        .context("resolving LLM credentials")?;
    let report = LlmClient::from_credentials_report(resolved.credentials, catalog).await;

    Ok(LlmClientResult {
        client:              report.client,
        auth_issues:         resolved.auth_issues,
        registration_issues: report.registration_issues,
    })
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

fn resolve_interp_string(value: &InterpString) -> anyhow::Result<String> {
    value
        .resolve(process_env_var)
        .map(|resolved| resolved.value)
        .map_err(anyhow::Error::from)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Server state owns process-env lookup facades for interpolation and vault fallbacks."
)]
pub(crate) fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
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
                Ok(envelope) => {
                    // Resolve the run's web URL once per event so the Slack
                    // message can deep-link back to Fabro. Returns None when
                    // the web UI is disabled or `server.web.url` is unset, in
                    // which case `question_to_blocks` simply omits the link.
                    let run_web_url = event_state.run_web_url(&envelope.event.run_id);
                    event_service
                        .handle_event(&envelope.event, run_web_url.as_deref())
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
        slack_connection::run(
            &service.client,
            &service.app_token,
            &service.thread_registry,
            on_submit,
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
    build_router_with_options(
        state,
        &auth_mode,
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions::default(),
    )
}

#[derive(Clone, Debug)]
pub struct RouterOptions {
    pub web_enabled:                 bool,
    pub static_asset_root:           Option<PathBuf>,
    pub github_endpoints:            Option<Arc<GithubEndpoints>>,
    pub github_webhook_ip_allowlist: Option<Arc<IpAllowlistConfig>>,
    /// Set when serving with the `--watch-web` dev flag. The static-file
    /// handler then refuses to fall back to the embedded SPA snapshot and
    /// returns a 503 "build in progress" page on miss, so developers see
    /// their edits or a clear signal — never stale embedded bytes.
    pub watch_web:                   bool,
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            web_enabled:                 true,
            static_asset_root:           None,
            github_endpoints:            None,
            github_webhook_ip_allowlist: None,
            watch_web:                   false,
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
    ip_allowlist_config: Arc<IpAllowlistConfig>,
    options: RouterOptions,
) -> Router {
    start_optional_slack_service(&state);
    let web_enabled = options.web_enabled;
    let static_asset_root = options.static_asset_root.clone();
    let watch_web = options.watch_web;
    let webhook_ip_allowlist = options.github_webhook_ip_allowlist;
    let translation_state = Arc::clone(&state);
    let state_for_canonical_host = Arc::clone(&state);
    let github_endpoints = options
        .github_endpoints
        .clone()
        .unwrap_or_else(|| Arc::new(GithubEndpoints::production_defaults()));
    let webhook_secret = state.server_secret(WEBHOOK_SECRET_ENV);
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
        Arc::clone(&ip_allowlist_config),
        ip_allowlist_middleware,
    ));
    app_router = app_router.layer(middleware::from_fn_with_state(
        translation_state,
        auth_translation_middleware,
    ));
    app_router = app_router.layer(middleware::from_fn(demo_routing_middleware));
    app_router = app_router.layer(axum::Extension(auth_mode.clone()));

    let mut router = app_router;
    if let Some(secret) = webhook_secret {
        let allowlist = webhook_ip_allowlist.unwrap_or(ip_allowlist_config);
        let secret: Arc<[u8]> = Arc::from(secret.into_bytes().into_boxed_slice());
        router = github_webhook_routes(secret, allowlist).merge(router);
    }

    router
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
    let principal_kind = auth_context.principal.kind();
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
                Principal::User(user) => emit_http_log!(
                    $level,
                    user_auth_method = user.auth_method.as_str(),
                    idp_issuer = user.identity.issuer(),
                    idp_subject = user.identity.subject(),
                    login = user.login.as_str(),
                ),
                Principal::Worker { run_id } => {
                    emit_http_log!($level, run_id = run_id.to_string().as_str(),)
                }
                Principal::Webhook { delivery_id } => {
                    emit_http_log!($level, delivery_id = delivery_id.as_str(),)
                }
                Principal::Slack {
                    team_id, user_id, ..
                } => emit_http_log!(
                    $level,
                    team_id = team_id.as_str(),
                    user_id = user_id.as_str(),
                ),
                Principal::Agent { .. } | Principal::System { .. } | Principal::Anonymous => {
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

fn github_webhook_routes(secret: Arc<[u8]>, ip_allowlist_config: Arc<IpAllowlistConfig>) -> Router {
    Router::new()
        .route(WEBHOOK_ROUTE, post(github_webhook))
        .with_state(secret)
        .layer(middleware::from_fn_with_state(
            ip_allowlist_config,
            ip_allowlist_middleware,
        ))
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

fn system_features(
    server_settings: &ServerSettings,
    _manifest_run_settings: &std::result::Result<RunNamespace, SharedError>,
) -> SystemFeatures {
    let session_sandboxes = server_settings.features.session_sandboxes;
    SystemFeatures {
        session_sandboxes: Some(session_sandboxes),
    }
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

#[cfg(test)]
fn resolve_manifest_run_settings(
    manifest_run_defaults: &RunLayer,
) -> std::result::Result<RunNamespace, SharedError> {
    RunSettingsBuilder::from_run_layer(manifest_run_defaults)
        .map_err(|err| SharedError::new(anyhow::Error::new(err)))
}

fn system_sandbox_provider(
    manifest_run_settings: &std::result::Result<RunNamespace, SharedError>,
) -> String {
    manifest_run_settings.as_ref().map_or_else(
        |_| SandboxProvider::default().to_string(),
        |settings| settings.sandbox.provider.clone(),
    )
}

fn clone_sandbox_can_use_github_credentials(provider: &str) -> bool {
    matches!(provider, "docker" | "daytona")
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

pub(crate) fn build_app_state(config: AppStateConfig) -> anyhow::Result<Arc<AppState>> {
    let AppStateConfig {
        resolved_settings,
        registry_factory_override,
        max_concurrent_runs,
        store,
        artifact_store,
        vault_path,
        server_secrets,
        env_lookup,
        github_api_base_url,
        active_config_path,
        http_client,
        shutdown,
    } = config;

    match vault_legacy_migration::migrate_legacy_vault_file(&vault_path) {
        Ok(report) if report.changed() => {
            let backup_path = report
                .backup_path
                .as_ref()
                .map_or_else(|| "<none>".to_string(), |path| path.display().to_string());
            warn!(
                migrated_entries = report.migrated_entries,
                skipped_entries = report.skipped_entries,
                backup_path = %backup_path,
                removal_deadline = vault_legacy_migration::REMOVAL_DEADLINE,
                "Migrated legacy vault file"
            );
        }
        Ok(_) => {}
        Err(err) => {
            warn!(
                error = %err,
                removal_deadline = vault_legacy_migration::REMOVAL_DEADLINE,
                "Legacy vault migration failed; continuing with normal vault load"
            );
        }
    }
    let vault = Vault::load(vault_path.clone())
        .with_context(|| format!("load vault {}", vault_path.display()))?;
    let vault = Arc::new(AsyncRwLock::new(vault));
    let llm_source: Arc<dyn CredentialSource> = Arc::new(VaultCredentialSource::with_env_lookup(
        Arc::clone(&vault),
        {
            let env_lookup = Arc::clone(&env_lookup);
            move |name| env_lookup(name)
        },
    ));
    let (global_event_tx, _) = broadcast::channel(4096);
    let current_server_settings = Arc::new(resolved_settings.server_settings);
    let current_manifest_run_defaults = Arc::new(resolved_settings.manifest_run_defaults);
    let current_manifest_run_settings = resolved_settings.manifest_run_settings;
    let current_catalog = Arc::new(
        Catalog::from_builtin_with_overrides(&resolved_settings.llm_catalog_settings)
            .context("building LLM model catalog")?,
    );
    let slack_service = {
        current_server_settings
            .server
            .integrations
            .slack
            .default_channel
            .as_ref()
            .map(|value| {
                value
                    .resolve(process_env_var)
                    .map(|resolved| resolved.value)
                    .map_err(anyhow::Error::from)
            })
            .transpose()?
            .and_then(|default_channel| {
                resolve_slack_credentials().map(|credentials| {
                    Arc::new(SlackService::new(
                        credentials.bot_token,
                        credentials.app_token,
                        default_channel,
                    ))
                })
            })
    };
    let worker_tokens = worker_token_keys_from_server_secrets(&server_secrets)?;
    let github_api_base_url = github_api_base_url.unwrap_or_else(fabro_github::github_api_base_url);
    Ok(Arc::new(AppState {
        runs: Mutex::new(HashMap::new()),
        aggregate_billing: Mutex::new(BillingAccumulator::default()),
        store,
        session_runtimes: SessionRuntimeManager::new(),
        artifact_store,
        worker_tokens,
        started_at: Instant::now(),
        resource_sampler: resource_sampler::ResourceSampler::new(),
        max_concurrent_runs,
        scheduler_notify: Notify::new(),
        global_event_tx,
        files_in_flight: new_files_in_flight(),
        pull_request_create_locks: Arc::new(Mutex::new(HashMap::new())),
        parent_link_lock: AsyncMutex::new(()),
        vault,
        server_secrets,
        llm_source,
        manifest_run_defaults: RwLock::new(current_manifest_run_defaults),
        manifest_run_settings: RwLock::new(current_manifest_run_settings),
        server_settings: RwLock::new(current_server_settings),
        catalog: RwLock::new(current_catalog),
        env_lookup: Arc::clone(&env_lookup),
        github_api_base_url,
        active_config_path,
        http_client,
        shutdown,
        shutting_down: AtomicBool::new(false),
        registry_factory_override,
        slack_service,
        slack_started: AtomicBool::new(false),
    }))
}

const MAX_PAGE_OFFSET: u32 = 1_000_000;

enum DeleteRunOutcome {
    NoContent,
    Preserved(DeleteRunResponse),
}

async fn delete_run_internal(
    state: &Arc<AppState>,
    id: RunId,
    force: bool,
) -> Result<DeleteRunOutcome, Response> {
    if !force {
        reject_active_delete_without_force(state.as_ref(), &id).await?;
    }

    let mut managed_run = if let Ok(mut runs) = state.runs.lock() {
        runs.remove(&id)
    } else {
        None
    };
    let durable_status = if managed_run.is_some() {
        load_durable_run_status(state.as_ref(), &id).await
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
        // Terminal runs can still carry a stale worker PID briefly after their
        // completion events land, so avoid paying the full cancellation grace.
        let delete_grace = if should_signal_cancel && managed_run.status.requires_force_to_delete()
        {
            WORKER_CANCEL_GRACE
        } else {
            TERMINAL_DELETE_WORKER_GRACE
        };
        terminate_worker_for_deletion(
            managed_run.worker_pid,
            managed_run.worker_pgid,
            delete_grace,
        )
        .await;
    }

    let delete_outcome = delete_run_sandbox_resource(state, id, force).await?;

    if let Some(mut managed_run) = managed_run {
        if let Some(run_dir) = managed_run.run_dir.take() {
            remove_run_dir(&run_dir).map_err(|err| {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            })?;
        }
    } else {
        let storage = Storage::new(state.server_storage_dir());
        let run_dir = storage.run_scratch(&id).root().to_path_buf();
        remove_run_dir(&run_dir).map_err(|err| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        })?;
    }

    state.store.delete_run(&id).await.map_err(|err| {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
    })?;
    state
        .artifact_store
        .delete_for_run(&id)
        .await
        .map_err(|err| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        })?;
    Ok(delete_outcome)
}

async fn load_durable_run_status(state: &AppState, id: &RunId) -> Option<RunStatus> {
    let run_store = state.store.open_run(id).await.ok()?;
    let projection = run_store.state().await.ok()?;
    Some(projection.status)
}

async fn delete_run_sandbox_resource(
    state: &Arc<AppState>,
    id: RunId,
    force: bool,
) -> Result<DeleteRunOutcome, Response> {
    let Ok(run_store) = state.store.open_run(&id).await else {
        return Ok(DeleteRunOutcome::NoContent);
    };
    let projection = match run_store.state().await {
        Ok(projection) => projection,
        Err(err) if force => {
            tracing::warn!(
                run_id = %id,
                error = %render_with_causes(&err.to_string(), &collect_causes(&err)),
                "Skipping sandbox provider delete because run projection cannot be loaded"
            );
            return Ok(DeleteRunOutcome::NoContent);
        }
        Err(err) => {
            return Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            );
        }
    };
    let delete_started = matches!(projection.status, RunStatus::Removing);
    let can_mark_removing = projection.status.can_transition_to(RunStatus::Removing);
    if !delete_started && can_mark_removing {
        workflow_event::append_event(&run_store, &id, &workflow_event::Event::RunRemoving)
            .await
            .map_err(|err| {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            })?;
    }

    let preserve = projection.spec().settings.run.sandbox.preserve;
    let Some(record) = projection.sandbox else {
        return Ok(DeleteRunOutcome::NoContent);
    };
    if preserve {
        return Ok(DeleteRunOutcome::Preserved(DeleteRunResponse {
            deleted:           true,
            sandbox_preserved: true,
            sandbox:           DeleteRunSandbox {
                provider: record.provider,
                id:       record
                    .runtime
                    .as_ref()
                    .map(|runtime| runtime.id.clone())
                    .unwrap_or_default(),
            },
        }));
    }

    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    let sandbox = match reconnect_for_run(&record, daytona_api_key, Some(id)).await {
        Ok(sandbox) => sandbox,
        Err(err) if force || delete_started => {
            tracing::warn!(
                run_id = %id,
                error = %render_with_causes(&err.to_string(), &collect_causes(err.as_ref())),
                "Skipping sandbox provider delete during run deletion"
            );
            return Ok(DeleteRunOutcome::NoContent);
        }
        Err(err) => {
            let detail = render_with_causes(&err.to_string(), &collect_causes(err.as_ref()));
            return Err(ApiError::new(StatusCode::CONFLICT, detail).into_response());
        }
    };
    if let Err(err) = sandbox.delete().await {
        if force || delete_started {
            tracing::warn!(
                run_id = %id,
                error = %err.display_with_causes(),
                "Skipping failed sandbox provider delete during run deletion"
            );
            return Ok(DeleteRunOutcome::NoContent);
        }
        return Err(ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response());
    }

    Ok(DeleteRunOutcome::NoContent)
}

async fn reject_active_delete_without_force(
    state: &AppState,
    run_id: &RunId,
) -> Result<(), Response> {
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
            )
            .into_response());
        }
        return Ok(());
    }

    match state.store.runs().find(run_id).await {
        Ok(Some(summary)) if summary.lifecycle.status.requires_force_to_delete() => {
            Err(ApiError::new(
                StatusCode::CONFLICT,
                active_run_delete_message(*run_id, summary.lifecycle.status),
            )
            .into_response())
        }
        Ok(_) => Ok(()),
        Err(err) => {
            Err(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response())
        }
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
    worker_pid: Option<u32>,
    worker_pgid: Option<u32>,
    grace: Duration,
) {
    #[cfg(unix)]
    if let Some(process_group_id) = worker_pgid.or(worker_pid) {
        fabro_proc::sigterm_process_group(process_group_id);

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && fabro_proc::process_group_alive(process_group_id) {
            sleep(Duration::from_millis(50)).await;
        }

        if fabro_proc::process_group_alive(process_group_id) {
            fabro_proc::sigkill_process_group(process_group_id);

            let kill_deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < kill_deadline
                && fabro_proc::process_group_alive(process_group_id)
            {
                sleep(Duration::from_millis(50)).await;
            }
        }
    }

    #[cfg(not(unix))]
    if let Some(worker_pid) = worker_pid {
        fabro_proc::sigterm(worker_pid);

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && fabro_proc::process_running(worker_pid) {
            sleep(Duration::from_millis(50)).await;
        }

        if fabro_proc::process_running(worker_pid) {
            fabro_proc::sigkill(worker_pid);

            let kill_deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < kill_deadline && fabro_proc::process_running(worker_pid) {
                sleep(Duration::from_millis(50)).await;
            }
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
    let mut queued: Vec<(&RunId, &ManagedRun)> = runs
        .iter()
        .filter(|(_, r)| r.status == RunStatus::Queued)
        .collect();
    queued.sort_by_key(|(_, r)| r.created_at);
    queued
        .into_iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i64::try_from(i + 1).unwrap()))
        .collect()
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
    reason = "Blob ID parsing returns HTTP 400 responses directly."
)]
pub(crate) fn parse_blob_id_path(blob_id: &str) -> Result<RunBlobId, Response> {
    RunBlobId::from_str(blob_id)
        .map_err(|_| ApiError::bad_request("Invalid blob ID.").into_response())
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
    run.active_api_targets.clear();
    run.active_steerable_stages.clear();
    run.active_non_steerable_stages.clear();
    run.event_tx = None;
    run.cancel_tx = None;
    run.cancel_token = None;
    run.worker_pid = None;
    run.worker_pgid = None;
}

fn reconcile_live_interview_state_for_event(run: &mut ManagedRun, event: &RunEvent) {
    match &event.body {
        EventBody::InterviewCompleted(props) => {
            run.accepted_questions.remove(&props.question_id);
        }
        EventBody::InterviewTimeout(props) => {
            run.accepted_questions.remove(&props.question_id);
        }
        EventBody::InterviewInterrupted(props) => {
            run.accepted_questions.remove(&props.question_id);
        }
        EventBody::RunCompleted(_) | EventBody::RunFailed(_) => {
            run.accepted_questions.clear();
        }
        _ => {}
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

#[derive(Clone, Copy)]
struct LiveWorkerProcess {
    run_id:           RunId,
    process_group_id: u32,
}

fn failure_for_incomplete_run(
    pending_control: Option<RunControlAction>,
    terminated_message: String,
) -> (WorkflowError, FailureReason) {
    if pending_control == Some(RunControlAction::Cancel) {
        (WorkflowError::Cancelled, FailureReason::Cancelled)
    } else {
        (
            WorkflowError::engine(terminated_message),
            FailureReason::Terminated,
        )
    }
}

fn should_reconcile_run_on_startup(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Queued
            | RunStatus::Starting
            | RunStatus::Running
            | RunStatus::Blocked { .. }
            | RunStatus::Paused { .. }
            | RunStatus::Removing
    )
}

pub(crate) async fn reconcile_incomplete_runs_on_startup(
    state: &Arc<AppState>,
) -> anyhow::Result<usize> {
    let summaries = state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await?;
    let mut reconciled = 0usize;

    for summary in summaries {
        if !should_reconcile_run_on_startup(summary.lifecycle.status) {
            continue;
        }

        let run_store = state.store.open_run(&summary.id).await?;
        let (error, reason) = failure_for_incomplete_run(
            summary.lifecycle.pending_control,
            "Fabro server restarted before the run reached a terminal state.".to_string(),
        );
        let failure_event = workflow_event::Event::workflow_run_failed_from_error(
            &error,
            fabro_types::RunTiming::default(),
            reason,
            None,
            None,
            None,
            None,
        );
        workflow_event::append_event(&run_store, &summary.id, &failure_event).await?;
        reconciled += 1;
    }

    Ok(reconciled)
}

fn live_worker_processes(state: &AppState) -> Vec<LiveWorkerProcess> {
    let runs = state.runs.lock().expect("runs lock poisoned");
    runs.iter()
        .filter_map(|(run_id, managed_run)| {
            managed_run
                .worker_pgid
                .or(managed_run.worker_pid)
                .map(|process_group_id| LiveWorkerProcess {
                    run_id: *run_id,
                    process_group_id,
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
        let run_store = state.store.open_run(&run_id).await?;
        let run_state = run_store.state().await?;
        if run_state.status.is_terminal() {
            continue;
        }

        let (error, reason) = failure_for_incomplete_run(
            run_state.pending_control,
            "Fabro server shut down before the run reached a terminal state.".to_string(),
        );
        let failure_event = workflow_event::Event::workflow_run_failed_from_error(
            &error,
            fabro_types::RunTiming::default(),
            reason,
            None,
            None,
            None,
            None,
        );
        workflow_event::append_event(&run_store, &run_id, &failure_event).await?;
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

    #[cfg(unix)]
    {
        let process_groups = workers
            .iter()
            .map(|worker| worker.process_group_id)
            .collect::<HashSet<_>>();

        for process_group_id in &process_groups {
            fabro_proc::sigterm_process_group(*process_group_id);
        }

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline
            && process_groups
                .iter()
                .any(|process_group_id| fabro_proc::process_group_alive(*process_group_id))
        {
            sleep(poll_interval).await;
        }

        let survivors = process_groups
            .into_iter()
            .filter(|process_group_id| fabro_proc::process_group_alive(*process_group_id))
            .collect::<Vec<_>>();
        for process_group_id in &survivors {
            fabro_proc::sigkill_process_group(*process_group_id);
        }
        if !survivors.is_empty() {
            let kill_deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < kill_deadline
                && survivors
                    .iter()
                    .any(|process_group_id| fabro_proc::process_group_alive(*process_group_id))
            {
                sleep(poll_interval).await;
            }
        }
    }

    persist_shutdown_run_failures(state, &workers).await?;
    Ok(workers.len())
}

async fn persist_cancelled_run_status(state: &AppState, run_id: RunId) -> anyhow::Result<()> {
    let run_store = state.store.open_run(&run_id).await?;
    let run_state = run_store.state().await?;
    if run_state.status.is_terminal() {
        return Ok(());
    }

    let failure_event = workflow_event::Event::workflow_run_failed_from_error(
        &WorkflowError::Cancelled,
        fabro_types::RunTiming::default(),
        FailureReason::Cancelled,
        None,
        None,
        None,
        None,
    );
    workflow_event::append_event(&run_store, &run_id, &failure_event).await
}

async fn finish_cancelled_run_before_execution(state: &Arc<AppState>, run_id: RunId) {
    if let Err(err) = persist_cancelled_run_status(state.as_ref(), run_id).await {
        error!(run_id = %run_id, error = %err, "Failed to persist cancelled run status");
    }

    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        managed_run.status = RunStatus::Failed {
            reason: FailureReason::Cancelled,
        };
        clear_live_run_state(managed_run);
    }
    drop(runs);
    state.scheduler_notify.notify_one();
}

async fn fail_run_before_execution(
    state: &Arc<AppState>,
    run_id: RunId,
    reason: FailureReason,
    message: String,
) {
    match state.store.open_run(&run_id).await {
        Ok(run_store) => {
            let failure_event = workflow_event::Event::workflow_run_failed_from_error(
                &WorkflowError::engine(message.clone()),
                fabro_types::RunTiming::default(),
                reason,
                None,
                None,
                None,
                None,
            );
            if let Err(err) =
                workflow_event::append_event(&run_store, &run_id, &failure_event).await
            {
                error!(run_id = %run_id, error = %err, "Failed to persist run failure status");
            }
        }
        Err(err) => {
            error!(run_id = %run_id, error = %err, "Failed to open run store while persisting run failure");
        }
    }

    fail_managed_run(state, run_id, reason, message);
    state.scheduler_notify.notify_one();
}

async fn forward_run_events_to_global(
    state: Arc<AppState>,
    run_id: RunId,
    mut run_events: broadcast::Receiver<EventEnvelope>,
) {
    loop {
        match run_events.recv().await {
            Ok(event) => {
                let mut runs = state.runs.lock().expect("runs lock poisoned");
                if let Some(managed_run) = runs.get_mut(&run_id) {
                    reconcile_live_interview_state_for_event(managed_run, &event.event);
                }
                let _ = state.global_event_tx.send(event);
            }
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => break,
        }
    }
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
        enqueued_at: Instant::now(),
        answer_transport: None,
        accepted_questions: HashSet::new(),
        active_api_targets: HashMap::new(),
        active_steerable_stages: HashMap::new(),
        active_non_steerable_stages: HashMap::new(),
        event_tx: None,
        checkpoint: None,
        cancel_tx: None,
        cancel_token: None,
        worker_pid: None,
        worker_pgid: None,
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
        .store
        .runs()
        .find(&run_id)
        .await?
        .and_then(|summary| summary.lifecycle.pending_control))
}

async fn durable_run_status(state: &AppState, run_id: RunId) -> anyhow::Result<Option<RunStatus>> {
    Ok(state
        .store
        .runs()
        .find(&run_id)
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
}

fn update_live_run_from_event(state: &AppState, run_id: RunId, event: &RunEvent) {
    use fabro_types::EventBody;

    let mut runs = state.runs.lock().expect("runs lock poisoned");
    let Some(managed_run) = runs.get_mut(&run_id) else {
        return;
    };

    match &event.body {
        EventBody::RunSubmitted(_) => managed_run.status = RunStatus::Submitted,
        EventBody::RunQueued(_) => managed_run.status = RunStatus::Queued,
        EventBody::RunStarting(_) => managed_run.status = RunStatus::Starting,
        EventBody::RunRunning(_) => managed_run.status = RunStatus::Running,
        EventBody::RunBlocked(props) => {
            managed_run.status = match managed_run.status {
                RunStatus::Paused { .. } => RunStatus::Paused {
                    prior_block: Some(props.blocked_reason),
                },
                _ => RunStatus::Blocked {
                    blocked_reason: props.blocked_reason,
                },
            };
        }
        EventBody::RunUnblocked(_) => {
            managed_run.status = match managed_run.status {
                RunStatus::Paused {
                    prior_block: Some(_) | None,
                } => RunStatus::Paused { prior_block: None },
                _ => RunStatus::Running,
            };
        }
        EventBody::RunPaused(_) => {
            let prior_block = match managed_run.status {
                RunStatus::Blocked { blocked_reason } => Some(blocked_reason),
                RunStatus::Paused { prior_block } => prior_block,
                _ => None,
            };
            managed_run.status = RunStatus::Paused { prior_block };
        }
        EventBody::RunUnpaused(_) => {
            managed_run.status = match managed_run.status {
                RunStatus::Paused {
                    prior_block: Some(blocked_reason),
                } => RunStatus::Blocked { blocked_reason },
                _ => RunStatus::Running,
            };
        }
        EventBody::RunRemoving(_) => managed_run.status = RunStatus::Removing,
        EventBody::RunCompleted(_) => {
            let EventBody::RunCompleted(props) = &event.body else {
                unreachable!();
            };
            managed_run.status = RunStatus::Succeeded {
                reason: props.reason,
            };
            managed_run.error = None;
            managed_run.active_api_targets.clear();
            managed_run.active_steerable_stages.clear();
            managed_run.active_non_steerable_stages.clear();
        }
        EventBody::RunFailed(props) => {
            managed_run.status = RunStatus::Failed {
                reason: props.failure.reason,
            };
            managed_run.error = Some(render_compact_with_causes(
                &props.failure.detail.message,
                &props.failure.detail.causes,
            ));
            managed_run.active_api_targets.clear();
            managed_run.active_steerable_stages.clear();
            managed_run.active_non_steerable_stages.clear();
        }
        // Track active agent sessions by steerability. Activated/deactivated
        // are leased by session id so stale deactivations cannot clear a newer
        // binding for the same stage.
        EventBody::AgentSessionActivated(props) => {
            if let (Some(stage_id), Some(session_id)) =
                (event.stage_id.as_ref(), event.session_id.as_ref())
            {
                if props.capabilities.contains(&SessionCapability::Steer) {
                    managed_run
                        .active_steerable_stages
                        .insert(stage_id.clone(), session_id.clone());
                    managed_run.active_non_steerable_stages.remove(stage_id);
                    let acp_provider: &'static str = AgentBackend::Acp.into();
                    if props.provider.as_deref() == Some(acp_provider) {
                        managed_run.active_api_targets.remove(stage_id);
                    } else {
                        managed_run
                            .active_api_targets
                            .insert(stage_id.clone(), PairTarget {
                                stage_id:   stage_id.clone(),
                                node_label: event
                                    .node_label
                                    .clone()
                                    .unwrap_or_else(|| stage_id.node_id().to_string()),
                            });
                    }
                } else {
                    managed_run
                        .active_non_steerable_stages
                        .insert(stage_id.clone(), session_id.clone());
                    managed_run.active_steerable_stages.remove(stage_id);
                    managed_run.active_api_targets.remove(stage_id);
                }
            }
        }
        EventBody::AgentSessionDeactivated(_) => {
            if let (Some(stage_id), Some(session_id)) =
                (event.stage_id.as_ref(), event.session_id.as_ref())
            {
                if managed_run
                    .active_steerable_stages
                    .get(stage_id)
                    .is_some_and(|current| current == session_id)
                {
                    managed_run.active_steerable_stages.remove(stage_id);
                    managed_run.active_api_targets.remove(stage_id);
                }
                if managed_run
                    .active_non_steerable_stages
                    .get(stage_id)
                    .is_some_and(|current| current == session_id)
                {
                    managed_run.active_non_steerable_stages.remove(stage_id);
                }
            }
        }
        // ACP sessions are steerable via `agent.session.activated`; terminal
        // ACP events and stage lifecycle events are still backstops for cleanup.
        EventBody::AgentAcpCompleted(_)
        | EventBody::AgentAcpCancelled(_)
        | EventBody::AgentAcpTimedOut(_)
        | EventBody::StageCompleted(_)
        | EventBody::StageFailed(_) => {
            if let Some(stage_id) = &event.stage_id {
                managed_run.active_api_targets.remove(stage_id);
                managed_run.active_steerable_stages.remove(stage_id);
                managed_run.active_non_steerable_stages.remove(stage_id);
            }
        }
        _ => {}
    }
}

async fn drain_worker_stderr(run_id: RunId, stderr: ChildStderr) -> anyhow::Result<()> {
    let mut lines = BufReader::new(stderr).lines();

    while let Some(line) = lines.next_line().await? {
        tracing::warn!(run_id = %run_id, "Worker stderr: {line}");
    }

    Ok(())
}

async fn pump_worker_control_jsonl(
    mut stdin: ChildStdin,
    mut control_rx: mpsc::Receiver<WorkerControlEnvelope>,
) -> anyhow::Result<()> {
    while let Some(message) = control_rx.recv().await {
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        stdin.flush().await?;
    }

    Ok(())
}

async fn append_worker_exit_failure(
    run_store: &fabro_store::RunDatabase,
    run_id: RunId,
    wait_status: &std::process::ExitStatus,
) {
    let state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Failed to load run state after worker exit");
            return;
        }
    };

    let terminal = state.status.is_terminal();
    if terminal {
        return;
    }

    let (error, reason) = failure_for_incomplete_run(
        state.pending_control,
        format!("Worker exited before emitting a terminal run event: {wait_status}"),
    );
    let failure_event = workflow_event::Event::workflow_run_failed_from_error(
        &error,
        fabro_types::RunTiming::default(),
        reason,
        None,
        None,
        None,
        None,
    );

    if let Err(err) = workflow_event::append_event(run_store, &run_id, &failure_event).await {
        tracing::warn!(run_id = %run_id, error = %err, "Failed to append worker exit failure");
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Worker subprocess startup resolves Cargo's test binary env override when present."
)]
fn worker_command(
    state: &AppState,
    run_id: RunId,
    mode: RunExecutionMode,
    run_dir: &std::path::Path,
) -> anyhow::Result<Command> {
    let current_exe = std::env::current_exe().context("reading current executable path")?;
    let exe = std::env::var_os(EnvVars::CARGO_BIN_EXE_FABRO).map_or(current_exe, PathBuf::from);
    let storage_dir = state.server_storage_dir();
    let runtime_directory = Storage::new(&storage_dir).runtime_directory();
    let daemon = ServerDaemon::read(&runtime_directory)?.with_context(|| {
        format!(
            "server record {} is missing",
            runtime_directory.record_path().display()
        )
    })?;
    let server_target = daemon.bind.to_target();
    let worker_token = issue_worker_token_with_scopes(
        state.worker_token_keys(),
        &run_id,
        WorkerScopeSet::run_worker_with_agent_run_tools(),
    )
    .map_err(|_| anyhow::anyhow!("failed to sign worker token"))?;
    let server_destination = resolved_log_destination(state)?;
    let worker_stdout = match server_destination {
        LogDestination::Stdout => Stdio::inherit(),
        LogDestination::File => Stdio::null(),
    };
    let mut cmd = Command::new(exe);
    cmd.arg("__run-worker")
        .arg("--server")
        .arg(server_target)
        .arg("--storage-dir")
        .arg(&storage_dir)
        .arg("--run-dir")
        .arg(run_dir)
        .arg("--run-id")
        .arg(run_id.to_string())
        .arg("--mode")
        .arg(worker_mode_arg(mode))
        .stdin(Stdio::piped())
        .stdout(worker_stdout)
        .stderr(Stdio::piped());

    apply_worker_env(&mut cmd);
    if (state.env_lookup)(EnvVars::FABRO_LOG).is_none() {
        if let Some(level) = state.server_settings().server.logging.level.as_deref() {
            cmd.env(EnvVars::FABRO_LOG, level);
        }
    }
    let value: &'static str = server_destination.into();
    cmd.env(EnvVars::FABRO_LOG_DESTINATION, value);
    cmd.env(EnvVars::FABRO_CONFIG, state.active_config_path());
    cmd.env_remove(EnvVars::FABRO_WORKER_TOKEN);
    cmd.env(EnvVars::FABRO_WORKER_TOKEN, worker_token);
    if let Some(pem) = state.server_secret(EnvVars::GITHUB_APP_PRIVATE_KEY) {
        cmd.env(EnvVars::GITHUB_APP_PRIVATE_KEY, pem);
    }

    #[cfg(unix)]
    fabro_proc::pre_exec_setpgid(cmd.as_std_mut());

    Ok(cmd)
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
    }
}

fn api_question_from_interview_record(question: &InterviewQuestionRecord) -> ApiQuestion {
    ApiQuestion {
        id:              question.id.clone(),
        text:            question.text.clone(),
        stage:           question.stage.clone(),
        question_type:   question.question_type,
        options:         question
            .options
            .iter()
            .map(|option| ApiQuestionOption {
                key:   option.key.clone(),
                label: option.label.clone(),
            })
            .collect(),
        allow_freeform:  question.allow_freeform,
        timeout_seconds: question.timeout_seconds,
        context_display: question.context_display.clone(),
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
    let cached = match state.store.get_cached_run(&run_id).await {
        Ok(Some(cached)) => cached,
        Ok(None) => return Err(ApiError::not_found("Run not found.").into_response()),
        Err(err) => {
            return Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            );
        }
    };
    let Some(record) = cached.projection.pending_interviews.get(qid) else {
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

    if let Ok(()) = transport.submit(qid, submission).await {
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

/// Execute a single run: transitions queued → starting → running →
/// completed/failed/cancelled.
async fn execute_run(state: Arc<AppState>, run_id: RunId) {
    if state.is_shutting_down() {
        return;
    }

    if state.registry_factory_override.is_some() {
        Box::pin(execute_run_in_process(state, run_id)).await;
        return;
    }

    execute_run_subprocess(state, run_id).await;
}

async fn execute_run_in_process(state: Arc<AppState>, run_id: RunId) {
    // Transition to Starting and set up cancel infrastructure
    let (cancel_rx, run_dir, event_tx, cancel_token, execution_mode, queued_for) = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = match runs.get_mut(&run_id) {
            Some(r) if r.status == RunStatus::Queued => r,
            _ => return,
        };
        let Some(run_dir) = managed_run.run_dir.clone() else {
            return;
        };

        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let cancel_token = CancellationToken::new();
        let (event_tx, _) = broadcast::channel(256);

        managed_run.status = RunStatus::Starting;
        managed_run.cancel_tx = Some(cancel_tx);
        managed_run.cancel_token = Some(cancel_token.clone());
        managed_run.event_tx = Some(event_tx);

        (
            cancel_rx,
            run_dir,
            managed_run.event_tx.clone(),
            cancel_token,
            managed_run.execution_mode,
            managed_run.enqueued_at.elapsed(),
        )
    };
    let _ = queued_for;

    // Create interviewer and event plumbing (this is the "provisioning" phase)
    let interviewer = Arc::new(ControlInterviewer::new());
    let interview_runtime: Arc<dyn Interviewer> = interviewer.clone();
    let emitter = Emitter::new(run_id);
    if let Some(tx_clone) = event_tx {
        emitter.on_event(move |event| {
            let _ = tx_clone.send(event.clone());
        });
    }
    let registry_override = state
        .registry_factory_override
        .as_ref()
        .map(|factory| Arc::new(factory(Arc::clone(&interview_runtime))));
    let emitter = Arc::new(emitter);
    let steering_hub = Arc::new(fabro_workflow::SteeringHub::new(Arc::clone(&emitter)));

    // Transition to Running, populate interviewer
    let cancelled_during_setup = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&run_id) {
            if managed_run.status == RunStatus::Starting {
                managed_run.status = RunStatus::Running;
                managed_run.answer_transport = Some(RunAnswerTransport::InProcess {
                    interviewer:  Arc::clone(&interviewer),
                    steering_hub: Arc::clone(&steering_hub),
                });
                false
            } else {
                // Was cancelled during setup
                clear_live_run_state(managed_run);
                state.scheduler_notify.notify_one();
                true
            }
        } else {
            false
        }
    };
    if cancelled_during_setup {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), run_id).await {
            error!(run_id = %run_id, error = %err, "Failed to persist cancelled run status");
        }
        return;
    }

    let run_store = match state.store.open_run(&run_id).await {
        Ok(run_store) => run_store,
        Err(e) => {
            tracing::error!(run_id = %run_id, error = %e, "Failed to open run store");
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            if let Some(managed_run) = runs.get_mut(&run_id) {
                managed_run.status = RunStatus::Failed {
                    reason: FailureReason::WorkflowError,
                };
                managed_run.error = Some(format!("Failed to open run store: {e}"));
                clear_live_run_state(managed_run);
            }
            state.scheduler_notify.notify_one();
            return;
        }
    };
    tokio::spawn(forward_run_events_to_global(
        Arc::clone(&state),
        run_id,
        run_store.subscribe(),
    ));
    let persisted = match Persisted::load_from_store(&run_store.clone().into(), &run_dir).await {
        Ok(persisted) => persisted,
        Err(e) => {
            tracing::error!(run_id = %run_id, error = %e, "Failed to load persisted run");
            fail_run_before_execution(
                &state,
                run_id,
                FailureReason::WorkflowError,
                format!("Failed to load persisted run: {e}"),
            )
            .await;
            return;
        }
    };
    let server_settings = state.server_settings();
    let github_settings = &server_settings.server.integrations.github;
    if cancel_token.is_cancelled() {
        finish_cancelled_run_before_execution(&state, run_id).await;
        return;
    }
    let github_app_result = {
        let run_spec = persisted.run_spec();
        let settings = &run_spec.settings.run;
        let clone_can_use_github_credentials = settings.execution.mode != RunMode::DryRun
            && clone_sandbox_can_use_github_credentials(&settings.sandbox.provider)
            && run_spec
                .repo_origin_url()
                .is_some_and(|origin| !origin.trim().is_empty());
        let pull_request_can_use_github_credentials =
            settings.execution.mode != RunMode::DryRun && settings.pull_request.is_some();
        if settings.integrations.github.is_token_requested() {
            state.github_credentials(github_settings)
        } else if clone_can_use_github_credentials || pull_request_can_use_github_credentials {
            match state.github_credentials(github_settings) {
                Ok(github_app) => Ok(github_app),
                Err(err) => {
                    tracing::warn!(
                        run_id = %run_id,
                        error = %err,
                        "GitHub credentials unavailable; pull request creation will be skipped"
                    );
                    Ok(None)
                }
            }
        } else {
            Ok(None)
        }
    };
    let github_app = match github_app_result {
        Ok(github_app) => github_app,
        Err(e) => {
            if cancel_token.is_cancelled() {
                finish_cancelled_run_before_execution(&state, run_id).await;
                return;
            }
            tracing::error!(run_id = %run_id, error = %e, "Invalid GitHub credentials");
            fail_run_before_execution(
                &state,
                run_id,
                FailureReason::WorkflowError,
                format!("Invalid GitHub credentials: {e}"),
            )
            .await;
            return;
        }
    };
    let github_permissions = persisted
        .run_spec()
        .settings
        .run
        .integrations
        .github
        .resolve_permissions(process_env_var);
    let services = operations::StartServices {
        run_id,
        cancel_token: cancel_token.clone(),
        emitter: Arc::clone(&emitter),
        interviewer: Arc::clone(&interview_runtime),
        steering_hub: Arc::clone(&steering_hub),
        run_store: run_store.clone().into(),
        event_sink: workflow_event::RunEventSink::store(run_store.clone()),
        artifact_sink: Some(ArtifactSink::Store(state.artifact_store.clone())),
        run_control: None,
        github_app,
        github_permissions,
        vault: Some(Arc::clone(&state.vault)),
        catalog: state.catalog(),
        on_node: None,
        registry_override,
        fabro_run_tools: None,
    };

    let execution = async {
        match execution_mode {
            RunExecutionMode::Start => operations::start(&run_dir, services).await,
            RunExecutionMode::Resume => operations::resume(&run_dir, services).await,
        }
    };

    let result = tokio::select! {
        result = execution => ExecutionResult::Completed(Box::new(result)),
        _ = cancel_rx => {
            cancel_token.cancel();
            ExecutionResult::CancelledBySignal
        }
    };

    if matches!(&result, ExecutionResult::CancelledBySignal) {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), run_id).await {
            error!(run_id = %run_id, error = %err, "Failed to persist cancelled run status");
        }
    }

    // Save final projection
    let final_projection = match run_store.state().await {
        Ok(state) => Some(state),
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Failed to load run state from store");
            None
        }
    };

    // Accumulate aggregate usage after execution completes.
    if let Some(ref projection) = final_projection {
        if projection.current_checkpoint().is_some() {
            let mut agg = state
                .aggregate_billing
                .lock()
                .expect("aggregate_billing lock poisoned");
            accumulate_billing_rollup(
                &mut agg,
                &fabro_workflow::billing_rollup_from_projection(projection, None),
            );
        }
    }

    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        match &result {
            ExecutionResult::Completed(result) => match result.as_ref() {
                Ok(started) => match &started.finalized.outcome {
                    Ok(_) => {
                        info!(run_id = %run_id, "Run completed");
                        managed_run.status = RunStatus::Succeeded {
                            reason: SuccessReason::Completed,
                        };
                    }
                    Err(WorkflowError::Cancelled) => {
                        info!(run_id = %run_id, "Run cancelled");
                        managed_run.status = RunStatus::Failed {
                            reason: FailureReason::Cancelled,
                        };
                    }
                    Err(e) => {
                        error!(run_id = %run_id, error = %e, "Run failed");
                        managed_run.status = RunStatus::Failed {
                            reason: FailureReason::WorkflowError,
                        };
                        managed_run.error = Some(e.to_string());
                    }
                },
                Err(WorkflowError::Cancelled) => {
                    info!(run_id = %run_id, "Run cancelled");
                    managed_run.status = RunStatus::Failed {
                        reason: FailureReason::Cancelled,
                    };
                }
                Err(e) => {
                    error!(run_id = %run_id, error = %e, "Run failed");
                    managed_run.status = RunStatus::Failed {
                        reason: FailureReason::WorkflowError,
                    };
                    managed_run.error = Some(e.to_string());
                }
            },
            ExecutionResult::CancelledBySignal => {
                info!(run_id = %run_id, "Run cancelled");
                managed_run.status = RunStatus::Failed {
                    reason: FailureReason::Cancelled,
                };
            }
        }
        managed_run.checkpoint = final_projection
            .as_ref()
            .and_then(|projection| projection.current_checkpoint().cloned());
        managed_run.run_dir = Some(run_dir);
        clear_live_run_state(managed_run);
    }
    drop(runs);
    state.scheduler_notify.notify_one();
}

async fn execute_run_subprocess(state: Arc<AppState>, run_id: RunId) {
    let (run_dir, execution_mode) = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if state.is_shutting_down() {
            return;
        }
        let managed_run = match runs.get_mut(&run_id) {
            Some(run) if run.status == RunStatus::Queued => run,
            _ => return,
        };
        let Some(run_dir) = managed_run.run_dir.clone() else {
            return;
        };
        managed_run.status = RunStatus::Starting;
        (run_dir, managed_run.execution_mode)
    };

    let run_store = match state.store.open_run(&run_id).await {
        Ok(run_store) => run_store,
        Err(err) => {
            tracing::error!(run_id = %run_id, error = %err, "Failed to open run store");
            fail_managed_run(
                &state,
                run_id,
                FailureReason::WorkflowError,
                format!("Failed to open run store: {err}"),
            );
            state.scheduler_notify.notify_one();
            return;
        }
    };
    tokio::spawn(forward_run_events_to_global(
        Arc::clone(&state),
        run_id,
        run_store.subscribe(),
    ));

    let state_for_build = Arc::clone(&state);
    let run_dir_for_build = run_dir.clone();
    let build_cmd_result = spawn_blocking(move || {
        worker_command(
            state_for_build.as_ref(),
            run_id,
            execution_mode,
            &run_dir_for_build,
        )
    })
    .await;

    let mut child = match build_cmd_result
        .context("worker_command task failed")
        .and_then(|inner| inner)
        .and_then(|mut cmd| cmd.spawn().context("spawning run worker process"))
    {
        Ok(child) => child,
        Err(err) => {
            tracing::error!(run_id = %run_id, error = %err, "Failed to spawn worker");
            let message = format!("Failed to spawn worker: {err}");
            let failure_event = workflow_event::Event::workflow_run_failed_from_error(
                &WorkflowError::engine_with_anyhow("Failed to spawn worker", err),
                fabro_types::RunTiming::default(),
                FailureReason::LaunchFailed,
                None,
                None,
                None,
                None,
            );
            let _ = workflow_event::append_event(&run_store, &run_id, &failure_event).await;
            fail_managed_run(&state, run_id, FailureReason::LaunchFailed, message);
            state.scheduler_notify.notify_one();
            return;
        }
    };

    let Some(worker_pid) = child.id() else {
        let message = "Worker process did not report a PID".to_string();
        tracing::error!(run_id = %run_id, "{message}");
        let _ = child.start_kill();
        let failure_event = workflow_event::Event::workflow_run_failed_from_error(
            &WorkflowError::engine(message.clone()),
            fabro_types::RunTiming::default(),
            FailureReason::LaunchFailed,
            None,
            None,
            None,
            None,
        );
        let _ = workflow_event::append_event(&run_store, &run_id, &failure_event).await;
        fail_managed_run(&state, run_id, FailureReason::LaunchFailed, message);
        state.scheduler_notify.notify_one();
        return;
    };

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&run_id) {
            managed_run.worker_pid = Some(worker_pid);
            managed_run.worker_pgid = Some(worker_pid);
            managed_run.run_dir = Some(run_dir.clone());
        }
    }

    let Some(stdin) = child.stdin.take() else {
        let message = "Worker stdin pipe was unavailable".to_string();
        tracing::error!(run_id = %run_id, "{message}");
        let _ = child.start_kill();
        let failure_event = workflow_event::Event::workflow_run_failed_from_error(
            &WorkflowError::engine(message.clone()),
            fabro_types::RunTiming::default(),
            FailureReason::LaunchFailed,
            None,
            None,
            None,
            None,
        );
        let _ = workflow_event::append_event(&run_store, &run_id, &failure_event).await;
        fail_managed_run(&state, run_id, FailureReason::LaunchFailed, message);
        state.scheduler_notify.notify_one();
        return;
    };

    let Some(stderr) = child.stderr.take() else {
        let message = "Worker stderr pipe was unavailable".to_string();
        tracing::error!(run_id = %run_id, "{message}");
        let _ = child.start_kill();
        let failure_event = workflow_event::Event::workflow_run_failed_from_error(
            &WorkflowError::engine(message.clone()),
            fabro_types::RunTiming::default(),
            FailureReason::LaunchFailed,
            None,
            None,
            None,
            None,
        );
        let _ = workflow_event::append_event(&run_store, &run_id, &failure_event).await;
        fail_managed_run(&state, run_id, FailureReason::LaunchFailed, message);
        state.scheduler_notify.notify_one();
        return;
    };

    let (control_tx, control_rx) = mpsc::channel(WORKER_CONTROL_QUEUE_CAPACITY);
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&run_id) {
            managed_run.answer_transport = Some(RunAnswerTransport::Subprocess { control_tx });
        }
    }

    let control_task = tokio::spawn(pump_worker_control_jsonl(stdin, control_rx));
    let stderr_task = tokio::spawn(drain_worker_stderr(run_id, stderr));

    let wait_status = match child.wait().await {
        Ok(status) => status,
        Err(err) => {
            tracing::error!(run_id = %run_id, error = %err, "Failed while waiting on worker");
            let message = format!("Worker wait failed: {err}");
            let _ = child.start_kill();
            let failure_event = workflow_event::Event::workflow_run_failed_from_error(
                &WorkflowError::engine_with_source("Worker wait failed", err),
                fabro_types::RunTiming::default(),
                FailureReason::Terminated,
                None,
                None,
                None,
                None,
            );
            let _ = workflow_event::append_event(&run_store, &run_id, &failure_event).await;
            fail_managed_run(&state, run_id, FailureReason::Terminated, message);
            state.scheduler_notify.notify_one();
            return;
        }
    };

    control_task.abort();
    let _ = control_task.await;

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
            .is_some_and(|managed_run| managed_run.worker_pid != Some(worker_pid))
    };
    if superseded {
        tracing::info!(
            run_id = %run_id,
            worker_pid,
            "Skipping stale worker cleanup for superseded run execution"
        );
        return;
    }

    append_worker_exit_failure(&run_store, run_id, &wait_status).await;

    let final_state = match run_store.state().await {
        Ok(state) => state,
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

    if final_state.current_checkpoint().is_some() {
        let mut agg = state
            .aggregate_billing
            .lock()
            .expect("aggregate_billing lock poisoned");
        accumulate_billing_rollup(
            &mut agg,
            &fabro_workflow::billing_rollup_from_projection(&final_state, None),
        );
    }

    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        if final_state.status != managed_run.status {
            managed_run.status = final_state.status;
        } else if !wait_status.success() {
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
        managed_run.checkpoint = final_state.current_checkpoint().cloned();
        managed_run.run_dir = Some(run_dir);
        clear_live_run_state(managed_run);
    }
    drop(runs);
    state.scheduler_notify.notify_one();
}

/// Background task that promotes queued runs when capacity is available.
pub fn spawn_scheduler(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = state.scheduler_notify.notified() => {},
                () = sleep(std::time::Duration::from_secs(1)) => {},
            }
            if state.is_shutting_down() {
                break;
            }
            // Promote as many queued runs as capacity allows
            loop {
                if state.is_shutting_down() {
                    break;
                }
                let run_to_start = {
                    let runs = state.runs.lock().expect("runs lock poisoned");
                    let active = runs
                        .values()
                        .filter(|r| {
                            matches!(
                                r.status,
                                RunStatus::Starting
                                    | RunStatus::Running
                                    | RunStatus::Blocked { .. }
                                    | RunStatus::Paused { .. }
                            )
                        })
                        .count();
                    if active >= state.max_concurrent_runs {
                        break;
                    }
                    runs.iter()
                        .filter(|(_, r)| r.status == RunStatus::Queued)
                        .min_by_key(|(_, r)| r.created_at)
                        .map(|(id, _)| *id)
                };
                match run_to_start {
                    Some(id) => {
                        let state_clone = Arc::clone(&state);
                        tokio::spawn(
                            execute_run(state_clone, id)
                                .instrument(tracing::info_span!("run", id = %id)),
                        );
                    }
                    None => break,
                }
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
    let run_store = state.store.open_run(&run_id).await?;
    let event = match action {
        RunControlAction::Cancel => workflow_event::Event::RunCancelRequested { actor },
        RunControlAction::Pause => workflow_event::Event::RunPauseRequested { actor },
        RunControlAction::Unpause => workflow_event::Event::RunUnpauseRequested { actor },
    };
    workflow_event::append_event(&run_store, &run_id, &event).await
}

/// Returns a 409 response with an actionable "unarchive first" message if the
/// run is currently archived. Returns `None` otherwise (including when the run
/// doesn't exist — the caller's own not-found handling will surface that).
async fn reject_if_archived(state: &AppState, run_id: &RunId) -> Option<Response> {
    let run_store = state.store.open_run_reader(run_id).await.ok()?;
    let projection = run_store.state().await.ok()?;
    projection.archived_at.is_some().then(|| {
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
