mod close;
mod create;
mod list;
mod merge;
mod view;

use anyhow::{Context, Result};

use fabro_types::PullRequestRecord;
use fabro_types::settings::InterpString;

use crate::args::{GlobalArgs, PrCommand, PrNamespace, ServerTargetArgs};
use crate::command_context::CommandContext;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::github::build_github_app_credentials;

pub(crate) async fn dispatch(ns: PrNamespace, globals: &GlobalArgs) -> Result<()> {
    let ctx = CommandContext::base()?;
    let server_settings =
        fabro_config::resolve_server_from_file(ctx.machine_settings()).map_err(|errors| {
            anyhow::anyhow!(
                "failed to resolve server settings:\n{}",
                errors
                    .into_iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })?;
    let github_app = build_github_app_credentials(
        server_settings
            .integrations
            .github
            .app_id
            .as_ref()
            .map(InterpString::as_source)
            .as_deref(),
    )?;
    match ns.command {
        PrCommand::Create(args) => {
            Box::pin(create::create_command(args, github_app, globals)).await
        }
        PrCommand::List(args) => list::list_command(args, github_app, globals).await,
        PrCommand::View(args) => view::view_command(args, github_app, globals).await,
        PrCommand::Merge(args) => merge::merge_command(args, github_app, globals).await,
        PrCommand::Close(args) => close::close_command(args, github_app, globals).await,
    }
}

pub(crate) async fn load_pr_record(
    server: &ServerTargetArgs,
    run_id: &str,
) -> Result<(PullRequestRecord, fabro_types::RunId)> {
    let ctx = CommandContext::for_target(server)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
    let run = lookup.resolve(run_id)?;
    let run_id = run.run_id();
    let state = lookup.client().get_run_state(&run_id).await?;
    let record = state.pull_request.with_context(|| {
        format!("No pull request found in store. Create one first with: fabro pr create {run_id}")
    })?;
    Ok((record, run_id))
}
