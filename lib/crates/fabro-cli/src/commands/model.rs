use anyhow::Result;
use fabro_llm::cli::{ModelsCommand, run_models};

use crate::args::GlobalArgs;
use crate::server_client;
use crate::user_config;

pub(crate) async fn execute(command: Option<ModelsCommand>, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = user_config::load_user_settings_with_globals(globals)?;
    let client = match globals.server_url.as_deref() {
        Some(base_url) => {
            let tls = cli_settings
                .server
                .as_ref()
                .and_then(|server| server.tls.as_ref());
            server_client::connect_remote_api_client(base_url, tls)?
        }
        None => server_client::connect_api_client(&cli_settings.storage_dir()).await?,
    };

    run_models(command, client, globals.json).await
}
