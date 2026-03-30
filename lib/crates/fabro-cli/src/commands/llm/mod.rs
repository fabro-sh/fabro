mod chat;
mod prompt;

use anyhow::Result;

use crate::args::{GlobalArgs, LlmCommand, LlmNamespace};
use crate::cli_config::load_cli_settings_with_globals;

pub(crate) async fn dispatch(ns: LlmNamespace, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_cli_settings_with_globals(globals)?;

    match ns.command {
        LlmCommand::Prompt(args) => prompt::execute(args, &cli_settings, globals).await,
        LlmCommand::Chat(args) => chat::execute(args, &cli_settings, globals).await,
    }
}
