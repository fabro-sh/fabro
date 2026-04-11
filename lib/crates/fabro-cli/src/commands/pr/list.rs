use anyhow::{Context, Result};
use fabro_util::printer::Printer;
use futures::future::join_all;
use serde::Serialize;
use tracing::info;

use crate::args::{GlobalArgs, PrListArgs};
use crate::command_context::CommandContext;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::print_json_pretty;

#[derive(Serialize)]
struct PrRow {
    run_id: String,
    number: u64,
    state:  String,
    title:  String,
    url:    String,
}

pub(super) async fn list_command(
    args: PrListArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;
    let ctx = CommandContext::for_target(&args.server, printer)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;

    let mut entries = Vec::new();
    for run in lookup.runs() {
        if let Ok(state) = lookup.client().get_run_state(&run.run_id()).await {
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
        fabro_util::printout!(printer, "No pull requests found.");
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
        fabro_util::printout!(
            printer,
            "No open pull requests found. Use --all to include closed/merged."
        );
        return Ok(());
    }

    fabro_util::printout!(
        printer,
        "{:<12} {:<6} {:<8} {:<50} URL",
        "RUN",
        "#",
        "STATE",
        "TITLE"
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
        fabro_util::printout!(
            printer,
            "{:<12} {:<6} {:<8} {:<50} {}",
            short_id,
            row.number,
            row.state,
            short_title,
            row.url
        );
    }

    info!(count = rows.len(), "Listed pull requests");
    Ok(())
}
