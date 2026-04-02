use anyhow::Result;
use fabro_store::RuntimeState;
use fabro_workflow::assets::scan_assets;
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};

use crate::args::{AssetListArgs, GlobalArgs};
use crate::shared::format_size;
use crate::store;
use crate::user_config::load_user_settings_with_globals;

pub(super) async fn list_command(args: &AssetListArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let run = resolve_run_combined(store.as_ref(), &base, &args.run_id).await?;
    let runtime_state = RuntimeState::new(&run.path);
    let entries = scan_assets(
        &runtime_state.assets_dir(),
        args.node.as_deref(),
        args.retry,
    )?;

    if globals.json {
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
