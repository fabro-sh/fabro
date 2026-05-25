use std::collections::HashMap;
use std::path::Path;

use fabro_auth::ResolveError;
use fabro_config::envfile;
use fabro_llm::client::{Client, ProviderRegistrationIssue};
use fabro_model::ProviderId;

#[expect(
    clippy::disallowed_methods,
    reason = "ServerSecrets snapshots process env once at startup by design."
)]
pub fn process_env_snapshot() -> HashMap<String, String> {
    std::env::vars().collect()
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) struct ServerSecrets {
    env_entries:  HashMap<String, String>,
    file_entries: HashMap<String, String>,
}

impl ServerSecrets {
    pub(crate) fn load(
        path: impl AsRef<Path>,
        env_entries: HashMap<String, String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            env_entries,
            file_entries: envfile::read_env_file(path.as_ref())?,
        })
    }

    pub(crate) fn get(&self, name: &str) -> Option<String> {
        self.env_entries
            .get(name)
            .cloned()
            .or_else(|| self.file_entries.get(name).cloned())
    }
}

impl std::fmt::Debug for ServerSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerSecrets")
            .field("env_entries", &self.env_entries.keys().collect::<Vec<_>>())
            .field(
                "file_entries",
                &self.file_entries.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

pub(crate) struct LlmClientResult {
    pub client:              Client,
    pub auth_issues:         Vec<(ProviderId, ResolveError)>,
    pub registration_issues: Vec<ProviderRegistrationIssue>,
}

impl LlmClientResult {
    pub(crate) fn provider_ids(&self) -> Vec<ProviderId> {
        self.client
            .provider_names()
            .into_iter()
            .map(ProviderId::new)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_config::envfile;

    use super::ServerSecrets;

    #[test]
    fn bootstrap_server_secrets_snapshot_prefers_env_over_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("server.env");
        envfile::write_env_file(
            &env_path,
            &HashMap::from([
                ("SESSION_SECRET".to_string(), "file-value".to_string()),
                ("FABRO_DEV_TOKEN".to_string(), "file-dev-token".to_string()),
            ]),
        )
        .unwrap();

        let secrets = ServerSecrets::load(
            env_path,
            HashMap::from([("SESSION_SECRET".to_string(), "env-value".to_string())]),
        )
        .unwrap();

        assert_eq!(secrets.get("SESSION_SECRET").as_deref(), Some("env-value"));
        assert_eq!(
            secrets.get("FABRO_DEV_TOKEN").as_deref(),
            Some("file-dev-token")
        );
    }
}
