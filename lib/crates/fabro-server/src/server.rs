use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
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
    CreateRunPullRequestRequest, CreateSecretRequest, DeleteSecretRequest, DiskUsageResponse,
    DiskUsageRunRow, DiskUsageSummaryRow, ForkRequest, ForkResponse, MergeRunPullRequestRequest,
    MergeRunPullRequestResponse, ModelReference, PaginatedEventList, PaginatedRunList,
    PaginationMeta, PreflightResponse, PreviewUrlRequest, PreviewUrlResponse, PruneRunEntry,
    PruneRunsRequest, PruneRunsResponse, RenderWorkflowGraphDirection, RenderWorkflowGraphRequest,
    RewindRequest, RewindResponse, RunArtifactEntry, RunArtifactListResponse, RunBilling,
    RunBillingStage, RunBillingTotals, RunError, RunManifest, RunStage, RunStatusResponse,
    SandboxFileEntry, SandboxFileListResponse, SshAccessRequest, SshAccessResponse, StageState,
    StartRunRequest, SubmitAnswerRequest, SystemFeatures, SystemInfoResponse, SystemRunCounts,
    TimelineEntryResponse, WriteBlobResponse,
};
use fabro_auth::{
    CredentialSource, VaultCredentialSource, auth_issue_message, parse_credential_secret,
};
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
use fabro_model::{BilledModelUsage, BilledTokenCounts, Catalog, ModelTestMode, Provider};
use fabro_redact::redact_jsonl_line;
use fabro_sandbox::daytona::DaytonaSandbox;
use fabro_sandbox::reconnect::reconnect;
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
    CommandOutputStream, EventBody, InterviewQuestionRecord, Principal, PullRequestRecord,
    QuestionType, RunBlobId, RunClientProvenance, RunControlAction, RunEvent, RunId, RunProvenance,
    RunServerProvenance, ServerSettings, UserPrincipal, parse_blob_ref,
};
use fabro_util::error::{SharedError, collect_causes, render_with_causes};
use fabro_util::version::FABRO_VERSION;
use fabro_vault::{Error as VaultError, SecretType, Vault};
use fabro_workflow::artifact_upload::ArtifactSink;
use fabro_workflow::command_log::{command_log_path, read_json_string_blob, read_log_slice};
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
    AuthContextSlot, RequestAuth, RequestAuthContext, RequireCommandLog, RequireRunBlob,
    RequireRunScoped, RequireStageArtifact, RequiredUser, principal_middleware, require_user,
};
use crate::request_id::{self, RequestId};
use crate::run_files::{FilesInFlight, list_run_files, new_files_in_flight};
use crate::run_selector::{ResolveRunError, resolve_run_by_selector};
use crate::server_secrets::{LlmClientResult, ServerSecrets};
use crate::spawn_env::{apply_render_graph_env, apply_worker_env};
use crate::worker_token::{WorkerTokenKeys, issue_worker_token};
use crate::{
    canonical_host, demo, diagnostics, run_manifest, security_headers, static_files, web_auth,
};

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

#[derive(serde::Deserialize)]
struct ListRunsParams {
    #[serde(rename = "page[limit]", default = "default_page_limit")]
    limit:            u32,
    #[serde(rename = "page[offset]", default)]
    offset:           u32,
    #[serde(default)]
    include_archived: bool,
}

impl ListRunsParams {
    fn pagination(&self) -> PaginationParams {
        PaginationParams {
            limit:  self.limit,
            offset: self.offset,
        }
    }
}

#[derive(serde::Deserialize)]
struct ModelListParams {
    #[serde(rename = "page[limit]", default = "default_page_limit")]
    limit:    u32,
    #[serde(rename = "page[offset]", default)]
    offset:   u32,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    query:    Option<String>,
}

#[derive(serde::Deserialize)]
struct ModelTestParams {
    #[serde(default)]
    mode: Option<String>,
}

#[derive(serde::Deserialize)]
struct EventListParams {
    #[serde(default)]
    since_seq: Option<u32>,
    #[serde(default)]
    limit:     Option<usize>,
}

impl EventListParams {
    fn since_seq(&self) -> u32 {
        self.since_seq.unwrap_or(1).max(1)
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 1000)
    }
}

#[derive(serde::Deserialize)]
struct AttachParams {
    #[serde(default)]
    since_seq: Option<u32>,
}

#[derive(serde::Deserialize)]
pub(crate) struct DfParams {
    #[serde(default)]
    pub(crate) verbose: bool,
}

#[derive(serde::Deserialize)]
struct GlobalAttachParams {
    #[serde(default)]
    run_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct ArtifactFilenameParams {
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    retry:    Option<u32>,
}

#[derive(serde::Deserialize)]
struct SandboxFilesParams {
    path:  String,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(serde::Deserialize)]
struct SandboxFileParams {
    path: String,
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
    dot_source:         String,
    status:             RunStatus,
    error:              Option<String>,
    created_at:         chrono::DateTime<chrono::Utc>,
    enqueued_at:        Instant,
    // Populated when running:
    answer_transport:   Option<RunAnswerTransport>,
    accepted_questions: HashSet<String>,
    event_tx:           Option<broadcast::Sender<RunEvent>>,
    checkpoint:         Option<Checkpoint>,
    cancel_tx:          Option<oneshot::Sender<()>>,
    cancel_token:       Option<Arc<AtomicBool>>,
    worker_pid:         Option<u32>,
    worker_pgid:        Option<u32>,
    run_dir:            Option<std::path::PathBuf>,
    execution_mode:     RunExecutionMode,
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
const MAX_SINGLE_ARTIFACT_BYTES: u64 = 10 * 1024 * 1024;
const MAX_MULTIPART_ARTIFACTS: usize = 100;
const RENDER_ERROR_PREFIX: &[u8] = b"RENDER_ERROR:";
const GRAPHVIZ_RENDER_CONCURRENCY_LIMIT: usize = 4;

static GRAPHVIZ_RENDER_SEMAPHORE: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(GRAPHVIZ_RENDER_CONCURRENCY_LIMIT));

#[derive(Debug, thiserror::Error)]
enum RenderSubprocessError {
    #[error("failed to spawn render subprocess: {0}")]
    SpawnFailed(String),
    #[error("render subprocess crashed: {0}")]
    ChildCrashed(String),
    #[error("render subprocess returned invalid output: {0}")]
    ProtocolViolation(String),
    #[error("{0}")]
    RenderFailed(String),
}
const MAX_MULTIPART_REQUEST_BYTES: u64 = 50 * 1024 * 1024;
const MAX_MULTIPART_MANIFEST_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ArtifactBatchUploadManifest {
    entries: Vec<ArtifactBatchUploadEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ArtifactBatchUploadEntry {
    part:           String,
    path:           String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_type:   Option<String>,
}

/// Per-model billing totals.
#[derive(Default)]
struct ModelBillingTotals {
    stages:  i64,
    billing: BilledTokenCounts,
}

/// In-memory aggregate billing counters, reset on server restart.
#[derive(Default)]
struct BillingAccumulator {
    total_runs:         i64,
    total_runtime_secs: f64,
    by_model:           HashMap<String, ModelBillingTotals>,
}

pub(crate) type RegistryFactoryOverride =
    dyn Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync;

#[derive(Clone)]
enum RunAnswerTransport {
    Subprocess {
        control_tx: mpsc::Sender<WorkerControlEnvelope>,
    },
    InProcess {
        interviewer: Arc<ControlInterviewer>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerTransportError {
    Closed,
    Timeout,
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
            Self::InProcess { interviewer } => interviewer
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
            Self::InProcess { interviewer } => {
                interviewer.cancel_all().await;
                Ok(())
            }
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

    async fn handle_event(&self, event: &RunEvent) {
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
    artifact_store: ArtifactStore,
    worker_tokens: WorkerTokenKeys,
    started_at: Instant,
    max_concurrent_runs: usize,
    scheduler_notify: Notify,
    global_event_tx: broadcast::Sender<EventEnvelope>,
    /// Per-run coalescing registry for `GET /runs/{id}/files`. Concurrent
    /// callers for the same run share one materialization; different runs
    /// proceed in parallel. See `crate::run_files` for semantics.
    pub(crate) files_in_flight: FilesInFlight,
    pull_request_create_locks: PullRequestCreateLocks,

    pub(crate) vault:               Arc<AsyncRwLock<Vault>>,
    pub(super) server_secrets:      ServerSecrets,
    pub(crate) llm_source:          Arc<dyn CredentialSource>,
    manifest_run_defaults:          RwLock<Arc<RunLayer>>,
    manifest_run_settings:          RwLock<std::result::Result<RunNamespace, SharedError>>,
    pub(crate) server_settings:     RwLock<Arc<ServerSettings>>,
    pub(crate) env_lookup:          EnvLookup,
    pub(crate) github_api_base_url: String,
    http_client:                    Option<fabro_http::HttpClient>,
    shutting_down:                  AtomicBool,
    registry_factory_override:      Option<Box<RegistryFactoryOverride>>,
    slack_service:                  Option<Arc<SlackService>>,
    slack_started:                  AtomicBool,
}

type PullRequestCreateLocks = Arc<Mutex<HashMap<RunId, Arc<AsyncMutex<()>>>>>;

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
    pub(crate) http_client:               Option<fabro_http::HttpClient>,
}

#[derive(Clone)]
pub(crate) struct ResolvedAppStateSettings {
    pub(crate) server_settings:       ServerSettings,
    pub(crate) manifest_run_defaults: RunLayer,
    pub(crate) manifest_run_settings: std::result::Result<RunNamespace, SharedError>,
}

fn accumulate_model_billing(entry: &mut ModelBillingTotals, usage: &BilledModelUsage) {
    let tokens = usage.tokens();
    entry.stages += 1;
    entry.billing.input_tokens += tokens.input_tokens;
    entry.billing.output_tokens += tokens.output_tokens;
    entry.billing.reasoning_tokens += tokens.reasoning_tokens;
    entry.billing.cache_read_tokens += tokens.cache_read_tokens;
    entry.billing.cache_write_tokens += tokens.cache_write_tokens;
    entry.billing.total_tokens += tokens.total_tokens();
    if let Some(value) = usage.total_usd_micros {
        *entry.billing.total_usd_micros.get_or_insert(0) += value;
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

    pub(crate) async fn resolve_llm_client(&self) -> anyhow::Result<LlmClientResult> {
        resolve_llm_client_from_source(self.llm_source.as_ref()).await
    }

    pub(crate) fn vault_or_env(&self, name: &str) -> Option<String> {
        process_env_var(name).or_else(|| {
            self.vault
                .try_read()
                .ok()
                .and_then(|vault| vault.get(name).map(str::to_string))
        })
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
                    Some(token) => Ok(Some(fabro_github::GitHubCredentials::Token(token))),
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
        } = resolved_settings;
        let server_settings = Arc::new(server_settings);
        let manifest_run_defaults = Arc::new(manifest_run_defaults);
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
        Ok(())
    }
}

async fn resolve_llm_client_from_source(
    source: &dyn CredentialSource,
) -> anyhow::Result<LlmClientResult> {
    let resolved = source
        .resolve()
        .await
        .context("resolving LLM credentials")?;
    let client = LlmClient::from_credentials(resolved.credentials)
        .await
        .context("creating LLM client")?;

    Ok(LlmClientResult {
        client,
        auth_issues: resolved.auth_issues,
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
                    event_service.handle_event(&envelope.event).await;
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
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            web_enabled:                 true,
            static_asset_root:           None,
            github_endpoints:            None,
            github_webhook_ip_allowlist: None,
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
            .route("/openapi.json", get(openapi_spec))
            .merge(web_auth::api_routes())
    } else {
        Router::new().route("/openapi.json", get(openapi_spec))
    };

    let demo_router = Router::new()
        .nest(
            "/api/v1",
            api_common
                .clone()
                .merge(demo_routes())
                .layer(principal_layer.clone()),
        )
        .layer(axum::Extension(auth_mode.clone()))
        .layer(axum::Extension(Arc::clone(&github_endpoints)))
        .with_state(state.clone());

    let mut real_router = Router::new().nest(
        "/api/v1",
        api_common.merge(real_routes()).layer(principal_layer),
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
        .route("/health", get(health))
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

fn demo_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs", get(demo::list_runs).post(demo::create_run_stub))
        .route("/runs/resolve", get(demo::resolve_run))
        .route("/boards/runs", get(demo::list_board_runs))
        .route("/preflight", post(run_preflight))
        .route("/validate", post(validate_run_manifest))
        .route("/graph/render", post(render_graph_from_manifest))
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
            "/runs/{id}/stages/{stageId}/logs/{stream}",
            get(not_implemented),
        )
        .route("/runs/{id}/checkpoint", get(demo::checkpoint_stub))
        .route("/runs/{id}/cancel", post(demo::cancel_stub))
        .route("/runs/{id}/start", post(demo::start_run_stub))
        .route("/runs/{id}/pause", post(demo::pause_stub))
        .route("/runs/{id}/unpause", post(demo::unpause_stub))
        .route("/runs/{id}/graph", get(demo::get_run_graph))
        .route("/runs/{id}/graph/source", get(demo::get_run_graph_source))
        .route("/runs/{id}/stages", get(demo::get_run_stages))
        .route("/runs/{id}/artifacts", get(demo::list_run_artifacts_stub))
        .route("/runs/{id}/files", get(demo::list_run_files_stub))
        .route(
            "/runs/{id}/stages/{stageId}/turns",
            get(demo::get_stage_turns),
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
        .route("/models", get(list_models))
        .route("/models/{id}/test", post(test_model))
        .route(
            "/secrets",
            get(demo::list_secrets)
                .post(demo::create_secret)
                .delete(demo::delete_secret_by_name),
        )
        .route("/repos/github/{owner}/{name}", get(demo::get_github_repo))
        .route("/health/diagnostics", post(demo::run_diagnostics))
        .route("/completions", post(create_completion))
        .route("/settings", get(demo::get_server_settings))
        .route("/system/info", get(demo::get_system_info))
        .route("/system/df", get(demo::get_system_disk_usage))
        .route("/system/prune/runs", post(demo::prune_runs))
        .route("/billing", get(demo::get_aggregate_billing))
}

fn real_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs", get(list_runs).post(create_run))
        .route("/runs/resolve", get(resolve_run))
        .route("/preflight", post(run_preflight))
        .route("/validate", post(validate_run_manifest))
        .route("/graph/render", post(render_graph_from_manifest))
        .route("/attach", get(attach_events))
        .route("/boards/runs", get(list_board_runs))
        .route("/runs/{id}", get(get_run_status).delete(delete_run))
        .route("/runs/{id}/questions", get(get_questions))
        .route("/runs/{id}/questions/{qid}/answer", post(submit_answer))
        .route("/runs/{id}/state", get(get_run_state))
        .route("/runs/{id}/logs", get(get_run_logs))
        .route(
            "/runs/{id}/pull_request",
            get(get_run_pull_request).post(create_run_pull_request),
        )
        .route(
            "/runs/{id}/pull_request/merge",
            post(merge_run_pull_request),
        )
        .route(
            "/runs/{id}/pull_request/close",
            post(close_run_pull_request),
        )
        .route(
            "/runs/{id}/events",
            get(list_run_events).post(append_run_event),
        )
        .route("/runs/{id}/attach", get(attach_run_events))
        .route("/runs/{id}/blobs", post(write_run_blob))
        .route("/runs/{id}/blobs/{blobId}", get(read_run_blob))
        .route(
            "/runs/{id}/stages/{stageId}/logs/{stream}",
            get(get_run_stage_command_log),
        )
        .route("/runs/{id}/checkpoint", get(get_checkpoint))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/start", post(start_run))
        .route("/runs/{id}/pause", post(pause_run))
        .route("/runs/{id}/unpause", post(unpause_run))
        .route("/runs/{id}/archive", post(archive_run))
        .route("/runs/{id}/rewind", post(rewind_run))
        .route("/runs/{id}/fork", post(fork_run))
        .route("/runs/{id}/timeline", get(run_timeline))
        .route("/runs/{id}/unarchive", post(unarchive_run))
        .route("/runs/{id}/graph", get(get_graph))
        .route("/runs/{id}/graph/source", get(get_graph_source))
        .route("/runs/{id}/stages", get(list_run_stages))
        .route("/runs/{id}/artifacts", get(list_run_artifacts))
        .route("/runs/{id}/files", get(list_run_files))
        .route("/runs/{id}/stages/{stageId}/turns", get(not_implemented))
        .route(
            "/runs/{id}/stages/{stageId}/artifacts",
            get(list_stage_artifacts)
                .post(put_stage_artifact)
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/runs/{id}/stages/{stageId}/artifacts/download",
            get(get_stage_artifact),
        )
        .route("/runs/{id}/billing", get(get_run_billing))
        .route("/runs/{id}/settings", get(get_run_settings))
        .route("/runs/{id}/steer", post(not_implemented))
        .route("/runs/{id}/preview", post(generate_preview_url))
        .route("/runs/{id}/ssh", post(create_ssh_access))
        .route("/runs/{id}/sandbox/files", get(list_sandbox_files))
        .route(
            "/runs/{id}/sandbox/file",
            get(get_sandbox_file).put(put_sandbox_file),
        )
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
        .route("/models", get(list_models))
        .route("/models/{id}/test", post(test_model))
        .route(
            "/secrets",
            get(list_secrets)
                .post(create_secret)
                .delete(delete_secret_by_name),
        )
        .route("/repos/github/{owner}/{name}", get(get_github_repo))
        .route("/health/diagnostics", post(run_diagnostics))
        .route("/completions", post(create_completion))
        .route("/settings", get(get_server_settings))
        .route("/system/info", get(get_system_info))
        .route("/system/df", get(get_system_df))
        .route("/system/prune/runs", post(prune_runs))
        .route("/billing", get(get_aggregate_billing))
}

async fn not_implemented() -> Response {
    ApiError::new(StatusCode::NOT_IMPLEMENTED, "Not implemented.").into_response()
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

async fn health() -> Response {
    Json(serde_json::json!({
        "status": "ok",
    }))
    .into_response()
}

async fn get_server_settings(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    (
        StatusCode::OK,
        Json(state.server_settings().as_ref().clone()),
    )
        .into_response()
}

async fn get_system_info(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    let manifest_run_settings = state.manifest_run_settings();
    let server_settings = state.server_settings();
    let (total_runs, active_runs) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        let active = runs
            .values()
            .filter(|run| {
                matches!(
                    run.status,
                    RunStatus::Queued
                        | RunStatus::Starting
                        | RunStatus::Running
                        | RunStatus::Blocked { .. }
                        | RunStatus::Paused { .. }
                )
            })
            .count();
        (runs.len(), active)
    };

    let response = SystemInfoResponse {
        version:          Some(FABRO_VERSION.to_string()),
        server_url:       Some(server_settings.server.web.url.as_source()),
        git_sha:          option_env!("FABRO_GIT_SHA").map(str::to_string),
        build_date:       option_env!("FABRO_BUILD_DATE").map(str::to_string),
        profile:          option_env!("FABRO_BUILD_PROFILE").map(str::to_string),
        os:               Some(std::env::consts::OS.to_string()),
        arch:             Some(std::env::consts::ARCH.to_string()),
        storage_engine:   Some("slatedb".to_string()),
        storage_dir:      Some(state.server_storage_dir().display().to_string()),
        uptime_secs:      Some(to_i64(state.started_at.elapsed().as_secs())),
        runs:             Some(SystemRunCounts {
            total:  Some(to_i64(total_runs)),
            active: Some(to_i64(active_runs)),
        }),
        sandbox_provider: Some(system_sandbox_provider(&manifest_run_settings)),
        features:         Some(system_features(
            server_settings.as_ref(),
            &manifest_run_settings,
        )),
    };
    (StatusCode::OK, Json(response)).into_response()
}

fn system_features(
    server_settings: &ServerSettings,
    manifest_run_settings: &std::result::Result<RunNamespace, SharedError>,
) -> SystemFeatures {
    let session_sandboxes = server_settings.features.session_sandboxes;
    let retros = manifest_run_settings
        .as_ref()
        .is_ok_and(|settings| settings.execution.retros);
    SystemFeatures {
        session_sandboxes: Some(session_sandboxes),
        retros:            Some(retros),
    }
}

async fn get_system_df(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<DfParams>,
) -> Response {
    let storage_dir = state.server_storage_dir();
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(summaries) => summaries,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let response = match spawn_blocking(move || {
        build_disk_usage_response(&summaries, &storage_dir, params.verbose)
    })
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    (StatusCode::OK, Json(response)).into_response()
}

async fn prune_runs(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<PruneRunsRequest>,
) -> Response {
    let storage_dir = state.server_storage_dir();
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(summaries) => summaries,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let dry_run = body.dry_run;
    let body_for_plan = body.clone();
    let prune_plan =
        match spawn_blocking(move || build_prune_plan(&body_for_plan, &summaries, &storage_dir))
            .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(err)) => {
                return ApiError::new(StatusCode::BAD_REQUEST, err.to_string()).into_response();
            }
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        };

    if dry_run {
        return (
            StatusCode::OK,
            Json(PruneRunsResponse {
                dry_run:          Some(true),
                runs:             Some(prune_plan.rows),
                total_count:      Some(to_i64(prune_plan.run_ids.len())),
                total_size_bytes: Some(to_i64(prune_plan.total_size_bytes)),
                deleted_count:    Some(0),
                freed_bytes:      Some(0),
            }),
        )
            .into_response();
    }

    for run_id in &prune_plan.run_ids {
        if let Err(response) = delete_run_internal(&state, *run_id, true).await {
            return response;
        }
    }

    (
        StatusCode::OK,
        Json(PruneRunsResponse {
            dry_run:          Some(false),
            runs:             None,
            total_count:      Some(to_i64(prune_plan.run_ids.len())),
            total_size_bytes: Some(to_i64(prune_plan.total_size_bytes)),
            deleted_count:    Some(to_i64(prune_plan.run_ids.len())),
            freed_bytes:      Some(to_i64(prune_plan.total_size_bytes)),
        }),
    )
        .into_response()
}

async fn attach_events(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<GlobalAttachParams>,
) -> Response {
    let run_filter = match parse_global_run_filter(params.run_id.as_deref()) {
        Ok(filter) => filter,
        Err(err) => return ApiError::new(StatusCode::BAD_REQUEST, err).into_response(),
    };

    let stream =
        filtered_global_events(state.global_event_tx.subscribe(), run_filter).filter_map(|event| {
            sse_event_from_store(&event).map(Ok::<Event, std::convert::Infallible>)
        });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn filtered_global_events(
    event_rx: broadcast::Receiver<EventEnvelope>,
    run_filter: Option<HashSet<RunId>>,
) -> impl tokio_stream::Stream<Item = EventEnvelope> {
    BroadcastStream::new(event_rx).filter_map(move |result| match result {
        Ok(event) if event_matches_run_filter(&event, run_filter.as_ref()) => Some(event),
        Ok(_) | Err(_) => None,
    })
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
    summaries: &[fabro_types::RunSummary],
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
                workflow_name: Some(run.workflow_name()),
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
        ],
        total_size_bytes:        Some(to_i64(total_run_size + total_log_size)),
        total_reclaimable_bytes: Some(to_i64(reclaimable_run_size + total_log_size)),
        runs:                    verbose.then_some(run_rows),
    })
}

fn build_prune_plan(
    request: &PruneRunsRequest,
    summaries: &[fabro_types::RunSummary],
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
            workflow_name: Some(run.workflow_name()),
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

fn parse_global_run_filter(raw: Option<&str>) -> Result<Option<HashSet<RunId>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };

    let mut run_ids = HashSet::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let run_id = part
            .parse::<RunId>()
            .map_err(|err| format!("invalid run_id '{part}': {err}"))?;
        run_ids.insert(run_id);
    }

    if run_ids.is_empty() {
        Ok(None)
    } else {
        Ok(Some(run_ids))
    }
}

fn event_matches_run_filter(event: &EventEnvelope, run_filter: Option<&HashSet<RunId>>) -> bool {
    let Some(run_filter) = run_filter else {
        return true;
    };
    run_filter.contains(&event.event.run_id)
}

fn sse_event_from_store(event: &EventEnvelope) -> Option<Event> {
    let data = serde_json::to_string(event).ok()?;
    let data = redact_jsonl_line(&data);
    Some(Event::default().data(data))
}

fn attach_event_is_terminal(event: &EventEnvelope) -> bool {
    matches!(
        &event.event.body,
        EventBody::RunCompleted(_) | EventBody::RunFailed(_)
    )
}

fn run_projection_is_active(state: &fabro_store::RunProjection) -> bool {
    state.status.is_some_and(RunStatus::is_active)
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

async fn list_secrets(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    let data = state.vault.read().await.list();
    (StatusCode::OK, Json(serde_json::json!({ "data": data }))).into_response()
}

async fn create_secret(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSecretRequest>,
) -> Response {
    let secret_type = body.type_;
    let name = body.name;
    let value = body.value;
    let description = body.description;
    if secret_type == SecretType::Credential {
        if let Err(err) = parse_credential_secret(&name, &value) {
            return ApiError::bad_request(err).into_response();
        }
    }
    let state_for_write = Arc::clone(&state);
    let result = spawn_blocking(move || {
        let mut vault = state_for_write.vault.blocking_write();
        vault.set(&name, &value, secret_type, description.as_deref())
    })
    .await;

    match result {
        Ok(Ok(meta)) => (StatusCode::OK, Json(meta)).into_response(),
        Ok(Err(VaultError::InvalidName(_))) => {
            ApiError::bad_request("invalid secret name").into_response()
        }
        Ok(Err(VaultError::Io(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(Err(VaultError::Serde(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(Err(VaultError::NotFound(_))) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secret unexpectedly missing",
        )
        .into_response(),
        Err(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("secret write task failed: {err}"),
        )
        .into_response(),
    }
}

async fn delete_secret_by_name(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<DeleteSecretRequest>,
) -> Response {
    let name = body.name;
    let state_for_write = Arc::clone(&state);
    let result = spawn_blocking(move || {
        let mut vault = state_for_write.vault.blocking_write();
        vault.remove(&name)
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(VaultError::InvalidName(_))) => {
            ApiError::bad_request("invalid secret name").into_response()
        }
        Ok(Err(VaultError::NotFound(name))) => {
            ApiError::new(StatusCode::NOT_FOUND, format!("secret not found: {name}"))
                .into_response()
        }
        Ok(Err(VaultError::Io(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(Err(VaultError::Serde(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Err(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("secret delete task failed: {err}"),
        )
        .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct GitHubRepoResponse {
    default_branch: String,
    private:        bool,
    permissions:    Option<serde_json::Value>,
}

/// Reject owner/repo path segments that could rewrite the GitHub API endpoint
/// via `..` traversal after URL normalization. Conservative compared to
/// GitHub's real rules, which is fine for server-side input validation.
#[allow(
    clippy::result_large_err,
    reason = "GitHub slug validation returns HTTP 400 responses directly."
)]
fn validate_github_slug(kind: &str, value: &str, max_len: usize) -> Result<(), Response> {
    if value.is_empty() || value.len() > max_len || matches!(value, "." | "..") {
        return Err(ApiError::bad_request(format!("invalid github {kind}")).into_response());
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::bad_request(format!("invalid github {kind}")).into_response());
    }
    Ok(())
}

async fn get_github_repo(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((owner, name)): Path<(String, String)>,
) -> Response {
    if let Err(response) = validate_github_slug("owner", &owner, 39) {
        return response;
    }
    if let Err(response) = validate_github_slug("repo", &name, 100) {
        return response;
    }
    let settings = state.server_settings();
    let github_settings = &settings.server.integrations.github;
    let base_url = fabro_github::github_api_base_url();
    let mut client: Option<fabro_http::HttpClient> = None;
    let token = match github_settings.strategy {
        GithubIntegrationStrategy::App => {
            let Some(app_id) = github_settings.app_id.as_ref() else {
                return ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "server.integrations.github.app_id is not configured",
                )
                .into_response();
            };
            if let Err(err) = resolve_interp_string(app_id) {
                return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                    .into_response();
            }
            let creds = match state.github_credentials(github_settings) {
                Ok(Some(fabro_github::GitHubCredentials::App(creds))) => creds,
                Ok(Some(_)) => unreachable!("app strategy should not return token credentials"),
                Ok(None) => {
                    return ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "GITHUB_APP_PRIVATE_KEY is not configured",
                    )
                    .into_response();
                }
                Err(err) => {
                    return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err).into_response();
                }
            };

            let jwt = match fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem) {
                Ok(jwt) => jwt,
                Err(err) => {
                    tracing::error!(error = ?err, "failed to sign GitHub App JWT");
                    return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                        .into_response();
                }
            };
            let install_url = match github_settings.slug.as_ref() {
                Some(slug) => match resolve_interp_string(slug) {
                    Ok(slug) => format!("https://github.com/apps/{slug}/installations/new"),
                    Err(err) => {
                        return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                            .into_response();
                    }
                },
                None => format!("https://github.com/organizations/{owner}/settings/installations"),
            };

            if client.is_none() {
                client = Some(match state.http_client() {
                    Ok(http) => http,
                    Err(err) => {
                        return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                            .into_response();
                    }
                });
            }
            let client_ref = client.as_ref().expect("client initialized above");
            let installed =
                match fabro_github::check_app_installed(client_ref, &jwt, &owner, &name, &base_url)
                    .await
                {
                    Ok(installed) => installed,
                    Err(err) => {
                        tracing::error!(error = ?err, "failed to check GitHub App installation");
                        return ApiError::new(StatusCode::BAD_GATEWAY, err.to_string())
                            .into_response();
                    }
                };

            if !installed {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "owner": owner,
                        "name": name,
                        "accessible": false,
                        "default_branch": null,
                        "private": null,
                        "permissions": null,
                        "install_url": install_url,
                    })),
                )
                    .into_response();
            }

            match fabro_github::create_installation_access_token_with_permissions_and_install_url(
                client_ref,
                &jwt,
                &owner,
                &name,
                &base_url,
                serde_json::json!({ "contents": "write", "pull_requests": "write" }),
                Some(&install_url),
            )
            .await
            {
                Ok(token) => token,
                Err(err) => {
                    tracing::error!(
                        error = ?err,
                        "failed to create GitHub App installation token"
                    );
                    return ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response();
                }
            }
        }
        GithubIntegrationStrategy::Token => match state.github_credentials(github_settings) {
            Ok(Some(fabro_github::GitHubCredentials::Token(token))) => token,
            Ok(Some(_)) => unreachable!("token strategy should not return app credentials"),
            Ok(None) => {
                return ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "GITHUB_TOKEN is not configured",
                )
                .into_response();
            }
            Err(err) => {
                return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err).into_response();
            }
        },
    };

    let client = match client {
        Some(client) => client,
        None => match state.http_client() {
            Ok(http) => http,
            Err(err) => {
                return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                    .into_response();
            }
        },
    };
    let repo_response = match client
        .get(format!("{base_url}/repos/{owner}/{name}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro-server")
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        Ok(response)
            if github_settings.strategy == GithubIntegrationStrategy::Token
                && matches!(
                    response.status(),
                    fabro_http::StatusCode::FORBIDDEN | fabro_http::StatusCode::NOT_FOUND
                ) =>
        {
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "owner": owner,
                    "name": name,
                    "accessible": false,
                    "default_branch": null,
                    "private": null,
                    "permissions": null,
                    "install_url": serde_json::Value::Null,
                })),
            )
                .into_response();
        }
        Ok(response)
            if github_settings.strategy == GithubIntegrationStrategy::Token
                && response.status() == fabro_http::StatusCode::UNAUTHORIZED =>
        {
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "Stored GitHub token is invalid — run fabro install or update GITHUB_TOKEN",
            )
            .into_response();
        }
        Ok(response) => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("GitHub repo lookup failed: {}", response.status()),
            )
            .into_response();
        }
        Err(err) => return ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
    };

    let repo = match repo_response.json::<GitHubRepoResponse>().await {
        Ok(repo) => repo,
        Err(err) => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("Failed to parse GitHub repo response: {err}"),
            )
            .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "owner": owner,
            "name": name,
            "accessible": true,
            "default_branch": repo.default_branch,
            "private": repo.private,
            "permissions": repo.permissions,
            "install_url": serde_json::Value::Null,
        })),
    )
        .into_response()
}

async fn run_diagnostics(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    (
        StatusCode::OK,
        Json(diagnostics::run_all(state.as_ref()).await),
    )
        .into_response()
}

async fn openapi_spec() -> Response {
    let yaml = include_str!("../../../../docs/public/api-reference/fabro-api.yaml");
    let value: serde_json::Value =
        serde_yaml::from_str(yaml).expect("embedded OpenAPI YAML is invalid");
    Json(value).into_response()
}

fn active_stage_state_from_events(events: &[EventEnvelope], node_id: &str) -> StageState {
    let latest = events.iter().rev().find(|envelope| {
        envelope.event.node_id.as_deref() == Some(node_id)
            && matches!(
                envelope.event.event_name(),
                "stage.retrying" | "stage.started" | "stage.completed" | "stage.failed"
            )
    });

    if latest.is_some_and(|e| e.event.event_name() == "stage.retrying") {
        StageState::Retrying
    } else {
        StageState::Running
    }
}

async fn get_aggregate_billing(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
) -> Response {
    let agg = state
        .aggregate_billing
        .lock()
        .expect("aggregate_billing lock poisoned");
    let by_model: Vec<BillingByModel> = agg
        .by_model
        .iter()
        .map(|(model, totals)| BillingByModel {
            billing: totals.billing.clone(),
            model:   ModelReference { id: model.clone() },
            stages:  totals.stages,
        })
        .collect();
    let total_billing =
        agg.by_model
            .values()
            .fold(BilledTokenCounts::default(), |mut acc, totals| {
                let billing = &totals.billing;
                acc.input_tokens += billing.input_tokens;
                acc.output_tokens += billing.output_tokens;
                acc.reasoning_tokens += billing.reasoning_tokens;
                acc.cache_read_tokens += billing.cache_read_tokens;
                acc.cache_write_tokens += billing.cache_write_tokens;
                acc.total_tokens += billing.total_tokens;
                if let Some(value) = billing.total_usd_micros {
                    *acc.total_usd_micros.get_or_insert(0) += value;
                }
                acc
            });
    let response = AggregateBilling {
        totals: AggregateBillingTotals {
            cache_read_tokens:  total_billing.cache_read_tokens,
            cache_write_tokens: total_billing.cache_write_tokens,
            input_tokens:       total_billing.input_tokens,
            output_tokens:      total_billing.output_tokens,
            reasoning_tokens:   total_billing.reasoning_tokens,
            runs:               agg.total_runs,
            runtime_secs:       agg.total_runtime_secs,
            total_tokens:       total_billing.total_tokens,
            total_usd_micros:   total_billing.total_usd_micros,
        },
        by_model,
    };
    (StatusCode::OK, Json(response)).into_response()
}

async fn list_run_stages(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(_pagination): Query<PaginationParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    // Try live run first.
    let (checkpoint, run_is_active) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => {
                let active = !matches!(
                    managed_run.status,
                    RunStatus::Succeeded { .. } | RunStatus::Failed { .. } | RunStatus::Dead
                );
                (managed_run.checkpoint.clone(), active)
            }
            None => (None, false),
        }
    };

    // Fall back to stored run.
    let (checkpoint, run_is_active) = if checkpoint.is_some() {
        (checkpoint, run_is_active)
    } else {
        match state.store.open_run_reader(&id).await {
            Ok(run_store) => match run_store.state().await {
                Ok(run_state) => {
                    let active = run_state.status.is_some_and(|status| !status.is_terminal());
                    (run_state.checkpoint, active)
                }
                Err(_) => (None, false),
            },
            Err(_) => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    let Some(checkpoint) = checkpoint else {
        return (
            StatusCode::OK,
            Json(ListResponse::new(Vec::<RunStage>::new())),
        )
            .into_response();
    };

    let events = match state.store.open_run_reader(&id).await {
        Ok(run_store) => run_store.list_events().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let stage_durations = fabro_workflow::extract_stage_durations_from_events(&events);

    let mut stages = Vec::new();
    for node_id in &checkpoint.completed_nodes {
        let duration_ms = stage_durations.get(node_id).copied().unwrap_or(0);
        let status = match checkpoint.node_outcomes.get(node_id) {
            Some(outcome) => StageState::from(outcome.status),
            None => StageState::Succeeded,
        };
        stages.push(RunStage {
            id: node_id.clone(),
            name: node_id.clone(),
            status,
            duration_secs: Some(duration_ms as f64 / 1000.0),
            dot_id: Some(node_id.clone()),
        });
    }

    // Add next node as running if the run is still active.
    // The checkpoint's current_node is the last *completed* stage; next_node_id
    // is the stage that is currently executing.
    if let Some(next_id) = &checkpoint.next_node_id {
        if run_is_active && next_id != "exit" && !checkpoint.completed_nodes.contains(next_id) {
            stages.push(RunStage {
                id:            next_id.clone(),
                name:          next_id.clone(),
                status:        active_stage_state_from_events(&events, next_id),
                duration_secs: None,
                dot_id:        Some(next_id.clone()),
            });
        }
    }

    (StatusCode::OK, Json(ListResponse::new(stages))).into_response()
}

async fn get_run_billing(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<RunId>,
) -> Response {
    let run_store = match state.store.open_run_reader(&id).await {
        Ok(run_store) => run_store,
        Err(err) => {
            return ApiError::new(StatusCode::NOT_FOUND, err.to_string()).into_response();
        }
    };

    let checkpoint = match run_store.state().await {
        Ok(state) => state.checkpoint,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let Some(checkpoint) = checkpoint else {
        let empty = RunBilling {
            by_model: Vec::new(),
            stages:   Vec::new(),
            totals:   RunBillingTotals {
                cache_read_tokens:  0,
                cache_write_tokens: 0,
                input_tokens:       0,
                output_tokens:      0,
                reasoning_tokens:   0,
                runtime_secs:       0.0,
                total_tokens:       0,
                total_usd_micros:   None,
            },
        };
        return (StatusCode::OK, Json(empty)).into_response();
    };

    let stage_durations = match run_store.list_events().await {
        Ok(events) => fabro_workflow::extract_stage_durations_from_events(&events),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let mut by_model_totals = HashMap::<String, ModelBillingTotals>::new();
    let mut billed_usages = Vec::new();
    let mut runtime_secs = 0.0_f64;
    let mut stages = Vec::new();

    for node_id in &checkpoint.completed_nodes {
        let duration_ms = stage_durations.get(node_id).copied().unwrap_or(0);
        runtime_secs += duration_ms as f64 / 1000.0;

        let Some(usage) = checkpoint
            .node_outcomes
            .get(node_id)
            .and_then(|outcome| outcome.usage.as_ref())
        else {
            continue;
        };

        billed_usages.push(usage.clone());
        let tokens = usage.tokens();
        let billing = BilledTokenCounts {
            cache_read_tokens:  tokens.cache_read_tokens,
            cache_write_tokens: tokens.cache_write_tokens,
            input_tokens:       tokens.input_tokens,
            output_tokens:      tokens.output_tokens,
            reasoning_tokens:   tokens.reasoning_tokens,
            total_tokens:       tokens.total_tokens(),
            total_usd_micros:   usage.total_usd_micros,
        };
        let model_id = usage.model_id().to_string();
        accumulate_model_billing(by_model_totals.entry(model_id.clone()).or_default(), usage);
        stages.push(RunBillingStage {
            billing,
            model: ModelReference { id: model_id },
            runtime_secs: duration_ms as f64 / 1000.0,
            stage: BillingStageRef {
                id:   node_id.clone(),
                name: node_id.clone(),
            },
        });
    }

    let totals = BilledTokenCounts::from_billed_usage(&billed_usages);
    let by_model = by_model_totals
        .into_iter()
        .map(|(model, totals)| BillingByModel {
            billing: totals.billing,
            model:   ModelReference { id: model },
            stages:  totals.stages,
        })
        .collect::<Vec<_>>();

    let response = RunBilling {
        by_model,
        stages,
        totals: RunBillingTotals {
            cache_read_tokens: totals.cache_read_tokens,
            cache_write_tokens: totals.cache_write_tokens,
            input_tokens: totals.input_tokens,
            output_tokens: totals.output_tokens,
            reasoning_tokens: totals.reasoning_tokens,
            runtime_secs,
            total_tokens: totals.total_tokens,
            total_usd_micros: totals.total_usd_micros,
        },
    };

    (StatusCode::OK, Json(response)).into_response()
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
        http_client,
    } = config;

    let vault = Arc::new(AsyncRwLock::new(Vault::load(vault_path)?));
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
        artifact_store,
        worker_tokens,
        started_at: Instant::now(),
        max_concurrent_runs,
        scheduler_notify: Notify::new(),
        global_event_tx,
        files_in_flight: new_files_in_flight(),
        pull_request_create_locks: Arc::new(Mutex::new(HashMap::new())),
        vault,
        server_secrets,
        llm_source,
        manifest_run_defaults: RwLock::new(current_manifest_run_defaults),
        manifest_run_settings: RwLock::new(current_manifest_run_settings),
        server_settings: RwLock::new(current_server_settings),
        env_lookup: Arc::clone(&env_lookup),
        github_api_base_url,
        http_client,
        shutting_down: AtomicBool::new(false),
        registry_factory_override,
        slack_service,
        slack_started: AtomicBool::new(false),
    }))
}

fn board_column(status: RunStatus) -> Option<&'static str> {
    match status {
        RunStatus::Submitted | RunStatus::Queued | RunStatus::Starting => Some("initializing"),
        RunStatus::Running | RunStatus::Paused { .. } => Some("running"),
        RunStatus::Blocked { .. } => Some("blocked"),
        RunStatus::Succeeded { .. } => Some("succeeded"),
        RunStatus::Failed { .. } | RunStatus::Dead => Some("failed"),
        RunStatus::Removing | RunStatus::Archived { .. } => None,
    }
}

pub(crate) fn board_columns() -> serde_json::Value {
    serde_json::json!([
        {"id": "initializing", "name": "Initializing"},
        {"id": "running", "name": "Running"},
        {"id": "blocked", "name": "Blocked"},
        {"id": "succeeded", "name": "Succeeded"},
        {"id": "failed", "name": "Failed"},
    ])
}

async fn board_run_metadata(
    state: &AppState,
    run_id: RunId,
) -> serde_json::Map<String, serde_json::Value> {
    let mut metadata = serde_json::Map::new();
    let Ok(run_store) = state.store.open_run_reader(&run_id).await else {
        return metadata;
    };
    let Ok(run_state) = run_store.state().await else {
        return metadata;
    };

    if let Some(pull_request) = run_state.pull_request {
        metadata.insert(
            "pull_request".to_string(),
            serde_json::json!({
                "number": pull_request.number,
            }),
        );
    }

    if let Some(sandbox) = run_state.sandbox {
        let mut sandbox_metadata = serde_json::Map::new();
        sandbox_metadata.insert(
            "working_directory".to_string(),
            serde_json::json!(sandbox.working_directory),
        );
        if let Some(identifier) = sandbox.identifier {
            sandbox_metadata.insert("id".to_string(), serde_json::json!(identifier));
        }
        metadata.insert(
            "sandbox".to_string(),
            serde_json::Value::Object(sandbox_metadata),
        );
    }

    if let Some((_, record)) =
        run_state
            .pending_interviews
            .iter()
            .min_by(|(left_id, left), (right_id, right)| {
                left.started_at
                    .cmp(&right.started_at)
                    .then_with(|| left_id.cmp(right_id))
            })
    {
        metadata.insert(
            "question".to_string(),
            serde_json::json!({
                "text": record.question.text,
            }),
        );
    }

    metadata
}

const MAX_PAGE_OFFSET: u32 = 1_000_000;

fn paginate_items<T>(items: Vec<T>, pagination: &PaginationParams) -> (Vec<T>, bool) {
    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset.min(MAX_PAGE_OFFSET) as usize;
    let mut data: Vec<_> = items.into_iter().skip(offset).take(limit + 1).collect();
    let has_more = data.len() > limit;
    data.truncate(limit);
    (data, has_more)
}

async fn list_board_runs(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => runs,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let board_summaries: Vec<_> = summaries
        .into_iter()
        .filter_map(|summary| {
            let column = board_column(summary.status)?;
            Some((summary, column))
        })
        .collect();
    let (page_summaries, has_more) = paginate_items(board_summaries, &pagination);

    let mut data = Vec::with_capacity(page_summaries.len());
    for (summary, column) in page_summaries {
        let run_id = summary.run_id;
        let mut item =
            serde_json::to_value(&summary).expect("RunSummary serialization is infallible");
        item["column"] = serde_json::json!(column);
        if let Some(object) = item.as_object_mut() {
            object.extend(board_run_metadata(state.as_ref(), run_id).await);
        }
        data.push(item);
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "columns": board_columns(),
            "data": data,
            "meta": { "has_more": has_more }
        })),
    )
        .into_response()
}

async fn list_runs(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListRunsParams>,
) -> Response {
    match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => {
            let include_archived = params.include_archived;
            let items = runs
                .into_iter()
                .filter(|summary| {
                    include_archived || !matches!(summary.status, RunStatus::Archived { .. })
                })
                .collect::<Vec<_>>();
            let (data, has_more) = paginate_items(items, &params.pagination());
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "data": data,
                    "meta": { "has_more": has_more }
                })),
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
    stream:         CommandOutputStream,
    offset:         u64,
    next_offset:    u64,
    total_bytes:    u64,
    bytes_base64:   String,
    eof:            bool,
    cas_ref:        Option<String>,
    live_streaming: bool,
}

async fn resolve_run(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ResolveRunQuery>,
) -> Response {
    let runs = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
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
        |run| run.run_id.to_string(),
        |run| run.workflow_slug.clone(),
        |run| run.workflow_name.clone(),
        |run| run.run_id.created_at(),
        |run| run.run_id.created_at().to_rfc3339(),
        |run| run.repo_origin_url.clone(),
    ) {
        Ok(run) => (StatusCode::OK, Json(run.clone())).into_response(),
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
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(response) => response,
    }
}

async fn delete_run_internal(
    state: &Arc<AppState>,
    id: RunId,
    force: bool,
) -> Result<(), Response> {
    if !force {
        reject_active_delete_without_force(state.as_ref(), &id).await?;
    }

    let managed_run = if let Ok(mut runs) = state.runs.lock() {
        runs.remove(&id)
    } else {
        None
    };

    if let Some(mut managed_run) = managed_run {
        if let Some(token) = &managed_run.cancel_token {
            token.store(true, Ordering::SeqCst);
        }
        if let Some(answer_transport) = managed_run.answer_transport.clone() {
            let _ = answer_transport.cancel_run().await;
        }
        if let Some(cancel_tx) = managed_run.cancel_tx.take() {
            let _ = cancel_tx.send(());
        }
        // Terminal runs can still carry a stale worker PID briefly after their
        // completion events land, so avoid paying the full cancellation grace.
        let delete_grace = if matches!(
            managed_run.status,
            RunStatus::Submitted
                | RunStatus::Queued
                | RunStatus::Starting
                | RunStatus::Running
                | RunStatus::Blocked { .. }
                | RunStatus::Paused { .. }
        ) {
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
    Ok(())
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
        if matches!(
            status,
            RunStatus::Submitted
                | RunStatus::Queued
                | RunStatus::Starting
                | RunStatus::Running
                | RunStatus::Blocked { .. }
                | RunStatus::Paused { .. }
        ) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                active_run_delete_message(*run_id, status),
            )
            .into_response());
        }
        return Ok(());
    }

    match state.store.runs().find(run_id).await {
        Ok(Some(summary)) if summary.status.is_active() => Err(ApiError::new(
            StatusCode::CONFLICT,
            active_run_delete_message(*run_id, summary.status),
        )
        .into_response()),
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
        if !should_reconcile_run_on_startup(summary.status) {
            continue;
        }

        let run_store = state.store.open_run(&summary.run_id).await?;
        let (error, reason) = failure_for_incomplete_run(
            summary.pending_control,
            "Fabro server restarted before the run reached a terminal state.".to_string(),
        );
        workflow_event::append_event(
            &run_store,
            &summary.run_id,
            &workflow_event::Event::WorkflowRunFailed {
                error,
                duration_ms: 0,
                reason,
                git_commit_sha: None,
                final_patch: None,
            },
        )
        .await?;
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
        if run_state.status.is_some_and(RunStatus::is_terminal) {
            continue;
        }

        let (error, reason) = failure_for_incomplete_run(
            run_state.pending_control,
            "Fabro server shut down before the run reached a terminal state.".to_string(),
        );
        workflow_event::append_event(
            &run_store,
            &run_id,
            &workflow_event::Event::WorkflowRunFailed {
                error,
                duration_ms: 0,
                reason,
                git_commit_sha: None,
                final_patch: None,
            },
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
    workflow_event::append_event(
        &run_store,
        &run_id,
        &workflow_event::Event::WorkflowRunFailed {
            error:          WorkflowError::Cancelled,
            duration_ms:    0,
            reason:         FailureReason::Cancelled,
            git_commit_sha: None,
            final_patch:    None,
        },
    )
    .await
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
            if let Err(err) = workflow_event::append_event(
                &run_store,
                &run_id,
                &workflow_event::Event::WorkflowRunFailed {
                    error: WorkflowError::engine(message.clone()),
                    duration_ms: 0,
                    reason,
                    git_commit_sha: None,
                    final_patch: None,
                },
            )
            .await
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
        .and_then(|summary| summary.pending_control))
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
        }
        EventBody::RunFailed(props) => {
            managed_run.status = RunStatus::Failed {
                reason: props.reason,
            };
            managed_run.error = Some(props.error.clone());
        }
        EventBody::RunArchived(_) => {
            if let Some(prior) = managed_run.status.terminal_status() {
                managed_run.status = RunStatus::Archived { prior };
            }
        }
        EventBody::RunUnarchived(_) => {
            if let RunStatus::Archived { prior } = managed_run.status {
                managed_run.status = prior.into();
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

    let terminal = state.status.is_some_and(RunStatus::is_terminal);
    if terminal {
        return;
    }

    let (error, reason) = failure_for_incomplete_run(
        state.pending_control,
        format!("Worker exited before emitting a terminal run event: {wait_status}"),
    );

    if let Err(err) = workflow_event::append_event(
        run_store,
        &run_id,
        &workflow_event::Event::WorkflowRunFailed {
            error,
            duration_ms: 0,
            reason,
            git_commit_sha: None,
            final_patch: None,
        },
    )
    .await
    {
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
    let worker_token = issue_worker_token(state.worker_token_keys(), &run_id)
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
    let run_store = match state.store.open_run_reader(&run_id).await {
        Ok(run_store) => run_store,
        Err(fabro_store::Error::RunNotFound(_)) => {
            return Err(ApiError::not_found("Run not found.").into_response());
        }
        Err(err) => {
            return Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            );
        }
    };
    let run_state = match run_store.state().await {
        Ok(run_state) => run_state,
        Err(err) => {
            return Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            );
        }
    };
    let Some(record) = run_state.pending_interviews.get(qid) else {
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
    if let Some(key) = req.selected_option_key {
        let option = question
            .options
            .iter()
            .find(|option| option.key == key)
            .cloned();
        match option {
            Some(option) => Ok(Answer::selected(key, option)),
            None => Err(ApiError::bad_request("Invalid option key.").into_response()),
        }
    } else if !req.selected_option_keys.is_empty() {
        for key in &req.selected_option_keys {
            let valid = question.options.iter().any(|option| option.key == *key);
            if !valid {
                return Err(ApiError::bad_request("Invalid option key.").into_response());
            }
        }
        Ok(Answer::multi_selected(req.selected_option_keys))
    } else if let Some(value) = req.value {
        Ok(Answer::text(value))
    } else {
        Err(ApiError::bad_request(
            "One of value, selected_option_key, or selected_option_keys is required.",
        )
        .into_response())
    }
}

async fn create_run(
    RequestAuth(auth_slot): RequestAuth,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let subject = match require_user(&auth_slot) {
        Ok(subject) => subject,
        Err(err) => return err.into_response(),
    };
    let req = match serde_json::from_slice::<RunManifest>(&body) {
        Ok(req) => req,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let manifest_run_defaults = state.manifest_run_defaults();
    let prepared = match run_manifest::prepare_manifest(manifest_run_defaults.as_ref(), &req) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let run_id = prepared.run_id.unwrap_or_else(RunId::new);
    info!(run_id = %run_id, "Run created");

    let configured_providers = state.llm_source.configured_providers().await;
    let mut create_input = run_manifest::create_run_input(prepared.clone(), configured_providers);
    create_input.run_id = Some(run_id);
    create_input.provenance = Some(run_provenance(&headers, &subject));
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

    (
        StatusCode::CREATED,
        Json(RunStatusResponse {
            id: run_id.to_string(),
            status: RunStatus::Submitted,
            error: None,
            queue_position: None,
            pending_control: None,
            created_at,
        }),
    )
        .into_response()
}

fn run_provenance(headers: &HeaderMap, subject: &UserPrincipal) -> RunProvenance {
    RunProvenance {
        server:  Some(RunServerProvenance {
            version: FABRO_VERSION.to_string(),
        }),
        client:  run_client_provenance(headers),
        subject: Some(Principal::User(subject.clone())),
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
    let prepared = match run_manifest::prepare_manifest(manifest_run_defaults.as_ref(), &req) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let validated = match run_manifest::validate_prepared_manifest(&prepared) {
        Ok(validated) => validated,
        Err(WorkflowError::Parse(_)) => {
            return ApiError::bad_request("Validation failed").into_response();
        }
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
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
    let prepared = match run_manifest::prepare_manifest(manifest_run_defaults.as_ref(), &req) {
        Ok(prepared) => prepared,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    let validated = match run_manifest::validate_prepared_manifest(&prepared) {
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

async fn render_graph_from_manifest(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<RenderWorkflowGraphRequest>,
) -> Response {
    let manifest_run_defaults = state.manifest_run_defaults();
    let prepared =
        match run_manifest::prepare_manifest(manifest_run_defaults.as_ref(), &req.manifest) {
            Ok(prepared) => prepared,
            Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
        };
    let validated = match run_manifest::validate_prepared_manifest(&prepared) {
        Ok(validated) => validated,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    if validated.has_errors() {
        return ApiError::bad_request("Validation failed").into_response();
    }

    let direction = req.direction.as_ref().map(|direction| match direction {
        RenderWorkflowGraphDirection::Lr => "LR",
        RenderWorkflowGraphDirection::Tb => "TB",
    });
    let dot_source = run_manifest::graph_source(&prepared, direction);
    render_graph_bytes(&dot_source).await
}

async fn start_run(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<StartRunRequest>>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let resume = body.is_some_and(|Json(req)| req.resume);

    {
        let runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get(&id) {
            if matches!(
                managed_run.status,
                RunStatus::Queued
                    | RunStatus::Starting
                    | RunStatus::Running
                    | RunStatus::Blocked { .. }
                    | RunStatus::Paused { .. }
            ) {
                return ApiError::new(
                    StatusCode::CONFLICT,
                    if resume {
                        "an engine process is still running for this run — cannot resume"
                    } else {
                        "an engine process is still running for this run — cannot start"
                    },
                )
                .into_response();
            }
        }
    }

    let Ok(run_store) = state.store.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let run_state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to load run state: {err}"),
            )
            .into_response();
        }
    };

    if resume {
        if run_state.checkpoint.is_none() {
            return ApiError::new(StatusCode::CONFLICT, "no checkpoint to resume from")
                .into_response();
        }
    } else if let Some(status) = run_state.status {
        if !matches!(
            status,
            RunStatus::Submitted | RunStatus::Queued | RunStatus::Starting
        ) {
            return ApiError::new(
                StatusCode::CONFLICT,
                format!("cannot start run: status is {status}, expected submitted"),
            )
            .into_response();
        }
    }

    if run_state.spec.is_none() {
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "run spec missing from store",
        )
        .into_response();
    }
    let run_dir = Storage::new(state.server_storage_dir())
        .run_scratch(&id)
        .root()
        .to_path_buf();
    let dot_source = run_state.graph_source.unwrap_or_default();
    if let Err(err) =
        workflow_event::append_event(&run_store, &id, &workflow_event::Event::RunQueued).await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            id,
            managed_run(
                dot_source,
                RunStatus::Queued,
                id.created_at(),
                run_dir,
                if resume {
                    RunExecutionMode::Resume
                } else {
                    RunExecutionMode::Start
                },
            ),
        );
    }

    state.scheduler_notify.notify_one();
    (
        StatusCode::OK,
        Json(RunStatusResponse {
            id:              id.to_string(),
            status:          RunStatus::Queued,
            error:           None,
            queue_position:  None,
            pending_control: None,
            created_at:      id.created_at(),
        }),
    )
        .into_response()
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
        let cancel_token = Arc::new(AtomicBool::new(false));
        let (event_tx, _) = broadcast::channel(256);

        managed_run.status = RunStatus::Starting;
        managed_run.cancel_tx = Some(cancel_tx);
        managed_run.cancel_token = Some(Arc::clone(&cancel_token));
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

    // Transition to Running, populate interviewer
    let cancelled_during_setup = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&run_id) {
            if managed_run.status == RunStatus::Starting {
                managed_run.status = RunStatus::Running;
                managed_run.answer_transport = Some(RunAnswerTransport::InProcess {
                    interviewer: Arc::clone(&interviewer),
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
    if cancel_token.load(Ordering::SeqCst) {
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
        if !github_settings.permissions.is_empty() {
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
            if cancel_token.load(Ordering::SeqCst) {
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
    let github_permissions = github_settings
        .permissions
        .iter()
        .map(|(name, value)| {
            let resolved = value
                .resolve(process_env_var)
                .map_or_else(|_| value.as_source(), |resolved| resolved.value);
            (name.clone(), resolved)
        })
        .collect();
    let services = operations::StartServices {
        run_id,
        cancel_token: Some(Arc::clone(&cancel_token)),
        emitter: Arc::clone(&emitter),
        interviewer: Arc::clone(&interview_runtime),
        run_store: run_store.clone().into(),
        event_sink: workflow_event::RunEventSink::store(run_store.clone()),
        artifact_sink: Some(ArtifactSink::Store(state.artifact_store.clone())),
        run_control: None,
        github_app,
        github_permissions,
        vault: Some(Arc::clone(&state.vault)),
        on_node: None,
        registry_override,
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
            cancel_token.store(true, Ordering::SeqCst);
            ExecutionResult::CancelledBySignal
        }
    };

    if matches!(&result, ExecutionResult::CancelledBySignal) {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), run_id).await {
            error!(run_id = %run_id, error = %err, "Failed to persist cancelled run status");
        }
    }

    // Save final checkpoint
    let checkpoint = match run_store.state().await {
        Ok(state) => state.checkpoint,
        Err(err) => {
            tracing::warn!(run_id = %run_id, error = %err, "Failed to load run state from store");
            None
        }
    };

    // Accumulate aggregate usage after execution completes.
    if let Some(ref cp) = checkpoint {
        let stage_durations = match run_store.list_events().await {
            Ok(events) => fabro_workflow::extract_stage_durations_from_events(&events),
            Err(err) => {
                tracing::warn!(run_id = %run_id, error = %err, "Failed to load run events from store");
                HashMap::default()
            }
        };
        let mut agg = state
            .aggregate_billing
            .lock()
            .expect("aggregate_billing lock poisoned");
        agg.total_runs += 1;
        let mut run_runtime: f64 = 0.0;
        for (node_id, outcome) in &cp.node_outcomes {
            if let Some(usage) = &outcome.usage {
                let entry = agg
                    .by_model
                    .entry(usage.model_id().to_string())
                    .or_default();
                accumulate_model_billing(entry, usage);
            }
            let duration_ms = stage_durations.get(node_id).copied().unwrap_or(0);
            run_runtime += duration_ms as f64 / 1000.0;
        }
        agg.total_runtime_secs += run_runtime;
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
        managed_run.checkpoint = checkpoint;
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
            let _ = workflow_event::append_event(
                &run_store,
                &run_id,
                &workflow_event::Event::WorkflowRunFailed {
                    error:          WorkflowError::engine(err.to_string()),
                    duration_ms:    0,
                    reason:         FailureReason::LaunchFailed,
                    git_commit_sha: None,
                    final_patch:    None,
                },
            )
            .await;
            fail_managed_run(
                &state,
                run_id,
                FailureReason::LaunchFailed,
                format!("Failed to spawn worker: {err}"),
            );
            state.scheduler_notify.notify_one();
            return;
        }
    };

    let Some(worker_pid) = child.id() else {
        let message = "Worker process did not report a PID".to_string();
        tracing::error!(run_id = %run_id, "{message}");
        let _ = child.start_kill();
        let _ = workflow_event::append_event(
            &run_store,
            &run_id,
            &workflow_event::Event::WorkflowRunFailed {
                error:          WorkflowError::engine(message.clone()),
                duration_ms:    0,
                reason:         FailureReason::LaunchFailed,
                git_commit_sha: None,
                final_patch:    None,
            },
        )
        .await;
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
        let _ = workflow_event::append_event(
            &run_store,
            &run_id,
            &workflow_event::Event::WorkflowRunFailed {
                error:          WorkflowError::engine(message.clone()),
                duration_ms:    0,
                reason:         FailureReason::LaunchFailed,
                git_commit_sha: None,
                final_patch:    None,
            },
        )
        .await;
        fail_managed_run(&state, run_id, FailureReason::LaunchFailed, message);
        state.scheduler_notify.notify_one();
        return;
    };

    let Some(stderr) = child.stderr.take() else {
        let message = "Worker stderr pipe was unavailable".to_string();
        tracing::error!(run_id = %run_id, "{message}");
        let _ = child.start_kill();
        let _ = workflow_event::append_event(
            &run_store,
            &run_id,
            &workflow_event::Event::WorkflowRunFailed {
                error:          WorkflowError::engine(message.clone()),
                duration_ms:    0,
                reason:         FailureReason::LaunchFailed,
                git_commit_sha: None,
                final_patch:    None,
            },
        )
        .await;
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
            let _ = child.start_kill();
            let _ = workflow_event::append_event(
                &run_store,
                &run_id,
                &workflow_event::Event::WorkflowRunFailed {
                    error:          WorkflowError::engine(err.to_string()),
                    duration_ms:    0,
                    reason:         FailureReason::Terminated,
                    git_commit_sha: None,
                    final_patch:    None,
                },
            )
            .await;
            fail_managed_run(
                &state,
                run_id,
                FailureReason::Terminated,
                format!("Worker wait failed: {err}"),
            );
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

    if let Some(ref checkpoint) = final_state.checkpoint {
        let stage_durations = match run_store.list_events().await {
            Ok(events) => fabro_workflow::extract_stage_durations_from_events(&events),
            Err(err) => {
                tracing::warn!(run_id = %run_id, error = %err, "Failed to load run events from store");
                HashMap::default()
            }
        };
        let mut agg = state
            .aggregate_billing
            .lock()
            .expect("aggregate_billing lock poisoned");
        agg.total_runs += 1;
        let mut run_runtime: f64 = 0.0;
        for (node_id, outcome) in &checkpoint.node_outcomes {
            if let Some(usage) = &outcome.usage {
                let entry = agg
                    .by_model
                    .entry(usage.model_id().to_string())
                    .or_default();
                accumulate_model_billing(entry, usage);
            }
            let duration_ms = stage_durations.get(node_id).copied().unwrap_or(0);
            run_runtime += duration_ms as f64 / 1000.0;
        }
        agg.total_runtime_secs += run_runtime;
    }

    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        if let Some(status) = final_state.status {
            managed_run.status = status;
        } else if !wait_status.success() {
            managed_run.status = RunStatus::Failed {
                reason: FailureReason::Terminated,
            };
        }
        managed_run.error = final_state
            .conclusion
            .as_ref()
            .and_then(|conclusion| conclusion.failure_reason.clone())
            .or_else(|| managed_run.error.clone());
        managed_run.checkpoint = final_state.checkpoint;
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

async fn get_run_status(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => match runs.into_iter().find(|run| run.run_id == id) {
            Some(run) => (StatusCode::OK, Json(run)).into_response(),
            None => ApiError::not_found("Run not found.").into_response(),
        },
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
    let run_store = match state.store.open_run_reader(&id).await {
        Ok(store) => store,
        Err(fabro_store::Error::RunNotFound(_)) => {
            return ApiError::not_found("Run not found.").into_response();
        }
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let run_state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let Some(run_spec) = run_state.spec else {
        return ApiError::not_found("Run not found.").into_response();
    };
    (StatusCode::OK, Json(run_spec.settings)).into_response()
}

async fn get_questions(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => {
                let questions = run_state
                    .pending_interviews
                    .values()
                    .map(api_question_from_pending_interview)
                    .collect::<Vec<_>>();
                (StatusCode::OK, Json(ListResponse::new(questions))).into_response()
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(fabro_store::Error::RunNotFound(_)) => {
            ApiError::not_found("Run not found.").into_response()
        }
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn submit_answer(
    auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((id, qid)): Path<(String, String)>,
    Json(req): Json<SubmitAnswerRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
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
    let submission = AnswerSubmission::new(answer, Principal::User(auth.0));
    match submit_pending_interview_answer(state.as_ref(), &pending, submission).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(response) => response,
    }
}

async fn get_run_state(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => Json(run_state).into_response(),
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
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
    RequireCommandLog(id, stage_id, stream): RequireCommandLog,
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

    let stream_value = match stream {
        CommandOutputStream::Stdout => node.stdout.as_deref(),
        CommandOutputStream::Stderr => node.stderr.as_deref(),
    };
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
    let scratch_path = command_log_path(&run_dir, &stage_id, stream);

    match read_log_slice(&scratch_path, query.offset, limit).await {
        Ok((bytes, total_bytes)) => {
            return build_command_log_response(
                stream,
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
            stream,
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
            stream,
            query.offset,
            limit,
            LogSource::Full(inline_text.as_bytes()),
            true,
            None,
            live_streaming,
        );
    }

    build_command_log_response(
        stream,
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
    stream: CommandOutputStream,
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
        stream,
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

#[expect(
    clippy::disallowed_types,
    reason = "Pull-request API validates public github.com URLs; these raw URLs are not credential-bearing log output."
)]
fn parse_github_owner_repo_from_url(url: &str, kind: &str) -> Result<(String, String), ApiError> {
    let parsed = fabro_http::Url::parse(url)
        .map_err(|err| ApiError::bad_request(format!("Invalid {kind}: {err}")))?;
    match parsed.host_str() {
        Some("github.com") => {}
        Some(host) => {
            return Err(ApiError::with_code(
                StatusCode::BAD_REQUEST,
                format!("Pull request operations support github.com only (got {host})."),
                "unsupported_host",
            ));
        }
        None => {
            return Err(ApiError::bad_request(format!(
                "Invalid {kind}: missing host"
            )));
        }
    }

    fabro_github::parse_github_owner_repo(url).map_err(|err| ApiError::bad_request(err.to_string()))
}

fn load_server_github_credentials(
    state: &AppState,
) -> Result<fabro_github::GitHubCredentials, ApiError> {
    let settings = state.server_settings();
    match state.github_credentials(&settings.server.integrations.github) {
        Ok(Some(creds)) => Ok(creds),
        Ok(None) => {
            warn!("GitHub integration unavailable on server: credentials not configured");
            Err(ApiError::with_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "GitHub integration unavailable on server.",
                "integration_unavailable",
            ))
        }
        Err(err) => {
            warn!(error = %err, "GitHub integration unavailable on server");
            Err(ApiError::with_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "GitHub integration unavailable on server.",
                "integration_unavailable",
            ))
        }
    }
}

fn server_github_context<'a>(
    state: &'a AppState,
    creds: &'a fabro_github::GitHubCredentials,
) -> Result<fabro_github::GitHubContext<'a>, ApiError> {
    let http_client = state.http_client().map_err(|err| {
        ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("GitHub integration unavailable on server: {err}"),
            "integration_unavailable",
        )
    })?;
    Ok(fabro_github::GitHubContext::with_http_client(
        creds,
        state.github_api_base_url.as_str(),
        http_client,
    ))
}

fn github_pull_request_not_found_error(record: &PullRequestRecord) -> ApiError {
    ApiError::with_code(
        StatusCode::BAD_GATEWAY,
        format!("Pull request #{} was deleted on GitHub.", record.number),
        "github_not_found",
    )
}

struct PullRequestGithubContext {
    record: PullRequestRecord,
    creds:  fabro_github::GitHubCredentials,
}

async fn load_pull_request_github_context(
    state: &Arc<AppState>,
    id: &RunId,
) -> Result<PullRequestGithubContext, ApiError> {
    let run_store = state
        .store
        .open_run_reader(id)
        .await
        .map_err(|_| ApiError::not_found("Run not found."))?;
    let run_state = run_store
        .state()
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let record = run_state.pull_request.ok_or_else(|| {
        ApiError::with_code(
            StatusCode::NOT_FOUND,
            format!("No pull request found in store. Create one first with: fabro pr create {id}"),
            "no_stored_record",
        )
    })?;
    parse_github_owner_repo_from_url(&record.html_url, "pull request URL")?;
    let creds = load_server_github_credentials(state.as_ref())?;
    Ok(PullRequestGithubContext { record, creds })
}

struct RunPrInputs<'a> {
    goal:              &'a str,
    base_branch:       &'a str,
    run_branch:        &'a str,
    diff:              &'a str,
    conclusion:        &'a fabro_types::Conclusion,
    normalized_origin: String,
}

impl<'a> RunPrInputs<'a> {
    fn extract(run_state: &'a fabro_store::RunProjection, force: bool) -> Result<Self, ApiError> {
        if let Some(record) = run_state.pull_request.as_ref() {
            return Err(ApiError::with_code(
                StatusCode::CONFLICT,
                format!("Pull request already exists at {}", record.html_url),
                "pull_request_exists",
            ));
        }
        let run_spec = run_state.spec.as_ref().ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Run spec missing from store.",
            )
        })?;
        let origin_url = run_spec.repo_origin_url().ok_or_else(|| {
            ApiError::with_code(
                StatusCode::BAD_REQUEST,
                "Run has no repo origin URL — pull request creation requires git metadata.",
                "missing_repo_origin",
            )
        })?;
        let base_branch = run_spec.base_branch().ok_or_else(|| {
            ApiError::with_code(
                StatusCode::BAD_REQUEST,
                "Run has no base branch — pull request creation requires git metadata.",
                "missing_base_branch",
            )
        })?;
        let run_branch = run_state
            .start
            .as_ref()
            .and_then(|start| start.run_branch.as_deref())
            .ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "Run has no run_branch — was it run with git push enabled?",
                    "missing_run_branch",
                )
            })?;
        let diff = run_state
            .final_patch
            .as_deref()
            .filter(|d| !d.trim().is_empty())
            .ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "Stored diff is empty — nothing to create a PR for",
                    "empty_diff",
                )
            })?;
        let conclusion = run_state.conclusion.as_ref().ok_or_else(|| {
            ApiError::with_code(
                StatusCode::BAD_REQUEST,
                "Run is not finished yet.",
                "run_not_finished",
            )
        })?;
        if !force && !conclusion.status.is_successful() {
            return Err(ApiError::with_code(
                StatusCode::BAD_REQUEST,
                format!(
                    "Run status is '{}', expected succeeded or partially_succeeded",
                    conclusion.status
                ),
                "run_not_successful",
            ));
        }
        let normalized_origin = fabro_github::normalize_repo_origin_url(origin_url);
        parse_github_owner_repo_from_url(&normalized_origin, "repo origin URL")?;
        Ok(Self {
            goal: run_spec.graph.goal(),
            base_branch,
            run_branch,
            diff,
            conclusion,
            normalized_origin,
        })
    }
}

async fn create_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateRunPullRequestRequest>,
) -> Response {
    let _create_guard = lock_pull_request_create(&state.pull_request_create_locks, &id).await;
    let Ok(run_store) = state.store.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let run_state = match run_store.state().await {
        Ok(run_state) => run_state,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let inputs = match RunPrInputs::extract(&run_state, body.force) {
        Ok(inputs) => inputs,
        Err(err) => return err.into_response(),
    };
    let creds = match load_server_github_credentials(state.as_ref()) {
        Ok(creds) => creds,
        Err(err) => return err.into_response(),
    };
    let github = match server_github_context(state.as_ref(), &creds) {
        Ok(ctx) => ctx,
        Err(err) => return err.into_response(),
    };
    let model = if let Some(model) = body.model {
        model
    } else {
        let configured = state.llm_source.configured_providers().await;
        Catalog::builtin()
            .default_for_configured(&configured)
            .id
            .clone()
    };

    let run_store_handle = run_store.clone().into();
    let request = pull_request::OpenPullRequestRequest {
        github,
        origin_url: &inputs.normalized_origin,
        base_branch: inputs.base_branch,
        head_branch: inputs.run_branch,
        goal: inputs.goal,
        diff: inputs.diff,
        model: &model,
        draft: true,
        auto_merge: None,
        run_store: &run_store_handle,
        llm_source: state.llm_source.as_ref(),
        conclusion: Some(inputs.conclusion),
        run_state: Some(&run_state),
    };
    let pull_request = match pull_request::maybe_open_pull_request(request).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Pull request creation returned no record unexpectedly.",
            )
            .into_response();
        }
        Err(err) => return ApiError::new(StatusCode::BAD_GATEWAY, err).into_response(),
    };

    let event = workflow_event::Event::pull_request_created(&pull_request, true);
    if let Err(err) = workflow_event::append_event(&run_store, &id, &event).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    Json(pull_request).into_response()
}

async fn get_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    let ctx = match load_pull_request_github_context(&state, &id).await {
        Ok(ctx) => ctx,
        Err(err) => return err.into_response(),
    };
    let github = match server_github_context(state.as_ref(), &ctx.creds) {
        Ok(github) => github,
        Err(err) => return err.into_response(),
    };

    match fabro_github::get_pull_request(
        &github,
        &ctx.record.owner,
        &ctx.record.repo,
        ctx.record.number,
    )
    .await
    {
        Ok(github) => Json(fabro_types::PullRequestDetail {
            record: ctx.record,
            github,
        })
        .into_response(),
        Err(fabro_github::PullRequestApiError::NotFound { .. }) => {
            github_pull_request_not_found_error(&ctx.record).into_response()
        }
        Err(err) => ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
    }
}

async fn merge_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Json(body): Json<MergeRunPullRequestRequest>,
) -> Response {
    let ctx = match load_pull_request_github_context(&state, &id).await {
        Ok(ctx) => ctx,
        Err(err) => return err.into_response(),
    };
    let github = match server_github_context(state.as_ref(), &ctx.creds) {
        Ok(github) => github,
        Err(err) => return err.into_response(),
    };

    match fabro_github::merge_pull_request(
        &github,
        &ctx.record.owner,
        &ctx.record.repo,
        ctx.record.number,
        body.method,
    )
    .await
    {
        Ok(()) => Json(MergeRunPullRequestResponse {
            number:   i64::try_from(ctx.record.number)
                .expect("stored pull request number should fit in i64"),
            html_url: ctx.record.html_url,
            method:   body.method,
        })
        .into_response(),
        Err(fabro_github::PullRequestApiError::NotFound { .. }) => {
            github_pull_request_not_found_error(&ctx.record).into_response()
        }
        Err(err) => ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
    }
}

async fn close_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    let ctx = match load_pull_request_github_context(&state, &id).await {
        Ok(ctx) => ctx,
        Err(err) => return err.into_response(),
    };
    let github = match server_github_context(state.as_ref(), &ctx.creds) {
        Ok(github) => github,
        Err(err) => return err.into_response(),
    };

    match fabro_github::close_pull_request(
        &github,
        &ctx.record.owner,
        &ctx.record.repo,
        ctx.record.number,
    )
    .await
    {
        Ok(()) => Json(CloseRunPullRequestResponse {
            number:   i64::try_from(ctx.record.number)
                .expect("stored pull request number should fit in i64"),
            html_url: ctx.record.html_url,
        })
        .into_response(),
        Err(fabro_github::PullRequestApiError::NotFound { .. }) => {
            github_pull_request_not_found_error(&ctx.record).into_response()
        }
        Err(err) => ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
    }
}

async fn append_run_event(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Json(value): Json<serde_json::Value>,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let event = match RunEvent::from_value(value.clone()) {
        Ok(event) => event,
        Err(err) => {
            return ApiError::bad_request(format!("Invalid run event: {err}")).into_response();
        }
    };
    if event.run_id != id {
        return ApiError::bad_request("Event run_id does not match path run ID.").into_response();
    }
    if let Some(denied) = denied_lifecycle_event_name(&event.body) {
        return ApiError::bad_request(format!(
            "{denied} is a lifecycle event; clients must call the corresponding operation endpoint instead of injecting it via append_run_event"
        ))
        .into_response();
    }
    let payload = match EventPayload::new(value, &id) {
        Ok(payload) => payload,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };

    match state.store.open_run(&id).await {
        Ok(run_store) => match run_store.append_event(&payload).await {
            Ok(seq) => {
                update_live_run_from_event(&state, id, &event);
                Json(AppendEventResponse {
                    seq: i64::from(seq),
                })
                .into_response()
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn list_run_events(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Query(params): Query<EventListParams>,
) -> Response {
    let since_seq = params.since_seq();
    let limit = params.limit();
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store
            .list_events_from_with_limit(since_seq, limit)
            .await
        {
            Ok(mut events) => {
                let has_more = events.len() > limit;
                events.truncate(limit);
                Json(PaginatedEventList {
                    data: events,
                    meta: PaginationMeta { has_more },
                })
                .into_response()
            }
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn attach_run_events(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<AttachParams>,
) -> Response {
    const ATTACH_REPLAY_BATCH_LIMIT: usize = 256;

    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let Ok(run_store) = state.store.open_run_reader(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let start_seq = match params.since_seq {
        Some(seq) if seq >= 1 => seq,
        Some(_) => 1,
        None => match run_store.list_events().await {
            Ok(events) => events.last().map_or(1, |event| event.seq.saturating_add(1)),
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        },
    };
    let (sender, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut next_seq = start_seq;

        loop {
            let Ok(replay_batch) = run_store
                .list_events_from_with_limit(next_seq, ATTACH_REPLAY_BATCH_LIMIT)
                .await
            else {
                return;
            };
            let replay_has_more = replay_batch.len() > ATTACH_REPLAY_BATCH_LIMIT;

            for event in replay_batch.into_iter().take(ATTACH_REPLAY_BATCH_LIMIT) {
                next_seq = event.seq.saturating_add(1);
                let terminal = attach_event_is_terminal(&event);
                if let Some(sse_event) = sse_event_from_store(&event) {
                    if sender
                        .send(Ok::<Event, std::convert::Infallible>(sse_event))
                        .is_err()
                    {
                        return;
                    }
                }
                if terminal {
                    return;
                }
            }

            if replay_has_more {
                continue;
            }

            let Ok(state) = run_store.state().await else {
                return;
            };

            if run_projection_is_active(&state) {
                break;
            }

            let Ok(tail_batch) = run_store
                .list_events_from_with_limit(next_seq, ATTACH_REPLAY_BATCH_LIMIT)
                .await
            else {
                return;
            };
            let tail_has_more = tail_batch.len() > ATTACH_REPLAY_BATCH_LIMIT;

            for event in tail_batch.into_iter().take(ATTACH_REPLAY_BATCH_LIMIT) {
                next_seq = event.seq.saturating_add(1);
                let terminal = attach_event_is_terminal(&event);
                if let Some(sse_event) = sse_event_from_store(&event) {
                    if sender
                        .send(Ok::<Event, std::convert::Infallible>(sse_event))
                        .is_err()
                    {
                        return;
                    }
                }
                if terminal {
                    return;
                }
            }

            if tail_has_more {
                continue;
            }

            return;
        }

        let Ok(mut live_stream) = run_store.watch_events_from(next_seq) else {
            return;
        };

        while let Some(result) = live_stream.next().await {
            let Ok(event) = result else {
                return;
            };
            let terminal = attach_event_is_terminal(&event);
            if let Some(sse_event) = sse_event_from_store(&event) {
                if sender
                    .send(Ok::<Event, std::convert::Infallible>(sse_event))
                    .is_err()
                {
                    return;
                }
            }
            if terminal {
                return;
            }
        }
    });

    Sse::new(UnboundedReceiverStream::new(receiver))
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn get_checkpoint(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let live_checkpoint = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => managed_run.checkpoint.clone(),
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };
    if let Some(cp) = live_checkpoint {
        return (StatusCode::OK, Json(cp)).into_response();
    }

    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => match run_state.checkpoint {
                Some(cp) => (StatusCode::OK, Json(cp)).into_response(),
                None => (StatusCode::OK, Json(serde_json::json!(null))).into_response(),
            },
            Err(err) => {
                tracing::warn!(run_id = %id, error = %err, "Failed to load checkpoint state from store");
                (StatusCode::OK, Json(serde_json::json!(null))).into_response()
            }
        },
        Err(err) => {
            tracing::warn!(run_id = %id, error = %err, "Failed to open run store reader");
            ApiError::not_found("Run not found.").into_response()
        }
    }
}

async fn write_run_blob(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    match state.store.open_run(&id).await {
        Ok(run_store) => match run_store.write_blob(&body).await {
            Ok(blob_id) => Json(WriteBlobResponse {
                id: blob_id.to_string(),
            })
            .into_response(),
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn read_run_blob(
    RequireRunBlob(id, blob_id): RequireRunBlob,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.store.open_run_reader(&id).await {
        Ok(run_store) => match run_store.read_blob(&blob_id).await {
            Ok(Some(bytes)) => octet_stream_response(bytes),
            Ok(None) => ApiError::not_found("Blob not found.").into_response(),
            Err(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        },
        Err(_) => ApiError::not_found("Run not found.").into_response(),
    }
}

async fn load_run_spec(state: &AppState, run_id: &RunId) -> Result<fabro_types::RunSpec, Response> {
    let run_store = state
        .store
        .open_run_reader(run_id)
        .await
        .map_err(|_| ApiError::not_found("Run not found.").into_response())?;
    let run_state = run_store.state().await.map_err(|err| {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
    })?;
    run_state.spec.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "run spec missing from store",
        )
        .into_response()
    })
}

async fn list_run_artifacts(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Err(response) = load_run_spec(state.as_ref(), &id).await {
        return response;
    }

    match state.artifact_store.list_for_run(&id).await {
        Ok(entries) => Json(RunArtifactListResponse {
            data: entries.into_iter().map(run_artifact_entry_from).collect(),
        })
        .into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

fn run_artifact_entry_from(entry: NodeArtifact) -> RunArtifactEntry {
    RunArtifactEntry {
        stage_id:      entry.node.to_string(),
        node_slug:     entry.node.node_id().to_string(),
        retry:         entry.retry.cast_signed(),
        relative_path: entry.filename,
        size:          entry.size.cast_signed(),
    }
}

fn artifact_entry_from(entry: StageArtifactEntry) -> ArtifactEntry {
    ArtifactEntry {
        filename: entry.filename,
        retry:    entry.retry.cast_signed(),
        size:     entry.size.cast_signed(),
    }
}

async fn list_stage_artifacts(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((id, stage_id)): Path<(String, String)>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let stage_id = match parse_stage_id_path(&stage_id) {
        Ok(stage_id) => stage_id,
        Err(response) => return response,
    };
    if let Err(response) = load_run_spec(state.as_ref(), &id).await {
        return response;
    }

    match state.artifact_store.list_for_node(&id, &stage_id).await {
        Ok(entries) => Json(ArtifactListResponse {
            data: entries.into_iter().map(artifact_entry_from).collect(),
        })
        .into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

enum ArtifactUploadContentType {
    OctetStream,
    Multipart { boundary: String },
}

struct ValidatedArtifactBatchEntry {
    path:           String,
    sha256:         Option<String>,
    expected_bytes: Option<u64>,
}

#[allow(
    clippy::result_large_err,
    reason = "Upload content-type parsing returns HTTP client errors directly."
)]
fn artifact_upload_content_type(
    headers: &HeaderMap,
) -> Result<ArtifactUploadContentType, Response> {
    let value = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "artifact uploads require a supported Content-Type",
            )
            .into_response()
        })?;

    let mime = value.split(';').next().unwrap_or(value).trim();
    match mime {
        "application/octet-stream" => Ok(ArtifactUploadContentType::OctetStream),
        "multipart/form-data" => multer::parse_boundary(value)
            .map(|boundary| ArtifactUploadContentType::Multipart { boundary })
            .map_err(|err| bad_request_response(format!("invalid multipart boundary: {err}"))),
        _ => Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "artifact uploads only support application/octet-stream or multipart/form-data",
        )
        .into_response()),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "Content-Length parsing returns HTTP client errors directly."
)]
fn content_length_from_headers(headers: &HeaderMap) -> Result<Option<u64>, Response> {
    headers
        .get(header::CONTENT_LENGTH)
        .map(|value| {
            value
                .to_str()
                .map_err(|err| {
                    bad_request_response(format!("invalid content-length header: {err}"))
                })
                .and_then(|value| {
                    value.parse::<u64>().map_err(|err| {
                        bad_request_response(format!("invalid content-length header: {err}"))
                    })
                })
        })
        .transpose()
}

#[allow(
    clippy::result_large_err,
    reason = "Multipart manifest parsing returns HTTP client errors directly."
)]
async fn read_multipart_manifest(
    field: &mut multer::Field<'_>,
) -> Result<ArtifactBatchUploadManifest, Response> {
    let mut manifest_bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|err| bad_request_response(format!("invalid multipart body: {err}")))?
    {
        manifest_bytes.extend_from_slice(&chunk);
        if manifest_bytes.len() > MAX_MULTIPART_MANIFEST_BYTES {
            return Err(payload_too_large_response(
                "multipart manifest exceeds the server limit",
            ));
        }
    }

    serde_json::from_slice(&manifest_bytes)
        .map_err(|err| bad_request_response(format!("invalid multipart manifest: {err}")))
}

#[allow(
    clippy::result_large_err,
    reason = "Artifact batch validation returns HTTP client errors directly."
)]
fn validate_artifact_batch_manifest(
    manifest: ArtifactBatchUploadManifest,
) -> Result<HashMap<String, ValidatedArtifactBatchEntry>, Response> {
    if manifest.entries.is_empty() {
        return Err(bad_request_response(
            "multipart manifest must include at least one artifact entry",
        ));
    }
    if manifest.entries.len() > MAX_MULTIPART_ARTIFACTS {
        return Err(payload_too_large_response(format!(
            "multipart upload exceeds the {MAX_MULTIPART_ARTIFACTS} artifact limit"
        )));
    }

    let mut entries = HashMap::with_capacity(manifest.entries.len());
    let mut seen_paths = HashSet::new();
    let mut expected_total_bytes = 0_u64;

    for entry in manifest.entries {
        if entry.part.is_empty() {
            return Err(bad_request_response(
                "multipart manifest part names must not be empty",
            ));
        }
        if entry.part == "manifest" {
            return Err(bad_request_response(
                "multipart manifest part name 'manifest' is reserved",
            ));
        }
        let path = validate_relative_artifact_path("manifest path", &entry.path)?;
        if !seen_paths.insert(path.clone()) {
            return Err(bad_request_response(format!(
                "duplicate artifact path in multipart manifest: {path}"
            )));
        }
        if let Some(sha256) = entry.sha256.as_ref() {
            if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(bad_request_response(format!(
                    "invalid sha256 for multipart part {}",
                    entry.part
                )));
            }
        }
        if let Some(expected_bytes) = entry.expected_bytes {
            if expected_bytes > MAX_SINGLE_ARTIFACT_BYTES {
                return Err(payload_too_large_response(format!(
                    "artifact {path} exceeds the {MAX_SINGLE_ARTIFACT_BYTES} byte limit"
                )));
            }
            expected_total_bytes = expected_total_bytes.saturating_add(expected_bytes);
            if expected_total_bytes > MAX_MULTIPART_REQUEST_BYTES {
                return Err(payload_too_large_response(format!(
                    "multipart upload exceeds the {MAX_MULTIPART_REQUEST_BYTES} byte limit"
                )));
            }
        }
        if entries
            .insert(entry.part.clone(), ValidatedArtifactBatchEntry {
                path,
                sha256: entry.sha256.map(|value| value.to_ascii_lowercase()),
                expected_bytes: entry.expected_bytes,
            })
            .is_some()
        {
            return Err(bad_request_response(format!(
                "duplicate multipart part name in manifest: {}",
                entry.part
            )));
        }
    }

    Ok(entries)
}

async fn upload_stage_artifact_octet_stream(
    state: &AppState,
    run_id: &RunId,
    stage_id: &StageId,
    retry: u32,
    filename: String,
    body: Body,
    content_length: Option<u64>,
) -> Response {
    let relative_path = match validate_relative_artifact_path("filename", &filename) {
        Ok(path) => path,
        Err(response) => return response,
    };

    if content_length.is_some_and(|length| length > MAX_SINGLE_ARTIFACT_BYTES) {
        return payload_too_large_response(format!(
            "artifact exceeds the {MAX_SINGLE_ARTIFACT_BYTES} byte limit"
        ));
    }

    let mut writer = match state.artifact_store.writer(
        run_id,
        &ArtifactKey::new(stage_id.clone(), retry, relative_path),
    ) {
        Ok(writer) => writer,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let mut bytes_written = 0_u64;
    let mut data_stream = body.into_data_stream();
    while let Some(chunk) = data_stream.next().await {
        let chunk = match chunk
            .map_err(|err| bad_request_response(format!("invalid request body: {err}")))
        {
            Ok(chunk) => chunk,
            Err(response) => return response,
        };
        bytes_written =
            bytes_written.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if bytes_written > MAX_SINGLE_ARTIFACT_BYTES {
            return payload_too_large_response(format!(
                "artifact exceeds the {MAX_SINGLE_ARTIFACT_BYTES} byte limit"
            ));
        }
        if let Err(err) = writer.write_all(&chunk).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    match writer.shutdown().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn upload_stage_artifact_multipart(
    state: &AppState,
    run_id: &RunId,
    stage_id: &StageId,
    retry: u32,
    boundary: String,
    body: Body,
) -> Response {
    let mut multipart = multer::Multipart::new(body.into_data_stream(), boundary);
    let Some(mut manifest_field) = (match multipart
        .next_field()
        .await
        .map_err(|err| bad_request_response(format!("invalid multipart body: {err}")))
    {
        Ok(field) => field,
        Err(response) => return response,
    }) else {
        return bad_request_response("multipart upload must begin with a manifest part");
    };

    if manifest_field.name() != Some("manifest") {
        return bad_request_response("multipart upload must begin with a manifest part");
    }

    let manifest = match read_multipart_manifest(&mut manifest_field).await {
        Ok(manifest) => manifest,
        Err(response) => return response,
    };
    drop(manifest_field);
    let mut expected_parts = match validate_artifact_batch_manifest(manifest) {
        Ok(entries) => entries,
        Err(response) => return response,
    };
    let mut total_bytes = 0_u64;

    while let Some(mut field) = match multipart
        .next_field()
        .await
        .map_err(|err| bad_request_response(format!("invalid multipart body: {err}")))
    {
        Ok(field) => field,
        Err(response) => return response,
    } {
        let Some(part_name) = field.name().map(ToOwned::to_owned) else {
            return bad_request_response("multipart file parts must be named");
        };
        let Some(entry) = expected_parts.remove(&part_name) else {
            return bad_request_response(format!("unexpected multipart part: {part_name}"));
        };

        let mut writer = match state.artifact_store.writer(
            run_id,
            &ArtifactKey::new(stage_id.clone(), retry, entry.path.clone()),
        ) {
            Ok(writer) => writer,
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        };
        let mut bytes_written = 0_u64;
        let mut sha256 = Sha256::new();

        while let Some(chunk) = match field
            .chunk()
            .await
            .map_err(|err| bad_request_response(format!("invalid multipart body: {err}")))
        {
            Ok(chunk) => chunk,
            Err(response) => return response,
        } {
            let chunk_len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            bytes_written = bytes_written.saturating_add(chunk_len);
            total_bytes = total_bytes.saturating_add(chunk_len);

            if bytes_written > MAX_SINGLE_ARTIFACT_BYTES {
                return payload_too_large_response(format!(
                    "artifact {} exceeds the {MAX_SINGLE_ARTIFACT_BYTES} byte limit",
                    entry.path
                ));
            }
            if total_bytes > MAX_MULTIPART_REQUEST_BYTES {
                return payload_too_large_response(format!(
                    "multipart upload exceeds the {MAX_MULTIPART_REQUEST_BYTES} byte limit"
                ));
            }

            sha256.update(&chunk);
            if let Err(err) = writer.write_all(&chunk).await {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        }

        if let Some(expected_bytes) = entry.expected_bytes {
            if bytes_written != expected_bytes {
                return bad_request_response(format!(
                    "multipart part {part_name} expected {expected_bytes} bytes but received {bytes_written}"
                ));
            }
        }
        if let Some(expected_sha256) = entry.sha256.as_ref() {
            let actual_sha256 = hex::encode(sha256.finalize());
            if actual_sha256 != *expected_sha256 {
                return bad_request_response(format!(
                    "multipart part {part_name} sha256 did not match manifest"
                ));
            }
        }

        if let Err(err) = writer.shutdown().await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    if !expected_parts.is_empty() {
        let mut missing = expected_parts.into_keys().collect::<Vec<_>>();
        missing.sort();
        return bad_request_response(format!(
            "multipart upload is missing part(s): {}",
            missing.join(", ")
        ));
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn put_stage_artifact(
    State(state): State<Arc<AppState>>,
    RequireStageArtifact(id, stage_id): RequireStageArtifact,
    Query(params): Query<ArtifactFilenameParams>,
    request: axum_extract::Request,
) -> Response {
    let (parts, body) = request.into_parts();
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    if let Err(response) = load_run_spec(state.as_ref(), &id).await.map(|_| ()) {
        return response;
    }
    let retry = match required_query_param(params.retry.as_ref(), "retry") {
        Ok(retry) => retry,
        Err(response) => return response,
    };

    let content_length = match content_length_from_headers(&parts.headers) {
        Ok(length) => length,
        Err(response) => return response,
    };
    match artifact_upload_content_type(&parts.headers) {
        Ok(ArtifactUploadContentType::OctetStream) => {
            let filename = match required_query_param(params.filename.as_ref(), "filename") {
                Ok(filename) => filename,
                Err(response) => return response,
            };
            upload_stage_artifact_octet_stream(
                state.as_ref(),
                &id,
                &stage_id,
                retry,
                filename,
                body,
                content_length,
            )
            .await
        }
        Ok(ArtifactUploadContentType::Multipart { boundary }) => {
            if content_length.is_some_and(|length| length > MAX_MULTIPART_REQUEST_BYTES) {
                return payload_too_large_response(format!(
                    "multipart upload exceeds the {MAX_MULTIPART_REQUEST_BYTES} byte limit"
                ));
            }
            upload_stage_artifact_multipart(state.as_ref(), &id, &stage_id, retry, boundary, body)
                .await
        }
        Err(response) => response,
    }
}

async fn get_stage_artifact(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((id, stage_id)): Path<(String, String)>,
    Query(params): Query<ArtifactFilenameParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let stage_id = match parse_stage_id_path(&stage_id) {
        Ok(stage_id) => stage_id,
        Err(response) => return response,
    };
    let filename = match required_query_param(params.filename.as_ref(), "filename") {
        Ok(filename) => filename,
        Err(response) => return response,
    };
    let retry = match required_query_param(params.retry.as_ref(), "retry") {
        Ok(retry) => retry,
        Err(response) => return response,
    };
    let relative_path = match validate_relative_artifact_path("filename", &filename) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if let Err(response) = load_run_spec(state.as_ref(), &id).await {
        return response;
    }

    match state
        .artifact_store
        .get(
            &id,
            &ArtifactKey::new(stage_id.clone(), retry, relative_path),
        )
        .await
    {
        Ok(Some(bytes)) => octet_stream_response(bytes),
        Ok(None) => ApiError::not_found("Artifact not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn generate_preview_url(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<PreviewUrlRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let Ok(port) = u16::try_from(request.port) else {
        return ApiError::bad_request("Port must fit in a u16.").into_response();
    };
    let Ok(expires_in_secs) = i32::try_from(request.expires_in_secs.get()) else {
        return ApiError::bad_request("Preview expiry exceeds supported range.").into_response();
    };

    let sandbox = match reconnect_daytona_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };

    let response = if request.signed {
        match sandbox
            .get_signed_preview_url(port, Some(expires_in_secs))
            .await
        {
            Ok(preview) => PreviewUrlResponse {
                token: None,
                url:   preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err.display_with_causes())
                    .into_response();
            }
        }
    } else {
        match sandbox.get_preview_link(port).await {
            Ok(preview) => PreviewUrlResponse {
                token: Some(preview.token),
                url:   preview.url,
            },
            Err(err) => {
                return ApiError::new(StatusCode::CONFLICT, err.display_with_causes())
                    .into_response();
            }
        }
    };

    (StatusCode::CREATED, Json(response)).into_response()
}

async fn create_ssh_access(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<SshAccessRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let sandbox = match reconnect_daytona_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    match sandbox.create_ssh_access(Some(request.ttl_minutes)).await {
        Ok(command) => (StatusCode::CREATED, Json(SshAccessResponse { command })).into_response(),
        Err(err) => ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response(),
    }
}

async fn list_sandbox_files(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFilesParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    match sandbox.list_directory(&params.path, params.depth).await {
        Ok(entries) => Json(SandboxFileListResponse {
            data: entries
                .into_iter()
                .map(|entry| SandboxFileEntry {
                    is_dir: entry.is_dir,
                    name:   entry.name,
                    size:   entry.size.map(u64::cast_signed),
                })
                .collect(),
        })
        .into_response(),
        Err(err) => ApiError::new(StatusCode::NOT_FOUND, err.display_with_causes()).into_response(),
    }
}

async fn get_sandbox_file(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFileParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let temp = match NamedTempFile::new() {
        Ok(temp) => temp,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Err(err) = sandbox
        .download_file_to_local(&params.path, temp.path())
        .await
    {
        return ApiError::new(StatusCode::NOT_FOUND, err.display_with_causes()).into_response();
    }
    match fs::read(temp.path()).await {
        Ok(bytes) => octet_stream_response(bytes.into()),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn put_sandbox_file(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SandboxFileParams>,
    body: Bytes,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let sandbox = match reconnect_run_sandbox(&state, &id).await {
        Ok(sandbox) => sandbox,
        Err(response) => return response,
    };
    let temp = match NamedTempFile::new() {
        Ok(temp) => temp,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    if let Err(err) = fs::write(temp.path(), &body).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    match sandbox
        .upload_file_from_local(temp.path(), &params.path)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.display_with_causes())
            .into_response(),
    }
}

async fn reconnect_run_sandbox(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<Box<dyn Sandbox>, Response> {
    let record = load_run_sandbox_record(state, run_id).await?;
    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    reconnect(&record, daytona_api_key).await.map_err(|err| {
        let detail = render_with_causes(&err.to_string(), &collect_causes(err.as_ref()));
        ApiError::new(StatusCode::CONFLICT, detail).into_response()
    })
}

async fn reconnect_daytona_sandbox(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<DaytonaSandbox, Response> {
    let record = load_run_sandbox_record(state, run_id).await?;
    if record.provider != SandboxProvider::Daytona.to_string() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox provider does not support this capability.",
        )
        .into_response());
    }
    let Some(name) = record.identifier.as_deref() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox record is missing the Daytona identifier.",
        )
        .into_response());
    };
    let Some(repo_cloned) = record.repo_cloned else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Sandbox record is missing clone metadata.",
        )
        .into_response());
    };
    let daytona_api_key = state.vault_or_env(EnvVars::DAYTONA_API_KEY);
    DaytonaSandbox::reconnect(
        name,
        daytona_api_key,
        repo_cloned,
        record.clone_origin_url.clone(),
        record.clone_branch.clone(),
    )
    .await
    .map_err(|err| ApiError::new(StatusCode::CONFLICT, err.display_with_causes()).into_response())
}

async fn load_run_sandbox_record(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> Result<fabro_types::SandboxRecord, Response> {
    match state.store.open_run_reader(run_id).await {
        Ok(run_store) => match run_store.state().await {
            Ok(run_state) => run_state.sandbox.ok_or_else(|| {
                ApiError::new(StatusCode::CONFLICT, "Run has no active sandbox.").into_response()
            }),
            Err(err) => Err(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            ),
        },
        Err(_) => Err(ApiError::not_found("Run not found.").into_response()),
    }
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

/// Returns the wire event name if the given body has a dedicated operation
/// endpoint that clients must use instead of injecting via `append_run_event`.
/// These endpoints enforce authorization and status-transition preconditions
/// (e.g. "archive only from terminal") that a direct event append would
/// bypass. Other run-lifecycle events flow through this endpoint legitimately:
/// the worker subprocess emits state transitions during execution.
fn denied_lifecycle_event_name(body: &EventBody) -> Option<&'static str> {
    match body {
        EventBody::RunArchived(_) => Some("run.archived"),
        EventBody::RunUnarchived(_) => Some("run.unarchived"),
        EventBody::RunCancelRequested(_) => Some("run.cancel.requested"),
        EventBody::RunPauseRequested(_) => Some("run.pause.requested"),
        EventBody::RunUnpauseRequested(_) => Some("run.unpause.requested"),
        _ => None,
    }
}

/// Returns a 409 response with an actionable "unarchive first" message if the
/// run is currently archived. Returns `None` otherwise (including when the run
/// doesn't exist — the caller's own not-found handling will surface that).
async fn reject_if_archived(state: &AppState, run_id: &RunId) -> Option<Response> {
    let run_store = state.store.open_run_reader(run_id).await.ok()?;
    let projection = run_store.state().await.ok()?;
    let status = projection.status?;
    matches!(status, RunStatus::Archived { .. }).then(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            operations::archived_rejection_message(run_id),
        )
        .into_response()
    })
}

fn schedule_worker_kill(state: Arc<AppState>, run_id: RunId, worker_pid: u32) {
    tokio::spawn(async move {
        sleep(WORKER_CANCEL_GRACE).await;
        let current_pid = {
            let runs = state.runs.lock().expect("runs lock poisoned");
            runs.get(&run_id).and_then(|run| run.worker_pid)
        };
        if current_pid == Some(worker_pid) && fabro_proc::process_group_alive(worker_pid) {
            #[cfg(unix)]
            fabro_proc::sigkill_process_group(worker_pid);
        }
    });
}

async fn cancel_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let (
        created_at,
        response_status,
        persist_cancelled_status,
        answer_transport,
        cancel_token,
        cancel_tx,
        worker_pid,
    ) = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get_mut(&id) {
            Some(managed_run) => match managed_run.status {
                RunStatus::Submitted
                | RunStatus::Queued
                | RunStatus::Starting
                | RunStatus::Running
                | RunStatus::Blocked { .. }
                | RunStatus::Paused { .. } => {
                    let use_cancel_signal = !matches!(
                        managed_run.answer_transport,
                        Some(RunAnswerTransport::InProcess { .. })
                    );
                    let persist_cancelled_status =
                        matches!(managed_run.status, RunStatus::Submitted | RunStatus::Queued);
                    let response_status = if persist_cancelled_status {
                        let cancelled = RunStatus::Failed {
                            reason: FailureReason::Cancelled,
                        };
                        managed_run.status = cancelled;
                        cancelled
                    } else {
                        managed_run.status
                    };
                    (
                        managed_run.created_at,
                        response_status,
                        persist_cancelled_status,
                        managed_run.answer_transport.clone(),
                        managed_run.cancel_token.clone(),
                        use_cancel_signal
                            .then(|| managed_run.cancel_tx.take())
                            .flatten(),
                        managed_run.worker_pid,
                    )
                }
                _ => {
                    return ApiError::new(StatusCode::CONFLICT, "Run is not cancellable.")
                        .into_response();
                }
            },
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if pending_control != Some(RunControlAction::Cancel) {
        if let Err(err) = append_control_request(
            state.as_ref(),
            id,
            RunControlAction::Cancel,
            Some(Principal::User(subject.0.clone())),
        )
        .await
        {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    if let Some(token) = &cancel_token {
        token.store(true, Ordering::SeqCst);
    }
    let sent_cancel_signal = if let Some(cancel_tx) = cancel_tx {
        let _ = cancel_tx.send(());
        true
    } else {
        false
    };
    if let Some(answer_transport) = answer_transport {
        if !(sent_cancel_signal && matches!(answer_transport, RunAnswerTransport::InProcess { .. }))
        {
            let _ = answer_transport.cancel_run().await;
        }
    }
    if let Some(worker_pid) = worker_pid {
        #[cfg(unix)]
        fabro_proc::sigterm(worker_pid);
        schedule_worker_kill(Arc::clone(&state), id, worker_pid);
    }

    if persist_cancelled_status {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), id).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(RunStatusResponse {
            id: id.to_string(),
            status: response_status,
            error: None,
            queue_position: None,
            pending_control,
            created_at,
        }),
    )
        .into_response()
}

/// How `pause_run` should enact the transition, chosen from the current run
/// status.
enum PauseMode {
    /// Worker is running; ask it to pause via SIGUSR1. Status flips to
    /// `Paused` once the worker acknowledges.
    Signal { worker_pid: u32 },
    /// Worker is blocked on a human gate; flip to `Paused` directly by
    /// appending `RunPaused` ourselves.
    AppendEvent,
}

/// How `unpause_run` should enact the transition.
enum UnpauseMode {
    /// No outstanding block; ask the worker to resume via SIGUSR2.
    Signal { worker_pid: u32 },
    /// Was paused while blocked; append `RunUnpaused` and let the reducer
    /// restore the underlying blocked state from `Paused { prior_block }`.
    AppendEvent,
}

async fn pause_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let (created_at, mode) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) if managed_run.status == RunStatus::Running => {
                let Some(worker_pid) = managed_run.worker_pid else {
                    return ApiError::new(StatusCode::CONFLICT, "Run worker is not available.")
                        .into_response();
                };
                (managed_run.created_at, PauseMode::Signal { worker_pid })
            }
            Some(managed_run) if matches!(managed_run.status, RunStatus::Blocked { .. }) => {
                (managed_run.created_at, PauseMode::AppendEvent)
            }
            Some(_) => {
                return ApiError::new(StatusCode::CONFLICT, "Run is not pausable.").into_response();
            }
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if pending_control.is_some() {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Run control request is already pending.",
        )
        .into_response();
    }
    if let Err(err) = append_control_request(
        state.as_ref(),
        id,
        RunControlAction::Pause,
        Some(Principal::User(subject.0.clone())),
    )
    .await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    let response_status = match mode {
        PauseMode::Signal { worker_pid } => {
            #[cfg(unix)]
            fabro_proc::sigusr1(worker_pid);
            #[cfg(not(unix))]
            let _ = worker_pid;
            RunStatus::Running
        }
        PauseMode::AppendEvent => {
            if let Some(response) = synchronous_transition(state.as_ref(), id, |events| {
                events.push(workflow_event::Event::RunPaused);
            })
            .await
            {
                return response;
            }
            state
                .runs
                .lock()
                .expect("runs lock poisoned")
                .get(&id)
                .map_or(RunStatus::Paused { prior_block: None }, |run| run.status)
        }
    };
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(RunStatusResponse {
            id: id.to_string(),
            status: response_status,
            error: None,
            queue_position: None,
            pending_control,
            created_at,
        }),
    )
        .into_response()
}

async fn unpause_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let (created_at, mode) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => match managed_run.status {
                RunStatus::Paused {
                    prior_block: Some(_),
                } => (managed_run.created_at, UnpauseMode::AppendEvent),
                RunStatus::Paused { prior_block: None } => {
                    let Some(worker_pid) = managed_run.worker_pid else {
                        return ApiError::new(StatusCode::CONFLICT, "Run worker is not available.")
                            .into_response();
                    };
                    (managed_run.created_at, UnpauseMode::Signal { worker_pid })
                }
                _ => {
                    return ApiError::new(StatusCode::CONFLICT, "Run is not paused.")
                        .into_response();
                }
            },
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if pending_control.is_some() {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Run control request is already pending.",
        )
        .into_response();
    }
    if let Err(err) = append_control_request(
        state.as_ref(),
        id,
        RunControlAction::Unpause,
        Some(Principal::User(subject.0.clone())),
    )
    .await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    let response_status = match mode {
        UnpauseMode::Signal { worker_pid } => {
            #[cfg(unix)]
            fabro_proc::sigusr2(worker_pid);
            #[cfg(not(unix))]
            let _ = worker_pid;
            RunStatus::Paused { prior_block: None }
        }
        UnpauseMode::AppendEvent => {
            if let Some(response) = synchronous_transition(state.as_ref(), id, |events| {
                events.push(workflow_event::Event::RunUnpaused);
            })
            .await
            {
                return response;
            }
            state
                .runs
                .lock()
                .expect("runs lock poisoned")
                .get(&id)
                .map_or(RunStatus::Running, |run| run.status)
        }
    };
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(RunStatusResponse {
            id: id.to_string(),
            status: response_status,
            error: None,
            queue_position: None,
            pending_control,
            created_at,
        }),
    )
        .into_response()
}

async fn archive_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    run_archive_action(state, subject, id, ArchiveAction::Archive).await
}

async fn unarchive_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    run_archive_action(state, subject, id, ArchiveAction::Unarchive).await
}

async fn rewind_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<RewindRequest>>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let request = body.map(|Json(body)| body).unwrap_or_default();
    let target = match parse_fork_target(request.target) {
        Ok(target) => target,
        Err(err) => return err.into_response(),
    };
    let input = operations::RewindInput { run_id: id, target };
    match Box::pin(operations::rewind(
        &state.store,
        &input,
        Some(Principal::User(subject.0.clone())),
    ))
    .await
    {
        Ok(operations::RewindOutcome::Full {
            source_run_id,
            new_run_id,
            target,
        }) => (
            StatusCode::OK,
            Json(RewindResponse {
                source_run_id: source_run_id.to_string(),
                new_run_id:    new_run_id.to_string(),
                target:        target.response_target(),
                archived:      true,
                archive_error: None,
            }),
        )
            .into_response(),
        Ok(operations::RewindOutcome::Partial {
            source_run_id,
            new_run_id,
            target,
            archive_error,
        }) => (
            StatusCode::MULTI_STATUS,
            Json(RewindResponse {
                source_run_id: source_run_id.to_string(),
                new_run_id:    new_run_id.to_string(),
                target:        target.response_target(),
                archived:      false,
                archive_error: Some(archive_error),
            }),
        )
            .into_response(),
        Err(err) => workflow_operation_error_response(err),
    }
}

async fn fork_run(
    _subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<ForkRequest>>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let request = body.map(|Json(body)| body).unwrap_or_default();
    let target = match parse_fork_target(request.target) {
        Ok(target) => target,
        Err(err) => return err.into_response(),
    };
    let input = operations::ForkRunInput {
        source_run_id: id,
        target,
    };
    match Box::pin(operations::fork_run(&state.store, &input)).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(ForkResponse {
                source_run_id: outcome.source_run_id.to_string(),
                new_run_id:    outcome.new_run_id.to_string(),
                target:        outcome.target.response_target(),
            }),
        )
            .into_response(),
        Err(err) => workflow_operation_error_response(err),
    }
}

async fn run_timeline(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match operations::timeline(&state.store, &id).await {
        Ok(entries) => Json(
            entries
                .into_iter()
                .map(|entry| TimelineEntryResponse {
                    ordinal:        std::num::NonZeroU64::new(entry.ordinal as u64)
                        .expect("timeline ordinals start at 1"),
                    node_name:      entry.node_name,
                    visit:          std::num::NonZeroU64::new(entry.visit as u64)
                        .expect("timeline visits start at 1"),
                    checkpoint_seq: std::num::NonZeroU64::new(u64::from(entry.checkpoint_seq))
                        .expect("checkpoint event sequence starts at 1"),
                    run_commit_sha: entry.run_commit_sha,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(err) => workflow_operation_error_response(err),
    }
}

fn parse_fork_target(target: Option<String>) -> Result<Option<operations::ForkTarget>, ApiError> {
    target
        .map(|target| {
            target
                .parse::<operations::ForkTarget>()
                .map_err(|err| ApiError::bad_request(err.to_string()))
        })
        .transpose()
}

fn workflow_operation_error_response(err: WorkflowError) -> Response {
    match err {
        WorkflowError::Parse(message) | WorkflowError::Validation(message) => {
            ApiError::bad_request(message).into_response()
        }
        WorkflowError::ValidationFailed { .. } => {
            ApiError::bad_request("Validation failed").into_response()
        }
        WorkflowError::Precondition(message) => {
            ApiError::new(StatusCode::CONFLICT, message).into_response()
        }
        WorkflowError::RunNotFound(_) => ApiError::not_found("Run not found.").into_response(),
        WorkflowError::Unsupported(message) => {
            ApiError::new(StatusCode::NOT_IMPLEMENTED, message).into_response()
        }
        err => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Clone, Copy)]
enum ArchiveAction {
    Archive,
    Unarchive,
}

async fn run_archive_action(
    state: Arc<AppState>,
    subject: RequiredUser,
    id: String,
    action: ArchiveAction,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let actor = Some(Principal::User(subject.0.clone()));
    let result = match action {
        ArchiveAction::Archive => operations::archive(&state.store, &id, actor)
            .await
            .map(|_| ()),
        ArchiveAction::Unarchive => operations::unarchive(&state.store, &id, actor)
            .await
            .map(|_| ()),
    };
    match result {
        Ok(()) => archive_status_response(state.as_ref(), id).await,
        Err(WorkflowError::Precondition(message)) => {
            ApiError::new(StatusCode::CONFLICT, message).into_response()
        }
        Err(WorkflowError::RunNotFound(_)) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

/// Build a `RunStatusResponse` reflecting the durable projection after an
/// archive/unarchive transition. The run is terminal in both directions, so no
/// live queue position or worker-only fields apply.
async fn archive_status_response(state: &AppState, id: RunId) -> Response {
    let Ok(run_store) = state.store.open_run_reader(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let projection = match run_store.state().await {
        Ok(projection) => projection,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let Some(status) = projection.status else {
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "run has no status after archive/unarchive",
        )
        .into_response();
    };
    (
        StatusCode::OK,
        Json(RunStatusResponse {
            id: id.to_string(),
            status,
            error: None,
            queue_position: None,
            pending_control: None,
            created_at: id.created_at(),
        }),
    )
        .into_response()
}

/// Persist a synchronous pause/unpause transition: append the caller-supplied
/// events to the run store and mirror the new status in the in-memory run map.
/// Returns `Some(Response)` on error, `None` on success.
async fn synchronous_transition(
    state: &AppState,
    id: RunId,
    append_events: impl FnOnce(&mut Vec<workflow_event::Event>),
) -> Option<Response> {
    let run_store = match state.store.open_run(&id).await {
        Ok(run_store) => run_store,
        Err(err) => {
            return Some(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            );
        }
    };
    let mut events = Vec::new();
    append_events(&mut events);
    for event in events {
        if let Err(err) = workflow_event::append_event(&run_store, &id, &event).await {
            return Some(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            );
        }
        let stored = workflow_event::to_run_event(&id, &event);
        update_live_run_from_event(state, id, &stored);
    }
    None
}

async fn list_models(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ModelListParams>,
) -> Response {
    let provider = match params.provider.as_deref() {
        Some(value) => match Provider::from_str(value) {
            Ok(provider) => Some(provider),
            Err(_) => {
                return ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("unknown provider: {value}"),
                )
                .into_response();
            }
        },
        None => None,
    };

    let query = params.query.as_ref().map(|value| value.to_lowercase());
    let limit = params.limit.clamp(1, 100) as usize;
    let offset = params.offset.min(MAX_PAGE_OFFSET) as usize;
    let configured: HashSet<Provider> = state
        .llm_source
        .configured_providers()
        .await
        .into_iter()
        .collect();

    let mut models = fabro_model::Catalog::builtin()
        .list(provider)
        .into_iter()
        .filter(|model| match &query {
            Some(query) => {
                model.id.to_lowercase().contains(query)
                    || model.display_name.to_lowercase().contains(query)
                    || model
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase().contains(query))
            }
            None => true,
        })
        .cloned()
        .map(|mut model| {
            model.configured = configured.contains(&model.provider);
            model
        })
        .collect::<Vec<_>>();

    let has_more = models.len() > offset.saturating_add(limit);
    let data = models.drain(offset..models.len().min(offset.saturating_add(limit)));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": data.collect::<Vec<_>>(),
            "meta": { "has_more": has_more }
        })),
    )
        .into_response()
}

async fn test_model(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ModelTestParams>,
) -> Response {
    let mode = match params.mode.as_deref() {
        Some(value) => match ModelTestMode::from_str(value) {
            Ok(mode) => mode,
            Err(_) => {
                return ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid model test mode: {value}"),
                )
                .into_response();
            }
        },
        None => ModelTestMode::Basic,
    };
    let Some(info) = fabro_model::Catalog::builtin().get(&id) else {
        return ApiError::not_found(format!("Model not found: {id}")).into_response();
    };

    let llm_result = match state.resolve_llm_client().await {
        Ok(result) => result,
        Err(err) => {
            error!(error = ?err, "Failed to resolve LLM client");
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to resolve LLM client: {err}"),
            )
            .into_response();
        }
    };
    if let Some((_, issue)) = llm_result
        .auth_issues
        .iter()
        .find(|(provider, _)| *provider == info.provider)
    {
        return ApiError::bad_request(auth_issue_message(info.provider, issue)).into_response();
    }
    let provider_name = <&'static str>::from(info.provider);
    if !llm_result.client.provider_names().contains(&provider_name) {
        return Json(serde_json::json!({
            "model_id": info.id,
            "status": "skip",
        }))
        .into_response();
    }
    let client = Arc::new(llm_result.client);

    let outcome = run_model_test(info, mode, client).await;
    Json(serde_json::json!({
        "model_id": info.id,
        "status": <&'static str>::from(outcome.status),
        "error_message": outcome.error_message,
    }))
    .into_response()
}

fn finish_reason_to_api_stop_reason(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop => "end_turn".to_string(),
        FinishReason::Length => "max_tokens".to_string(),
        FinishReason::ToolCalls => "tool_calls".to_string(),
        FinishReason::ContentFilter => "content_filter".to_string(),
        FinishReason::Error => "error".to_string(),
        FinishReason::Other(s) => s.clone(),
    }
}

fn convert_api_message(msg: &CompletionMessage) -> LlmMessage {
    let role = match msg.role {
        CompletionMessageRole::System => Role::System,
        CompletionMessageRole::User => Role::User,
        CompletionMessageRole::Assistant => Role::Assistant,
        CompletionMessageRole::Tool => Role::Tool,
        CompletionMessageRole::Developer => Role::Developer,
    };
    let content: Vec<ContentPart> = msg
        .content
        .iter()
        .filter_map(|part| {
            let json = serde_json::to_value(part).ok()?;
            serde_json::from_value(json).ok()
        })
        .collect();
    LlmMessage {
        role,
        content,
        name: msg.name.clone(),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

fn convert_llm_message(msg: &LlmMessage) -> CompletionMessage {
    let role = match msg.role {
        Role::System => CompletionMessageRole::System,
        Role::User => CompletionMessageRole::User,
        Role::Assistant => CompletionMessageRole::Assistant,
        Role::Tool => CompletionMessageRole::Tool,
        Role::Developer => CompletionMessageRole::Developer,
    };
    let content: Vec<CompletionContentPart> = msg
        .content
        .iter()
        .filter_map(|part| {
            let json = serde_json::to_value(part).ok()?;
            serde_json::from_value(json).ok()
        })
        .collect();
    CompletionMessage {
        role,
        content,
        name: msg.name.clone(),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

async fn create_completion(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateCompletionRequest>,
) -> Response {
    // Resolve model
    let model_id = req.model.unwrap_or_else(|| {
        fabro_model::Catalog::builtin()
            .list(None)
            .first()
            .map_or_else(|| "claude-sonnet-4-5".to_string(), |m| m.id.clone())
    });

    let catalog_info = fabro_model::Catalog::builtin().get(&model_id);

    // Resolve provider: explicit request > catalog > None
    let provider_name = req
        .provider
        .or_else(|| catalog_info.map(|i| i.provider.to_string()));

    info!(model = %model_id, provider = ?provider_name, "Completion request received");

    // Build messages list
    let mut messages: Vec<LlmMessage> = Vec::new();
    if let Some(system) = req.system {
        messages.push(LlmMessage::system(system));
    }
    for msg in &req.messages {
        messages.push(convert_api_message(msg));
    }

    // Convert tools
    let tools: Option<Vec<ToolDefinition>> = if req.tools.is_empty() {
        None
    } else {
        Some(
            req.tools
                .into_iter()
                .map(|t| ToolDefinition {
                    name:        t.name,
                    description: t.description,
                    parameters:  t.parameters,
                })
                .collect(),
        )
    };

    // Convert tool_choice
    let tool_choice: Option<ToolChoice> = req.tool_choice.map(|tc| match tc.mode {
        CompletionToolChoiceMode::Auto => ToolChoice::Auto,
        CompletionToolChoiceMode::None => ToolChoice::None,
        CompletionToolChoiceMode::Required => ToolChoice::Required,
        CompletionToolChoiceMode::Named => ToolChoice::named(tc.tool_name.unwrap_or_default()),
    });

    // Build the LLM request
    let request = LlmRequest {
        model: model_id.clone(),
        messages,
        provider: provider_name,
        tools,
        tool_choice,
        response_format: None,
        temperature: req.temperature,
        top_p: req.top_p,
        max_tokens: req.max_tokens,
        stop_sequences: if req.stop_sequences.is_empty() {
            None
        } else {
            Some(req.stop_sequences)
        },
        reasoning_effort: req.reasoning_effort.as_deref().and_then(|s| s.parse().ok()),
        speed: None,
        metadata: None,
        provider_options: req.provider_options,
    };

    // Force non-streaming for structured output
    let use_stream = req.stream && req.schema.is_none();

    let llm_result = match state.resolve_llm_client().await {
        Ok(result) => result,
        Err(err) => {
            error!(error = ?err, "Failed to create LLM client");
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create LLM client: {err}"),
            )
            .into_response();
        }
    };
    for (provider, issue) in &llm_result.auth_issues {
        warn!(provider = %provider, error = %issue, "LLM provider unavailable due to auth issue");
    }
    let client = llm_result.client;

    if use_stream {
        // Streaming path: forward all StreamEvents as SSE
        let stream_result = match client.stream(&request).await {
            Ok(s) => s,
            Err(e) => {
                return ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response();
            }
        };

        let sse_stream = tokio_stream::StreamExt::filter_map(stream_result, |event| match event {
            Ok(ref evt) => match serde_json::to_string(evt) {
                Ok(json) => Some(Ok::<_, std::convert::Infallible>(
                    Event::default().event("stream_event").data(json),
                )),
                Err(e) => Some(Ok(Event::default().event("stream_event").data(
                    serde_json::json!({
                        "type": "error",
                        "error": {"Stream": {"message": format!("failed to serialize event: {e}")}},
                        "raw": null
                    })
                    .to_string(),
                ))),
            },
            Err(e) => Some(Ok(Event::default().event("stream_event").data(
                serde_json::json!({
                    "type": "error",
                    "error": {"Stream": {"message": e.to_string()}},
                    "raw": null
                })
                .to_string(),
            ))),
        });

        Sse::new(sse_stream)
            .keep_alive(
                KeepAlive::new().interval(Duration::from_secs(15)).event(
                    Event::default()
                        .event("ping")
                        .data(serde_json::json!({"type": "ping"}).to_string()),
                ),
            )
            .into_response()
    } else {
        // Non-streaming path
        let msg_id = Ulid::new().to_string();

        if let Some(schema) = req.schema {
            // Structured output uses generate_object for JSON parsing logic
            let mut params =
                GenerateParams::new(&request.model, std::sync::Arc::new(client.clone()))
                    .messages(request.messages);
            if let Some(ref p) = request.provider {
                params = params.provider(p);
            }
            if let Some(temp) = request.temperature {
                params = params.temperature(temp);
            }
            if let Some(max_tokens) = request.max_tokens {
                params = params.max_tokens(max_tokens);
            }
            if let Some(top_p) = request.top_p {
                params = params.top_p(top_p);
            }
            match generate_object(params, schema).await {
                Ok(result) => Json(CompletionResponse {
                    id:          msg_id,
                    model:       model_id,
                    message:     convert_llm_message(&result.response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&result.finish_reason),
                    usage:       CompletionUsage {
                        input_tokens:  result.usage.input_tokens,
                        output_tokens: result.usage.output_tokens,
                    },
                    output:      result.output,
                })
                .into_response(),
                Err(e) => ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response(),
            }
        } else {
            match client.complete(&request).await {
                Ok(response) => Json(CompletionResponse {
                    id:          response.id,
                    model:       response.model,
                    message:     convert_llm_message(&response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&response.finish_reason),
                    usage:       CompletionUsage {
                        input_tokens:  response.usage.input_tokens,
                        output_tokens: response.usage.output_tokens,
                    },
                    output:      None,
                })
                .into_response(),
                Err(e) => ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response(),
            }
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Render-graph subprocess startup resolves Cargo's test binary env override when present."
)]
fn render_graph_subprocess_exe(
    exe_override: Option<&std::path::Path>,
) -> Result<PathBuf, RenderSubprocessError> {
    if let Some(path) = exe_override {
        Ok(path.to_path_buf())
    } else {
        if let Some(path) = std::env::var_os(EnvVars::CARGO_BIN_EXE_FABRO).map(PathBuf::from) {
            return Ok(path);
        }

        let current = std::env::current_exe()
            .map_err(|err| RenderSubprocessError::SpawnFailed(err.to_string()))?;
        let current_name = current.file_stem().and_then(|name| name.to_str());
        if current_name == Some("fabro") {
            return Ok(current);
        }

        let candidate = current
            .parent()
            .and_then(|parent| parent.parent())
            .map(|parent| parent.join(if cfg!(windows) { "fabro.exe" } else { "fabro" }));
        if let Some(candidate) = candidate.filter(|path| path.is_file()) {
            return Ok(candidate);
        }

        Ok(current)
    }
}

fn render_subprocess_failure(
    status: std::process::ExitStatus,
    stderr: &[u8],
) -> RenderSubprocessError {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        if let Some(signal) = status.signal() {
            let stderr = String::from_utf8_lossy(stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("terminated by signal {signal}")
            } else {
                format!("terminated by signal {signal}: {stderr}")
            };
            return RenderSubprocessError::ChildCrashed(detail);
        }
    }

    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let detail = match status.code() {
        Some(code) if stderr.is_empty() => format!("exited with status {code}"),
        Some(code) => format!("exited with status {code}: {stderr}"),
        None if stderr.is_empty() => "child exited unsuccessfully".to_string(),
        None => format!("child exited unsuccessfully: {stderr}"),
    };
    RenderSubprocessError::ChildCrashed(detail)
}

async fn render_dot_subprocess(
    styled_source: &str,
    exe_override: Option<&std::path::Path>,
) -> Result<Vec<u8>, RenderSubprocessError> {
    let _permit = GRAPHVIZ_RENDER_SEMAPHORE
        .acquire()
        .await
        .map_err(|err| RenderSubprocessError::SpawnFailed(err.to_string()))?;
    let exe = render_graph_subprocess_exe(exe_override)?;
    let mut cmd = Command::new(exe);
    apply_render_graph_env(&mut cmd);
    cmd.arg("__render-graph")
        .env(EnvVars::FABRO_TELEMETRY, "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|err| RenderSubprocessError::SpawnFailed(err.to_string()))?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        RenderSubprocessError::SpawnFailed("render subprocess stdin was not piped".to_string())
    })?;
    if let Err(err) = stdin.write_all(styled_source.as_bytes()).await {
        drop(stdin);
        let output = child
            .wait_with_output()
            .await
            .map_err(|wait_err| RenderSubprocessError::SpawnFailed(wait_err.to_string()))?;
        return Err(RenderSubprocessError::ChildCrashed(format!(
            "failed writing DOT to child stdin: {err}; {}",
            render_subprocess_failure(output.status, &output.stderr)
        )));
    }
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .map_err(|err| RenderSubprocessError::SpawnFailed(err.to_string()))?;

    if !output.status.success() {
        return Err(render_subprocess_failure(output.status, &output.stderr));
    }

    if let Some(error) = output.stdout.strip_prefix(RENDER_ERROR_PREFIX) {
        return Err(RenderSubprocessError::RenderFailed(
            String::from_utf8_lossy(error).trim().to_string(),
        ));
    }

    if output.stdout.starts_with(b"<?xml") || output.stdout.starts_with(b"<svg") {
        return Ok(output.stdout);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(RenderSubprocessError::ProtocolViolation(format!(
        "stdout did not contain SVG or error protocol (stdout: {:?}, stderr: {:?})",
        stdout.trim(),
        stderr.trim()
    )))
}

async fn render_graph_response(
    dot_source: &str,
    exe_override: Option<&std::path::Path>,
) -> Response {
    use fabro_graphviz::render::{inject_dot_style_defaults, postprocess_svg};

    let styled_source = inject_dot_style_defaults(dot_source);
    match render_dot_subprocess(&styled_source, exe_override).await {
        Ok(raw) => {
            let bytes = postprocess_svg(raw);
            (StatusCode::OK, [("content-type", "image/svg+xml")], bytes).into_response()
        }
        Err(RenderSubprocessError::RenderFailed(err)) => {
            ApiError::new(StatusCode::BAD_REQUEST, err).into_response()
        }
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

pub(crate) async fn render_graph_bytes(dot_source: &str) -> Response {
    render_graph_response(dot_source, None).await
}

#[cfg(test)]
async fn render_graph_bytes_with_exe_override(
    dot_source: &str,
    exe_override: Option<&std::path::Path>,
) -> Response {
    render_graph_response(dot_source, exe_override).await
}

#[derive(serde::Deserialize)]
struct GraphParams {
    #[serde(default)]
    direction: Option<String>,
}

async fn load_run_dot_source(state: &AppState, id: &RunId) -> Result<String, Response> {
    let live_dot_source = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        runs.get(id)
            .map(|managed_run| managed_run.dot_source.clone())
    };

    let dot_source = if let Some(dot) = live_dot_source.filter(|d| !d.is_empty()) {
        Some(dot)
    } else {
        match state.store.open_run_reader(id).await {
            Ok(run_store) => match run_store.state().await {
                Ok(run_state) => run_state.graph_source,
                Err(err) => {
                    return Err(
                        ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response()
                    );
                }
            },
            Err(_) => return Err(ApiError::not_found("Run not found.").into_response()),
        }
    };

    dot_source
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Graph not found.").into_response())
}

async fn get_graph(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let dot = match load_run_dot_source(&state, &id).await {
        Ok(dot) => dot,
        Err(response) => return response,
    };

    let dot = match params.direction.as_deref() {
        Some(dir @ ("LR" | "TB" | "BT" | "RL")) => {
            use fabro_graphviz::render;
            render::apply_direction(&dot, dir).into_owned()
        }
        _ => dot,
    };

    render_graph_bytes(&dot).await
}

async fn get_graph_source(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    match load_run_dot_source(&state, &id).await {
        Ok(dot) => (StatusCode::OK, [("content-type", "text/vnd.graphviz")], dot).into_response(),
        Err(response) => response,
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "server unit tests stage fixtures with sync std::fs writes"
)]
mod tests;
