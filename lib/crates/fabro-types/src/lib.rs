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
pub mod outcome;
pub mod principal;
pub mod pull_request;
pub mod repository;
pub mod retro;
pub mod run;
pub mod run_blob_id;
pub mod run_event;
pub mod run_id;
pub mod run_projection;
pub mod run_summary;
pub mod sandbox_record;
pub mod secret;
pub mod settings;
pub mod stage_completion;
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
pub use diff::DiffStats;
pub use event_envelope::EventEnvelope;
pub use failure_signature::FailureSignature;
pub use graph::{
    AttrValue, Edge, Graph, KNOWN_HANDLER_TYPES, Node, is_known_handler_type, is_llm_handler_type,
    shape_to_handler_type,
};
pub use interview::{InterviewQuestionRecord, QuestionType};
pub use outcome::{
    FailureCategory, FailureDetail, NodeResult, Outcome, OutcomeMeta, StageOutcome, StageState,
};
pub use principal::{AuthMethod, Principal, SystemActorKind, UserPrincipal};
pub use pull_request::{
    PullRequestDetail, PullRequestGithubDetail, PullRequestRecord, PullRequestRef, PullRequestUser,
};
pub use repository::RepositoryReference;
pub use retro::{
    AggregateStats, FrictionKind, FrictionPoint, Learning, LearningCategory, OpenItem,
    OpenItemKind, Retro, RetroNarrative, SmoothnessRating, StageRetro,
};
pub use run::{
    DirtyStatus, ForkSourceRef, GitContext, PreRunPushOutcome, RunClientProvenance, RunProvenance,
    RunServerProvenance, RunSpec,
};
pub use run_blob_id::RunBlobId;
pub use run_event::{
    EventBody, ExecOutputTail, InterviewOption, MetadataSnapshotFailureKind, MetadataSnapshotPhase,
    RunEvent, RunNoticeCode, RunNoticeLevel,
};
pub use run_id::{RunId, fixtures};
pub use run_projection::{PendingInterviewRecord, RunProjection, StageProjection, first_event_seq};
pub use run_summary::RunSummary;
pub use sandbox_record::SandboxRecord;
pub use secret::{SecretMetadata, SecretType};
pub use stage_completion::StageCompletion;
pub use stage_id::{ParallelBranchId, StageId};
pub use start::StartRecord;
pub use status::{
    BlockedReason, FailureReason, InvalidTransition, ParseFailureReasonError,
    ParseSuccessReasonError, RunControlAction, RunStatus, SuccessReason, TerminalStatus,
};
