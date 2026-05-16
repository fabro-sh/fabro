extern crate self as fabro_types;

pub mod artifact;
pub mod auth;
pub mod billing;
pub mod blob_ref;
pub mod checkpoint;
pub mod command_output;
pub mod conclusion;
pub mod dense;
pub mod diff;
pub mod event_envelope;
pub mod failure_signature;
pub mod graph;
pub mod interview;
pub mod llm_backend;
pub mod outcome;
pub mod principal;
pub mod pull_request;
pub mod repository;
pub mod run;
pub mod run_blob_id;
pub mod run_event;
pub mod run_failure;
pub mod run_id;
pub mod run_projection;
pub mod run_sandbox;
pub mod run_summary;
pub mod run_title;
pub mod sandbox_details;
pub mod sandbox_provider;
pub mod sandbox_services;
pub mod secret;
pub mod settings;
pub mod stage_completion;
pub mod stage_handler;
pub mod stage_id;
pub mod start;
pub mod status;

pub use artifact::ArtifactUpload;
pub use auth::{IdpIdentity, IdpIdentityError};
pub use billing::{
    AnthropicBillingFacts, AnthropicModelPricing, BilledModelUsage, BilledTokenCounts,
    GeminiBillingFacts, GeminiModelPricing, GeminiStoragePricing, GeminiStorageSegment,
    ModelBillingFacts, ModelBillingInput, ModelPricing, ModelPricingPolicy, ModelRef, ModelUsage,
    OpenAiBillingFacts, OpenAiModelPricing, PricePerMTok, Speed, TokenCounts, UsdMicros,
};
pub use blob_ref::{format_blob_ref, parse_blob_ref, parse_managed_blob_file_ref};
pub use checkpoint::Checkpoint;
pub use command_output::{CommandOutputStream, CommandTermination};
pub use conclusion::{Conclusion, StageSummary};
pub use dense::{ServerSettings, UserSettings, WorkflowSettings};
pub use diff::{DiffStats, DiffSummary, RunDiff};
pub use event_envelope::EventEnvelope;
pub use failure_signature::FailureSignature;
pub use graph::{
    AttrValue, Edge, Graph, KNOWN_HANDLER_TYPES, Node, is_known_handler_type, is_llm_handler_type,
    shape_to_handler_type,
};
pub use interview::{InterviewQuestionRecord, QuestionType};
pub use llm_backend::LlmBackend;
pub use outcome::{
    FailureCategory, FailureDetail, NodeResult, Outcome, OutcomeMeta, StageOutcome, StageState,
};
pub use principal::{AuthMethod, Principal, SystemActorKind, UserPrincipal};
pub use pull_request::{
    CheckRun, CheckRunStatus, PullRequest, PullRequestDetails, PullRequestGithubDetail,
    PullRequestRef, PullRequestTimestamps, PullRequestUser,
};
pub use repository::{RepositoryProvider, RepositoryRef};
pub type RepositoryReference = RepositoryRef;
pub use run::{
    DirtyStatus, ForkSourceRef, GitContext, PreRunPushOutcome, RunClientProvenance, RunProvenance,
    RunServerProvenance, RunSpec,
};
pub use run_blob_id::RunBlobId;
pub use run_event::{
    EventBody, ExecOutputTail, InterviewOption, MetadataSnapshotFailureKind, MetadataSnapshotPhase,
    RunEvent, RunNoticeCode, RunNoticeLevel, SessionCapability,
};
pub use run_failure::RunFailure;
pub use run_id::{RunId, fixtures};
pub use run_projection::{
    CheckpointRecord, PendingInterviewRecord, RunProjection, StageProjection, first_event_seq,
};
pub use run_sandbox::{RunSandbox, RunSandboxRuntime};
pub use run_summary::{
    AutomationRef, Run, RunBillingSummary, RunError, RunLifecycle, RunLinks, RunModel, RunOrigin,
    RunOriginKind, RunTimestamps, WorkflowRef,
};
pub type RunSummary = Run;
pub type PullRequestRecord = PullRequest;
pub type PullRequestDetail = PullRequestDetails;
pub use run_title::{RunTitleError, infer_run_title, normalize_explicit_run_title};
pub use sandbox_details::{
    SandboxDetails, SandboxNetwork, SandboxNetworkPolicy, SandboxNetworkPolicyMode,
    SandboxResources, SandboxState, SandboxTimestamps,
};
pub use sandbox_provider::SandboxProvider;
pub use sandbox_services::{
    SandboxService, SandboxServiceDiscoverySource, SandboxServiceListMeta,
    SandboxServiceListResponse,
};
pub use secret::{SecretMetadata, SecretType};
pub use stage_completion::StageCompletion;
pub use stage_handler::StageHandler;
pub use stage_id::{InvalidStageVisit, ParallelBranchId, StageId};
pub use start::StartRecord;
pub use status::{
    BlockedReason, FailureReason, InvalidTransition, ParseFailureReasonError,
    ParseSuccessReasonError, RunControlAction, RunStatus, RunStatusKind, SuccessReason,
    TerminalStatus,
};
