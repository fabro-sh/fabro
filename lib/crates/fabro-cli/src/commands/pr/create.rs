use anyhow::{Context, Result, bail};
use fabro_model::Catalog;
use fabro_sandbox::daytona::detect_repo_info;
use fabro_util::printer::Printer;
use fabro_workflow::outcome::StageStatus;
use fabro_workflow::pull_request::maybe_open_pull_request;
use tracing::info;

use crate::args::{GlobalArgs, PrCreateArgs};
use crate::command_context::CommandContext;
use crate::commands::store::rebuild::rebuild_run_store;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::print_json_pretty;
use crate::shared::repo::ensure_matching_repo_origin;

pub(super) async fn create_command(
    args: PrCreateArgs,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_target(&args.server, printer)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
    let run = lookup.resolve(&args.run_id)?;
    let run_id = run.run_id();
    let events = lookup.client().list_run_events(&run_id, None, None).await?;
    let run_store = rebuild_run_store(&run_id, &events).await?;
    let state = run_store.state().await?;

    let record = state.run.context("Failed to load run record from store")?;
    ensure_matching_repo_origin(
        record.repo_origin_url.as_deref(),
        "create a pull request for",
    )?;

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
        bail!("Stored diff is empty — nothing to create a PR for");
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

    let creds = super::load_github_credentials_required(printer)?;

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
        &run_store.clone().into(),
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
                fabro_util::printout!(printer, "{}", record.html_url);
            }
        }
        None => {
            if globals.json {
                print_json_pretty(&serde_json::Value::Null)?;
            } else {
                fabro_util::printout!(printer, "No pull request created (empty diff).");
            }
        }
    }

    Ok(())
}
