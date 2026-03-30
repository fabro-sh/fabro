use std::path::PathBuf;

#[allow(unused_imports)]
use anyhow::{Context, Result, bail};

#[cfg(feature = "daytona")]
use crate::daytona::DaytonaSandbox;
#[cfg(feature = "docker")]
use crate::docker::{DockerSandbox, DockerSandboxConfig};
#[cfg(feature = "exe")]
use crate::exe::{ExeSandbox, OpensshRunner as ExeOpensshRunner};
use crate::local::LocalSandbox;
use crate::sandbox_record::SandboxRecord;
#[cfg(feature = "ssh")]
use crate::ssh::{OpensshRunner, SshConfig, SshSandbox};

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

            let config = DockerSandboxConfig {
                host_working_directory: host_dir.to_string(),
                container_mount_point: mount_point.to_string(),
                ..DockerSandboxConfig::default()
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
        #[cfg(feature = "exe")]
        "exe" => {
            let data_host = record
                .data_host
                .as_deref()
                .context("Exe sandbox record missing data_host")?;

            let data_ssh = ExeOpensshRunner::connect(data_host).await.map_err(|e| {
                anyhow::anyhow!("Failed to connect to exe sandbox '{data_host}': {e}")
            })?;

            let sandbox = ExeSandbox::from_existing(Box::new(data_ssh));
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "ssh")]
        "ssh" => {
            let destination = record
                .data_host
                .as_deref()
                .context("SSH sandbox record missing data_host (destination)")?;

            let ssh = OpensshRunner::connect(destination, None)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to connect to SSH sandbox '{destination}': {e}")
                })?;

            let config = SshConfig {
                destination: destination.to_string(),
                working_directory: record.working_directory.clone(),
                config_file: None,
                preview_url_base: None,
            };
            let sandbox = SshSandbox::from_existing(Box::new(ssh), config);
            Ok(Box::new(sandbox))
        }
        other => bail!("Unknown sandbox provider: {other}"),
    }
}
