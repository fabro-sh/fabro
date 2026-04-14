use anyhow::Result;
use chrono::{DateTime, Utc};
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Style, Table};
use fabro_api::Client;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::OutputFormat;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::SecretListArgs;
use crate::server_client;
use crate::shared::print_json_pretty;

fn format_age(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let dur = now.signed_duration_since(dt);
    if dur.num_days() > 0 {
        format!("{}d ago", dur.num_days())
    } else if dur.num_hours() > 0 {
        format!("{}h ago", dur.num_hours())
    } else {
        format!("{}m ago", dur.num_minutes().max(1))
    }
}

pub(super) async fn list_command(
    client: &Client,
    _args: &SecretListArgs,
    cli: &CliSettings,
    printer: Printer,
) -> Result<()> {
    let response = client
        .list_secrets()
        .send()
        .await
        .map_err(server_client::map_api_error)?;
    let secrets = response.into_inner().data;
    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&secrets)?;
        return Ok(());
    }

    if secrets.is_empty() {
        fabro_util::printerr!(printer, "No secrets found.");
        return Ok(());
    }

    let styles = Styles::detect_stdout();
    let use_color = styles.use_color;
    let now = Utc::now();

    let title: Vec<CellStruct> = vec![
        "NAME".cell().bold(use_color),
        "TYPE".cell().bold(use_color),
        "UPDATED".cell().bold(use_color),
    ];

    let rows: Vec<Vec<CellStruct>> = secrets
        .iter()
        .map(|secret| {
            vec![
                secret.name.clone().cell().bold(use_color),
                secret.type_.to_string().cell(),
                format_age(secret.updated_at, now).cell(),
            ]
        })
        .collect();

    let color_choice = if use_color {
        cli_table::ColorChoice::Auto
    } else {
        cli_table::ColorChoice::Never
    };
    let table = rows
        .table()
        .title(title)
        .color_choice(color_choice)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    fabro_util::printout!(printer, "{}", table.display()?);

    Ok(())
}
