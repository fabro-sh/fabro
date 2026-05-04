#![expect(
    clippy::disallowed_methods,
    reason = "sync config loading utilities used at startup; not on a Tokio path"
)]
//! Configuration loading and resolution helpers.

extern crate self as fabro_config;

pub mod builders;
mod defaults;
mod layers;

pub mod bind;
pub mod daemon;
pub mod envfile;
pub mod error;
pub mod home;
mod load;
pub mod logging;
pub mod parse;
pub mod project;
pub mod resolve;
pub mod run;
pub mod storage;
#[cfg(test)]
mod tests;
pub mod user;

use std::path::Path;

pub use builders::{
    ResolveErrors, RunSettingsBuilder, ServerRuntimeSettings, ServerSettingsBuilder,
    UserSettingsBuilder, WorkflowSettingsBuilder, load_server_runtime_settings,
};
pub use error::{Error, Result};
pub use fabro_util::path::expand_tilde;
pub use home::Home;
pub use layers::{
    CliAuthLayer, CliExecAgentLayer, CliExecLayer, CliExecModelLayer, CliLayer, CliLoggingLayer,
    CliOutputLayer, CliTargetLayer, CliUpdatesLayer, CostRates, CredentialRef,
    CredentialRefParseError, DaytonaDockerfileLayer, DaytonaSandboxLayer, DaytonaSnapshotLayer,
    DiscordIntegrationLayer, DockerSandboxLayer, FeaturesLayer, GitAuthorLayer,
    GithubIntegrationLayer, HookAgentMarker, HookEntry, HookTlsMode, IntegrationWebhooksLayer,
    InterviewProviderLayer, InterviewsLayer, LlmLayer, LlmModelFeatures, LlmModelLimits,
    LocalSandboxLayer, LogFilter, McpEntryLayer, MergeMap, ModelControls, ModelCostTable,
    ModelRefOrSplice, ModelSettings, NotificationProviderLayer, NotificationRouteLayer,
    ObjectStoreLocalLayer, ObjectStoreS3Layer, PrepareStep, ProjectLayer, ProviderSettings,
    ReplaceMap, RunAgentLayer, RunArtifactsLayer, RunCheckpointLayer, RunExecutionLayer,
    RunGitLayer, RunGoalLayer, RunLayer, RunModelControlsLayer, RunModelLayer, RunPrepareLayer,
    RunPullRequestLayer, RunSandboxLayer, RunScmLayer, ScmGitHubLayer, ServerApiLayer,
    ServerArtifactsLayer,
    ServerAuthGithubLayer, ServerAuthLayer, ServerIntegrationsLayer, ServerIpAllowlistLayer,
    ServerIpAllowlistOverrideLayer, ServerLayer, ServerListenLayer, ServerLoggingLayer,
    ServerSchedulerLayer, ServerSlateDbLayer, ServerStorageLayer, ServerWebLayer,
    SlackIntegrationLayer, StickyMap, StringOrSplice, TeamsIntegrationLayer, WorkflowLayer,
};
pub(crate) use layers::{Combine, SettingsLayer};
pub use logging::{resolve_log_destination, resolve_log_destination_with_env};
pub use parse::ParseError;
pub use resolve::{
    ResolveError, resolve_cli, resolve_features, resolve_project, resolve_run, resolve_server,
    resolve_workflow,
};
use serde::de::DeserializeOwned;
pub use storage::{RunScratch, RuntimeDirectory, Storage};

/// Load a TOML config from an explicit path or `~/.fabro/{filename}`.
///
/// Returns `T::default()` when no explicit path is given and the default file
/// doesn't exist. An explicit path that doesn't exist is an error.
pub fn load_config_file<T>(path: Option<&Path>, filename: &str) -> Result<T>
where
    T: Default + DeserializeOwned,
{
    if let Some(explicit) = path {
        tracing::debug!(path = %explicit.display(), "Loading config from explicit path");
        let contents = std::fs::read_to_string(explicit)
            .map_err(|source| Error::read_file(explicit, source))?;
        return toml::from_str(&contents).map_err(|source| Error::toml_parse(explicit, source));
    }

    let default_path = Home::from_env().root().join(filename);
    tracing::debug!(path = %default_path.display(), "Loading config");
    match std::fs::read_to_string(&default_path) {
        Ok(contents) => {
            toml::from_str(&contents).map_err(|source| Error::toml_parse(&default_path, source))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(Error::read_file(&default_path, e)),
    }
}