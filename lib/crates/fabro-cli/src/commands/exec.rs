use anyhow::Result;
use fabro_agent::cli::{OutputFormat, run_with_args, run_with_args_and_client};
use fabro_llm::client::Client;
use fabro_llm::providers::FabroServerAdapter;
use fabro_mcp::config::{McpServerSettings, bridge_mcp_entry};
use fabro_types::settings::v2::InterpString;
use std::collections::HashMap;
use std::sync::Arc;

use crate::args::{ExecArgs, GlobalArgs};
use crate::user_config;

pub(crate) async fn execute(mut args: ExecArgs, globals: &GlobalArgs) -> Result<()> {
    use fabro_agent::cli::PermissionLevel as AgentPermissionLevel;
    use fabro_types::settings::v2::run::AgentPermissions;

    let cli_settings = user_config::load_settings()?;
    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = crate::sleep_inhibitor::guard(cli_settings.prevent_idle_sleep_enabled());
    let exec_defaults = cli_settings.cli_exec();
    let exec_model = exec_defaults.and_then(|e| e.model.as_ref());
    let exec_agent = exec_defaults.and_then(|e| e.agent.as_ref());
    let provider_str = exec_model
        .and_then(|m| m.provider.as_ref())
        .map(InterpString::as_source);
    let model_str = exec_model
        .and_then(|m| m.name.as_ref())
        .map(InterpString::as_source);
    let permissions = exec_agent
        .and_then(|agent| agent.permissions)
        .map(|p| match p {
            AgentPermissions::ReadOnly => AgentPermissionLevel::ReadOnly,
            AgentPermissions::ReadWrite => AgentPermissionLevel::ReadWrite,
            AgentPermissions::Full => AgentPermissionLevel::Full,
        });
    args.agent.apply_cli_defaults(
        provider_str.as_deref(),
        model_str.as_deref(),
        permissions,
        None,
    );
    if globals.json {
        args.agent.output_format = Some(OutputFormat::Json);
    }
    let server_target = user_config::exec_server_target(&args.server, &cli_settings)?;
    // v2 MCPs live under `cli.exec.agent.mcps` (owner-specific) or
    // `run.agent.mcps`. For `fabro exec` we use the cli.exec path, falling
    // back to run.agent.mcps if unset.
    let mcps_iter = exec_agent
        .map(|a| &a.mcps)
        .filter(|m| !m.is_empty())
        .or_else(|| cli_settings.run_agent_mcps());
    let mcp_servers: Vec<McpServerSettings> = mcps_iter
        .map(|mcps| {
            mcps.iter()
                .map(|(name, entry)| bridge_mcp_entry(entry).into_config(name.clone()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(target) = server_target {
        tracing::info!(transport = "server", "Agent session starting");
        let provider_name = args
            .agent
            .provider
            .clone()
            .unwrap_or_else(|| "anthropic".to_string());
        let (api_url, http_client) = match &target {
            user_config::ServerTarget::HttpUrl { api_url, tls } => (
                api_url.clone(),
                user_config::build_server_client(tls.as_ref())?,
            ),
            user_config::ServerTarget::UnixSocket(path) => {
                let http_client = reqwest::ClientBuilder::new()
                    .unix_socket(path.as_path())
                    .no_proxy()
                    .build()?;
                ("http://fabro".to_string(), http_client)
            }
        };
        let adapter = Arc::new(FabroServerAdapter::new(
            http_client,
            &api_url,
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
