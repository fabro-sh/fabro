use anyhow::Result;
use tracing::info;

use crate::args::PrViewArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn view_command(args: PrViewArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.run_id;
    let detail = client.get_run_pull_request(&run_id).await?;

    info!(
        number = detail.number,
        owner = %detail.record.owner,
        repo = %detail.record.repo,
        "Viewing pull request"
    );

    if ctx.json_output() {
        print_json_pretty(&detail)?;
        return Ok(());
    }

    let printer = ctx.printer();
    fabro_util::printout!(printer, "#{} {}", detail.number, detail.title);
    let state_display = if detail.merged {
        "merged"
    } else if detail.draft {
        "draft"
    } else {
        &detail.state
    };
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
