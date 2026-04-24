//! Namespaced settings schema.
//!
//! Top-level schema is strictly namespaced with `_version`, `[project]`,
//! `[workflow]`, `[run]`, `[cli]`, `[server]`, and `[features]`. Value-language
//! helpers live alongside the tree: durations, byte sizes, model references,
//! and env interpolation.
//!
//! Stage 6.5b promoted these modules up out of the transitional
//! `settings/v2/` subdirectory, so the `::v2::` path prefix no longer
//! exists.

pub mod cli;
pub mod duration;
pub mod features;
pub mod interp;
pub mod model_ref;
pub mod project;
pub mod run;
pub mod server;
pub mod size;
pub mod workflow;

pub use cli::{
    CliAuthSettings, CliExecAgentSettings, CliExecModelSettings, CliExecSettings,
    CliLoggingSettings, CliNamespace, CliOutputSettings, CliTargetSettings, CliUpdatesSettings,
};
pub use duration::{Duration, ParseDurationError};
pub use features::FeaturesNamespace;
pub use interp::{InterpString, Provenance, ResolveEnvError, Resolved};
pub use model_ref::{
    AmbiguousModelRef, ModelRef, ModelRegistry, ParseModelRefError, ResolvedModelRef,
};
pub use project::ProjectNamespace;
pub use run::{
    ArtifactsSettings, DaytonaSettings, DaytonaSnapshotSettings, DockerfileSource,
    GitAuthorSettings, HookDefinition, HookType, InterviewProviderSettings, McpServerSettings,
    McpTransport, NotificationProviderSettings, NotificationRouteSettings, PullRequestSettings,
    RunAgentSettings, RunCheckpointSettings, RunExecutionSettings, RunGitSettings, RunGoal,
    RunInterviewsSettings, RunModelSettings, RunNamespace, RunPrepareSettings, RunSandboxSettings,
    RunScmSettings, ScmGitHubSettings, TlsMode,
};
pub use server::{
    DiscordIntegrationSettings, GithubIntegrationSettings, IntegrationWebhooksSettings,
    IpAllowEntry, ObjectStoreSettings, ServerApiSettings, ServerArtifactsSettings,
    ServerAuthGithubSettings, ServerAuthMethod, ServerAuthSettings, ServerIntegrationsSettings,
    ServerIpAllowlistOverrideSettings, ServerIpAllowlistSettings, ServerListenSettings,
    ServerLoggingSettings, ServerNamespace, ServerSchedulerSettings, ServerSlateDbSettings,
    ServerStorageSettings, ServerWebSettings, SlackIntegrationSettings, TeamsIntegrationSettings,
};
pub use size::{ParseSizeError, Size};
pub use workflow::WorkflowNamespace;
