use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use tracing::info;

use crate::args::PrMergeArgs;
use crate::shared::print_json_pretty;

pub(super) async fn merge_command(
    args: PrMergeArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let (record, _run_id) =
        super::load_pr_record(&args.server, &args.run_id, cli, cli_layer, printer).await?;

    let creds = super::load_github_credentials_required(cli, cli_layer, printer)?;

    fabro_github::merge_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        &args.method,
        &fabro_github::github_api_base_url(),
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, method = %args.method, "Merged pull request");
    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&serde_json::json!({
            "number": record.number,
            "html_url": record.html_url,
            "method": args.method,
        }))?;
    } else {
        fabro_util::printout!(printer, "Merged #{} ({})", record.number, record.html_url);
    }

    Ok(())
}
