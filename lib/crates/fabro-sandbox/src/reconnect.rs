use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::sandbox_record::SandboxRecord;

/// Reconnect to a sandbox from a saved record.
///
/// Returns a sandbox that can perform file operations.
pub async fn reconnect(record: &SandboxRecord) -> Result<Box<dyn crate::Sandbox>> {
    match record.provider.as_str() {
        #[cfg(feature = "local")]
        "local" => {
            let sandbox = crate::local::LocalSandbox::new(PathBuf::from(&record.working_directory));
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

            let config = crate::docker::DockerSandboxConfig {
                host_working_directory: host_dir.to_string(),
                container_mount_point: mount_point.to_string(),
                ..crate::docker::DockerSandboxConfig::default()
            };
            let sandbox = crate::docker::DockerSandbox::new(config)
                .map_err(|e| anyhow::anyhow!("Failed to create Docker sandbox: {e}"))?;
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "daytona")]
        "daytona" => {
            let name = record
                .identifier
                .as_deref()
                .context("Daytona sandbox record missing identifier (sandbox name)")?;

            let sandbox = crate::daytona::DaytonaSandbox::reconnect(name)
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

            let data_ssh = crate::exe::OpensshRunner::connect(data_host)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to connect to exe sandbox '{data_host}': {e}")
                })?;

            let sandbox = crate::exe::ExeSandbox::from_existing(Box::new(data_ssh));
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "ssh")]
        "ssh" => {
            let destination = record
                .data_host
                .as_deref()
                .context("SSH sandbox record missing data_host (destination)")?;

            let ssh = crate::ssh::OpensshRunner::connect(destination, None)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to connect to SSH sandbox '{destination}': {e}")
                })?;

            let config = crate::ssh::SshConfig {
                destination: destination.to_string(),
                working_directory: record.working_directory.clone(),
                config_file: None,
                preview_url_base: None,
            };
            let sandbox = crate::ssh::SshSandbox::from_existing(Box::new(ssh), config);
            Ok(Box::new(sandbox))
        }
        other => bail!("Unknown sandbox provider: {other}"),
    }
}
