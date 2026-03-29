mod chat;
mod prompt;

use anyhow::Result;

use crate::args::{GlobalArgs, LlmCommand, LlmNamespace};
use crate::cli_config::load_cli_settings;

pub(crate) async fn dispatch(ns: LlmNamespace, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_cli_settings()?;

    match ns.command {
        LlmCommand::Prompt(args) => prompt::execute(args, &cli_settings, globals).await,
        LlmCommand::Chat(args) => chat::execute(args, &cli_settings, globals).await,
    }
}
