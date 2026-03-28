use anyhow::Result;
use fabro_config::FabroSettingsExt;
use fabro_store::RuntimeState;

use crate::args::AssetListArgs;
use crate::shared::format_size;

pub fn list_command(args: &AssetListArgs) -> Result<()> {
    let cli_config = crate::cli_config::load_cli_settings(None)?;
    let base = fabro_workflows::run_lookup::runs_base(&cli_config.storage_dir());
    let run = fabro_workflows::run_lookup::resolve_run(&base, &args.run_id)?;
    let runtime_state = RuntimeState::new(&run.path);
    let entries =
        fabro_workflows::assets::scan_assets(&runtime_state.assets_dir(), args.node.as_deref())?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("No assets found for this run.");
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
        "{} asset(s), {} total",
        entries.len(),
        format_size(total_size)
    );

    Ok(())
}
