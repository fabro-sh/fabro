use anyhow::Result;
#[cfg(feature = "server")]
use fabro_llm::cli::ServerConnection;
use fabro_llm::cli::{ModelsCommand, run_models};

use crate::args::GlobalArgs;
#[cfg(feature = "server")]
use crate::user_config;

pub(crate) async fn execute(command: Option<ModelsCommand>, globals: &GlobalArgs) -> Result<()> {
    let server = {
        #[cfg(feature = "server")]
        {
            let cli_settings = user_config::load_user_settings_with_globals(globals)?;
            let resolved = user_config::resolve_mode(
                globals.storage_dir.as_deref(),
                globals.server_url.as_deref(),
                &cli_settings,
            );
            match resolved.mode {
                user_config::ExecutionMode::Server => {
                    let client = user_config::build_server_client(resolved.tls.as_ref())?;
                    Some(ServerConnection {
                        client,
                        base_url: resolved.server_base_url,
                    })
                }
                user_config::ExecutionMode::Standalone => None,
            }
        }
        #[cfg(not(feature = "server"))]
        {
            let _ = globals;
            None
        }
    };

    run_models(command, server, globals.json).await
}
