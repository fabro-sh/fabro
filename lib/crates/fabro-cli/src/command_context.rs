use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use fabro_config::CliLayer;
use fabro_types::settings::RunNamespace;
use fabro_types::settings::cli::{OutputFormat, OutputVerbosity};
use fabro_types::{ServerSettings, UserSettings};
use fabro_util::printer::Printer;
use tokio::sync::OnceCell;

use crate::args::{
    ServerConnectionArgs, ServerTargetArgs, printer_from_verbosity, require_no_json_override,
};
use crate::server_client::Client;
use crate::user_config::LoadedSettings;
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
    storage_dir:        PathBuf,
    run_settings:       std::result::Result<RunNamespace, String>,
    server_settings:    std::result::Result<ServerSettings, String>,
    user_settings:      UserSettings,
    server_mode:        ServerMode,
    server:             OnceCell<Arc<Client>>,
}

struct ResolvedCommandSettings {
    storage_dir:     PathBuf,
    run_settings:    std::result::Result<RunNamespace, String>,
    server_settings: std::result::Result<ServerSettings, String>,
    user_settings:   UserSettings,
}

impl CommandContext {
    pub(crate) fn from_disk(cli_layer: &CliLayer, process_local_json: bool) -> Result<Self> {
        let resolved_settings = load_merged_settings(cli_layer, &ServerMode::None)?;
        let printer = printer_from_verbosity(resolved_settings.user_settings.cli.output.verbosity);
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        let base_config_path = user_config::active_settings_path(None);

        Ok(Self {
            printer,
            process_local_json,
            cwd,
            base_config_path,
            cli_layer: cli_layer.clone(),
            storage_dir: resolved_settings.storage_dir,
            run_settings: resolved_settings.run_settings,
            server_settings: resolved_settings.server_settings,
            user_settings: resolved_settings.user_settings,
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
        require_no_json_override(self.process_local_json)
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    #[expect(
        dead_code,
        reason = "no current consumers since PR commands moved server-side; kept for parity with run_settings/server_settings"
    )]
    pub(crate) fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }

    #[expect(
        dead_code,
        reason = "no current consumers since PR commands moved server-side"
    )]
    pub(crate) fn server_settings(&self) -> Result<&ServerSettings> {
        self.server_settings
            .as_ref()
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    pub(crate) fn run_settings(&self) -> Result<&RunNamespace> {
        self.run_settings
            .as_ref()
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    pub(crate) fn user_settings(&self) -> &UserSettings {
        &self.user_settings
    }

    pub(crate) fn json_output(&self) -> bool {
        self.user_settings.cli.output.format == OutputFormat::Json
    }

    pub(crate) fn verbose(&self) -> bool {
        self.user_settings.cli.output.verbosity == OutputVerbosity::Verbose
    }

    pub(crate) async fn server(&self) -> Result<Arc<Client>> {
        let server_mode = self.server_mode.clone();
        let base_config_path = self.base_config_path.clone();
        let storage_dir = self.storage_dir.clone();
        let user_settings = self.user_settings.clone();

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
                    &user_settings,
                    &storage_dir,
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
        let resolved_settings = load_merged_settings(&self.cli_layer, &server_mode)?;

        Ok(Self {
            printer: self.printer,
            process_local_json: self.process_local_json,
            cwd: self.cwd.clone(),
            base_config_path: self.base_config_path.clone(),
            cli_layer: self.cli_layer.clone(),
            storage_dir: resolved_settings.storage_dir,
            run_settings: resolved_settings.run_settings,
            server_settings: resolved_settings.server_settings,
            user_settings: resolved_settings.user_settings,
            server_mode,
            server: OnceCell::new(),
        })
    }
}

fn load_merged_settings(
    cli_layer: &CliLayer,
    server_mode: &ServerMode,
) -> Result<ResolvedCommandSettings> {
    let loaded_settings = match server_mode {
        ServerMode::None | ServerMode::ByTarget { .. } => {
            user_config::load_resolved_settings(None, None, Some(cli_layer))?
        }
        ServerMode::ByStorageDir {
            storage_dir_override,
            ..
        } => user_config::load_resolved_settings(
            None,
            storage_dir_override.as_deref(),
            Some(cli_layer),
        )?,
    };
    Ok(resolve_command_settings(loaded_settings))
}

fn resolve_command_settings(loaded_settings: LoadedSettings) -> ResolvedCommandSettings {
    ResolvedCommandSettings {
        storage_dir:     loaded_settings.storage_dir,
        run_settings:    loaded_settings.run_settings,
        server_settings: loaded_settings.server_settings,
        user_settings:   loaded_settings.user_settings,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fabro_config::{CliLayer, CliOutputLayer};
    use fabro_types::settings::cli::{OutputFormat, OutputVerbosity};
    use fabro_util::printer::Printer;
    use tokio::sync::OnceCell;

    use super::{CommandContext, ServerMode, resolve_command_settings};
    use crate::user_config;

    fn cli_layer_with_json_and_verbose() -> CliLayer {
        CliLayer {
            output: Some(CliOutputLayer {
                format:    Some(OutputFormat::Json),
                verbosity: Some(OutputVerbosity::Verbose),
            }),
            ..CliLayer::default()
        }
    }

    fn synthetic_context(process_local_json: bool, printer: Printer) -> CommandContext {
        let cli_layer = cli_layer_with_json_and_verbose();
        let resolved_settings = resolve_command_settings(
            user_config::load_resolved_settings_from_toml("_version = 1\n", None, Some(&cli_layer))
                .expect("settings should resolve"),
        );
        CommandContext {
            printer,
            process_local_json,
            cwd: PathBuf::from("/tmp/workspace"),
            base_config_path: PathBuf::from("/tmp/settings.toml"),
            cli_layer,
            storage_dir: resolved_settings.storage_dir,
            run_settings: resolved_settings.run_settings,
            server_settings: resolved_settings.server_settings,
            user_settings: resolved_settings.user_settings,
            server_mode: ServerMode::None,
            server: OnceCell::new(),
        }
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
    fn storage_dir_override_only_changes_storage_root_in_merged_settings() {
        let cli_layer = cli_layer_with_json_and_verbose();
        let base_settings = resolve_command_settings(
            user_config::load_resolved_settings_from_toml(
                r#"
_version = 1

[server.storage]
root = "/srv/fabro/default"
"#,
                None,
                Some(&cli_layer),
            )
            .expect("base settings should resolve"),
        );
        let connection_settings = resolve_command_settings(
            user_config::load_resolved_settings_from_toml(
                r#"
_version = 1

[server.storage]
root = "/srv/fabro/default"
"#,
                Some(std::path::Path::new("/srv/fabro/override")),
                Some(&cli_layer),
            )
            .expect("connection settings should resolve"),
        );

        assert_eq!(
            base_settings.user_settings,
            connection_settings.user_settings
        );
        assert_eq!(
            base_settings.user_settings.cli.output.format,
            OutputFormat::Json
        );
        assert_eq!(
            base_settings.storage_dir,
            PathBuf::from("/srv/fabro/default")
        );
        assert_eq!(
            connection_settings.storage_dir,
            PathBuf::from("/srv/fabro/override")
        );
        assert_eq!(base_settings.run_settings.unwrap().agent.mcps.len(), 0);
        assert_eq!(
            connection_settings.run_settings.unwrap().agent.mcps.len(),
            0
        );
        assert!(base_settings.server_settings.is_err());
        assert!(connection_settings.server_settings.is_err());
    }

    #[test]
    fn storage_dir_stays_available_when_server_settings_do_not_resolve() {
        let resolved = resolve_command_settings(
            user_config::load_resolved_settings_from_toml(
                r#"
_version = 1

[server.storage]
root = "/srv/fabro"
"#,
                None,
                Some(&CliLayer::default()),
            )
            .expect("settings should resolve"),
        );

        assert_eq!(resolved.storage_dir, PathBuf::from("/srv/fabro"));
        assert!(resolved.run_settings.is_ok());
        assert!(resolved.server_settings.is_err());
    }

    #[test]
    fn run_settings_include_run_agent_mcps() {
        let resolved = resolve_command_settings(
            user_config::load_resolved_settings_from_toml(
                r#"
_version = 1

[run.agent.mcps.demo]
type = "stdio"
command = ["demo-mcp"]
"#,
                None,
                Some(&CliLayer::default()),
            )
            .expect("settings should resolve"),
        );

        let run_settings = resolved.run_settings.expect("run settings should resolve");
        assert!(run_settings.agent.mcps.contains_key("demo"));
    }

    #[test]
    fn explicit_json_guard_uses_invocation_flag_not_resolved_output_format() {
        let json_ctx = synthetic_context(true, Printer::Default);
        let text_ctx = synthetic_context(false, Printer::Default);

        assert!(json_ctx.require_no_json_override().is_err());
        assert!(text_ctx.require_no_json_override().is_ok());
    }
}
