use anyhow::{Context, Result};
use fabro_checkpoint::git::Store;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_workflow::operations::{ForkRunInput, RewindTarget, build_timeline_or_rebuild, fork};
use git2::Repository;

use crate::args::ForkArgs;
use crate::command_context::CommandContext;
use crate::commands::store::rebuild::rebuild_run_store;
use crate::shared::print_json_pretty;
use crate::shared::repo::ensure_matching_repo_origin;

pub(crate) async fn run(
    args: &ForkArgs,
    styles: &Styles,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let ctx = CommandContext::for_target(&args.server, printer, cli.clone(), cli_layer)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.run_id;
    let state = client.get_run_state(&run_id).await?;
    let record = state.run.context("Failed to load run record from store")?;
    ensure_matching_repo_origin(record.repo_origin_url.as_deref(), "fork")?;
    let store = Store::new(repo);
    let events = client.list_run_events(&run_id, None, None).await?;
    let run_store = rebuild_run_store(&run_id, &events).await?;

    let timeline = build_timeline_or_rebuild(&store, Some(&run_store), &run_id).await?;

    if args.list {
        if cli.output.format == OutputFormat::Json {
            print_json_pretty(&super::rewind::timeline_entries_json(&timeline))?;
            return Ok(());
        }
        super::rewind::print_timeline(&timeline, styles, printer);
        return Ok(());
    }

    let target = args
        .target
        .as_deref()
        .map(str::parse::<RewindTarget>)
        .transpose()?;
    let new_run_id = fork(&store, &ForkRunInput {
        source_run_id: run_id,
        target,
        push: !args.no_push,
    })?;

    let run_id_string = run_id.to_string();
    let new_run_id_string = new_run_id.to_string();

    if cli.output.format == OutputFormat::Json {
        let target = args.target.clone().unwrap_or_else(|| "latest".to_string());
        print_json_pretty(&serde_json::json!({
            "source_run_id": run_id_string,
            "new_run_id": new_run_id_string,
            "target": target,
        }))?;
    } else {
        fabro_util::printerr!(
            printer,
            "\nForked run {} -> {}",
            &run_id_string[..8.min(run_id_string.len())],
            &new_run_id_string[..8.min(new_run_id_string.len())]
        );
        fabro_util::printerr!(
            printer,
            "To resume: fabro resume {}",
            &new_run_id_string[..8.min(new_run_id_string.len())]
        );
    }

    Ok(())
}
