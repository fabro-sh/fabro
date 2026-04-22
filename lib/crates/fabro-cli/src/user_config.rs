use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::Result;
pub(crate) use fabro_client::ServerTarget;
pub(crate) use fabro_config::user::*;
use fabro_types::settings::cli::CliTargetSettings;
use fabro_types::settings::{CliSettings, SettingsLayer};
use fabro_util::version::FABRO_VERSION;
use tracing::debug;

use crate::args::ServerTargetArgs;

pub(crate) fn load_settings() -> anyhow::Result<SettingsLayer> {
    load_settings_with_config_and_storage_dir(None, None)
}

pub(crate) fn load_settings_with_storage_dir(
    storage_dir: Option<&Path>,
) -> anyhow::Result<SettingsLayer> {
    load_settings_with_config_and_storage_dir(None, storage_dir)
}

pub(crate) fn load_settings_with_config_and_storage_dir(
    config_path: Option<&Path>,
    storage_dir: Option<&Path>,
) -> anyhow::Result<SettingsLayer> {
    let layer = load_settings_config(config_path)?;
    Ok(apply_storage_dir_override(layer, storage_dir))
}

fn render_resolve_errors(errors: Vec<fabro_config::ResolveError>) -> anyhow::Error {
    anyhow::anyhow!(
        "failed to resolve cli settings:\n{}",
        errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub(crate) fn resolve_cli_settings(file: &SettingsLayer) -> anyhow::Result<CliSettings> {
    fabro_config::resolve_cli_from_file(file).map_err(render_resolve_errors)
}

pub(crate) fn apply_storage_dir_override(
    mut layer: SettingsLayer,
    storage_dir: Option<&Path>,
) -> SettingsLayer {
    use fabro_types::settings::interp::InterpString;
    use fabro_types::settings::server::{ServerLayer, ServerStorageLayer};
    if let Some(dir) = storage_dir {
        let server = layer.server.get_or_insert_with(ServerLayer::default);
        let storage = server
            .storage
            .get_or_insert_with(ServerStorageLayer::default);
        storage.root = Some(InterpString::parse(&dir.display().to_string()));
    }

    layer
}

/// Pull the resolved CLI target configuration out of `[cli.target]`.
/// Returns either an http(s) URL or a unix socket path.
fn cli_target_from_settings(settings: &CliSettings) -> Option<String> {
    let target = settings.target.as_ref()?;
    match target {
        CliTargetSettings::Http { url } => Some(url.as_source()),
        CliTargetSettings::Unix { path } => Some(path.as_source()),
    }
}

fn configured_server_target(settings: &SettingsLayer) -> Result<Option<ServerTarget>> {
    let cli_settings = resolve_cli_settings(settings)?;
    let Some(value) = cli_target_from_settings(&cli_settings) else {
        return Ok(None);
    };
    parse_server_target(&value).map(Some)
}

pub(crate) fn default_server_target() -> ServerTarget {
    ServerTarget::unix_socket_path(default_socket_path()).expect("default socket path is absolute")
}

pub(crate) fn storage_dir(settings: &SettingsLayer) -> anyhow::Result<PathBuf> {
    let storage_root = fabro_config::resolve_storage_root(settings);
    let resolved_root = storage_root
        .resolve(|name| std::env::var(name).ok())
        .map_err(|err| {
            anyhow::anyhow!("failed to resolve {}: {err}", storage_root.as_source())
        })?;
    Ok(PathBuf::from(resolved_root.value))
}

fn parse_server_target(value: &str) -> Result<ServerTarget> {
    ServerTarget::from_str(value)
}

fn explicit_server_target(args: &ServerTargetArgs) -> Result<Option<ServerTarget>> {
    args.as_deref().map(parse_server_target).transpose()
}

pub(crate) fn resolve_nondefault_server_target(
    args: &ServerTargetArgs,
    settings: &SettingsLayer,
) -> Result<Option<ServerTarget>> {
    Ok(explicit_server_target(args)?.or(configured_server_target(settings)?))
}

pub(crate) fn resolve_server_target(
    args: &ServerTargetArgs,
    settings: &SettingsLayer,
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
mod tests {
    use fabro_config::parse_settings_layer;

    use super::*;
    use crate::args::ServerTargetArgs;

    fn server_target_args(value: Option<&str>) -> ServerTargetArgs {
        ServerTargetArgs {
            server: value.map(str::to_string),
        }
    }

    fn parse_v2(source: &str) -> SettingsLayer {
        parse_settings_layer(source).expect("fixture should parse")
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
        let settings = parse_v2(
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
        let settings = parse_v2(
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
        let settings = SettingsLayer::default();
        assert_eq!(
            resolve_server_target(&server_target_args(None), &settings).unwrap(),
            ServerTarget::unix_socket_path(dirs::home_dir().unwrap().join(".fabro/fabro.sock"))
                .unwrap()
        );
    }

    #[test]
    fn explicit_server_target_overrides_config_target() {
        let settings = parse_v2(
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
        let settings = SettingsLayer::default();

        assert_eq!(storage_dir(&settings).unwrap(), fabro_config::user::default_storage_dir());
    }

    #[test]
    fn storage_dir_uses_explicit_server_storage_root() {
        let settings = parse_v2(
            r#"
_version = 1

[server.storage]
root = "/srv/fabro"
"#,
        );

        assert_eq!(storage_dir(&settings).unwrap(), PathBuf::from("/srv/fabro"));
    }

    #[test]
    fn storage_dir_resolves_env_interpolated_root() {
        let settings = parse_v2(
            r#"
_version = 1

[server.storage]
root = "{{ env.FABRO_STORAGE_ROOT }}"
"#,
        );
        let temp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("FABRO_STORAGE_ROOT");
        std::env::set_var("FABRO_STORAGE_ROOT", temp.path());
        let _guard = scopeguard::guard(original, |original| match original {
            Some(value) => std::env::set_var("FABRO_STORAGE_ROOT", value),
            None => std::env::remove_var("FABRO_STORAGE_ROOT"),
        });

        assert_eq!(storage_dir(&settings).unwrap(), temp.path());
    }
}
