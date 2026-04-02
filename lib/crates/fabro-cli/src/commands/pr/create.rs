use std::path::Path;

use anyhow::{Context, Result, bail};
use fabro_model::Catalog;
use fabro_sandbox::daytona::detect_repo_info;
use fabro_workflow::outcome::StageStatus;
use fabro_workflow::pull_request::maybe_open_pull_request;
use fabro_workflow::records::RunRecordExt;
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};
use tracing::info;

use crate::args::{GlobalArgs, PrCreateArgs};
use crate::shared::print_json_pretty;
use crate::store;
use crate::user_config::load_user_settings_with_globals;

pub(super) async fn create_command(
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    create_from(&base, args, github_app, globals).await
}

async fn create_from(
    base: &Path,
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let storage_dir = base.parent().unwrap_or(base);
    let store = store::build_store(storage_dir)?;
    let run = resolve_run_combined(store.as_ref(), base, &args.run_id).await?;
    let run_dir = run.path.clone();
    let run_store = store::open_run_reader(storage_dir, &run.run_id).await?;
    let state = run_store.state().await?;

    let record = state
        .run
        .context("Failed to load run record from store")?;

    let start = state
        .start
        .context("Failed to load start record from store")?;

    let conclusion = state
        .conclusion
        .context("Failed to load conclusion from store — is the run finished?")?;

    match conclusion.status {
        StageStatus::Success | StageStatus::PartialSuccess => {}
        status => bail!("Run status is '{status}', expected success or partial_success"),
    }

    let run_branch = start
        .run_branch
        .as_deref()
        .context("Run has no run_branch — was it run with git push enabled?")?;

    let diff = state
        .final_patch
        .context("Failed to load final patch from store — no diff available")?;
    if diff.trim().is_empty() {
        bail!("final.patch is empty — nothing to create a PR for");
    }

    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    let (origin_url, detected_branch) =
        detect_repo_info(&cwd).map_err(|err| anyhow::anyhow!("{err}"))?;

    let base_branch = record
        .base_branch
        .as_deref()
        .or(detected_branch.as_deref())
        .unwrap_or("main");

    let https_url = fabro_github::ssh_url_to_https(&origin_url);
    let (owner, repo) = fabro_github::parse_github_owner_repo(&https_url)
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let branch_found = fabro_github::branch_exists(
        &creds,
        &owner,
        &repo,
        run_branch,
        &fabro_github::github_api_base_url(),
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    if !branch_found {
        bail!(
            "Branch '{run_branch}' not found on GitHub. \
             Was it pushed? Try: git push origin {run_branch}"
        );
    }

    let model = args
        .model
        .unwrap_or_else(|| Catalog::builtin().default_from_env().id.clone());

    let record = maybe_open_pull_request(
        &creds,
        &origin_url,
        base_branch,
        run_branch,
        record.goal(),
        &diff,
        &model,
        true,
        None,
        run_store.as_ref(),
        &run_dir,
        None,
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    match record {
        Some(record) => {
            info!(pr_url = %record.html_url, "Pull request created");
            if globals.json {
                print_json_pretty(&record)?;
            } else {
                println!("{}", record.html_url);
            }
        }
        None => {
            if globals.json {
                print_json_pretty(&serde_json::Value::Null)?;
            } else {
                println!("No pull request created (empty diff).");
            }
        }
    }

    Ok(())
}
