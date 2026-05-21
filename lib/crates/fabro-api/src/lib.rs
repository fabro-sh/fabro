#[allow(
    clippy::absolute_paths,
    clippy::all,
    clippy::derivable_impls,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::needless_lifetimes,
    clippy::unwrap_used,
    unreachable_pub,
    unused_imports,
    reason = "Generated OpenAPI client code intentionally preserves codegen output."
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/codegen.rs"));
}
pub mod types {
    pub use fabro_model::{
        Model, ModelCosts, ModelFeatures, ModelLimits, ModelRef as BillingModelRef, ModelTestMode,
        Provider, ReasoningEffortFeature, Speed as BillingSpeed,
    };
    pub use fabro_types::settings::server::{
        GithubIntegrationSettings, GithubIntegrationStrategy, IntegrationWebhooksSettings,
        IpAllowEntry, LogDestination, ObjectStoreSettings, ServerApiSettings,
        ServerArtifactsSettings, ServerAuthGithubSettings, ServerAuthMethod, ServerAuthSettings,
        ServerIntegrationsSettings, ServerIpAllowlistOverrideSettings, ServerIpAllowlistSettings,
        ServerListenSettings, ServerLoggingSettings, ServerSchedulerSettings,
        ServerSlateDbSettings, ServerStorageSettings, ServerWebSettings, SlackIntegrationSettings,
        WebhookStrategy,
    };
    pub use fabro_types::settings::{FeaturesNamespace, ServerNamespace};
    pub use fabro_types::status::{
        BlockedReason, FailureReason, RunControlAction, RunStatus, SuccessReason,
    };
    pub use fabro_types::{
        AuthMethod, BilledTokenCounts, CommandTermination, Conclusion, DiffStats, DiffSummary,
        DirtyStatus, EventEnvelope, ExecOutputTail, FailureCategory, FailureDetail,
        FailureSignature, GitContext, IdpIdentity, InterviewOption, InterviewQuestionRecord,
        PairId, PairMessageId, PairMessageRecord, PairMessageRequest, PairRecord, PairStartRequest,
        PairStatus, PairTarget, PairTargetSelector, PairTranscriptEntry, PairTranscriptResponse,
        PendingInterviewRecord, PreRunPushOutcome, Principal, PullRequest, PullRequestDetails,
        PullRequestDetailsStatus, PullRequestDetailsUnavailableReason, PullRequestLink,
        PullRequestMeta, PullRequestResponse, QuestionType, RepositoryRef, Run,
        RunClientProvenance, RunEvent, RunEventDetailContentKind, RunEventDetailResponse,
        RunFailure, RunPairStatusResponse, RunProjection, RunProvenance, RunSandbox,
        RunSandboxRuntime, RunServerProvenance, SandboxDetails, SandboxNetwork,
        SandboxNetworkPolicy, SandboxNetworkPolicyMode, SandboxProvider, SandboxResources,
        SandboxService, SandboxServiceListResponse, SandboxState, SandboxTimestamps,
        SecretMetadata, SecretType, ServerSettings, SessionId, SessionMessage, SessionRecord,
        SessionStatus, SessionSummary, StageCompletion, StageHandler, StageOutcome,
        StageProjection, StageState, SystemActorKind, TurnId, UserPrincipal, WorkflowSettings,
    };

    pub use crate::generated::types::*;
}
pub use generated::Client as ApiClient;
