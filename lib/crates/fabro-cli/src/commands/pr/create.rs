use std::path::Path;

use anyhow::{bail, Context, Result};
use fabro_config::FabroSettingsExt;
use fabro_model::Catalog;
use fabro_sandbox::daytona::detect_repo_info;
use fabro_workflows::outcome::StageStatus;
use fabro_workflows::pull_request::maybe_open_pull_request;
use fabro_workflows::records::{
    Conclusion, ConclusionExt, RunRecord, RunRecordExt, StartRecord, StartRecordExt,
};
use fabro_workflows::run_lookup::{resolve_run, runs_base};
use tracing::info;

use crate::args::PrCreateArgs;
use crate::cli_config::load_cli_settings;

pub async fn create_command(
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let cli_config = load_cli_settings(None)?;
    let base = runs_base(&cli_config.storage_dir());
    create_from(&base, args, github_app).await
}

async fn create_from(
    base: &Path,
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let run_dir = resolve_run(base, &args.run_id)?.path;

    let record = RunRecord::load(&run_dir).context("Failed to load run.json")?;

    let start = StartRecord::load(&run_dir).context("Failed to load start.json")?;

    let conclusion = Conclusion::load(&run_dir.join("conclusion.json"))
        .context("Failed to load conclusion.json — is the run finished?")?;

    match conclusion.status {
        StageStatus::Success | StageStatus::PartialSuccess => {}
        status => bail!("Run status is '{status}', expected success or partial_success"),
    }

    let run_branch = start
        .run_branch
        .as_deref()
        .context("Run has no run_branch — was it run with git push enabled?")?;

    let diff = std::fs::read_to_string(run_dir.join("final.patch"))
        .context("Failed to read final.patch — no diff available")?;
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
        fabro_github::GITHUB_API_BASE_URL,
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
        &run_dir,
        None,
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    match record {
        Some(record) => {
            info!(pr_url = %record.html_url, "Pull request created");
            if let Err(err) = record.save(&run_dir.join("pull_request.json")) {
                tracing::warn!(error = %err, "Failed to save pull_request.json");
            }
            println!("{}", record.html_url);
        }
        None => {
            println!("No pull request created (empty diff).");
        }
    }

    Ok(())
}
