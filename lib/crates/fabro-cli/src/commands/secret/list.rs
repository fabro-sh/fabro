use anyhow::Result;
use chrono::Utc;
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Style, Table};
use fabro_util::terminal::Styles;

use crate::args::SecretListArgs;
use crate::command_context::CommandContext;
use crate::shared::{format_age, print_json_pretty};

pub(super) async fn list_command(_args: &SecretListArgs, ctx: &CommandContext) -> Result<()> {
    let client = ctx.server().await?;
    let printer = ctx.printer();
    let secrets = client.list_secrets().await?;
    if ctx.json_output() {
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
                secret.secret_type.to_string().cell(),
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
