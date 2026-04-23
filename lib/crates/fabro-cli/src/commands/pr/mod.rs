mod close;
mod create;
mod list;
mod merge;
mod view;

use anyhow::{Context, Result, anyhow};
use fabro_config::Storage;
use fabro_github::GitHubCredentials;
use fabro_types::PullRequestRecord;
use fabro_types::settings::InterpString;

use crate::args::{PrCommand, PrNamespace, ServerTargetArgs};
use crate::command_context::CommandContext;
use crate::shared::github::build_github_credentials;
use crate::user_config;

const GITHUB_CREDENTIALS_REQUIRED: &str =
    "GitHub credentials required — run `fabro install` or set GITHUB_TOKEN";

pub(crate) async fn dispatch(ns: PrNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        PrCommand::Create(args) => Box::pin(create::create_command(args, base_ctx)).await,
        PrCommand::List(args) => list::list_command(args, base_ctx).await,
        PrCommand::View(args) => view::view_command(args, base_ctx).await,
        PrCommand::Merge(args) => merge::merge_command(args, base_ctx).await,
        PrCommand::Close(args) => close::close_command(args, base_ctx).await,
    }
}

#[allow(
    deprecated,
    reason = "boundary-exempt(pr-api): remove with follow-up #1 when PR ops move server-side"
)]
fn load_github_credentials_required(base_ctx: &CommandContext) -> Result<GitHubCredentials> {
    let server_settings = fabro_config::ServerSettings::from_layer(base_ctx.machine_settings())
        .map_err(anyhow::Error::from)?;
    let vault = user_config::storage_dir(base_ctx.machine_settings())
        .ok()
        .and_then(|dir| fabro_vault::Vault::load(Storage::new(&dir).secrets_path()).ok());
    let creds = build_github_credentials(
        server_settings.server.integrations.github.strategy,
        server_settings
            .server
            .integrations
            .github
            .app_id
            .as_ref()
            .map(InterpString::as_source)
            .as_deref(),
        vault.as_ref(),
    )
    .map_err(|_| anyhow!(GITHUB_CREDENTIALS_REQUIRED))?;
    creds.context(GITHUB_CREDENTIALS_REQUIRED)
}

pub(crate) async fn load_pr_record(
    server: &ServerTargetArgs,
    run_id: &str,
    base_ctx: &CommandContext,
) -> Result<(PullRequestRecord, fabro_types::RunId)> {
    let ctx = base_ctx.with_target(server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(run_id).await?.run_id;
    let state = client.get_run_state(&run_id).await?;
    let record = state.pull_request.with_context(|| {
        format!("No pull request found in store. Create one first with: fabro pr create {run_id}")
    })?;
    Ok((record, run_id))
}
