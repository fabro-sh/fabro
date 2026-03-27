use anyhow::Result;
use fabro_config::FabroConfig;

use crate::args::GlobalArgs;

pub async fn execute(
    mut args: fabro_llm::cli::PromptArgs,
    cli_config: &FabroConfig,
    globals: &GlobalArgs,
) -> Result<()> {
    let llm_defaults = cli_config.llm.as_ref();
    if args.model.is_none() {
        args.model = llm_defaults.and_then(|l| l.model.clone());
    }

    #[cfg(feature = "server")]
    {
        let resolved = crate::cli_config::resolve_mode(
            globals.mode.clone(),
            globals.server_url.as_deref(),
            cli_config,
        );
        match resolved.mode {
            crate::cli_config::ExecutionMode::Server => {
                let client = crate::cli_config::build_server_client(resolved.tls.as_ref())?;
                let server = fabro_llm::cli::ServerConnection {
                    client,
                    base_url: resolved.server_base_url,
                };
                fabro_llm::cli::run_prompt_via_server(args, &server).await?;
            }
            crate::cli_config::ExecutionMode::Standalone => {
                fabro_llm::cli::run_prompt(args).await?;
            }
        }
    }

    #[cfg(not(feature = "server"))]
    {
        let _ = globals;
        fabro_llm::cli::run_prompt(args).await?;
    }

    Ok(())
}
