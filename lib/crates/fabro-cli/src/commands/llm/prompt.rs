use anyhow::Result;
use fabro_llm::cli::{PromptArgs, ServerConnection, run_prompt, run_prompt_via_server};
use fabro_types::Settings;

use crate::args::GlobalArgs;
use crate::user_config::{ExecutionMode, build_server_client, resolve_mode};

pub(super) async fn execute(
    mut args: PromptArgs,
    cli_settings: &Settings,
    globals: &GlobalArgs,
) -> Result<()> {
    let llm_defaults = cli_settings.llm.as_ref();
    if args.model.is_none() {
        args.model = llm_defaults.and_then(|l| l.model.clone());
    }

    let resolved = resolve_mode(
        globals.storage_dir.as_deref(),
        globals.server_url.as_deref(),
        cli_settings,
    );
    match resolved.mode {
        ExecutionMode::Server => {
            let client = build_server_client(resolved.tls.as_ref())?;
            let server = ServerConnection {
                client,
                base_url: resolved.server_base_url,
            };
            run_prompt_via_server(args, &server, globals.json).await?;
        }
        ExecutionMode::Standalone => {
            run_prompt(args, globals.json).await?;
        }
    }

    Ok(())
}
