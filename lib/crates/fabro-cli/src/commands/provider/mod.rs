mod login;

use anyhow::Result;

use crate::args::{ProviderCommand, ProviderNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: ProviderNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        ProviderCommand::Login(args) => login::login_command(args, base_ctx).await,
    }
}
