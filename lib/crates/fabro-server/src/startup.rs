use std::collections::HashMap;
use std::path::Path;

use fabro_types::settings::ServerNamespace;

use crate::jwt_auth::{AuthMode, resolve_auth_mode_with_lookup};
use crate::server_secrets::ServerSecrets;

pub(crate) fn resolve_startup(
    env_path: &Path,
    env_entries: HashMap<String, String>,
    settings: &ServerNamespace,
) -> anyhow::Result<(AuthMode, ServerSecrets)> {
    let server_secrets = ServerSecrets::load(env_path, env_entries)?;
    let auth_mode = resolve_auth_mode_with_lookup(settings, |name| server_secrets.get(name))?;
    Ok((auth_mode, server_secrets))
}

pub fn validate_startup(
    env_path: &Path,
    env_entries: HashMap<String, String>,
    settings: &ServerNamespace,
) -> anyhow::Result<()> {
    resolve_startup(env_path, env_entries, settings).map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_config::ServerSettingsBuilder;
    use fabro_static::EnvVars;
    use fabro_types::settings::ServerNamespace;

    use super::validate_startup;

    fn resolved_settings(auth_methods: &[&str]) -> ServerNamespace {
        ServerSettingsBuilder::from_toml(&format!(
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
        .unwrap()
        .server
    }

    #[test]
    fn validate_startup_accepts_configured_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::from([
            (
                EnvVars::SESSION_SECRET.to_string(),
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            ),
            (
                EnvVars::FABRO_DEV_TOKEN.to_string(),
                "fabro_dev_abababababababababababababababababababababababababababababababab"
                    .to_string(),
            ),
        ]);
        let settings = resolved_settings(&["dev-token"]);

        assert!(validate_startup(dir.path().join("server.env").as_path(), env, &settings).is_ok());
    }

    #[test]
    fn validate_startup_rejects_missing_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let settings = resolved_settings(&["dev-token"]);

        assert!(
            validate_startup(
                dir.path().join("server.env").as_path(),
                HashMap::new(),
                &settings,
            )
            .is_err()
        );
    }
}
