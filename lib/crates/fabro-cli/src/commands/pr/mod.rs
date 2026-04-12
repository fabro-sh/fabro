mod close;
mod create;
mod list;
mod merge;
mod view;

use anyhow::{Context, Result, anyhow};
use fabro_github::GitHubCredentials;
use fabro_types::PullRequestRecord;
use fabro_types::settings::InterpString;
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, PrCommand, PrNamespace, ServerTargetArgs};
use crate::command_context::CommandContext;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::github::build_github_credentials;

const GITHUB_CREDENTIALS_REQUIRED: &str = "GitHub credentials required — run `gh auth login` or configure a GitHub App with `fabro install`";

pub(crate) async fn dispatch(
    ns: PrNamespace,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        PrCommand::Create(args) => Box::pin(create::create_command(args, globals, printer)).await,
        PrCommand::List(args) => list::list_command(args, globals, printer).await,
        PrCommand::View(args) => view::view_command(args, globals, printer).await,
        PrCommand::Merge(args) => merge::merge_command(args, globals, printer).await,
        PrCommand::Close(args) => close::close_command(args, globals, printer).await,
    }
}

async fn load_github_credentials_required(printer: Printer) -> Result<GitHubCredentials> {
    let ctx = CommandContext::base(printer)?;
    let server_settings =
        fabro_config::resolve_server_from_file(ctx.machine_settings()).map_err(|errors| {
            anyhow!(
                "failed to resolve server settings:\n{}",
                errors
                    .into_iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })?;
    let creds = build_github_credentials(
        server_settings.integrations.github.strategy,
        server_settings
            .integrations
            .github
            .app_id
            .as_ref()
            .map(InterpString::as_source)
            .as_deref(),
    )
    .await
    .map_err(|_| anyhow!(GITHUB_CREDENTIALS_REQUIRED))?;
    creds.context(GITHUB_CREDENTIALS_REQUIRED)
}

pub(crate) async fn load_pr_record(
    server: &ServerTargetArgs,
    run_id: &str,
    printer: Printer,
) -> Result<(PullRequestRecord, fabro_types::RunId)> {
    let ctx = CommandContext::for_target(server, printer)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
    let run = lookup.resolve(run_id)?;
    let run_id = run.run_id();
    let state = lookup.client().get_run_state(&run_id).await?;
    let record = state.pull_request.with_context(|| {
        format!("No pull request found in store. Create one first with: fabro pr create {run_id}")
    })?;
    Ok((record, run_id))
}
