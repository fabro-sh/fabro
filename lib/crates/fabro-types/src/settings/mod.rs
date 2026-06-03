//! Namespaced settings schema.
//!
//! Top-level schema is strictly namespaced with `_version`, `[project]`,
//! `[workflow]`, `[run]`, `[cli]`, and `[server]`. Value-language helpers live
//! alongside the tree: durations, byte sizes, model references, and env
//! interpolation.
//!
//! Stage 6.5b promoted these modules up out of the transitional
//! `settings/v2/` subdirectory, so the `::v2::` path prefix no longer
//! exists.

pub mod cli;
pub mod duration;
pub mod interp;
pub mod model_ref;
pub mod project;
pub mod public_url;
pub mod run;
pub mod server;
pub mod size;
pub mod workflow;

pub use cli::{
    CliAuthSettings, CliExecAgentSettings, CliExecModelSettings, CliExecSettings,
    CliLoggingSettings, CliNamespace, CliOutputSettings, CliTargetSettings, CliUpdatesSettings,
};
pub use duration::{Duration, ParseDurationError};
pub use interp::{InterpString, Provenance, ResolveCtx, ResolveError, ResolveErrorKind, Resolved};
pub use model_ref::{
    AmbiguousModelRef, ModelRef, ModelRegistry, ParseModelRefError, ResolvedModelRef,
};
pub use project::ProjectNamespace;
pub use public_url::{
    is_wildcard_host, replace_wildcard_host, validate_public_url, validate_public_url_with_label,
};
pub use run::{
    ArtifactsSettings, DockerfileSource, EnvironmentImageSettings, EnvironmentLifecycleSettings,
    EnvironmentNetworkMode, EnvironmentNetworkSettings, EnvironmentProvider,
    EnvironmentResourcesSettings, EnvironmentSettings, EnvironmentVolumeSettings,
    GitAuthorSettings, HookDefinition, HookType, InterviewProviderSettings, McpServerSettings,
    McpTransport, NotificationProviderSettings, NotificationRouteSettings, PullRequestSettings,
    RunAgentSettings, RunCheckpointSettings, RunEnvironmentSettings, RunExecutionSettings,
    RunGitSettings, RunGoal, RunIntegrationsGithubSettings, RunIntegrationsSettings,
    RunInterviewsSettings, RunModelControls, RunModelSettings, RunNamespace, RunPrepareSettings,
    RunScmSettings, ScmGitHubSettings, TlsMode,
};
pub use server::{
    GithubIntegrationSettings, IntegrationWebhooksSettings, LogDestination, ObjectStoreSettings,
    ServerApiSettings, ServerArtifactsSettings, ServerAuthGithubSettings, ServerAuthMethod,
    ServerAuthSettings, ServerIntegrationsSettings, ServerListenSettings, ServerLoggingSettings,
    ServerNamespace, ServerSchedulerSettings, ServerSlateDbSettings, ServerStorageSettings,
    ServerWebSettings, SlackIntegrationSettings,
};
pub use size::{ParseSizeError, Size};
pub use workflow::WorkflowNamespace;
