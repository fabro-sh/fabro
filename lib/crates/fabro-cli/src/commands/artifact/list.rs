use anyhow::Result;
use fabro_store::RuntimeState;
use fabro_workflow::artifacts::scan_artifacts;

use crate::args::{ArtifactListArgs, GlobalArgs};
use crate::server_runs::ServerRunLookup;
use crate::shared::format_size;
use crate::user_config::load_user_settings_with_globals;

pub(super) async fn list_command(args: &ArtifactListArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let lookup = ServerRunLookup::connect(&cli_settings.storage_dir()).await?;
    let run = lookup.resolve(&args.run_id)?;
    let runtime_state = RuntimeState::new(&run.path);
    let entries = scan_artifacts(
        &runtime_state.artifacts_dir(),
        args.node.as_deref(),
        args.retry,
    )?;

    if globals.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("No artifacts found for this run.");
        return Ok(());
    }

    let node_width = entries
        .iter()
        .map(|entry| entry.node_slug.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let retry_width = 5;
    let size_width = entries
        .iter()
        .map(|entry| format_size(entry.size).len())
        .max()
        .unwrap_or(4)
        .max(4);

    println!(
        "{:<node_width$}  {:>retry_width$}  {:>size_width$}  PATH",
        "NODE", "RETRY", "SIZE"
    );
    let total_size: u64 = entries.iter().map(|entry| entry.size).sum();
    for entry in &entries {
        println!(
            "{:<node_width$}  {:>retry_width$}  {:>size_width$}  {}",
            entry.node_slug,
            entry.retry,
            format_size(entry.size),
            entry.relative_path
        );
    }
    println!();
    println!(
        "{} artifact(s), {} total",
        entries.len(),
        format_size(total_size)
    );

    Ok(())
}
