pub(crate) mod artifact;
pub(crate) mod auth;
pub(crate) mod cli_reference;
pub(crate) mod config;
pub(crate) mod doctor;
pub(crate) mod dump;
pub(crate) mod exec;
pub(crate) mod graph;
pub(crate) mod install;
pub(crate) mod mcp;
pub(crate) mod model;
pub(crate) mod parent;
pub(crate) mod parse;
pub(crate) mod pr;
pub(crate) mod preflight;
pub(crate) mod provider;
pub(crate) mod render_graph;
pub(crate) mod repo;
pub(crate) mod run;
pub(crate) mod runs;
pub(crate) mod sandbox;
pub(crate) mod secret;
pub(crate) mod server;
pub(crate) mod system;
pub(crate) mod uninstall;
pub(crate) mod upgrade;
pub(crate) mod validate;
pub(crate) mod variable;
pub(crate) mod version;
pub(crate) mod workflow;

use std::sync::Arc;

use anyhow::Result;
use fabro_client::Client;
use fabro_types::RunId;

use crate::args::ServerTargetArgs;
use crate::command_context::CommandContext;

pub(crate) async fn resolve_run_id(client: &Client, selector: &str) -> Result<RunId> {
    match selector.parse::<RunId>() {
        Ok(run_id) => Ok(run_id),
        Err(_) => Ok(client.resolve_run(selector).await?.id),
    }
}

pub(crate) async fn resolve_run_selector(
    base_ctx: &CommandContext,
    server: &ServerTargetArgs,
    selector: &str,
) -> Result<(CommandContext, Arc<Client>, RunId)> {
    let ctx = base_ctx.with_target(server)?;
    let client = ctx.server().await?;
    let run_id = resolve_run_id(client.as_ref(), selector).await?;
    Ok((ctx, client, run_id))
}
