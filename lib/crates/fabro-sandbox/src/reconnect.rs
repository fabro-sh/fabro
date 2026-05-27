use std::path::PathBuf;

#[allow(
    unused_imports,
    reason = "Feature-gated branches consume these imports when optional backends are enabled."
)]
use anyhow::{Context, Result, bail};
use fabro_types::{RunId, RunSandboxInstance, SandboxProviderKind};

use crate::SandboxEventCallback;
#[cfg(feature = "daytona")]
use crate::daytona::DaytonaSandbox;
#[cfg(feature = "docker")]
use crate::docker::DockerSandbox;
use crate::local::LocalSandbox;

/// Reconnect to a sandbox from a saved record.
///
/// `daytona_api_key` is forwarded to the Daytona SDK when the provider is
/// `"daytona"`. Pass `None` to fall back to the `DAYTONA_API_KEY` env var.
#[allow(
    clippy::unused_async,
    unused_variables,
    reason = "Feature-gated sandbox backends leave some parameters unused on partial builds."
)]
pub async fn reconnect(
    record: &RunSandboxInstance,
    daytona_api_key: Option<String>,
) -> Result<Box<dyn crate::Sandbox>> {
    reconnect_for_run(record, daytona_api_key, None).await
}

#[allow(
    unused_variables,
    reason = "Feature-gated sandbox backends leave parameters unused on partial builds."
)]
pub async fn reconnect_for_run(
    record: &RunSandboxInstance,
    daytona_api_key: Option<String>,
    run_id: Option<RunId>,
) -> Result<Box<dyn crate::Sandbox>> {
    reconnect_for_run_with_callback(record, daytona_api_key, run_id, None).await
}

#[allow(
    unused_variables,
    reason = "Feature-gated sandbox backends leave parameters unused on partial builds."
)]
pub async fn reconnect_for_run_with_callback(
    record: &RunSandboxInstance,
    daytona_api_key: Option<String>,
    run_id: Option<RunId>,
    event_callback: Option<SandboxEventCallback>,
) -> Result<Box<dyn crate::Sandbox>> {
    let runtime = &record.runtime;
    match record.provider {
        SandboxProviderKind::Local => {
            let mut sandbox = LocalSandbox::new(PathBuf::from(&runtime.working_directory));
            if let Some(callback) = event_callback {
                sandbox.set_event_callback(callback);
            }
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "docker")]
        SandboxProviderKind::Docker => {
            let repo_cloned = runtime
                .repo_cloned
                .context("Docker run sandbox missing repo_cloned metadata")?;
            let mut sandbox = DockerSandbox::reconnect(
                &runtime.id,
                repo_cloned,
                runtime.working_directory.clone(),
                runtime.clone_origin_url.clone(),
                runtime.clone_branch.clone(),
                run_id,
            )
            .await
            .context("Failed to reconnect Docker sandbox")?;
            if let Some(callback) = event_callback {
                sandbox.set_event_callback(callback);
            }
            Ok(Box::new(sandbox))
        }
        #[cfg(not(feature = "docker"))]
        SandboxProviderKind::Docker => bail!("Docker sandbox support is not enabled"),
        #[cfg(feature = "daytona")]
        SandboxProviderKind::Daytona => {
            let repo_cloned = runtime
                .repo_cloned
                .context("Daytona run sandbox missing repo_cloned metadata")?;

            let mut sandbox = DaytonaSandbox::reconnect(
                &runtime.id,
                daytona_api_key,
                repo_cloned,
                runtime.working_directory.clone(),
                runtime.clone_origin_url.clone(),
                runtime.clone_branch.clone(),
            )
            .await
            .map_err(anyhow::Error::new)?;
            if let Some(callback) = event_callback {
                sandbox.set_event_callback(callback);
            }
            Ok(Box::new(sandbox))
        }
        #[cfg(not(feature = "daytona"))]
        SandboxProviderKind::Daytona => bail!("Daytona sandbox support is not enabled"),
    }
}
