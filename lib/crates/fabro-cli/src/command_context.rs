use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use fabro_types::Settings;
use tokio::sync::OnceCell;

use crate::args::{ServerConnectionArgs, ServerTargetArgs};
use crate::server_client::ServerStoreClient;
use crate::{server_client, user_config};

#[derive(Clone, Debug)]
pub(crate) enum ServerMode {
    None,
    ByTarget {
        target_override: Option<String>,
    },
    ByStorageDir {
        target_override: Option<String>,
        storage_dir_override: Option<PathBuf>,
    },
}

pub(crate) struct CommandContext {
    cwd: PathBuf,
    base_config_path: PathBuf,
    machine_settings: Settings,
    server_mode: ServerMode,
    server: OnceCell<Arc<ServerStoreClient>>,
}

impl CommandContext {
    pub(crate) fn base() -> Result<Self> {
        Self::new(ServerMode::None)
    }

    pub(crate) fn for_target(args: &ServerTargetArgs) -> Result<Self> {
        Self::new(ServerMode::ByTarget {
            target_override: args.server.clone(),
        })
    }

    pub(crate) fn for_connection(args: &ServerConnectionArgs) -> Result<Self> {
        Self::new(ServerMode::ByStorageDir {
            target_override: args.target.server.clone(),
            storage_dir_override: args.storage_dir.clone_path(),
        })
    }

    fn new(server_mode: ServerMode) -> Result<Self> {
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        let base_config_path = user_config::active_settings_path(None);
        let machine_settings = match &server_mode {
            ServerMode::None | ServerMode::ByTarget { .. } => user_config::load_settings()?,
            ServerMode::ByStorageDir {
                storage_dir_override,
                ..
            } => user_config::load_settings_with_storage_dir(storage_dir_override.as_deref())?,
        };

        Ok(Self {
            cwd,
            base_config_path,
            machine_settings,
            server_mode,
            server: OnceCell::new(),
        })
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn base_config_path(&self) -> &Path {
        &self.base_config_path
    }

    pub(crate) fn machine_settings(&self) -> &Settings {
        &self.machine_settings
    }

    pub(crate) async fn server(&self) -> Result<Arc<ServerStoreClient>> {
        let server_mode = self.server_mode.clone();
        let base_config_path = self.base_config_path.clone();
        let machine_settings = self.machine_settings.clone();

        let client = self
            .server
            .get_or_try_init(|| async move {
                let target = match server_mode {
                    ServerMode::None => bail!("This command context does not have server access"),
                    ServerMode::ByTarget { target_override }
                    | ServerMode::ByStorageDir {
                        target_override, ..
                    } => ServerTargetArgs {
                        server: target_override,
                    },
                };
                server_client::connect_server_with_settings(
                    &target,
                    &machine_settings,
                    &base_config_path,
                )
                .await
                .map(Arc::new)
            })
            .await?;

        Ok(Arc::clone(client))
    }
}
