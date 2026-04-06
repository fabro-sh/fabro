use anyhow::{Result, bail};
use tracing::info;

use crate::args::{GlobalArgs, SshArgs};
use crate::server_runs::ServerSummaryLookup;
use crate::shared::print_json_pretty;

pub(crate) async fn run(args: SshArgs, globals: &GlobalArgs) -> Result<()> {
    if globals.json && !args.print {
        globals.require_no_json()?;
    }

    let lookup = ServerSummaryLookup::connect(&args.server).await?;
    let run = lookup.resolve(&args.run)?;
    let run_id = run.run_id();
    let ssh = lookup
        .client()
        .create_run_ssh_access(&run_id, args.ttl)
        .await?;

    info!(run_id = %args.run, ttl_minutes = args.ttl, "Creating SSH access");

    if args.print {
        if globals.json {
            print_json_pretty(&serde_json::json!({ "command": ssh.command }))?;
        } else {
            print!("{}", format_output(&ssh.command));
        }
    } else {
        exec_ssh(&ssh.command)?;
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
        bail!("Empty SSH command returned from server");
    }
    let err = std::process::Command::new(parts[0])
        .args(&parts[1..])
        .exec();
    Err(anyhow::anyhow!("Failed to exec SSH: {err}"))
}

#[cfg(not(unix))]
fn exec_ssh(_ssh_cmd: &str) -> Result<()> {
    bail!("Direct SSH connection is only supported on Unix systems; use --print instead")
}
