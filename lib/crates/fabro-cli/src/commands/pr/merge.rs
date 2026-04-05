use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

use fabro_workflow::run_lookup::runs_base;

use crate::args::{GlobalArgs, PrMergeArgs};
use crate::shared::print_json_pretty;
use crate::user_config::load_user_settings_with_storage_dir;

pub(super) async fn merge_command(
    args: PrMergeArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let cli_settings = load_user_settings_with_storage_dir(args.storage_dir.as_deref())?;
    let base = runs_base(&cli_settings.storage_dir());
    merge_from(&base, args, github_app, globals).await
}

async fn merge_from(
    base: &Path,
    args: PrMergeArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let (record, _run_dir) = super::load_pr_record(base, &args.run_id).await?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    fabro_github::merge_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        &args.method,
        &fabro_github::github_api_base_url(),
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, method = %args.method, "Merged pull request");
    if globals.json {
        print_json_pretty(&serde_json::json!({
            "number": record.number,
            "html_url": record.html_url,
            "method": args.method,
        }))?;
    } else {
        println!("Merged #{} ({})", record.number, record.html_url);
    }

    Ok(())
}
