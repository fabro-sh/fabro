use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

use fabro_workflow::run_lookup::runs_base;

use crate::args::{GlobalArgs, PrViewArgs};
use crate::shared::print_json_pretty;
use crate::user_config::load_user_settings_with_globals;

pub(super) async fn view_command(
    args: PrViewArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    view_from(&base, args, github_app, globals).await
}

async fn view_from(
    base: &Path,
    args: PrViewArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let (record, _run_dir) = super::load_pr_record(base, &args.run_id).await?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let detail = fabro_github::get_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        &fabro_github::github_api_base_url(),
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = detail.number, owner = %record.owner, repo = %record.repo, "Viewing pull request");

    if globals.json {
        print_json_pretty(&detail)?;
        return Ok(());
    }

    println!("#{} {}", detail.number, detail.title);
    let state_display = if detail.draft { "draft" } else { &detail.state };
    println!("State:   {state_display}");
    println!("URL:     {}", detail.html_url);
    println!(
        "Branch:  {} -> {}",
        detail.head.ref_name, detail.base.ref_name
    );
    println!("Author:  {}", detail.user.login);
    println!(
        "Changes: +{} -{} ({} files)",
        detail.additions, detail.deletions, detail.changed_files
    );
    if let Some(body) = &detail.body {
        if !body.is_empty() {
            println!();
            println!("{body}");
        }
    }

    Ok(())
}
