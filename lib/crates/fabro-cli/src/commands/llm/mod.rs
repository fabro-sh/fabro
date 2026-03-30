mod chat;
mod prompt;

use anyhow::Result;

use crate::args::{GlobalArgs, LlmCommand, LlmNamespace};
use crate::user_config::load_user_settings_with_globals;

pub(crate) async fn dispatch(ns: LlmNamespace, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;

    match ns.command {
        LlmCommand::Prompt(args) => prompt::execute(args, &cli_settings, globals).await,
        LlmCommand::Chat(args) => chat::execute(args, &cli_settings, globals).await,
    }
}
