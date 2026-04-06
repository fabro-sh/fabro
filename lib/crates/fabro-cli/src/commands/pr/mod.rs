mod close;
mod create;
mod list;
mod merge;
mod view;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use fabro_types::PullRequestRecord;

use crate::args::{GlobalArgs, PrCommand, PrNamespace};
use crate::server_runs::ServerRunLookup;
use crate::shared::github::build_github_app_credentials;
use crate::user_config::load_settings_with_storage_dir;

pub(crate) async fn dispatch(ns: PrNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        PrCommand::Create(args) => {
            let cli_settings = load_settings_with_storage_dir(args.storage_dir.as_deref())?;
            let github_app = build_github_app_credentials(cli_settings.app_id())?;
            Box::pin(create::create_command(args, github_app, globals)).await
        }
        PrCommand::List(args) => {
            let cli_settings = load_settings_with_storage_dir(args.storage_dir.as_deref())?;
            let github_app = build_github_app_credentials(cli_settings.app_id())?;
            list::list_command(args, github_app, globals).await
        }
        PrCommand::View(args) => {
            let cli_settings = load_settings_with_storage_dir(args.storage_dir.as_deref())?;
            let github_app = build_github_app_credentials(cli_settings.app_id())?;
            view::view_command(args, github_app, globals).await
        }
        PrCommand::Merge(args) => {
            let cli_settings = load_settings_with_storage_dir(args.storage_dir.as_deref())?;
            let github_app = build_github_app_credentials(cli_settings.app_id())?;
            merge::merge_command(args, github_app, globals).await
        }
        PrCommand::Close(args) => {
            let cli_settings = load_settings_with_storage_dir(args.storage_dir.as_deref())?;
            let github_app = build_github_app_credentials(cli_settings.app_id())?;
            close::close_command(args, github_app, globals).await
        }
    }
}

pub(crate) async fn load_pr_record(
    base: &Path,
    run_id: &str,
) -> Result<(PullRequestRecord, PathBuf)> {
    let lookup = ServerRunLookup::connect_from_runs_base(base).await?;
    let run = lookup.resolve(run_id)?;
    let run_id = run.run_id();
    let run_dir = run.path;
    let state = lookup.client().get_run_state(&run_id).await?;
    let record = state.pull_request.with_context(|| {
        format!("No pull request found in store. Create one first with: fabro pr create {run_id}")
    })?;
    Ok((record, run_dir))
}
