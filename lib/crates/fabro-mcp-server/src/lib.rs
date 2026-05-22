mod config;
mod manifest_builder;
mod server;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
pub use config::{config_json, init_agent};
use fabro_client::Client;
pub use server::start;

pub type FabroClientFuture = Pin<Box<dyn Future<Output = Result<Client>> + Send>>;

pub type FabroClientFactory = Arc<dyn Fn() -> FabroClientFuture + Send + Sync>;

#[derive(Clone)]
pub struct FabroMcpServerSettings {
    pub client_factory: FabroClientFactory,
    pub config_path:    PathBuf,
    pub cwd:            PathBuf,
}

impl std::fmt::Debug for FabroMcpServerSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FabroMcpServerSettings")
            .field("client_factory", &"<factory>")
            .field("config_path", &self.config_path)
            .field("cwd", &self.cwd)
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct McpConfigSettings {
    pub server:      Option<String>,
    pub storage_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct McpInitSettings {
    pub agent:    McpAgent,
    pub config:   McpConfigSettings,
    pub home_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub enum McpAgent {
    Claude,
    Cursor,
    Windsurf,
}
