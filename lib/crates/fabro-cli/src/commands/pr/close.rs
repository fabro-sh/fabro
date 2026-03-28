use std::path::Path;

use anyhow::{Context, Result};
use fabro_config::FabroSettingsExt;
use fabro_workflows::run_lookup::runs_base;
use tracing::info;

use crate::args::PrCloseArgs;
use crate::cli_config::load_cli_settings;

pub(super) async fn close_command(
    args: PrCloseArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let cli_settings = load_cli_settings(None)?;
    let base = runs_base(&cli_settings.storage_dir());
    close_from(&base, args, github_app).await
}

async fn close_from(
    base: &Path,
    args: PrCloseArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let (record, _run_dir) = super::load_pr_record(base, &args.run_id)?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    fabro_github::close_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        fabro_github::GITHUB_API_BASE_URL,
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, "Closed pull request");
    println!("Closed #{} ({})", record.number, record.html_url);

    Ok(())
}
