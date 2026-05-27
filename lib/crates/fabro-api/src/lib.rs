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
    pub use fabro_automation::{
        Automation, AutomationDraft as CreateAutomationRequest,
        AutomationReplace as ReplaceAutomationRequest, AutomationTarget, AutomationTrigger,
    };
    pub use fabro_model::{
        Model, ModelCosts, ModelFeatures, ModelLimits, ModelRef as BillingModelRef, ModelTestMode,
        Provider, ReasoningEffort, ReasoningEffortFeature, Speed as BillingSpeed,
    };
    pub use fabro_types::run_event::AgentSessionActivatedProps;
    pub use fabro_types::settings::ServerNamespace;
    pub use fabro_types::settings::server::{
        GithubIntegrationSettings, GithubIntegrationStrategy, IntegrationWebhooksSettings,
        IpAllowEntry, LogDestination, ObjectStoreSettings, ServerApiSettings,
        ServerArtifactsSettings, ServerAuthGithubSettings, ServerAuthMethod, ServerAuthSettings,
        ServerIntegrationsSettings, ServerIpAllowlistOverrideSettings, ServerIpAllowlistSettings,
        ServerListenSettings, ServerLoggingSettings, ServerSandboxProviderSettings,
        ServerSandboxProvidersSettings, ServerSandboxSettings, ServerSchedulerSettings,
        ServerSlateDbSettings, ServerStorageSettings, ServerWebSettings, SlackIntegrationSettings,
        WebhookStrategy,
    };
    pub use fabro_types::status::{
        BlockedReason, FailureReason, PendingReason, RunControlAction, RunStatus, SuccessReason,
    };
    pub use fabro_types::{
        ActivatedSkill, AgentMcpToolSummary, AgentSkillActivationSource, AgentSkillSummary,
        AgentToolCategory, AgentToolSource, AgentToolSummary, AgentToolsAvailableProps, AskFabro,
        AuthMethod, BilledTokenCounts, CommandTermination, Conclusion, CreateVariableRequest,
        DiffStats, DiffSummary, DirtyStatus, EventEnvelope, ExecOutputTail, FailureCategory,
        FailureDetail, FailureSignature, GitContext, IdpIdentity, IntegrationConnectionKind,
        IntegrationConnectionState, IntegrationConnectionStatus, IntegrationProvider,
        IntegrationStatus, InterviewOption, InterviewQuestionRecord, McpServerProjection,
        McpServerStatus, PairId, PairMessageId, PairMessageRecord, PairMessageRequest, PairRecord,
        PairStartRequest, PairStatus, PairTarget, PairTranscriptEntry, PairTranscriptResponse,
        PendingInterviewRecord, PermissionLevel, PreRunPushOutcome, Principal, PullRequest,
        PullRequestDetails, PullRequestDetailsStatus, PullRequestDetailsUnavailableReason,
        PullRequestLink, PullRequestMeta, PullRequestResponse, QuestionType, RepositoryRef, Run,
        RunApproval, RunApprovalState, RunClientProvenance, RunEvent, RunEventDetailContentKind,
        RunEventDetailResponse, RunFailure, RunPairStatusResponse, RunProjection, RunProvenance,
        RunRunnableSource, RunSandbox, RunSandboxFailure, RunSandboxInstance, RunSandboxKind,
        RunSandboxPlan, RunSandboxRuntime, RunServerProvenance, RunSize, SandboxDetails,
        SandboxInfo, SandboxListMeta, SandboxListResponse, SandboxNetwork, SandboxNetworkPolicy,
        SandboxNetworkPolicyMode, SandboxProviderKind, SandboxProviderLookupError,
        SandboxResources, SandboxService, SandboxServiceListResponse, SandboxState,
        SandboxTimestamps, SecretMetadata, SecretType, ServerSettings, SessionDetail, SessionId,
        SessionMessage, SessionRecord, SessionStatus, SessionSummary, SessionTurn,
        SkillsProjection, StageCompletion, StageContextWindow, StageContextWindowBreakdownItem,
        StageContextWindowCategory, StageContextWindowCountMethod, StageContextWindowProjection,
        StageContextWindowStaleness, StageContextWindowUnavailableReason,
        StageContextWindowWarning, StageHandler, StageModelUsage, StageOutcome, StageProjection,
        StageState, SubAgentProjection, SubAgentStatus, SystemActorKind, SystemIntegrationStatus,
        SystemIntegrationsResponse, TodoListProjection, TurnId, UpdateVariableRequest,
        UserPrincipal, Variable, VariableListResponse, WorkflowSettings,
    };

    pub use crate::generated::types::*;
}
pub use generated::Client as ApiClient;
