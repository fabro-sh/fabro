use anyhow::Result;
use fabro_types::PullRequestProvider;
use tracing::info;

use crate::args::PrViewArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn view_command(args: PrViewArgs, base_ctx: &CommandContext) -> Result<()> {
    let (ctx, client, run_id) =
        super::resolve_run_selector(base_ctx, &args.server, &args.run_id).await?;
    let detail = client.get_run_pull_request(&run_id).await?;
    let pull_request = &detail.data.link;
    let github_details = detail.data.details.as_ref();

    info!(
        number = pull_request.number,
        owner_path = %pull_request.owner_path,
        repo = %pull_request.repo,
        details_status = %detail.meta.details_status,
        "Viewing pull request"
    );

    if ctx.json_output() {
        print_json_pretty(&detail)?;
        return Ok(());
    }

    let printer = ctx.printer();
    let label = pull_request_label(pull_request.provider);
    let title = github_details.map_or(label.name, |details| details.title.as_str());
    fabro_util::printout!(printer, "{}{} {title}", label.prefix, pull_request.number);
    let state_display = if github_details.is_some_and(|details| details.merged) {
        "merged"
    } else if github_details.is_some_and(|details| details.draft) {
        "draft"
    } else {
        github_details.map_or("", |details| details.state.as_str())
    };
    if !state_display.is_empty() {
        fabro_util::printout!(printer, "State:   {state_display}");
    }
    fabro_util::printout!(printer, "URL:     {}", pull_request.html_url());
    if let Some(reason) = detail.meta.details_unavailable_reason {
        fabro_util::printout!(printer, "Details: unavailable ({reason})");
    }
    if let Some(details) = github_details {
        let head_branch = &details.head_branch;
        let base_branch = &details.base_branch;
        let additions = details
            .additions
            .map_or_else(|| "unknown".to_string(), |value| value.to_string());
        let deletions = details
            .deletions
            .map_or_else(|| "unknown".to_string(), |value| value.to_string());
        fabro_util::printout!(printer, "Branch:  {head_branch} -> {base_branch}");
        fabro_util::printout!(printer, "Author:  {}", details.author.login);
        fabro_util::printout!(
            printer,
            "Changes: +{} -{} ({} files)",
            additions,
            deletions,
            details.changed_files
        );
    }

    Ok(())
}

struct PullRequestLabel {
    name:   &'static str,
    prefix: &'static str,
}

fn pull_request_label(provider: PullRequestProvider) -> PullRequestLabel {
    match provider {
        PullRequestProvider::Github => PullRequestLabel {
            name:   "Pull request",
            prefix: "#",
        },
        PullRequestProvider::Gitlab => PullRequestLabel {
            name:   "Merge request",
            prefix: "!",
        },
    }
}
