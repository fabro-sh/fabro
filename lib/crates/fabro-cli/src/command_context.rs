use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use fabro_config::UserSettings;
use fabro_types::settings::cli::CliLayer;
use fabro_types::settings::{Combine, SettingsLayer};
use fabro_util::printer::Printer;
use tokio::sync::OnceCell;

use crate::args::{ServerConnectionArgs, ServerTargetArgs};
use crate::server_client::Client;
use crate::{server_client, user_config};

#[derive(Clone, Debug)]
pub(crate) enum ServerMode {
    None,
    ByTarget {
        target_override: Option<String>,
    },
    ByStorageDir {
        target_override:      Option<String>,
        storage_dir_override: Option<PathBuf>,
    },
}

pub(crate) struct CommandContext {
    #[allow(
        dead_code,
        reason = "This item is kept for command plumbing that is still being wired through."
    )]
    printer:          Printer,
    cwd:              PathBuf,
    base_config_path: PathBuf,
    machine_settings: SettingsLayer,
    user_settings:    UserSettings,
    server_mode:      ServerMode,
    server:           OnceCell<Arc<Client>>,
}

impl CommandContext {
    pub(crate) fn base(printer: Printer, cli_layer: &CliLayer) -> Result<Self> {
        Self::new(printer, ServerMode::None, cli_layer)
    }

    pub(crate) fn for_target(
        args: &ServerTargetArgs,
        printer: Printer,
        cli_layer: &CliLayer,
    ) -> Result<Self> {
        Self::new(
            printer,
            ServerMode::ByTarget {
                target_override: args.server.clone(),
            },
            cli_layer,
        )
    }

    pub(crate) fn for_connection(
        args: &ServerConnectionArgs,
        printer: Printer,
        cli_layer: &CliLayer,
    ) -> Result<Self> {
        Self::new(
            printer,
            ServerMode::ByStorageDir {
                target_override:      args.target.server.clone(),
                storage_dir_override: args.storage_dir.clone_path(),
            },
            cli_layer,
        )
    }

    fn new(printer: Printer, server_mode: ServerMode, cli_layer: &CliLayer) -> Result<Self> {
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        let base_config_path = user_config::active_settings_path(None);
        let disk_settings = match &server_mode {
            ServerMode::None | ServerMode::ByTarget { .. } => user_config::load_settings()?,
            ServerMode::ByStorageDir {
                storage_dir_override,
                ..
            } => user_config::load_settings_with_storage_dir(storage_dir_override.as_deref())?,
        };
        let machine_settings = SettingsLayer {
            cli: Some(cli_layer.clone()),
            ..SettingsLayer::default()
        }
        .combine(disk_settings);
        let user_settings = fabro_config::UserSettings::from_layer(&machine_settings)?;

        Ok(Self {
            printer,
            cwd,
            base_config_path,
            machine_settings,
            user_settings,
            server_mode,
            server: OnceCell::new(),
        })
    }

    #[allow(
        dead_code,
        reason = "This item is kept for command plumbing that is still being wired through."
    )]
    pub(crate) fn printer(&self) -> Printer {
        self.printer
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn machine_settings(&self) -> &SettingsLayer {
        &self.machine_settings
    }

    pub(crate) fn user_settings(&self) -> &UserSettings {
        &self.user_settings
    }

    pub(crate) async fn server(&self) -> Result<Arc<Client>> {
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
