use std::fmt::Write as _;

use anyhow::{Context as _, Result};

use crate::args::{McpAgent, McpCommand, McpNamespace, ServerConnectionArgs};
use crate::command_context::CommandContext;
use crate::server_client;

pub(crate) async fn dispatch(ns: McpNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        McpCommand::Start(args) => {
            fabro_mcp_server::start(server_settings(base_ctx, &args.connection)?).await
        }
        McpCommand::Config(args) => {
            let json = fabro_mcp_server::config_json(&config_settings(&args.connection))?;
            let _ = write!(base_ctx.printer().stdout_important(), "{json}");
            Ok(())
        }
        McpCommand::Init(args) => {
            fabro_mcp_server::init_agent(&init_settings(args.agent, &args.connection)?)?;
            Ok(())
        }
    }
}

fn server_settings(
    base_ctx: &CommandContext,
    connection: &ServerConnectionArgs,
) -> Result<fabro_mcp_server::FabroMcpServerSettings> {
    let connection_ctx = base_ctx.with_connection(connection)?;
    let target = connection.target.clone();
    let user_settings = connection_ctx.user_settings().clone();
    let storage_dir = connection_ctx.storage_dir().to_path_buf();
    let base_config_path = connection_ctx.base_config_path().to_path_buf();
    let config_path = base_config_path.clone();
    let client_factory: fabro_mcp_server::FabroClientFactory = std::sync::Arc::new(move || {
        let target = target.clone();
        let user_settings = user_settings.clone();
        let storage_dir = storage_dir.clone();
        let base_config_path = base_config_path.clone();
        let future: fabro_mcp_server::FabroClientFuture = Box::pin(async move {
            server_client::connect_server_with_settings(
                &target,
                &user_settings,
                &storage_dir,
                &base_config_path,
            )
            .await
        });
        future
    });
    Ok(fabro_mcp_server::FabroMcpServerSettings {
        client_factory,
        config_path,
        cwd: base_ctx.cwd().to_path_buf(),
    })
}

fn init_settings(
    agent: McpAgent,
    connection: &ServerConnectionArgs,
) -> Result<fabro_mcp_server::McpInitSettings> {
    Ok(fabro_mcp_server::McpInitSettings {
        agent:    McpAgentForServer(agent).into(),
        config:   config_settings(connection),
        home_dir: home_dir()?,
    })
}

fn config_settings(connection: &ServerConnectionArgs) -> fabro_mcp_server::McpConfigSettings {
    fabro_mcp_server::McpConfigSettings {
        server:      connection.target.server.clone(),
        storage_dir: connection.storage_dir.clone_path(),
    }
}

fn home_dir() -> Result<std::path::PathBuf> {
    dirs::home_dir().context("failed to resolve home directory for MCP config")
}

struct McpAgentForServer(McpAgent);

impl From<McpAgentForServer> for fabro_mcp_server::McpAgent {
    fn from(value: McpAgentForServer) -> Self {
        match value.0 {
            McpAgent::Claude => Self::Claude,
            McpAgent::Cursor => Self::Cursor,
            McpAgent::Windsurf => Self::Windsurf,
        }
    }
}
