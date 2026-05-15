use anyhow::Result;
use tracing::info;

use crate::args::PrViewArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn view_command(args: PrViewArgs, base_ctx: &CommandContext) -> Result<()> {
    let (ctx, client, run_id) =
        super::resolve_run_for_pr(base_ctx, &args.server, &args.run_id).await?;
    let detail = client.get_run_pull_request(&run_id).await?;
    let pull_request = &detail.pull_request;

    info!(
        number = ?pull_request.number,
        owner = ?pull_request.owner,
        repo = ?pull_request.repo,
        "Viewing pull request"
    );

    if ctx.json_output() {
        print_json_pretty(&detail)?;
        return Ok(());
    }

    let printer = ctx.printer();
    let title = pull_request.title.as_deref().unwrap_or("Pull request");
    match pull_request.number {
        Some(number) => fabro_util::printout!(printer, "#{number} {title}"),
        None => fabro_util::printout!(printer, "Pull request {title}"),
    }
    let state_display = if detail.merged.unwrap_or(false) {
        "merged"
    } else if detail.draft.unwrap_or(false) {
        "draft"
    } else {
        detail.state.as_deref().unwrap_or_default()
    };
    if !state_display.is_empty() {
        fabro_util::printout!(printer, "State:   {state_display}");
    }
    if pull_request.provider != "github" || pull_request.number.is_none() {
        fabro_util::printout!(printer, "Provider: {}", pull_request.provider);
        fabro_util::printout!(printer, "URL:      {}", pull_request.html_url);
    } else {
        fabro_util::printout!(printer, "URL:     {}", pull_request.html_url);
    }
    if let (Some(head_branch), Some(base_branch)) = (
        pull_request.head_branch.as_deref(),
        pull_request.base_branch.as_deref(),
    ) {
        fabro_util::printout!(printer, "Branch:  {head_branch} -> {base_branch}");
    }
    if let Some(author) = detail.author.as_ref() {
        fabro_util::printout!(printer, "Author:  {}", author.login);
    }
    if let (Some(additions), Some(deletions), Some(changed_files)) =
        (detail.additions, detail.deletions, detail.changed_files)
    {
        fabro_util::printout!(
            printer,
            "Changes: +{} -{} ({} files)",
            additions,
            deletions,
            changed_files
        );
    }

    Ok(())
}
