//! Namespaced settings schema.
//!
//! Top-level schema is strictly namespaced with `_version`, `[project]`,
//! `[workflow]`, `[run]`, `[cli]`, `[server]`, and `[features]`.
//! Value-language helpers live alongside the tree: durations, byte sizes,
//! model references, env interpolation, and splice-capable arrays.
//!
//! Stage 6.5b promoted these modules up out of the transitional
//! `settings/v2/` subdirectory, so the `::v2::` path prefix no longer
//! exists.

pub mod cli;
pub mod combine;
pub mod duration;
pub mod features;
pub mod interp;
pub mod layer;
pub mod maps;
pub mod model_ref;
pub mod project;
pub mod run;
pub mod server;
pub mod size;
pub mod splice_array;
pub mod workflow;

pub use cli::{
    CliAuthSettings, CliExecAgentSettings, CliExecModelSettings, CliExecSettings, CliLayer,
    CliLoggingSettings, CliNamespace, CliOutputSettings, CliTargetSettings, CliUpdatesSettings,
};
pub use combine::Combine;
pub use duration::{Duration, ParseDurationError};
pub use features::{FeaturesLayer, FeaturesNamespace};
pub use interp::{InterpString, Provenance, ResolveEnvError, Resolved};
pub use layer::SettingsLayer;
pub use maps::{MergeMap, ReplaceMap, StickyMap};
pub use model_ref::{
    AmbiguousModelRef, ModelRef, ModelRegistry, ParseModelRefError, ResolvedModelRef,
};
pub use project::{ProjectLayer, ProjectNamespace};
pub use run::{
    ArtifactsSettings, DaytonaSettings, DaytonaSnapshotSettings, DockerfileSource,
    GitAuthorSettings, HookDefinition, HookType, InterviewProviderSettings, McpServerSettings,
    McpTransport, NotificationProviderSettings, NotificationRouteSettings, PullRequestSettings,
    RunAgentSettings, RunCheckpointSettings, RunExecutionSettings, RunGitSettings, RunGoal,
    RunInterviewsSettings, RunLayer, RunModelSettings, RunNamespace, RunPrepareSettings,
    RunSandboxSettings, RunScmSettings, ScmGitHubSettings, TlsMode,
};
pub use server::{
    DiscordIntegrationSettings, GithubIntegrationSettings, IntegrationWebhooksSettings,
    IpAllowEntry, ObjectStoreSettings, ServerApiSettings, ServerArtifactsSettings,
    ServerAuthGithubSettings, ServerAuthMethod, ServerAuthSettings, ServerIntegrationsSettings,
    ServerIpAllowlistLayer, ServerIpAllowlistOverrideLayer, ServerIpAllowlistOverrideSettings,
    ServerIpAllowlistSettings, ServerLayer, ServerListenSettings, ServerLoggingSettings,
    ServerNamespace, ServerSchedulerSettings, ServerSlateDbSettings, ServerStorageSettings,
    ServerWebSettings, SlackIntegrationSettings, TeamsIntegrationSettings,
};
pub use size::{ParseSizeError, Size};
pub use splice_array::{SPLICE_MARKER, SpliceArray, SpliceArrayError};
pub use workflow::{WorkflowLayer, WorkflowNamespace};
