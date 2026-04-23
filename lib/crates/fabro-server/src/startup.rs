use std::path::Path;

use fabro_types::settings::ServerSettings as ResolvedServerSettings;

use crate::jwt_auth::{AuthMode, resolve_auth_mode_with_lookup};
pub use crate::server_secrets::{EnvSource, ProcessEnv};
use crate::server_secrets::{Error as ServerSecretsError, ServerSecrets};

#[derive(Debug)]
pub(crate) struct StartupResolution {
    pub(crate) auth_mode:      AuthMode,
    pub(crate) server_secrets: ServerSecrets,
}

#[derive(Debug, thiserror::Error)]
pub enum StartupValidationError {
    #[error("{0}")]
    Message(String),
}

impl From<ServerSecretsError> for StartupValidationError {
    fn from(err: ServerSecretsError) -> Self {
        Self::Message(err.to_string())
    }
}

impl From<anyhow::Error> for StartupValidationError {
    fn from(err: anyhow::Error) -> Self {
        Self::Message(err.to_string())
    }
}

pub(crate) fn resolve_startup(
    env_path: &Path,
    env: &dyn EnvSource,
    settings: &ResolvedServerSettings,
) -> std::result::Result<StartupResolution, StartupValidationError> {
    let server_secrets = ServerSecrets::load(env_path, env)?;
    let auth_mode = resolve_auth_mode_with_lookup(settings, |name| server_secrets.get(name))?;
    Ok(StartupResolution {
        auth_mode,
        server_secrets,
    })
}

pub fn validate_startup(
    env_path: &Path,
    env: &dyn EnvSource,
    settings: &ResolvedServerSettings,
) -> std::result::Result<(), StartupValidationError> {
    resolve_startup(env_path, env, settings).map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_config::parse_settings_layer;
    use fabro_types::settings::ServerSettings as ResolvedServerSettings;

    use super::{resolve_startup, validate_startup};
    use crate::server_secrets::StubEnv;

    fn resolved_settings(auth_methods: &[&str]) -> ResolvedServerSettings {
        let settings = parse_settings_layer(&format!(
            r"
_version = 1

[server.auth]
methods = [{}]
",
            auth_methods
                .iter()
                .map(|method| format!("\"{method}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .unwrap();
        fabro_config::resolve_server_from_file(&settings).unwrap()
    }

    #[test]
    fn validate_startup_matches_resolve_startup() {
        let dir = tempfile::tempdir().unwrap();
        let env = StubEnv(HashMap::from([
            (
                "SESSION_SECRET".to_string(),
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            ),
            (
                "FABRO_DEV_TOKEN".to_string(),
                "fabro_dev_abababababababababababababababababababababababababababababababab"
                    .to_string(),
            ),
        ]));
        let settings = resolved_settings(&["dev-token"]);

        assert!(validate_startup(dir.path().join("server.env").as_path(), &env, &settings).is_ok());
        assert!(resolve_startup(dir.path().join("server.env").as_path(), &env, &settings).is_ok());
    }

    #[test]
    fn validate_startup_and_resolve_startup_share_errors() {
        let dir = tempfile::tempdir().unwrap();
        let env = StubEnv::default();
        let settings = resolved_settings(&["dev-token"]);

        let validate_err =
            validate_startup(dir.path().join("server.env").as_path(), &env, &settings)
                .unwrap_err()
                .to_string();
        let resolve_err = resolve_startup(dir.path().join("server.env").as_path(), &env, &settings)
            .unwrap_err()
            .to_string();

        assert_eq!(validate_err, resolve_err);
    }
}
