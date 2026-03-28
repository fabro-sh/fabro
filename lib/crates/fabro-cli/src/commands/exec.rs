use anyhow::Result;
#[cfg(feature = "server")]
use fabro_agent::cli::run_with_args_and_client;
use fabro_agent::cli::{run_with_args, AgentArgs};
use fabro_config::mcp::McpServerEntry;
use fabro_mcp::config::McpServerConfig;

use crate::args::GlobalArgs;
use crate::cli_config;

pub async fn execute(mut args: AgentArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_config = cli_config::load_cli_settings(None)?;
    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = crate::sleep_inhibitor::guard(cli_config.prevent_idle_sleep_enabled());
    let exec_defaults = cli_config.exec.as_ref();
    args.apply_cli_defaults(
        exec_defaults.and_then(|a| a.provider.as_deref()),
        exec_defaults.and_then(|a| a.model.as_deref()),
        exec_defaults.and_then(|a| a.permissions),
        exec_defaults.and_then(|a| a.output_format),
    );
    #[cfg(feature = "server")]
    let resolved = cli_config::resolve_mode(
        globals.mode.clone(),
        globals.server_url.as_deref(),
        &cli_config,
    );
    let mcp_servers: Vec<McpServerConfig> = cli_config
        .mcp_servers
        .into_iter()
        .map(|(name, entry): (String, McpServerEntry)| entry.into_config(name))
        .collect();
    #[cfg(feature = "server")]
    {
        match resolved.mode {
            cli_config::ExecutionMode::Server => {
                tracing::info!(mode = "server", "Agent session starting");
                let http_client = cli_config::build_server_client(resolved.tls.as_ref())?;
                let provider_name = args
                    .provider
                    .clone()
                    .unwrap_or_else(|| "anthropic".to_string());
                let adapter = std::sync::Arc::new(fabro_llm::providers::FabroServerAdapter::new(
                    http_client,
                    &resolved.server_base_url,
                    &provider_name,
                ));
                let mut client =
                    fabro_llm::client::Client::new(std::collections::HashMap::new(), None, vec![]);
                client
                    .register_provider(adapter)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to register fabro server adapter: {e}"))?;
                run_with_args_and_client(args, Some(client), mcp_servers).await?
            }
            cli_config::ExecutionMode::Standalone => {
                tracing::info!(mode = "standalone", "Agent session starting");
                run_with_args(args, mcp_servers).await?
            }
        }
    }
    #[cfg(not(feature = "server"))]
    {
        let _ = globals;
        tracing::info!(mode = "standalone", "Agent session starting");
        run_with_args(args, mcp_servers).await?
    }

    Ok(())
}
