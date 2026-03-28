mod login;

use anyhow::Result;

use crate::args::{ProviderCommand, ProviderNamespace};

pub(crate) async fn dispatch(ns: ProviderNamespace) -> Result<()> {
    match ns.command {
        ProviderCommand::Login(args) => login::login_command(args).await,
    }
}
