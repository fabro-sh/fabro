use anyhow::Result;
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table};
use fabro_util::terminal::Styles;
use futures::stream::{self, StreamExt};
use serde::Serialize;
use tracing::info;

use crate::args::PrListArgs;
use crate::command_context::CommandContext;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::{color_if, print_json_pretty};

#[derive(Serialize)]
struct PrRow {
    run_id: String,
    number: i64,
    state:  String,
    merged: bool,
    title:  String,
    url:    String,
}

pub(super) async fn list_command(args: PrListArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let printer = ctx.printer();
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
        if ctx.json_output() {
            print_json_pretty(&Vec::<PrRow>::new())?;
            return Ok(());
        }
        fabro_util::printout!(printer, "No pull requests found.");
        return Ok(());
    }

    let client = lookup.client().clone_for_reuse();
    let all_rows = stream::iter(entries)
        .map(|(run_id, record)| {
            let client = client.clone_for_reuse();
            async move {
                match client
                    .get_run_pull_request(&run_id.parse().expect("run id should parse"))
                    .await
                {
                    Ok(detail) => {
                        let state = if detail.merged {
                            "merged".to_string()
                        } else if detail.draft {
                            "draft".to_string()
                        } else if detail.state == "open" {
                            "open".to_string()
                        } else if detail.state == "closed" {
                            "closed".to_string()
                        } else {
                            "unknown".to_string()
                        };
                        Ok(PrRow {
                            run_id,
                            number: detail.number,
                            state,
                            merged: detail.merged,
                            title: detail.title,
                            url: detail.html_url,
                        })
                    }
                    Err(err) => {
                        let message = err.to_string();
                        if message.contains("GitHub integration unavailable on server.") {
                            return Err(err);
                        }

                        tracing::warn!(run_id, error = %message, "Failed to fetch PR state");
                        Ok(PrRow {
                            run_id,
                            number: i64::try_from(record.number)
                                .expect("stored pull request number should fit in i64"),
                            state: "unknown".to_string(),
                            merged: false,
                            title: record.title,
                            url: record.html_url,
                        })
                    }
                }
            }
        })
        .buffer_unordered(10)
        .collect::<Vec<Result<PrRow>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<PrRow>>>()?;
    let rows: Vec<_> = if args.all {
        all_rows
    } else {
        all_rows
            .into_iter()
            .filter(|row| row.state == "open" || row.state == "draft" || row.state == "unknown")
            .collect()
    };

    if ctx.json_output() {
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

    let styles = Styles::detect_stdout();
    let use_color = styles.use_color;

    let title: Vec<CellStruct> = vec![
        "RUN".cell().bold(use_color),
        "#".cell().bold(use_color),
        "STATE".cell().bold(use_color),
        "TITLE".cell().bold(use_color),
        "URL".cell().bold(use_color),
    ];

    let table_rows: Vec<Vec<CellStruct>> = rows
        .iter()
        .map(|row| {
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
            let state_color = match row.state.as_str() {
                "open" => Color::Green,
                "closed" => Color::Red,
                "merged" => Color::Magenta,
                "draft" => Color::Yellow,
                _ => Color::Ansi256(8),
            };
            vec![
                short_id
                    .cell()
                    .foreground_color(color_if(use_color, Color::Ansi256(8))),
                row.number.cell(),
                row.state
                    .clone()
                    .cell()
                    .foreground_color(color_if(use_color, state_color)),
                short_title.cell(),
                row.url.clone().cell(),
            ]
        })
        .collect();

    let color_choice = if use_color {
        cli_table::ColorChoice::Auto
    } else {
        cli_table::ColorChoice::Never
    };
    let table = table_rows
        .table()
        .title(title)
        .color_choice(color_choice)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    fabro_util::printout!(printer, "{}", table.display()?);

    info!(count = rows.len(), "Listed pull requests");
    Ok(())
}
