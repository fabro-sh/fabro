use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::records::SandboxRecord;

/// Reconnect to a sandbox from a saved record.
///
/// Returns a sandbox that can perform file operations.
pub async fn reconnect(record: &SandboxRecord) -> Result<Box<dyn fabro_agent::sandbox::Sandbox>> {
    match record.provider.as_str() {
        "local" => {
            let sandbox = fabro_agent::local_sandbox::LocalSandbox::new(PathBuf::from(
                &record.working_directory,
            ));
            Ok(Box::new(sandbox))
        }
        "docker" => {
            let host_dir = record
                .host_working_directory
                .as_deref()
                .context("Docker sandbox record missing host_working_directory")?;
            let mount_point = record
                .container_mount_point
                .as_deref()
                .unwrap_or("/workspace");

            let config = fabro_agent::docker_sandbox::DockerSandboxConfig {
                host_working_directory: host_dir.to_string(),
                container_mount_point: mount_point.to_string(),
                ..fabro_agent::docker_sandbox::DockerSandboxConfig::default()
            };
            let sandbox = fabro_agent::docker_sandbox::DockerSandbox::new(config)
                .map_err(|e| anyhow::anyhow!("Failed to create Docker sandbox: {e}"))?;
            Ok(Box::new(sandbox))
        }
        "daytona" => {
            let name = record
                .identifier
                .as_deref()
                .context("Daytona sandbox record missing identifier (sandbox name)")?;

            let sandbox = fabro_sandbox::daytona::DaytonaSandbox::reconnect(name)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "exedev")]
        "exe" => {
            let data_host = record
                .data_host
                .as_deref()
                .context("Exe sandbox record missing data_host")?;

            let data_ssh = fabro_sandbox::exe::OpensshRunner::connect(data_host)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to connect to exe sandbox '{data_host}': {e}")
                })?;

            let sandbox = fabro_sandbox::exe::ExeSandbox::from_existing(Box::new(data_ssh));
            Ok(Box::new(sandbox))
        }
        "ssh" => {
            let destination = record
                .data_host
                .as_deref()
                .context("SSH sandbox record missing data_host (destination)")?;

            let ssh = fabro_sandbox::ssh::OpensshRunner::connect(destination, None)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to connect to SSH sandbox '{destination}': {e}")
                })?;

            let config = fabro_sandbox::ssh::SshConfig {
                destination: destination.to_string(),
                working_directory: record.working_directory.clone(),
                config_file: None,
                preview_url_base: None,
            };
            let sandbox = fabro_sandbox::ssh::SshSandbox::from_existing(Box::new(ssh), config);
            Ok(Box::new(sandbox))
        }
        other => bail!("Unknown sandbox provider: {other}"),
    }
}
