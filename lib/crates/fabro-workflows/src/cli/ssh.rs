use anyhow::{bail, Context, Result};
use clap::Args;
use tracing::info;

use crate::cli::runs::{default_runs_base, resolve_run};
use crate::sandbox_record::SandboxRecord;

#[derive(Args)]
pub struct SshArgs {
    /// Run ID or prefix
    pub run: String,
    /// SSH access expiry in minutes (default 60)
    #[arg(long, default_value = "60")]
    pub ttl: f64,
    /// Print the SSH command instead of connecting
    #[arg(long)]
    pub print: bool,
}

fn validate_provider(record: &SandboxRecord) -> Result<()> {
    if record.provider != "daytona" {
        bail!(
            "SSH access is only supported for Daytona sandboxes (this run uses '{}')",
            record.provider
        );
    }
    Ok(())
}

fn format_output(ssh_command: &str) -> String {
    format!("{ssh_command}\n")
}

pub async fn ssh_command(args: SshArgs) -> Result<()> {
    let base = default_runs_base();
    let run_dir = resolve_run(&base, &args.run)?.path;
    let sandbox_json = run_dir.join("sandbox.json");
    let record = SandboxRecord::load(&sandbox_json).context(
        "Failed to load sandbox.json — was this run started with a recent version of arc?",
    )?;

    validate_provider(&record)?;

    let name = record
        .identifier
        .as_deref()
        .context("Daytona sandbox record missing identifier (sandbox name)")?;

    info!(run_id = %args.run, ttl_minutes = args.ttl, "Creating SSH access");

    let client = daytona_sdk::Client::new()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create Daytona client: {e}"))?;

    let sdk_sandbox = client
        .get(name)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to reconnect to Daytona sandbox '{name}': {e}"))?;

    let daytona = crate::daytona_sandbox::DaytonaSandbox::from_existing(client, sdk_sandbox);

    let ssh_cmd = daytona
        .create_ssh_access(Some(args.ttl))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if args.print {
        let output = format_output(&ssh_cmd);
        print!("{output}");
    } else {
        exec_ssh(&ssh_cmd)?;
    }

    Ok(())
}

#[cfg(unix)]
fn exec_ssh(ssh_cmd: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let parts: Vec<&str> = ssh_cmd.split_whitespace().collect();
    if parts.is_empty() {
        bail!("Empty SSH command returned from Daytona");
    }
    let err = std::process::Command::new(parts[0])
        .args(&parts[1..])
        .exec();
    // exec() only returns on error
    Err(anyhow::anyhow!("Failed to exec SSH: {err}"))
}

#[cfg(not(unix))]
fn exec_ssh(ssh_cmd: &str) -> Result<()> {
    bail!("Direct SSH connection is only supported on Unix systems; use --print instead");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_provider_rejects_local() {
        let record = SandboxRecord {
            provider: "local".to_string(),
            working_directory: "/tmp".to_string(),
            identifier: None,
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        };
        let err = validate_provider(&record).unwrap_err();
        assert!(
            err.to_string()
                .contains("SSH access is only supported for Daytona sandboxes"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_provider_rejects_docker() {
        let record = SandboxRecord {
            provider: "docker".to_string(),
            working_directory: "/workspace".to_string(),
            identifier: None,
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        };
        let err = validate_provider(&record).unwrap_err();
        assert!(
            err.to_string().contains("this run uses 'docker'"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_provider_accepts_daytona() {
        let record = SandboxRecord {
            provider: "daytona".to_string(),
            working_directory: "/home/daytona/workspace".to_string(),
            identifier: Some("sandbox-abc".to_string()),
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        };
        validate_provider(&record).unwrap();
    }

    #[test]
    fn format_output_produces_ssh_command_with_newline() {
        let output = format_output("ssh -p 2222 daytona@sandbox-123.daytona.work");
        assert_eq!(output, "ssh -p 2222 daytona@sandbox-123.daytona.work\n");
    }
}
