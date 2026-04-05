use anyhow::{Context, Result, bail};
use fabro_sandbox::daytona::DaytonaSandbox;
use tracing::info;

use crate::args::{GlobalArgs, SshArgs};
use crate::server_runs::ServerRunLookup;
use crate::shared::{print_json_pretty, validate_daytona_provider};
use crate::user_config::load_user_settings_with_globals;

pub(crate) async fn run(args: SshArgs, globals: &GlobalArgs) -> Result<()> {
    if globals.json && !args.print {
        globals.require_no_json()?;
    }

    let cli_settings = load_user_settings_with_globals(globals)?;
    let lookup = ServerRunLookup::connect(&cli_settings.storage_dir()).await?;
    let run = lookup.resolve(&args.run)?;
    let run_id = run.run_id();
    let record = lookup
        .client()
        .get_run_state(&run_id)
        .await?
        .sandbox
        .context("Failed to load sandbox record from store")?;

    validate_daytona_provider(&record, "SSH access")?;

    let name = record
        .identifier
        .as_deref()
        .context("Daytona sandbox record missing identifier (sandbox name)")?;

    info!(run_id = %args.run, ttl_minutes = args.ttl, "Creating SSH access");

    let daytona = DaytonaSandbox::reconnect(name)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let ssh_cmd = daytona
        .create_ssh_access(Some(args.ttl))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if args.print {
        if globals.json {
            print_json_pretty(&serde_json::json!({ "command": ssh_cmd }))?;
        } else {
            print!("{}", format_output(&ssh_cmd));
        }
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
