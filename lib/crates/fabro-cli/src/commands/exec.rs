use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use fabro_agent::cli::{OutputFormat, run_with_args, run_with_args_and_client};
use fabro_llm::client::Client;
use fabro_llm::providers::FabroServerAdapter;
use fabro_mcp::config::{McpServerSettings, McpTransport};
use fabro_types::settings::cli::OutputFormat as SettingsOutputFormat;
use fabro_types::settings::run::McpEntryLayer;
use fabro_types::settings::{CliSettings, InterpString};
use fabro_util::printer::Printer;

use crate::args::ExecArgs;
use crate::user_config;

fn runtime_mcp_server(name: &str, entry: &McpEntryLayer) -> McpServerSettings {
    let transport = match entry {
        McpEntryLayer::Stdio {
            script,
            command,
            env,
            ..
        } => {
            let command = if let Some(script) = script {
                vec!["sh".to_string(), "-c".to_string(), script.as_source()]
            } else {
                command
                    .as_ref()
                    .map(|command| command.iter().map(InterpString::as_source).collect())
                    .unwrap_or_default()
            };
            McpTransport::Stdio {
                command,
                env: env
                    .iter()
                    .map(|(key, value)| (key.clone(), value.as_source()))
                    .collect(),
            }
        }
        McpEntryLayer::Http { url, headers, .. } => McpTransport::Http {
            url:     url.as_source(),
            headers: headers
                .iter()
                .map(|(key, value)| (key.clone(), value.as_source()))
                .collect(),
        },
        McpEntryLayer::Sandbox {
            script,
            command,
            port,
            env,
            ..
        } => {
            let command = if let Some(script) = script {
                vec!["sh".to_string(), "-c".to_string(), script.as_source()]
            } else {
                command
                    .as_ref()
                    .map(|command| command.iter().map(InterpString::as_source).collect())
                    .unwrap_or_default()
            };
            McpTransport::Sandbox {
                command,
                port: *port,
                env: env
                    .iter()
                    .map(|(key, value)| (key.clone(), value.as_source()))
                    .collect(),
            }
        }
    };
    let (startup_timeout_secs, tool_timeout_secs) = match entry {
        McpEntryLayer::Http {
            startup_timeout,
            tool_timeout,
            ..
        }
        | McpEntryLayer::Stdio {
            startup_timeout,
            tool_timeout,
            ..
        }
        | McpEntryLayer::Sandbox {
            startup_timeout,
            tool_timeout,
            ..
        } => (
            startup_timeout.map_or(10, |duration| duration.as_std().as_secs()),
            tool_timeout.map_or(60, |duration| duration.as_std().as_secs()),
        ),
    };
    McpServerSettings {
        name: name.to_string(),
        transport,
        startup_timeout_secs,
        tool_timeout_secs,
    }
}

pub(crate) async fn execute(
    mut args: ExecArgs,
    cli: &CliSettings,
    _printer: Printer,
) -> Result<()> {
    use fabro_agent::cli::PermissionLevel as AgentPermissionLevel;
    use fabro_types::settings::run::AgentPermissions;

    let raw_settings = user_config::load_settings()?;
    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = crate::sleep_inhibitor::guard(cli.exec.prevent_idle_sleep);
    let provider_str = cli
        .exec
        .model
        .provider
        .as_ref()
        .map(InterpString::as_source);
    let model_str = cli.exec.model.name.as_ref().map(InterpString::as_source);
    let permissions = cli.exec.agent.permissions.map(|p| match p {
        AgentPermissions::ReadOnly => AgentPermissionLevel::ReadOnly,
        AgentPermissions::ReadWrite => AgentPermissionLevel::ReadWrite,
        AgentPermissions::Full => AgentPermissionLevel::Full,
    });
    let output_format = Some(match cli.output.format {
        SettingsOutputFormat::Text => OutputFormat::Text,
        SettingsOutputFormat::Json => OutputFormat::Json,
    });
    args.agent.apply_cli_defaults(
        provider_str.as_deref(),
        model_str.as_deref(),
        permissions,
        output_format,
    );
    let server_target = user_config::exec_server_target(&args.server, &raw_settings)?;
    // v2 MCPs live under `cli.exec.agent.mcps` (owner-specific) or
    // `run.agent.mcps`. For `fabro exec` we use the cli.exec path, falling
    // back to run.agent.mcps if unset.
    let mcp_servers: Vec<McpServerSettings> = if !cli.exec.agent.mcps.is_empty() {
        cli.exec
            .agent
            .mcps
            .values()
            .map(|server| McpServerSettings {
                name:                 server.name.clone(),
                transport:            server.transport.clone(),
                startup_timeout_secs: server.startup_timeout_secs,
                tool_timeout_secs:    server.tool_timeout_secs,
            })
            .collect()
    } else if let Some(mcps) = raw_settings
        .cli
        .as_ref()
        .and_then(|cli| cli.exec.as_ref())
        .and_then(|exec| exec.agent.as_ref())
        .map(|agent| &agent.mcps)
        .filter(|mcps| !mcps.is_empty())
    {
        mcps.iter()
            .map(|(name, entry)| runtime_mcp_server(name, entry))
            .collect()
    } else {
        fabro_config::resolve_run_from_file(&raw_settings)
            .map(|settings| {
                settings
                    .agent
                    .mcps
                    .values()
                    .map(|server| McpServerSettings {
                        name:                 server.name.clone(),
                        transport:            server.transport.clone(),
                        startup_timeout_secs: server.startup_timeout_secs,
                        tool_timeout_secs:    server.tool_timeout_secs,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
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
                let http_client = fabro_http::HttpClientBuilder::new()
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
