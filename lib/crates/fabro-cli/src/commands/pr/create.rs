use anyhow::{Context, Result, bail};
use fabro_model::Catalog;
use fabro_sandbox::daytona::detect_repo_info;
use fabro_workflow::outcome::StageStatus;
use fabro_workflow::pull_request::maybe_open_pull_request;
use fabro_workflow::services::RunServices;
use tracing::info;

use crate::args::PrCreateArgs;
use crate::command_context::CommandContext;
use crate::commands::rebuild::rebuild_run_store;
use crate::shared::print_json_pretty;
use crate::shared::repo::ensure_matching_repo_origin;
#[allow(
    deprecated,
    reason = "boundary-exempt(pr-api): remove with follow-up #1 when PR ops move server-side"
)]
pub(super) async fn create_command(args: PrCreateArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let printer = ctx.printer();
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.run_id;
    let events = client.list_run_events(&run_id, None, None).await?;
    let run_store = rebuild_run_store(&run_id, &events).await?;
    let state = run_store.state().await?;

    let run_spec = state.spec.context("Failed to load run spec from store")?;
    ensure_matching_repo_origin(
        run_spec.repo_origin_url.as_deref(),
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
        status if args.force => {
            tracing::warn!("Run status is '{status}', proceeding because --force was specified");
        }
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

    let base_branch = run_spec
        .base_branch
        .as_deref()
        .or(detected_branch.as_deref())
        .unwrap_or("main");

    let https_url = fabro_github::ssh_url_to_https(&origin_url);
    let (owner, repo) = fabro_github::parse_github_owner_repo(&https_url)
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    let creds = super::load_github_credentials_required(base_ctx)?;

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

    let llm_source = ctx.llm_source().await?;
    let configured = llm_source.configured_providers().await;
    let model = args.model.unwrap_or_else(|| {
        Catalog::builtin()
            .default_for_configured(&configured)
            .id
            .clone()
    });
    let pr_services = RunServices::for_cli(run_store.clone().into(), llm_source);

    let pull_request = maybe_open_pull_request(
        &creds,
        &origin_url,
        base_branch,
        run_branch,
        run_spec.graph.goal(),
        &diff,
        &model,
        true,
        None,
        pr_services.as_ref(),
        None,
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    match pull_request {
        Some(record) => {
            info!(pr_url = %record.html_url, "Pull request created");
            if ctx.json_output() {
                print_json_pretty(&record)?;
            } else {
                fabro_util::printout!(printer, "{}", record.html_url);
            }
        }
        None => {
            if ctx.json_output() {
                print_json_pretty(&serde_json::Value::Null)?;
            } else {
                fabro_util::printout!(printer, "No pull request created (empty diff).");
            }
        }
    }

    Ok(())
}
