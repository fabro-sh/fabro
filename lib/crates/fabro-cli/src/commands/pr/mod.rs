mod close;
mod create;
mod list;
mod merge;
mod view;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::args::{PrCommand, PrNamespace};

pub async fn dispatch(ns: PrNamespace) -> Result<()> {
    let cli_config = crate::cli_config::load_cli_settings(None)?;
    let github_app = crate::shared::github::build_github_app_credentials(cli_config.app_id());

    match ns.command {
        PrCommand::Create(args) => create::create_command(args, github_app).await,
        PrCommand::List(args) => list::list_command(args, github_app).await,
        PrCommand::View(args) => view::view_command(args, github_app).await,
        PrCommand::Merge(args) => merge::merge_command(args, github_app).await,
        PrCommand::Close(args) => close::close_command(args, github_app).await,
    }
}

pub(crate) fn load_pr_record(
    base: &Path,
    run_id: &str,
) -> Result<(fabro_workflows::pull_request::PullRequestRecord, PathBuf)> {
    let run_dir = fabro_workflows::run_lookup::resolve_run(base, run_id)?.path;
    let pr_path = run_dir.join("pull_request.json");
    let content = std::fs::read_to_string(&pr_path).with_context(|| {
        format!(
            "No pull_request.json found in run directory. \
             Create one first with: fabro pr create {run_id}"
        )
    })?;
    let record: fabro_workflows::pull_request::PullRequestRecord =
        serde_json::from_str(&content).context("Failed to parse pull_request.json")?;
    Ok((record, run_dir))
}
