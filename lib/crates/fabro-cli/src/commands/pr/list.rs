use std::path::Path;

use anyhow::{Context, Result};
use fabro_types::PullRequestRecord;
use fabro_workflow::run_lookup::{runs_base, scan_runs_with_summaries};
use futures::future::join_all;
use serde::Serialize;
use tracing::info;

use crate::args::{GlobalArgs, PrListArgs};
use crate::server_client;
use crate::shared::print_json_pretty;
use crate::user_config::load_user_settings_with_globals;

#[derive(Serialize)]
struct PrRow {
    run_id: String,
    number: u64,
    state: String,
    title: String,
    url: String,
}

pub(super) async fn list_command(
    args: PrListArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let client = server_client::connect_server(&cli_settings.storage_dir()).await?;
    let summaries = client.list_store_runs().await?;
    list_from(&client, &summaries, &base, args, github_app, globals).await
}

async fn list_from(
    client: &server_client::ServerStoreClient,
    summaries: &[fabro_store::RunSummary],
    base: &Path,
    args: PrListArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
) -> Result<()> {
    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let runs = scan_runs_with_summaries(summaries, base).context("Failed to scan runs")?;

    let mut entries: Vec<(String, PullRequestRecord)> = Vec::new();
    for run in &runs {
        if let Ok(state) = client.get_run_state(&run.run_id()).await {
            if let Some(record) = state.pull_request {
                entries.push((run.run_id().to_string(), record));
            }
        }
    }

    if entries.is_empty() {
        if globals.json {
            print_json_pretty(&Vec::<PrRow>::new())?;
            return Ok(());
        }
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
                    &fabro_github::github_api_base_url(),
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

    if globals.json {
        print_json_pretty(&rows)?;
        return Ok(());
    }

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
