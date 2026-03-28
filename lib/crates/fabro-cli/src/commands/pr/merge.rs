use std::path::Path;

use anyhow::{Context, Result};
use fabro_config::FabroSettingsExt;
use tracing::info;

use fabro_workflows::run_lookup::runs_base;

use crate::args::PrMergeArgs;
use crate::cli_config::load_cli_settings;

pub async fn merge_command(
    args: PrMergeArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let cli_config = load_cli_settings(None)?;
    let base = runs_base(&cli_config.storage_dir());
    merge_from(&base, args, github_app).await
}

async fn merge_from(
    base: &Path,
    args: PrMergeArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let (record, _run_dir) = super::load_pr_record(base, &args.run_id)?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    fabro_github::merge_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        &args.method,
        fabro_github::GITHUB_API_BASE_URL,
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, method = %args.method, "Merged pull request");
    println!("Merged #{} ({})", record.number, record.html_url);

    Ok(())
}
