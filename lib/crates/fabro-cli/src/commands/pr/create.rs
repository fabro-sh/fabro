use std::path::Path;

use anyhow::{Context, Result, bail};
use fabro_model::Catalog;
use fabro_sandbox::daytona::detect_repo_info;
use fabro_workflow::outcome::StageStatus;
use fabro_workflow::pull_request::maybe_open_pull_request;
use fabro_workflow::run_lookup::runs_base;
use tracing::info;

use crate::args::{GlobalArgs, PrCreateArgs};
use crate::commands::store::rebuild::rebuild_run_store;
use crate::server_runs::ServerRunLookup;
use crate::shared::print_json_pretty;
use crate::user_config::load_settings_with_storage_dir;

pub(super) async fn create_command(
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let cli_settings = load_settings_with_storage_dir(args.storage_dir.as_deref())?;
    let base = runs_base(&cli_settings.storage_dir());
    create_from(&base, args, github_app, globals).await
}

async fn create_from(
    base: &Path,
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let lookup = ServerRunLookup::connect_from_runs_base(base).await?;
    let run = lookup.resolve(&args.run_id)?;
    let run_id = run.run_id();
    let events = lookup.client().list_run_events(&run_id, None, None).await?;
    let run_store = rebuild_run_store(&run_id, &events).await?;
    let state = run_store.state().await?;

    let record = state.run.context("Failed to load run record from store")?;

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
        record.graph.goal(),
        &diff,
        &model,
        true,
        None,
        &run_store,
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
