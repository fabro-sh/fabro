use anyhow::{bail, Context, Result};
use tracing::info;

use crate::args::SshArgs;
use crate::shared::validate_daytona_provider;

pub async fn run(args: SshArgs) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    let run_dir = fabro_workflows::run_lookup::resolve_run(&base, &args.run)?.path;
    let sandbox_json = run_dir.join("sandbox.json");
    let record = fabro_sandbox::SandboxRecord::load(&sandbox_json).context(
        "Failed to load sandbox.json — was this run started with a recent version of arc?",
    )?;

    validate_daytona_provider(&record, "SSH access")?;

    let name = record
        .identifier
        .as_deref()
        .context("Daytona sandbox record missing identifier (sandbox name)")?;

    info!(run_id = %args.run, ttl_minutes = args.ttl, "Creating SSH access");

    let daytona = fabro_sandbox::daytona::DaytonaSandbox::reconnect(name)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let ssh_cmd = daytona
        .create_ssh_access(Some(args.ttl))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if args.print {
        print!("{}", format_output(&ssh_cmd));
    } else {
        exec_ssh(&ssh_cmd)?;
    }

    Ok(())
}

fn format_output(ssh_command: &str) -> String {
    format!("{ssh_command}\n")
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
    Err(anyhow::anyhow!("Failed to exec SSH: {err}"))
}

#[cfg(not(unix))]
fn exec_ssh(_ssh_cmd: &str) -> Result<()> {
    bail!("Direct SSH connection is only supported on Unix systems; use --print instead");
}
