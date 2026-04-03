mod close;
mod create;
mod list;
mod merge;
mod view;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use fabro_types::PullRequestRecord;
use fabro_workflow::run_lookup::resolve_run_combined;

use crate::args::{GlobalArgs, PrCommand, PrNamespace};
use crate::shared::github::build_github_app_credentials;
use crate::store;
use crate::user_config::load_user_settings_with_globals;

pub(crate) async fn dispatch(ns: PrNamespace, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let github_app = build_github_app_credentials(cli_settings.app_id())?;

    match ns.command {
        PrCommand::Create(args) => {
            Box::pin(create::create_command(args, github_app, globals)).await
        }
        PrCommand::List(args) => list::list_command(args, github_app, globals).await,
        PrCommand::View(args) => view::view_command(args, github_app, globals).await,
        PrCommand::Merge(args) => merge::merge_command(args, github_app, globals).await,
        PrCommand::Close(args) => close::close_command(args, github_app, globals).await,
    }
}

pub(crate) async fn load_pr_record(
    base: &Path,
    run_id: &str,
) -> Result<(PullRequestRecord, PathBuf)> {
    let storage_dir = base.parent().unwrap_or(base);
    let store = store::build_store(storage_dir)?;
    let run = resolve_run_combined(store.as_ref(), base, run_id).await?;
    let run_dir = run.path;
    let run_store = store::open_run_reader(storage_dir, &run.run_id).await?;
    let state = run_store.state().await?;
    let record = state.pull_request.with_context(|| {
        format!("No pull request found in store. Create one first with: fabro pr create {run_id}")
    })?;
    Ok((record, run_dir))
}
