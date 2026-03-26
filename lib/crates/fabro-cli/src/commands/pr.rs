use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;
use fabro_model::Catalog;
use tracing::info;

#[derive(Args)]
pub struct PrCreateArgs {
    /// Run ID or prefix
    pub run_id: String,
    /// LLM model for generating PR description
    #[arg(long)]
    pub model: Option<String>,
}

#[derive(Args)]
pub struct PrListArgs {
    /// Show all PRs (including closed/merged), not just open
    #[arg(long)]
    pub all: bool,
}

#[derive(Args)]
pub struct PrViewArgs {
    /// Run ID or prefix
    pub run_id: String,
}

#[derive(Args)]
pub struct PrMergeArgs {
    /// Run ID or prefix
    pub run_id: String,
    /// Merge method: merge, squash, or rebase
    #[arg(long, default_value = "squash")]
    pub method: String,
}

#[derive(Args)]
pub struct PrCloseArgs {
    /// Run ID or prefix
    pub run_id: String,
}

fn load_pr_record(
    base: &Path,
    run_id: &str,
) -> Result<(fabro_workflows::pull_request::PullRequestRecord, PathBuf)> {
    let run_dir = fabro_workflows::run_lookup::resolve_run(base, run_id)?.path;
    let pr_path = run_dir.join("pull_request.json");
    let content = std::fs::read_to_string(&pr_path).with_context(|| {
        format!(
            "No pull_request.json found in run directory. \
             Create one first with: fabro pr create {run_id}"
        )
    })?;
    let record: fabro_workflows::pull_request::PullRequestRecord =
        serde_json::from_str(&content).context("Failed to parse pull_request.json")?;
    Ok((record, run_dir))
}

pub async fn list_command(
    args: PrListArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    list_from(&base, args, github_app).await
}

async fn list_from(
    base: &Path,
    args: PrListArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let runs = fabro_workflows::run_lookup::scan_runs(base).context("Failed to scan runs")?;

    let mut entries: Vec<(String, fabro_workflows::pull_request::PullRequestRecord)> = Vec::new();
    for run in &runs {
        let pr_path = run.path.join("pull_request.json");
        if let Ok(content) = std::fs::read_to_string(&pr_path) {
            if let Ok(record) =
                serde_json::from_str::<fabro_workflows::pull_request::PullRequestRecord>(&content)
            {
                entries.push((run.run_id.clone(), record));
            }
        }
    }

    if entries.is_empty() {
        println!("No pull requests found.");
        return Ok(());
    }

    struct PrRow {
        run_id: String,
        number: u64,
        state: String,
        title: String,
        url: String,
    }

    let futures: Vec<_> = entries
        .iter()
        .map(|(run_id, record)| {
            let creds = creds.clone();
            let run_id = run_id.clone();
            let record = record.clone();
            async move {
                match fabro_github::get_pull_request(
                    &creds,
                    &record.owner,
                    &record.repo,
                    record.number,
                    fabro_github::GITHUB_API_BASE_URL,
                )
                .await
                {
                    Ok(detail) => PrRow {
                        run_id,
                        number: detail.number,
                        state: if detail.draft {
                            "draft".to_string()
                        } else {
                            detail.state
                        },
                        title: detail.title,
                        url: detail.html_url,
                    },
                    Err(err) => {
                        tracing::warn!(run_id, error = %err, "Failed to fetch PR state");
                        PrRow {
                            run_id,
                            number: record.number,
                            state: "unknown".to_string(),
                            title: record.title,
                            url: record.html_url,
                        }
                    }
                }
            }
        })
        .collect();

    let all_rows = futures::future::join_all(futures).await;
    let rows: Vec<_> = if args.all {
        all_rows
    } else {
        all_rows
            .into_iter()
            .filter(|row| row.state == "open" || row.state == "draft" || row.state == "unknown")
            .collect()
    };

    if rows.is_empty() {
        println!("No open pull requests found. Use --all to include closed/merged.");
        return Ok(());
    }

    println!(
        "{:<12} {:<6} {:<8} {:<50} URL",
        "RUN", "#", "STATE", "TITLE"
    );
    for row in &rows {
        let short_id = if row.run_id.len() > 12 {
            &row.run_id[..12]
        } else {
            &row.run_id
        };
        let short_title = if row.title.len() > 50 {
            format!("{}…", &row.title[..row.title.floor_char_boundary(49)])
        } else {
            row.title.clone()
        };
        println!(
            "{:<12} {:<6} {:<8} {:<50} {}",
            short_id, row.number, row.state, short_title, row.url
        );
    }

    info!(count = rows.len(), "Listed pull requests");
    Ok(())
}

pub async fn view_command(
    args: PrViewArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    view_from(&base, args, github_app).await
}

async fn view_from(
    base: &Path,
    args: PrViewArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let (record, _run_dir) = load_pr_record(base, &args.run_id)?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let detail = fabro_github::get_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        fabro_github::GITHUB_API_BASE_URL,
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = detail.number, owner = %record.owner, repo = %record.repo, "Viewing pull request");

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

pub async fn merge_command(
    args: PrMergeArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    merge_from(&base, args, github_app).await
}

async fn merge_from(
    base: &Path,
    args: PrMergeArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let (record, _run_dir) = load_pr_record(base, &args.run_id)?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    fabro_github::merge_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        &args.method,
        fabro_github::GITHUB_API_BASE_URL,
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, method = %args.method, "Merged pull request");
    println!("Merged #{} ({})", record.number, record.html_url);

    Ok(())
}

pub async fn close_command(
    args: PrCloseArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    close_from(&base, args, github_app).await
}

async fn close_from(
    base: &Path,
    args: PrCloseArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let (record, _run_dir) = load_pr_record(base, &args.run_id)?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    fabro_github::close_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        fabro_github::GITHUB_API_BASE_URL,
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, "Closed pull request");
    println!("Closed #{} ({})", record.number, record.html_url);

    Ok(())
}

pub async fn create_command(
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    create_from(&base, args, github_app).await
}

async fn create_from(
    base: &Path,
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let run_dir = fabro_workflows::run_lookup::resolve_run(base, &args.run_id)?.path;

    let record =
        fabro_workflows::records::RunRecord::load(&run_dir).context("Failed to load run.json")?;

    let start = fabro_workflows::records::StartRecord::load(&run_dir)
        .context("Failed to load start.json")?;

    let conclusion = fabro_workflows::records::Conclusion::load(&run_dir.join("conclusion.json"))
        .context("Failed to load conclusion.json — is the run finished?")?;

    match conclusion.status {
        fabro_workflows::outcome::StageStatus::Success
        | fabro_workflows::outcome::StageStatus::PartialSuccess => {}
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
        fabro_sandbox::daytona::detect_repo_info(&cwd).map_err(|err| anyhow::anyhow!("{err}"))?;

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

    let record = fabro_workflows::pull_request::maybe_open_pull_request(
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
