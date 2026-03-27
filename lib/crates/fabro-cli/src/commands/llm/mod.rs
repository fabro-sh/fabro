mod chat;
mod prompt;

use anyhow::Result;

use crate::args::{GlobalArgs, LlmCommand, LlmNamespace};

pub async fn dispatch(ns: LlmNamespace, globals: &GlobalArgs) -> Result<()> {
    let cli_config = crate::cli_config::load_cli_config(None)?;

    match ns.command {
        LlmCommand::Prompt(args) => prompt::execute(args, &cli_config, globals).await,
        LlmCommand::Chat(args) => chat::execute(args, &cli_config, globals).await,
    }
}
