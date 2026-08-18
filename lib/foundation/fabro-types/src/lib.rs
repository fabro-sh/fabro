extern crate self as fabro_types;

pub mod artifact;
pub mod auth;
pub mod billing;
pub mod blob_hash;
pub mod blob_ref;
pub mod checkpoint;
pub mod command_output;
pub mod conclusion;
pub mod dense;
pub mod diff;
pub mod event_envelope;
pub mod external_agent;
pub mod failure_signature;
pub mod graph;
mod id;
mod input_scalar;
pub mod interview;
pub mod llm_backend;
pub mod manifest_path;
pub mod mcp_store;
pub mod outcome;
pub mod pair;
pub mod parallel;
pub mod plane_dispatch;
pub mod principal;
pub mod pull_request;
pub mod reasoning;
pub mod repository;
pub mod run;
pub mod run_event;
pub mod run_failure;
pub mod run_id;
pub mod run_projection;
pub mod run_sandbox;
pub mod run_summary;
pub mod run_title;
pub mod sandbox_details;
pub mod sandbox_inventory;
pub mod sandbox_provider;
pub mod sandbox_services;
pub mod secret;
pub mod session;
pub mod settings;
pub mod stage_completion;
pub mod stage_handler;
pub mod stage_id;
pub mod start;
pub mod status;
pub mod steering;
pub mod system_integrations;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod timing;
pub mod todo;
pub mod transcript;
pub mod variable;
pub mod workflow_path;
pub mod workflow_version;
pub mod workflow_version_id;

pub use artifact::ArtifactUpload;
pub use auth::{IdpIdentity, IdpIdentityError};
pub use billing::{
    AnthropicBillingFacts, AnthropicModelPricing, BilledModelUsage, BilledTokenCounts,
    GeminiBillingFacts, GeminiModelPricing, GeminiStoragePricing, GeminiStorageSegment,
    ModelBillingFacts, ModelBillingInput, ModelPricing, ModelPricingPolicy, ModelRef, ModelUsage,
    OpenAiBillingFacts, OpenAiModelPricing, PricePerMTok, Speed, TokenCounts, UsdMicros,
};
pub use blob_hash::BlobHash;
pub use blob_ref::{format_blob_ref, parse_blob_ref, parse_managed_blob_file_ref};
pub use checkpoint::Checkpoint;
pub use command_output::{CommandOutputStream, CommandTermination};
pub use conclusion::{Conclusion, StageSummary};
pub use dense::{ServerSettings, UserSettings, WorkflowSettings};
pub use diff::{DiffStats, DiffSummary, RunDiff};
pub use event_envelope::EventEnvelope;
pub use external_agent::{ExternalAgentHarness, ExternalAgentProfile, ExternalAgentsSettings};
pub use fabro_model::ReasoningEffort;
pub use failure_signature::FailureSignature;
pub use graph::{
    AttrValue, ContextKeyAttr, Edge, Graph, KNOWN_HANDLER_TYPES, Node, is_known_handler_type,
    is_llm_handler_type, shape_to_handler_type,
};
pub use input_scalar::{JsonScalarToTomlError, json_scalar_to_toml_value};
pub use interview::{
    InterviewQuestionRecord, QuestionType, ReviewTarget, ReviewTargetError, ReviewTargetKind,
};
pub use llm_backend::AgentBackend;
pub use manifest_path::{ManifestPath, ManifestPathParseError};
pub use mcp_store::{
    McpServerDefinition, McpServerDraft, McpServerId, McpServerReplace, McpServerRevision,
    McpServerRevisionParseError, McpServerValidationError, McpServerView, McpTransportView,
    validate_mcp_server_fields,
};
pub use outcome::{
    FailureCategory, FailureDetail, NodeResult, Outcome, OutcomeMeta, StageOutcome, StageState,
};
pub use pair::{
    MAX_PAIR_MESSAGE_BYTES, PairId, PairMessageId, PairMessageRecord, PairMessageRequest,
    PairRecord, PairStartRequest, PairStatus, PairSystemMessageKind, PairTarget,
    PairTranscriptAssistantMessage, PairTranscriptDetailRef, PairTranscriptEntry,
    PairTranscriptError, PairTranscriptMeta, PairTranscriptResponse, PairTranscriptSystemMessage,
    PairTranscriptToolCall, PairTranscriptToolStatus, PairTranscriptUserMessage,
    PairTranscriptWarning, RunEventDetailContent, RunEventDetailContentKind,
    RunEventDetailEnvelope, RunEventDetailResponse, RunPairStatusResponse,
};
pub use parallel::ParallelBranchResult;
pub use plane_dispatch::{
    PlaneDispatch, PlaneDispatchListResponse, PlaneDispatchStatus, PlaneLabelResponse,
    PlaneProjectMetadataResponse, PlaneProjectResponse, PlaneProjectsResponse, PlaneStateResponse,
};
pub use principal::{AuthMethod, Principal, SystemActorKind, UserPrincipal};
pub use pull_request::{
    CheckRun, CheckRunStatus, PullRequest, PullRequestCreation, PullRequestCreationId,
    PullRequestCreationStatus, PullRequestDetails, PullRequestDetailsStatus,
    PullRequestDetailsUnavailableReason, PullRequestGithubDetail, PullRequestLink, PullRequestMeta,
    PullRequestRef, PullRequestResponse, PullRequestTimestamps, PullRequestUser,
};
pub use reasoning::ReasoningOutput;
pub use repository::{GitHubRepositorySlug, RepositoryProvider, RepositoryRef};
pub use run::{
    DirtyStatus, ForkSourceRef, GitContext, RunClientProvenance, RunProvenance,
    RunServerProvenance, RunSpec,
};
pub use run_event::{
    AgentMcpToolSummary, AgentMemoryFileProps, AgentSkillActivationSource, AgentSkillSummary,
    AgentToolCategory, AgentToolSource, AgentToolSummary, AgentToolsAvailableProps, EventBody,
    ExecOutputTail, FailoverProps, INITIAL_SUBAGENT_GENERATION, InterviewOption, LlmOutputKind,
    LlmRetryPhase, MetadataSnapshotFailureKind, MetadataSnapshotPhase, RunEvent, RunNoticeCode,
    RunNoticeLevel, RunPairEndedReason, RunPairFailedReason, RunRunnableSource, SessionCapability,
    TodoCreatedProps, TodoDeletedProps, TodoUpdatedProps, initial_subagent_generation,
};
pub use run_failure::RunFailure;
pub use run_id::{RunId, fixtures};
pub use run_projection::{
    ActivatedSkill, AgentControlState, CheckpointRecord, McpServerProjection, McpServerStatus,
    PendingInterviewRecord, RunProjection, SkillsProjection, StageContextWindow,
    StageContextWindowBreakdownItem, StageContextWindowCategory, StageContextWindowCountMethod,
    StageContextWindowProjection, StageContextWindowStaleness, StageContextWindowUnavailableReason,
    StageContextWindowWarning, StageInferenceProjection, StageModelUsage, StageProjection,
    StageToolBatchProjection, SubAgentProjection, SubAgentStatus, first_event_seq,
};
pub use run_sandbox::{
    RunSandbox, RunSandboxFailure, RunSandboxInstance, RunSandboxKind, RunSandboxPlan,
    RunSandboxRuntime,
};
pub use run_summary::{
    AskFabro, AskFabroUnavailableReason, AutomationRef, Run, RunApproval, RunApprovalState,
    RunBillingSummary, RunError, RunLifecycle, RunLinks, RunModel, RunOrigin, RunOriginKind,
    RunSize, RunTimestamps, WorkflowRef,
};
pub use run_title::{
    MAX_RUN_TITLE_CHARS, RunTitleError, infer_run_title, normalize_explicit_run_title,
};
pub use sandbox_details::{
    SandboxDetails, SandboxNetwork, SandboxNetworkPolicy, SandboxNetworkPolicyMode,
    SandboxResources, SandboxState, SandboxTimestamps,
};
pub use sandbox_inventory::{
    SandboxInfo, SandboxListMeta, SandboxListResponse, SandboxProviderLookupError,
};
pub use sandbox_provider::SandboxProviderKind;
pub use sandbox_services::{
    SandboxService, SandboxServiceDiscoverySource, SandboxServiceListMeta,
    SandboxServiceListResponse,
};
pub use secret::{OAuthConfig, OAuthCredential, OAuthTokens, SecretMetadata, SecretType};
pub use session::{
    PermissionLevel, SessionDetail, SessionId, SessionMessage, SessionRecord, SessionStatus,
    SessionSummary, SessionTurn, TurnId,
};
pub use stage_completion::StageCompletion;
pub use stage_handler::StageHandler;
pub use stage_id::{InvalidStageVisit, ParallelBranchId, StageId};
pub use start::StartRecord;
pub use status::{
    BlockedReason, FailureReason, InvalidTransition, PendingReason, RunControlAction, RunStatus,
    RunStatusKind, SuccessReason, TerminalStatus,
};
pub use steering::SteeringMessage;
pub use system_integrations::{
    IntegrationConnectionKind, IntegrationConnectionState, IntegrationConnectionStatus,
    IntegrationProvider, IntegrationStatus, SystemIntegrationStatus, SystemIntegrationsResponse,
};
pub use timing::{RunTiming, StageTiming};
pub use todo::{TodoListKind, TodoListProjection, TodoPatch, TodoProjection, TodoStatus};
pub use transcript::{
    AudioData, ContentPart, DocumentData, ImageData, Message, MessageId, MessageKind,
    MessageSource, PairMessageRef, Role, ThinkingData, ToolCall, ToolResult, TranscriptMessage,
};
pub use variable::{
    CreateVariableRequest, UpdateVariableRequest, Variable, VariableListResponse, is_env_style_name,
};
pub use workflow_path::{
    MAX_WORKFLOW_PATH_BYTES, MAX_WORKFLOW_PATH_COMPONENTS, WorkflowPath, WorkflowPathParseError,
};
pub use workflow_version::{
    MAX_WORKFLOW_VERSION_BYTES, MAX_WORKFLOW_VERSION_DEPENDENCIES, MAX_WORKFLOW_VERSION_FILE_BYTES,
    MAX_WORKFLOW_VERSION_FILES, WorkflowVersion, WorkflowVersionShapeError,
};
pub use workflow_version_id::{WorkflowVersionId, WorkflowVersionIdParseError};
