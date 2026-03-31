use anyhow::Result;
use fabro_config::FabroSettings;
use fabro_llm::cli::{PromptArgs, run_prompt};
#[cfg(feature = "server")]
use fabro_llm::cli::{ServerConnection, run_prompt_via_server};

use crate::args::GlobalArgs;

pub(super) async fn execute(
    mut args: PromptArgs,
    cli_settings: &FabroSettings,
    globals: &GlobalArgs,
) -> Result<()> {
    let llm_defaults = cli_settings.llm.as_ref();
    if args.model.is_none() {
        args.model = llm_defaults.and_then(|l| l.model.clone());
    }

    #[cfg(feature = "server")]
    {
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
                run_prompt_via_server(args, &server, globals.json).await?;
            }
            crate::user_config::ExecutionMode::Standalone => {
                run_prompt(args, globals.json).await?;
            }
        }
    }

    #[cfg(not(feature = "server"))]
    {
        let _ = globals;
        run_prompt(args, globals.json).await?;
    }

    Ok(())
}
