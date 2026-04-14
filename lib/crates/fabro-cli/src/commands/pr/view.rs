use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use tracing::info;

use crate::args::PrViewArgs;
use crate::shared::print_json_pretty;

pub(super) async fn view_command(
    args: PrViewArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let (record, _run_id) =
        super::load_pr_record(&args.server, &args.run_id, cli, cli_layer, printer).await?;

    let creds = super::load_github_credentials_required(cli, cli_layer, printer)?;

    let detail = fabro_github::get_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        &fabro_github::github_api_base_url(),
    )
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    info!(number = detail.number, owner = %record.owner, repo = %record.repo, "Viewing pull request");

    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&detail)?;
        return Ok(());
    }

    fabro_util::printout!(printer, "#{} {}", detail.number, detail.title);
    let state_display = if detail.draft { "draft" } else { &detail.state };
    fabro_util::printout!(printer, "State:   {state_display}");
    fabro_util::printout!(printer, "URL:     {}", detail.html_url);
    fabro_util::printout!(
        printer,
        "Branch:  {} -> {}",
        detail.head.ref_name,
        detail.base.ref_name
    );
    fabro_util::printout!(printer, "Author:  {}", detail.user.login);
    fabro_util::printout!(
        printer,
        "Changes: +{} -{} ({} files)",
        detail.additions,
        detail.deletions,
        detail.changed_files
    );
    if let Some(body) = &detail.body {
        if !body.is_empty() {
            fabro_util::printout!(printer, "");
            fabro_util::printout!(printer, "{body}");
        }
    }

    Ok(())
}
