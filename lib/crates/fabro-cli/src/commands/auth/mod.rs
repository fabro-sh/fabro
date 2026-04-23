mod login;
mod logout;
mod status;

use anyhow::Result;

use crate::args::{AuthCommand, AuthNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: AuthNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        AuthCommand::Login(args) => login::login_command(args, base_ctx).await,
        AuthCommand::Logout(args) => logout::logout_command(args, base_ctx).await,
        AuthCommand::Status(args) => status::status_command(&args, base_ctx),
    }
}
