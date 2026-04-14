use anyhow::Result;
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use futures::future::join_all;
use serde::Serialize;
use tracing::info;

use crate::args::PrListArgs;
use crate::command_context::CommandContext;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::{color_if, print_json_pretty};

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
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_target(&args.server, printer, cli.clone(), cli_layer)?;
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
        if cli.output.format == OutputFormat::Json {
            print_json_pretty(&Vec::<PrRow>::new())?;
            return Ok(());
        }
        fabro_util::printout!(printer, "No pull requests found.");
        return Ok(());
    }

    let creds = super::load_github_credentials_required(cli, cli_layer, printer)?;

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

    if cli.output.format == OutputFormat::Json {
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
