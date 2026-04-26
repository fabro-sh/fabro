use std::path::PathBuf;

#[allow(
    unused_imports,
    reason = "Feature-gated branches consume these imports when optional backends are enabled."
)]
use anyhow::{Context, Result, bail};

#[cfg(feature = "daytona")]
use crate::daytona::DaytonaSandbox;
#[cfg(feature = "docker")]
use crate::docker::DockerSandbox;
use crate::local::LocalSandbox;
use crate::sandbox_record::SandboxRecord;

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
    record: &SandboxRecord,
    daytona_api_key: Option<String>,
) -> Result<Box<dyn crate::Sandbox>> {
    match record.provider.as_str() {
        "local" => {
            let sandbox = LocalSandbox::new(PathBuf::from(&record.working_directory));
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "docker")]
        "docker" => {
            let identifier = record
                .identifier
                .as_deref()
                .context("Docker sandbox record missing identifier (container ID/name)")?;
            let repo_cloned = record
                .repo_cloned
                .context("Docker sandbox record missing repo_cloned metadata")?;
            let sandbox = DockerSandbox::reconnect(
                identifier,
                repo_cloned,
                record.clone_origin_url.clone(),
                record.clone_branch.clone(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to reconnect Docker sandbox: {e}"))?;
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "daytona")]
        "daytona" => {
            let name = record
                .identifier
                .as_deref()
                .context("Daytona sandbox record missing identifier (sandbox name)")?;
            let repo_cloned = record
                .repo_cloned
                .context("Daytona sandbox record missing repo_cloned metadata")?;

            let sandbox = DaytonaSandbox::reconnect(
                name,
                daytona_api_key,
                repo_cloned,
                record.clone_origin_url.clone(),
                record.clone_branch.clone(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(Box::new(sandbox))
        }
        other => bail!("Unknown sandbox provider: {other}"),
    }
}
