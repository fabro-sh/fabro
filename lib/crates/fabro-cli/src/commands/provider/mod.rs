mod login;

use anyhow::Result;

use crate::args::{GlobalArgs, ProviderCommand, ProviderNamespace};

pub(crate) async fn dispatch(ns: ProviderNamespace, globals: &GlobalArgs) -> Result<()> {
    match ns.command {
        ProviderCommand::Login(args) => login::login_command(args, globals).await,
    }
}
