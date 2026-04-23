use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use fabro_config::UserSettings;
use fabro_config::merge::combine_files;
use fabro_types::settings::SettingsLayer;
use fabro_types::settings::cli::CliLayer;
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
    printer:            Printer,
    process_local_json: bool,
    cwd:                PathBuf,
    base_config_path:   PathBuf,
    cli_layer:          CliLayer,
    machine_settings:   SettingsLayer,
    user_settings:      UserSettings,
    server_mode:        ServerMode,
    server:             OnceCell<Arc<Client>>,
}

impl CommandContext {
    pub(crate) fn base(
        printer: Printer,
        cli_layer: &CliLayer,
        process_local_json: bool,
    ) -> Result<Self> {
        let (machine_settings, user_settings) = load_merged_settings(cli_layer, &ServerMode::None)?;
        Self::base_with_settings(
            printer,
            cli_layer,
            process_local_json,
            machine_settings,
            user_settings,
        )
    }

    pub(crate) fn base_with_settings(
        printer: Printer,
        cli_layer: &CliLayer,
        process_local_json: bool,
        machine_settings: SettingsLayer,
        user_settings: UserSettings,
    ) -> Result<Self> {
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        let base_config_path = user_config::active_settings_path(None);

        Ok(Self {
            printer,
            process_local_json,
            cwd,
            base_config_path,
            cli_layer: cli_layer.clone(),
            machine_settings,
            user_settings,
            server_mode: ServerMode::None,
            server: OnceCell::new(),
        })
    }

    pub(crate) fn with_target(&self, args: &ServerTargetArgs) -> Result<Self> {
        self.with_server_mode(ServerMode::ByTarget {
            target_override: args.server.clone(),
        })
    }

    pub(crate) fn with_connection(&self, args: &ServerConnectionArgs) -> Result<Self> {
        self.with_server_mode(ServerMode::ByStorageDir {
            target_override:      args.target.server.clone(),
            storage_dir_override: args.storage_dir.clone_path(),
        })
    }

    pub(crate) fn printer(&self) -> Printer {
        self.printer
    }

    pub(crate) fn explicit_json_requested(&self) -> bool {
        self.process_local_json
    }

    pub(crate) fn require_no_json_override(&self) -> Result<()> {
        crate::args::require_no_json_override(self.process_local_json)
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

    fn with_server_mode(&self, server_mode: ServerMode) -> Result<Self> {
        // Always reload settings for the requested derivation mode so the result
        // depends only on the requested mode, not on whichever derived context
        // happened to call into this helper.
        let (machine_settings, user_settings) =
            load_merged_settings(&self.cli_layer, &server_mode)?;

        Ok(
            self.with_server_mode_from_loaded_settings(
                server_mode,
                machine_settings,
                user_settings,
            ),
        )
    }

    fn with_server_mode_from_loaded_settings(
        &self,
        server_mode: ServerMode,
        machine_settings: SettingsLayer,
        user_settings: UserSettings,
    ) -> Self {
        Self {
            printer: self.printer,
            process_local_json: self.process_local_json,
            cwd: self.cwd.clone(),
            base_config_path: self.base_config_path.clone(),
            cli_layer: self.cli_layer.clone(),
            machine_settings,
            user_settings,
            server_mode,
            server: OnceCell::new(),
        }
    }
}

fn load_merged_settings(
    cli_layer: &CliLayer,
    server_mode: &ServerMode,
) -> Result<(SettingsLayer, UserSettings)> {
    let disk_settings = match server_mode {
        ServerMode::None | ServerMode::ByTarget { .. } => user_config::load_settings()?,
        ServerMode::ByStorageDir {
            storage_dir_override,
            ..
        } => user_config::load_settings_with_storage_dir(storage_dir_override.as_deref())?,
    };
    merge_settings_layer(disk_settings, cli_layer)
}

fn merge_settings_layer(
    disk_settings: SettingsLayer,
    cli_layer: &CliLayer,
) -> Result<(SettingsLayer, UserSettings)> {
    let machine_settings = combine_files(disk_settings, SettingsLayer {
        cli: Some(cli_layer.clone()),
        ..SettingsLayer::default()
    });
    let user_settings = UserSettings::from_layer(&machine_settings)?;
    Ok((machine_settings, user_settings))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fabro_config::user::apply_storage_dir_override;
    use fabro_config::{UserSettings, parse_settings_layer};
    use fabro_types::settings::SettingsLayer;
    use fabro_types::settings::cli::{CliLayer, CliOutputLayer, OutputFormat, OutputVerbosity};
    use fabro_util::printer::Printer;
    use tokio::sync::OnceCell;

    use super::{CommandContext, ServerMode, merge_settings_layer};

    fn cli_layer_with_json_and_verbose() -> CliLayer {
        CliLayer {
            output: Some(CliOutputLayer {
                format:    Some(OutputFormat::Json),
                verbosity: Some(OutputVerbosity::Verbose),
            }),
            ..CliLayer::default()
        }
    }

    fn synthetic_context_with_settings(
        process_local_json: bool,
        printer: Printer,
        cli_layer: CliLayer,
        machine_settings: SettingsLayer,
        user_settings: UserSettings,
        server_mode: ServerMode,
    ) -> CommandContext {
        CommandContext {
            printer,
            process_local_json,
            cwd: PathBuf::from("/tmp/workspace"),
            base_config_path: PathBuf::from("/tmp/settings.toml"),
            cli_layer,
            machine_settings,
            user_settings,
            server_mode,
            server: OnceCell::new(),
        }
    }

    fn synthetic_context(process_local_json: bool, printer: Printer) -> CommandContext {
        let cli_layer = cli_layer_with_json_and_verbose();
        let (machine_settings, user_settings) =
            merge_settings_layer(parse_settings_layer("_version = 1\n").unwrap(), &cli_layer)
                .expect("settings should merge");
        synthetic_context_with_settings(
            process_local_json,
            printer,
            cli_layer,
            machine_settings,
            user_settings,
            ServerMode::None,
        )
    }

    #[test]
    fn context_exposes_resolved_output_and_explicit_json_state() {
        let ctx = synthetic_context(true, Printer::Default);

        assert_eq!(ctx.user_settings().cli.output.format, OutputFormat::Json);
        assert_eq!(
            ctx.user_settings().cli.output.verbosity,
            OutputVerbosity::Verbose
        );
        assert!(ctx.explicit_json_requested());
        assert_eq!(ctx.printer(), Printer::Default);
    }

    #[test]
    fn deriving_target_context_preserves_invocation_state() {
        let base = synthetic_context(true, Printer::Verbose);
        let derived = base.with_server_mode_from_loaded_settings(
            ServerMode::ByTarget {
                target_override: Some("https://fabro.example.com".to_string()),
            },
            base.machine_settings().clone(),
            base.user_settings().clone(),
        );

        assert_eq!(derived.printer(), Printer::Verbose);
        assert!(derived.explicit_json_requested());
        assert_eq!(derived.user_settings(), base.user_settings());
        assert_eq!(derived.machine_settings(), base.machine_settings());
        assert_eq!(derived.cwd(), base.cwd());
        assert!(matches!(
            derived.server_mode,
            ServerMode::ByTarget { target_override: Some(ref target) }
            if target == "https://fabro.example.com"
        ));
    }

    #[test]
    fn storage_dir_override_only_changes_storage_root_in_merged_settings() {
        let cli_layer = cli_layer_with_json_and_verbose();
        let base_disk_settings = parse_settings_layer(
            r#"
_version = 1

[server.storage]
root = "/srv/fabro/default"
"#,
        )
        .expect("settings fixture should parse");
        let override_disk_settings = apply_storage_dir_override(
            base_disk_settings.clone(),
            Some(std::path::Path::new("/srv/fabro/override")),
        );

        let (base_settings, base_user_settings) =
            merge_settings_layer(base_disk_settings, &cli_layer)
                .expect("base settings should merge");
        let (connection_settings, connection_user_settings) =
            merge_settings_layer(override_disk_settings, &cli_layer)
                .expect("connection settings should merge");

        assert_eq!(base_user_settings, connection_user_settings);
        assert_eq!(base_user_settings.cli.output.format, OutputFormat::Json);
        assert_eq!(
            base_settings
                .server
                .as_ref()
                .and_then(|server| server.storage.as_ref())
                .and_then(|storage| storage.root.as_ref())
                .map(|root| root.as_source()),
            Some("/srv/fabro/default".to_string())
        );
        assert_eq!(
            connection_settings
                .server
                .as_ref()
                .and_then(|server| server.storage.as_ref())
                .and_then(|storage| storage.root.as_ref())
                .map(|root| root.as_source()),
            Some("/srv/fabro/override".to_string())
        );
    }

    #[test]
    fn deriving_target_from_connection_context_discards_storage_override() {
        let cli_layer = cli_layer_with_json_and_verbose();
        let base_disk_settings = parse_settings_layer(
            r#"
_version = 1

[server.storage]
root = "/srv/fabro/default"
"#,
        )
        .expect("settings fixture should parse");
        let override_disk_settings = apply_storage_dir_override(
            base_disk_settings.clone(),
            Some(std::path::Path::new("/srv/fabro/override")),
        );

        let (base_settings, base_user_settings) =
            merge_settings_layer(base_disk_settings, &cli_layer)
                .expect("base settings should merge");
        let (connection_settings, connection_user_settings) =
            merge_settings_layer(override_disk_settings, &cli_layer)
                .expect("connection settings should merge");
        let connection_ctx = synthetic_context_with_settings(
            false,
            Printer::Default,
            cli_layer,
            connection_settings,
            connection_user_settings,
            ServerMode::ByStorageDir {
                target_override:      None,
                storage_dir_override: Some(PathBuf::from("/srv/fabro/override")),
            },
        );

        let derived = connection_ctx.with_server_mode_from_loaded_settings(
            ServerMode::ByTarget {
                target_override: Some("https://fabro.example.com".to_string()),
            },
            base_settings,
            base_user_settings,
        );

        assert_eq!(
            derived
                .machine_settings()
                .server
                .as_ref()
                .and_then(|server| server.storage.as_ref())
                .and_then(|storage| storage.root.as_ref())
                .map(|root| root.as_source()),
            Some("/srv/fabro/default".to_string())
        );
    }

    #[test]
    fn explicit_json_guard_uses_invocation_flag_not_resolved_output_format() {
        let json_ctx = synthetic_context(true, Printer::Default);
        let text_ctx = synthetic_context(false, Printer::Default);

        assert!(json_ctx.require_no_json_override().is_err());
        assert!(text_ctx.require_no_json_override().is_ok());
    }

    #[test]
    fn target_resolution_errors_remain_deferred() {
        let base = synthetic_context(false, Printer::Default);
        let ctx = base.with_server_mode_from_loaded_settings(
            ServerMode::ByTarget {
                target_override: Some("not-a-valid-target".to_string()),
            },
            base.machine_settings().clone(),
            base.user_settings().clone(),
        );

        assert!(matches!(ctx.server_mode, ServerMode::ByTarget { .. }));
    }
}
