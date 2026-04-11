use anyhow::{Context, Result};
use fabro_util::printer::Printer;
use tracing::info;

use crate::args::{GlobalArgs, PrMergeArgs};
use crate::shared::print_json_pretty;

pub(super) async fn merge_command(
    args: PrMergeArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    let (record, _run_id) = super::load_pr_record(&args.server, &args.run_id, printer).await?;

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
        fabro_util::printout!(printer, "Merged #{} ({})", record.number, record.html_url);
    }

    Ok(())
}
