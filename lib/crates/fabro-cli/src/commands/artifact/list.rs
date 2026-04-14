use anyhow::Result;
use cli_table::format::{Border, Justify, Separator};
use cli_table::{Cell, CellStruct, Style, Table};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::ArtifactListArgs;

pub(super) async fn list_command(
    args: &ArtifactListArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let (_run_id, _client, entries) = super::resolve_artifacts(
        &args.server,
        &args.run_id,
        args.node.as_deref(),
        args.retry,
        cli,
        cli_layer,
        printer,
    )
    .await?;

    if cli.output.format == OutputFormat::Json {
        fabro_util::printout!(printer, "{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        fabro_util::printout!(printer, "No artifacts found for this run.");
        return Ok(());
    }

    let styles = Styles::detect_stdout();
    let use_color = styles.use_color;

    let title: Vec<CellStruct> = vec![
        "NODE".cell().bold(use_color),
        "RETRY".cell().bold(use_color).justify(Justify::Right),
        "PATH".cell().bold(use_color),
    ];

    let rows: Vec<Vec<CellStruct>> = entries
        .iter()
        .map(|entry| {
            vec![
                entry.node_slug.clone().cell().bold(use_color),
                entry.retry.cell().justify(Justify::Right),
                entry.relative_path.clone().cell(),
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

    fabro_util::printerr!(printer, "\n{} artifact(s)", entries.len());

    Ok(())
}
