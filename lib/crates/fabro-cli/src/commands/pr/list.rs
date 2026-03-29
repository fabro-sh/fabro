use std::path::Path;

use anyhow::{Context, Result};
use fabro_config::FabroSettingsExt;
use fabro_workflows::pull_request::PullRequestRecord;
use fabro_workflows::run_lookup::{runs_base, scan_runs_combined};
use futures::future::join_all;
use tracing::info;

use crate::args::PrListArgs;
use crate::cli_config::load_cli_settings;

pub(super) async fn list_command(
    args: PrListArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let cli_settings = load_cli_settings()?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = crate::store::build_store(&cli_settings.storage_dir())?;
    list_from(store.as_ref(), &base, args, github_app).await
}

async fn list_from(
    store: &dyn fabro_store::Store,
    base: &Path,
    args: PrListArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    struct PrRow {
        run_id: String,
        number: u64,
        state: String,
        title: String,
        url: String,
    }

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let runs = scan_runs_combined(store, base)
        .await
        .context("Failed to scan runs")?;

    let mut entries: Vec<(String, PullRequestRecord)> = Vec::new();
    for run in &runs {
        let pr_path = run.path.join("pull_request.json");
        if let Ok(content) = std::fs::read_to_string(&pr_path) {
            if let Ok(record) = serde_json::from_str::<PullRequestRecord>(&content) {
                entries.push((run.run_id.clone(), record));
            }
        }
    }

    if entries.is_empty() {
        println!("No pull requests found.");
        return Ok(());
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

    let all_rows = join_all(futures).await;
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
