use anyhow::Result;
use chrono::Utc;
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Style, Table};
use fabro_util::terminal::Styles;

use crate::args::VariableListArgs;
use crate::command_context::CommandContext;
use crate::shared::{format_age, print_json_pretty};

pub(super) async fn list_command(_args: &VariableListArgs, ctx: &CommandContext) -> Result<()> {
    let client = ctx.server().await?;
    let printer = ctx.printer();
    let variables = client.list_variables().await?;
    if ctx.json_output() {
        print_json_pretty(&variables)?;
        return Ok(());
    }

    if variables.is_empty() {
        fabro_util::printerr!(printer, "No variables found.");
        return Ok(());
    }

    let styles = Styles::detect_stdout();
    let use_color = styles.use_color;
    let now = Utc::now();

    let title: Vec<CellStruct> = vec![
        "NAME".cell().bold(use_color),
        "VALUE".cell().bold(use_color),
        "UPDATED".cell().bold(use_color),
    ];

    let rows: Vec<Vec<CellStruct>> = variables
        .iter()
        .map(|variable| {
            vec![
                variable.name.clone().cell().bold(use_color),
                variable.value.clone().cell(),
                format_age(variable.updated_at, now).cell(),
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
