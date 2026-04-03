use anyhow::Result;
use fabro_llm::cli::{ChatArgs, ServerConnection, run_chat, run_chat_via_server};
use fabro_types::Settings;

use crate::args::GlobalArgs;

pub(super) async fn execute(
    mut args: ChatArgs,
    cli_settings: &Settings,
    globals: &GlobalArgs,
) -> Result<()> {
    globals.require_no_json()?;
    let llm_defaults = cli_settings.llm.as_ref();
    if args.model.is_none() {
        args.model = llm_defaults.and_then(|l| l.model.clone());
    }

    let resolved = crate::user_config::resolve_mode(
        globals.storage_dir.as_deref(),
        globals.server_url.as_deref(),
        cli_settings,
    );
    match resolved.mode {
        crate::user_config::ExecutionMode::Server => {
            let client = crate::user_config::build_server_client(resolved.tls.as_ref())?;
            let server = ServerConnection {
                client,
                base_url: resolved.server_base_url,
            };
            run_chat_via_server(args, &server).await?;
        }
        crate::user_config::ExecutionMode::Standalone => {
            run_chat(args).await?;
        }
    }

    Ok(())
}
