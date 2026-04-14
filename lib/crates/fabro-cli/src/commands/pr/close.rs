use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use tracing::info;

use crate::args::PrCloseArgs;
use crate::shared::print_json_pretty;

pub(super) async fn close_command(
    args: PrCloseArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let (record, _run_id) =
        super::load_pr_record(&args.server, &args.run_id, cli, cli_layer, printer).await?;

    let creds = super::load_github_credentials_required(cli, cli_layer, printer)?;

    fabro_github::close_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        &fabro_github::github_api_base_url(),
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, "Closed pull request");
    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&serde_json::json!({
            "number": record.number,
            "html_url": record.html_url,
        }))?;
    } else {
        fabro_util::printout!(printer, "Closed #{} ({})", record.number, record.html_url);
    }

    Ok(())
}
