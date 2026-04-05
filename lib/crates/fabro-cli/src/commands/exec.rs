use anyhow::Result;
use fabro_agent::cli::{OutputFormat, run_with_args, run_with_args_and_client};
use fabro_config::mcp::McpServerEntry;
use fabro_llm::client::Client;
use fabro_llm::providers::FabroServerAdapter;
use fabro_mcp::config::McpServerSettings;
use std::collections::HashMap;
use std::sync::Arc;

use crate::args::{ExecArgs, GlobalArgs};
use crate::user_config;

pub(crate) async fn execute(mut args: ExecArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = user_config::load_user_settings()?;
    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = crate::sleep_inhibitor::guard(cli_settings.prevent_idle_sleep_enabled());
    let exec_defaults = cli_settings.exec.as_ref();
    args.agent.apply_cli_defaults(
        exec_defaults.and_then(|a| a.provider.as_deref()),
        exec_defaults.and_then(|a| a.model.as_deref()),
        exec_defaults.and_then(|a| a.permissions),
        exec_defaults.and_then(|a| a.output_format),
    );
    if globals.json {
        args.agent.output_format = Some(OutputFormat::Json);
    }
    let server_target = user_config::exec_server_target(&args.server_url, &cli_settings);
    let mcp_servers: Vec<McpServerSettings> = cli_settings
        .mcp_servers
        .into_iter()
        .map(|(name, entry): (String, McpServerEntry)| entry.into_config(name))
        .collect();
    if let Some(target) = server_target {
        tracing::info!(transport = "server", "Agent session starting");
        let http_client = user_config::build_server_client(target.tls.as_ref())?;
        let provider_name = args
            .agent
            .provider
            .clone()
            .unwrap_or_else(|| "anthropic".to_string());
        let adapter = Arc::new(FabroServerAdapter::new(
            http_client,
            &target.server_base_url,
            &provider_name,
        ));
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client
            .register_provider(adapter)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to register fabro server adapter: {e}"))?;
        run_with_args_and_client(args.agent, Some(client), mcp_servers).await?;
    } else {
        tracing::info!(transport = "direct", "Agent session starting");
        run_with_args(args.agent, mcp_servers).await?;
    }

    Ok(())
}
