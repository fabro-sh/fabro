use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::Result;
pub(crate) use fabro_client::ServerTarget;
pub(crate) use fabro_config::user::{active_settings_path, default_storage_dir};
use fabro_config::user::{default_socket_path, load_settings_config};
use fabro_config::{RunSettingsBuilder, ServerSettingsBuilder, UserSettingsBuilder};
use fabro_types::settings::cli::{CliLayer, CliTargetSettings};
use fabro_types::settings::{CliNamespace, Combine, RunNamespace, SettingsLayer};
use fabro_types::{ServerSettings, UserSettings};
use fabro_util::version::FABRO_VERSION;
use tracing::debug;

use crate::args::ServerTargetArgs;

pub(crate) struct LoadedSettings {
    pub(crate) storage_dir:      PathBuf,
    pub(crate) config_log_level: Option<String>,
    pub(crate) run_settings:     std::result::Result<RunNamespace, String>,
    pub(crate) server_settings:  std::result::Result<ServerSettings, String>,
    pub(crate) user_settings:    UserSettings,
}

pub(crate) fn load_resolved_settings(
    config_path: Option<&Path>,
    storage_dir: Option<&Path>,
    cli_layer: Option<&CliLayer>,
) -> anyhow::Result<LoadedSettings> {
    let layer = load_settings_config(config_path)?;
    resolve_loaded_settings(layer, storage_dir, cli_layer)
}

fn resolve_loaded_settings(
    layer: SettingsLayer,
    storage_dir: Option<&Path>,
    cli_layer: Option<&CliLayer>,
) -> anyhow::Result<LoadedSettings> {
    let storage_override = storage_dir.map(Path::to_path_buf);
    let storage_dir = storage_dir_from_layer(&layer, storage_dir)?;
    let config_log_level = layer
        .server
        .as_ref()
        .and_then(|server| server.logging.as_ref())
        .and_then(|logging| logging.level.clone());
    let run_settings = RunSettingsBuilder::from_layer(&layer).map_err(|err| err.to_string());
    let server_settings = ServerSettingsBuilder::from_layer(&layer)
        .map(|settings| match storage_override.as_deref() {
            Some(dir) => settings.with_storage_override(dir),
            None => settings,
        })
        .map_err(|err| err.to_string());
    let user_settings_layer = if let Some(cli_layer) = cli_layer {
        SettingsLayer {
            cli: Some(cli_layer.clone()),
            ..SettingsLayer::default()
        }
        .combine(layer.clone())
    } else {
        layer.clone()
    };
    let user_settings = UserSettingsBuilder::from_layer(&user_settings_layer)?;

    Ok(LoadedSettings {
        storage_dir,
        config_log_level,
        run_settings,
        server_settings,
        user_settings,
    })
}

fn storage_dir_from_layer(
    layer: &SettingsLayer,
    storage_dir: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    storage_dir_from_layer_with_lookup(layer, storage_dir, &|name| std::env::var(name).ok())
}

fn storage_dir_from_layer_with_lookup(
    layer: &SettingsLayer,
    storage_dir: Option<&Path>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<PathBuf> {
    if let Some(dir) = storage_dir {
        return Ok(dir.to_path_buf());
    }

    let storage_root = layer
        .server
        .as_ref()
        .and_then(|server| server.storage.as_ref())
        .and_then(|storage| storage.root.clone())
        .unwrap_or_else(|| {
            fabro_types::settings::InterpString::parse(&default_storage_dir().to_string_lossy())
        });
    let resolved_root = storage_root.resolve(lookup)?;
    Ok(PathBuf::from(resolved_root.value))
}

/// Pull the resolved CLI target configuration out of `[cli.target]`.
/// Returns either an http(s) URL or a unix socket path.
fn cli_target_from_settings(settings: &CliNamespace) -> Option<String> {
    let target = settings.target.as_ref()?;
    match target {
        CliTargetSettings::Http { url } => Some(url.as_source()),
        CliTargetSettings::Unix { path } => Some(path.as_source()),
    }
}

fn configured_server_target(settings: &UserSettings) -> Result<Option<ServerTarget>> {
    let Some(value) = cli_target_from_settings(&settings.cli) else {
        return Ok(None);
    };
    parse_server_target(&value).map(Some)
}

pub(crate) fn default_server_target() -> ServerTarget {
    ServerTarget::unix_socket_path(default_socket_path()).expect("default socket path is absolute")
}

fn parse_server_target(value: &str) -> Result<ServerTarget> {
    ServerTarget::from_str(value)
}

fn explicit_server_target(args: &ServerTargetArgs) -> Result<Option<ServerTarget>> {
    args.as_deref().map(parse_server_target).transpose()
}

pub(crate) fn resolve_nondefault_server_target(
    args: &ServerTargetArgs,
    settings: &UserSettings,
) -> Result<Option<ServerTarget>> {
    Ok(explicit_server_target(args)?.or(configured_server_target(settings)?))
}

pub(crate) fn resolve_server_target(
    args: &ServerTargetArgs,
    settings: &UserSettings,
) -> Result<ServerTarget> {
    Ok(resolve_nondefault_server_target(args, settings)?.unwrap_or_else(default_server_target))
}

pub(crate) fn exec_server_target(args: &ServerTargetArgs) -> Result<Option<ServerTarget>> {
    let target = explicit_server_target(args)?;
    debug!(?target, "Resolved exec server target");
    Ok(target)
}

pub(crate) fn cli_http_client_builder() -> fabro_http::HttpClientBuilder {
    fabro_http::HttpClientBuilder::new().user_agent(format!("fabro-cli/{FABRO_VERSION}"))
}

#[cfg(test)]
pub(crate) fn load_resolved_settings_from_toml(
    source: &str,
    storage_dir: Option<&Path>,
    cli_layer: Option<&CliLayer>,
) -> anyhow::Result<LoadedSettings> {
    let layer: SettingsLayer = toml::from_str(source)
        .map_err(|err| anyhow::anyhow!("failed to parse settings file: {err}"))?;
    resolve_loaded_settings(layer, storage_dir, cli_layer)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fabro_config::UserSettingsBuilder;
    use fabro_config::user::default_storage_dir;
    use fabro_types::UserSettings;

    use super::*;
    use crate::args::ServerTargetArgs;

    fn server_target_args(value: Option<&str>) -> ServerTargetArgs {
        ServerTargetArgs {
            server: value.map(str::to_string),
        }
    }

    fn parse_user_settings(source: &str) -> UserSettings {
        UserSettingsBuilder::from_toml(source).expect("fixture should resolve")
    }

    #[test]
    fn exec_has_no_server_target_by_default() {
        assert_eq!(exec_server_target(&server_target_args(None)).unwrap(), None);
    }

    #[test]
    fn exec_uses_cli_server_target() {
        assert_eq!(
            exec_server_target(&server_target_args(Some("https://cli.example.com"))).unwrap(),
            Some(ServerTarget::http_url("https://cli.example.com").unwrap())
        );
    }

    #[test]
    fn exec_supports_explicit_unix_socket_target() {
        assert_eq!(
            exec_server_target(&server_target_args(Some("/tmp/fabro.sock"))).unwrap(),
            Some(ServerTarget::unix_socket_path("/tmp/fabro.sock").unwrap())
        );
    }

    #[test]
    fn exec_ignores_configured_server_target_without_cli_override() {
        assert_eq!(exec_server_target(&server_target_args(None)).unwrap(), None);
    }

    #[test]
    fn resolve_server_target_uses_configured_server_target() {
        let settings = parse_user_settings(
            r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"
"#,
        );
        assert_eq!(
            resolve_server_target(&server_target_args(None), &settings).unwrap(),
            ServerTarget::http_url("https://config.example.com").unwrap()
        );
    }

    #[test]
    fn resolve_server_target_explicit_target_overrides_config_target() {
        let settings = parse_user_settings(
            r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"
"#,
        );
        assert_eq!(
            resolve_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            ServerTarget::http_url("https://cli.example.com").unwrap()
        );
    }

    #[test]
    fn resolve_server_target_defaults_to_default_unix_socket_target() {
        let settings = UserSettings::default();
        assert_eq!(
            resolve_server_target(&server_target_args(None), &settings).unwrap(),
            ServerTarget::unix_socket_path(dirs::home_dir().unwrap().join(".fabro/fabro.sock"))
                .unwrap()
        );
    }

    #[test]
    fn explicit_server_target_overrides_config_target() {
        let settings = parse_user_settings(
            r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"
"#,
        );
        assert_eq!(
            resolve_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            ServerTarget::http_url("https://cli.example.com").unwrap()
        );
    }

    #[test]
    fn invalid_server_target_is_rejected() {
        let error = exec_server_target(&server_target_args(Some("fabro.internal"))).unwrap_err();
        assert_eq!(
            error.to_string(),
            "server target must be an http(s) URL or absolute Unix socket path"
        );
    }

    #[test]
    fn storage_dir_defaults_without_server_auth_methods() {
        let layer = SettingsLayer::default();

        assert_eq!(
            storage_dir_from_layer(&layer, None).unwrap(),
            default_storage_dir()
        );
    }

    #[test]
    fn storage_dir_uses_explicit_server_storage_root() {
        let layer: SettingsLayer = toml::from_str(
            r#"
_version = 1

[server.storage]
root = "/srv/fabro"
"#,
        )
        .expect("fixture should parse");

        assert_eq!(
            storage_dir_from_layer(&layer, None).unwrap(),
            PathBuf::from("/srv/fabro")
        );
    }

    #[test]
    fn storage_dir_resolves_env_interpolated_root() {
        let layer: SettingsLayer = toml::from_str(
            r#"
_version = 1

[server.storage]
root = "{{ env.FABRO_STORAGE_ROOT }}"
"#,
        )
        .expect("fixture should parse");
        let temp = tempfile::tempdir().unwrap();

        assert_eq!(
            storage_dir_from_layer_with_lookup(&layer, None, &|name| {
                (name == "FABRO_STORAGE_ROOT").then(|| temp.path().display().to_string())
            })
            .unwrap(),
            temp.path()
        );
    }
}
