use anyhow::Result;
use fabro_types::settings::cli::OutputFormat;
use tracing::info;

use crate::args::PrMergeArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn merge_command(args: PrMergeArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let printer = ctx.printer();
    let (record, _run_id) = super::load_pr_record(&args.server, &args.run_id, base_ctx).await?;

    let creds = super::load_github_credentials_required(base_ctx)?;

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
    if ctx.user_settings().cli.output.format == OutputFormat::Json {
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
