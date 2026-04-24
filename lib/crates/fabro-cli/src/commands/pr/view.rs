use anyhow::Result;
use tracing::info;

use crate::args::PrViewArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn view_command(args: PrViewArgs, base_ctx: &CommandContext) -> Result<()> {
    let (ctx, client, run_id) =
        super::resolve_run_for_pr(base_ctx, &args.server, &args.run_id).await?;
    let detail = client.get_run_pull_request(&run_id).await?;
    let github = &detail.github;

    info!(
        number = github.number,
        owner = %detail.record.owner,
        repo = %detail.record.repo,
        "Viewing pull request"
    );

    if ctx.json_output() {
        print_json_pretty(&detail)?;
        return Ok(());
    }

    let printer = ctx.printer();
    fabro_util::printout!(printer, "#{} {}", github.number, github.title);
    let state_display = if github.merged {
        "merged"
    } else if github.draft {
        "draft"
    } else {
        &github.state
    };
    fabro_util::printout!(printer, "State:   {state_display}");
    fabro_util::printout!(printer, "URL:     {}", github.html_url);
    fabro_util::printout!(
        printer,
        "Branch:  {} -> {}",
        github.head.ref_name,
        github.base.ref_name
    );
    fabro_util::printout!(printer, "Author:  {}", github.user.login);
    fabro_util::printout!(
        printer,
        "Changes: +{} -{} ({} files)",
        github.additions,
        github.deletions,
        github.changed_files
    );
    if let Some(body) = &github.body {
        if !body.is_empty() {
            fabro_util::printout!(printer, "");
            fabro_util::printout!(printer, "{body}");
        }
    }

    Ok(())
}
