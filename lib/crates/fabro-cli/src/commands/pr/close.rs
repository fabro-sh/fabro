use anyhow::{Context, Result};
use tracing::info;

use crate::args::{GlobalArgs, PrCloseArgs};
use crate::shared::print_json_pretty;

pub(super) async fn close_command(
    args: PrCloseArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let (record, _run_id) = super::load_pr_record(&args.server, &args.run_id).await?;

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
