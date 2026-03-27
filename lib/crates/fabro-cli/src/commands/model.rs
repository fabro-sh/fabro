use anyhow::Result;

use crate::args::GlobalArgs;
#[cfg(feature = "server")]
use crate::cli_config;

pub async fn execute(
    command: Option<fabro_llm::cli::ModelsCommand>,
    globals: &GlobalArgs,
) -> Result<()> {
    let server = {
        #[cfg(feature = "server")]
        {
            let cli_config = cli_config::load_cli_config(None)?;
            let resolved = cli_config::resolve_mode(
                globals.mode.clone(),
                globals.server_url.as_deref(),
                &cli_config,
            );
            match resolved.mode {
                cli_config::ExecutionMode::Server => {
                    let client = cli_config::build_server_client(resolved.tls.as_ref())?;
                    Some(fabro_llm::cli::ServerConnection {
                        client,
                        base_url: resolved.server_base_url,
                    })
                }
                cli_config::ExecutionMode::Standalone => None,
            }
        }
        #[cfg(not(feature = "server"))]
        {
            let _ = globals;
            None
        }
    };

    fabro_llm::cli::run_models(command, server).await
}
