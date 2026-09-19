extern crate self as fabro_types;

pub mod agent_props;
pub mod artifact;
pub mod auth;
pub mod blob_hash;
pub mod blob_ref;
pub mod catalog_api;
pub mod checkpoint;
pub mod command_output;
pub mod conclusion;
pub mod dense;
pub mod diagnostic;
pub mod diff;
pub mod engine;
pub mod failure_signature;
pub mod git_identity;
pub mod graph;
mod id;
mod input_scalar;
pub mod interview;
pub mod llm_backend;
pub mod manifest_path;
pub mod mcp_store;
pub mod model_test;
pub mod notice;
pub mod outcome;
pub mod pair;
pub mod parallel;
pub mod principal;
pub mod pull_request;
pub mod repository;
pub mod run;
pub mod run_failure;
pub mod run_id;
pub mod run_intent;
pub mod run_projection;
pub mod run_sandbox;
pub mod run_stream;
pub mod run_summary;
pub mod run_title;
pub mod sandbox_details;
pub mod sandbox_inventory;
pub mod sandbox_provider;
pub mod sandbox_services;
pub mod secret;
pub mod session;
pub mod session_event;
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
pub mod transcript;
pub mod usage;
pub mod usage_rollup;
pub mod variable;
pub mod workflow_path;
pub mod workflow_version;
pub mod workflow_version_id;

pub use agent_props::{
    AgentEventProps, AgentSessionActivatedProps, AgentToolsAvailableProps, CODING_EVENT_NAMES,
    SessionCapability, StagePromptProps, coding_event_name, is_coding_event_name,
};
pub use artifact::ArtifactUpload;
pub use auth::{IdpIdentity, IdpIdentityError};
pub use blob_hash::BlobHash;
pub use blob_ref::{
    BlobRefEncoding, format_blob_ref, parse_blob_ref, parse_blob_ref_encoded,
    parse_managed_blob_file_ref,
};
pub use catalog_api::{Model, ModelControls, ModelCosts, ModelFeatures, ModelLimits, Provider};
pub use checkpoint::Checkpoint;
pub use command_output::{CommandOutputStream, CommandTermination};
pub use conclusion::{Conclusion, StageSummary};
pub use dense::{ServerSettings, UserSettings, WorkflowSettings};
pub use diff::{DiffStats, DiffSummary, RunDiff};
pub use engine::{PetriAdmission, PetriGraphRef};
pub use failure_signature::FailureSignature;
pub use git_identity::{GitIdentity, GitIdentitySource};
pub use graph::{
    AttrValue, AttributeScope, ContextKeyAttr, Edge, Graph, KNOWN_HANDLER_TYPES, Node, OnFailure,
    ResolvedOnFailure, is_known_handler_type, is_llm_handler_type, shape_to_handler_type,
};
pub use input_scalar::{
    JsonScalarToTomlError, TomlScalarToJsonError, json_scalar_to_toml_value,
    toml_scalar_to_json_value,
};
pub use interview::{
    InterviewOption, InterviewQuestionRecord, QuestionType, ReviewTarget, ReviewTargetError,
    ReviewTargetKind,
};
pub use llm_backend::AgentBackend;
pub use manifest_path::{ManifestPath, ManifestPathParseError};
pub use mcp_store::{
    McpServerDefinition, McpServerDraft, McpServerId, McpServerReplace, McpServerRevision,
    McpServerRevisionParseError, McpServerValidationError, McpServerView, McpTransportView,
    validate_mcp_server_fields,
};
pub use model_test::ModelTestMode;
pub use notice::{RunNoticeCode, RunNoticeLevel};
pub use outcome::{
    FailureCategory, FailureDetail, NodeResult, Outcome, OutcomeMeta, StageOutcome, StageState,
};
pub use pair::{
    MAX_PAIR_MESSAGE_BYTES, PairId, PairMessageId, PairMessageRecord, PairMessageRequest,
    PairRecord, PairStartRequest, PairStatus, PairSystemMessageKind, PairTarget,
    PairTranscriptAssistantMessage, PairTranscriptDetailRef, PairTranscriptEntry,
    PairTranscriptError, PairTranscriptMeta, PairTranscriptResponse, PairTranscriptSystemMessage,
    PairTranscriptToolCall, PairTranscriptToolStatus, PairTranscriptUserMessage,
    PairTranscriptWarning, RunPairStatusResponse,
};
pub use parallel::ParallelBranchResult;
pub use pebble_coding_agent::events::{
    AgentProfileKind, CodingAgentEvent, CodingEvent, ContextWindowBreakdownItem,
    ContextWindowCategory, ContextWindowCountMethod, ContextWindowSnapshot, ContextWindowStaleness,
    ContextWindowWarning, ExecOutputTail, ExecOutputTailTrace, INITIAL_SUBAGENT_GENERATION,
    LlmOutputKind, LlmRetryPhase, MemoryFileSummary, PermissionLevel, SkillActivationSource,
    SkillSummary, TodoCreatedProps, TodoDeletedProps, TodoListKind, TodoListProjection,
    TodoProjection, TodoStatus, TodoUpdatedProps, ToolCategory, ToolSource, ToolSummary,
};
pub use principal::{AuthMethod, Principal, SystemActorKind, UserPrincipal};
pub use pull_request::{
    CheckRun, CheckRunStatus, PullRequest, PullRequestCreation, PullRequestCreationId,
    PullRequestCreationStatus, PullRequestDetails, PullRequestDetailsStatus,
    PullRequestDetailsUnavailableReason, PullRequestGithubDetail, PullRequestLink, PullRequestMeta,
    PullRequestRef, PullRequestResponse, PullRequestTimestamps, PullRequestUser,
};
pub use repository::{
    GitHubRepositorySlug, GitHubRepositorySlugError, RepositoryProvider, RepositoryRef,
    is_valid_git_branch_name, is_valid_git_tag_name, normalize_git_commit_sha,
};
pub use run::{
    DirtyStatus, ForkSourceRef, GitContext, RunClientProvenance, RunProvenance,
    RunServerProvenance, RunSpec,
};
pub use run_failure::RunFailure;
pub use run_id::{RunId, fixtures};
pub use run_intent::{
    GitCoordinateValidationError, GitRunTarget, RunIntent, RunIntentArgs, RunTarget,
    TargetValidationError, ValidatedGitRunTarget, ValidatedRunTarget,
};
pub use run_projection::{
    CheckpointRecord, ForkOrigin, PendingInterviewRecord, RunArtifact, RunProjection,
    StageContextWindow, StageContextWindowUnavailableReason, StageInferenceProjection,
    StageModelUsage, StageProjection, StageToolBatchProjection, first_event_seq,
};
pub use run_sandbox::{
    RunSandbox, RunSandboxFailure, RunSandboxInstance, RunSandboxKind, RunSandboxPlan,
    RunSandboxRuntime,
};
pub use run_stream::{RunStreamItem, RunStreamItemKind, petri_event_name};
pub use run_summary::{
    AskFabro, AskFabroUnavailableReason, AutomationRef, ResolvedAutomationGitWorkflowSource, Run,
    RunApproval, RunApprovalState, RunError, RunLifecycle, RunLinks, RunModel, RunOrigin,
    RunOriginKind, RunSize, RunTimestamps, WorkflowRef,
};
pub use run_title::{
    MAX_RUN_TITLE_CHARS, RunTitleError, infer_run_title, normalize_explicit_run_title,
};
pub use sandbox_details::SandboxDetails;
pub use sandbox_inventory::{
    SandboxInfo, SandboxListMeta, SandboxListResponse, SandboxProviderLookupError,
};
pub use sandbox_provider::{
    BundledProvider, InvalidSandboxProviderKind, SandboxProviderKind, WorkspacePolicy,
};
pub use sandbox_services::{SandboxService, SandboxServiceListResponse};
pub use secret::{OAuthConfig, OAuthCredential, OAuthTokens, SecretMetadata, SecretType};
pub use session::{
    RunSessionMetadata, SessionDetail, SessionId, SessionStatus, SessionSummary, SessionTurn,
    TurnId,
};
pub use session_event::{SessionEvent, SessionEventBody};
pub use stage_completion::StageCompletion;
pub use stage_handler::StageHandler;
pub use stage_id::{InvalidStageVisit, ParallelBranchId, StageId};
pub use start::StartRecord;
pub use status::{
    BlockedReason, FailureReason, InvalidTransition, PendingReason, RunControlAction,
    RunRunnableSource, RunStatus, RunStatusKind, SuccessReason, TerminalStatus,
};
pub use steering::SteeringMessage;
pub use system_integrations::{
    IntegrationConnectionKind, IntegrationConnectionState, IntegrationConnectionStatus,
    IntegrationProvider, IntegrationStatus, SystemIntegrationStatus, SystemIntegrationsResponse,
};
pub use timing::{RunTiming, StageTiming};
pub use transcript::{
    MessageId, MessageKind, MessageSource, PairMessageRef, TranscriptMessage, text_of,
    tool_call_arguments, tool_result_from_json, tool_result_to_json,
};
pub use usage::{ModelRef, ModelUsage, sum_usage, usage_is_empty};
pub use variable::{
    CreateVariableRequest, UpdateVariableRequest, Variable, VariableListResponse, is_env_style_name,
};
pub use workflow_path::{
    MAX_WORKFLOW_PATH_BYTES, MAX_WORKFLOW_PATH_COMPONENTS, WorkflowPath, WorkflowPathParseError,
};
pub use workflow_version::{
    MAX_WORKFLOW_VERSION_BYTES, MAX_WORKFLOW_VERSION_DEPENDENCIES, MAX_WORKFLOW_VERSION_FILE_BYTES,
    MAX_WORKFLOW_VERSION_FILES, WorkflowVersion, WorkflowVersionShapeError,
    validate_workflow_files, validate_workflow_source_paths,
};
pub use workflow_version_id::{WorkflowVersionId, WorkflowVersionIdParseError};
