use rmcp::model::{
    CancelledNotificationParam, ClientCapabilities, ClientInfo, Implementation, LoggingLevel,
    LoggingMessageNotificationParam, ProgressNotificationParam, ProtocolVersion,
    ResourceUpdatedNotificationParam,
};
use rmcp::service::NotificationContext;
use rmcp::{ClientHandler, RoleClient};
use tracing::{debug, error, info, warn};

/// Minimal MCP client handler that logs server notifications via tracing.
#[derive(Clone)]
pub(crate) struct LoggingClientHandler;

impl ClientHandler for LoggingClientHandler {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new("fabro-mcp", env!("CARGO_PKG_VERSION")),
        )
        .with_protocol_version(ProtocolVersion::V_2025_03_26)
    }

    async fn on_cancelled(
        &self,
        params: CancelledNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        info!(
            request_id = %params.request_id,
            reason = ?params.reason,
            "MCP server cancelled request"
        );
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        debug!(
            progress_token = ?params.progress_token,
            progress = params.progress,
            total = ?params.total,
            message = ?params.message,
            "MCP server progress"
        );
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        info!(uri = %params.uri, "MCP server resource updated");
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        info!("MCP server resource list changed");
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        info!("MCP server tool list changed");
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        info!("MCP server prompt list changed");
    }

    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let logger = params.logger.as_deref();
        let data_str = params.data.to_string();
        let truncated: &str = if data_str.len() > 200 {
            // Safety: find a char boundary at or before 200 bytes
            let end = (0..=200)
                .rev()
                .find(|&i| data_str.is_char_boundary(i))
                .unwrap_or(0);
            &data_str[..end]
        } else {
            &data_str
        };
        match params.level {
            LoggingLevel::Emergency
            | LoggingLevel::Alert
            | LoggingLevel::Critical
            | LoggingLevel::Error => {
                error!(level = ?params.level, ?logger, data = %truncated, "MCP server log");
            }
            LoggingLevel::Warning => {
                warn!(level = ?params.level, ?logger, data = %truncated, "MCP server log");
            }
            LoggingLevel::Notice | LoggingLevel::Info => {
                info!(level = ?params.level, ?logger, data = %truncated, "MCP server log");
            }
            LoggingLevel::Debug => {
                debug!(level = ?params.level, ?logger, data = %truncated, "MCP server log");
            }
        }
    }
}
