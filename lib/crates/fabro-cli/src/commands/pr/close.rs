use std::path::Path;

use anyhow::{Context, Result};
use fabro_workflow::run_lookup::runs_base;
use tracing::info;

use crate::args::{GlobalArgs, PrCloseArgs};
use crate::shared::print_json_pretty;
use crate::user_config::load_user_settings_with_globals;

pub(super) async fn close_command(
    args: PrCloseArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    close_from(&base, args, github_app, globals).await
}

async fn close_from(
    base: &Path,
    args: PrCloseArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let (record, _run_dir) = super::load_pr_record(base, &args.run_id).await?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    fabro_github::close_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        &fabro_github::github_api_base_url(),
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, "Closed pull request");
    if globals.json {
        print_json_pretty(&serde_json::json!({
            "number": record.number,
            "html_url": record.html_url,
        }))?;
    } else {
        println!("Closed #{} ({})", record.number, record.html_url);
    }

    Ok(())
}
