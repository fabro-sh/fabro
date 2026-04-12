use std::path::PathBuf;

#[allow(unused_imports)]
use anyhow::{Context, Result, bail};

#[cfg(feature = "daytona")]
use crate::daytona::DaytonaSandbox;
#[cfg(feature = "docker")]
use crate::docker::{DockerSandbox, DockerSandboxOptions};
use crate::local::LocalSandbox;
use crate::sandbox_record::SandboxRecord;

/// Reconnect to a sandbox from a saved record.
///
/// Returns a sandbox that can perform file operations.
pub async fn reconnect(record: &SandboxRecord) -> Result<Box<dyn crate::Sandbox>> {
    match record.provider.as_str() {
        "local" => {
            let sandbox = LocalSandbox::new(PathBuf::from(&record.working_directory));
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "docker")]
        "docker" => {
            let host_dir = record
                .host_working_directory
                .as_deref()
                .context("Docker sandbox record missing host_working_directory")?;
            let mount_point = record
                .container_mount_point
                .as_deref()
                .unwrap_or("/workspace");

            let config = DockerSandboxOptions {
                host_working_directory: host_dir.to_string(),
                container_mount_point: mount_point.to_string(),
                ..DockerSandboxOptions::default()
            };
            let sandbox = DockerSandbox::new(config)
                .map_err(|e| anyhow::anyhow!("Failed to create Docker sandbox: {e}"))?;
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "daytona")]
        "daytona" => {
            let name = record
                .identifier
                .as_deref()
                .context("Daytona sandbox record missing identifier (sandbox name)")?;

            let sandbox = DaytonaSandbox::reconnect(name)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(Box::new(sandbox))
        }
        other => bail!("Unknown sandbox provider: {other}"),
    }
}
