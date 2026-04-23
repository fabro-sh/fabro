use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use fabro_auth::{CredentialResolver, CredentialUsage, ResolveError, ResolvedCredential};
use fabro_config::envfile;
use fabro_llm::client::Client as LlmClient;
use fabro_model::Provider;
use fabro_vault::Vault;
use tokio::sync::RwLock as AsyncRwLock;

type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub trait EnvSource {
    fn snapshot(&self) -> HashMap<String, String>;
}

pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn snapshot(&self) -> HashMap<String, String> {
        std::env::vars().collect()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StubEnv(pub(crate) HashMap<String, String>);

impl EnvSource for StubEnv {
    fn snapshot(&self) -> HashMap<String, String> {
        self.0.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) struct ServerSecrets {
    path:         PathBuf,
    env_entries:  HashMap<String, String>,
    file_entries: HashMap<String, String>,
}

impl ServerSecrets {
    pub(crate) fn load(path: PathBuf, env: &dyn EnvSource) -> Result<Self, Error> {
        Ok(Self {
            env_entries: env.snapshot(),
            file_entries: envfile::read_env_file(&path)?,
            path,
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
            .field("path", &self.path)
            .field("env_entries", &self.env_entries.keys().collect::<Vec<_>>())
            .field(
                "file_entries",
                &self.file_entries.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) struct ProviderCredentials {
    vault:      Arc<AsyncRwLock<Vault>>,
    env_lookup: EnvLookup,
}

impl ProviderCredentials {
    pub(crate) fn with_env_lookup<F>(vault: Arc<AsyncRwLock<Vault>>, env_lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String> + Send + Sync + 'static,
    {
        Self {
            vault,
            env_lookup: Arc::new(env_lookup),
        }
    }

    #[cfg(test)]
    pub(crate) async fn get(&self, name: &str) -> Option<String> {
        let env_value = (self.env_lookup)(name);
        if env_value.is_some() {
            return env_value;
        }

        self.vault.read().await.get(name).map(str::to_string)
    }

    pub(crate) async fn build_llm_client(&self) -> Result<LlmClientResult, String> {
        let resolver =
            CredentialResolver::with_env_lookup(Arc::clone(&self.vault), self.env_lookup.clone());
        let mut api_credentials = Vec::new();
        let mut auth_issues = Vec::new();

        for provider in Provider::ALL {
            match resolver
                .resolve(*provider, CredentialUsage::ApiRequest)
                .await
            {
                Ok(ResolvedCredential::Api(credential)) => api_credentials.push(credential),
                Ok(ResolvedCredential::Cli(_)) | Err(ResolveError::NotConfigured(_)) => {}
                Err(err) => auth_issues.push((*provider, err)),
            }
        }

        let client = LlmClient::from_credentials(api_credentials)
            .await
            .map_err(|err| err.to_string())?;

        Ok(LlmClientResult {
            client,
            auth_issues,
        })
    }

    pub(crate) async fn configured_providers(&self) -> Vec<Provider> {
        let resolver =
            CredentialResolver::with_env_lookup(Arc::clone(&self.vault), self.env_lookup.clone());
        let vault = self.vault.read().await;
        resolver.configured_providers(&vault)
    }
}

pub(crate) struct LlmClientResult {
    pub client:      LlmClient,
    pub auth_issues: Vec<(Provider, ResolveError)>,
}

pub(crate) fn auth_issue_message(provider: Provider, err: &ResolveError) -> String {
    match err {
        ResolveError::NotConfigured(_) => {
            format!("{} is not configured", provider.display_name())
        }
        ResolveError::RefreshFailed { source, .. } => format!(
            "{} requires re-authentication: {}",
            provider.display_name(),
            source
        ),
        ResolveError::RefreshTokenMissing(_) => format!(
            "{} requires re-authentication: refresh token missing",
            provider.display_name()
        ),
    }
}

impl std::fmt::Debug for ProviderCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderCredentials")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use fabro_auth::{AuthCredential, AuthDetails};
    use fabro_config::envfile;
    use fabro_vault::{SecretType, Vault};
    use tokio::sync::RwLock as AsyncRwLock;

    use super::{ProviderCredentials, ServerSecrets, StubEnv};
    use crate::server_secrets::Provider;

    #[tokio::test]
    async fn configured_providers_respects_injected_env_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(AsyncRwLock::new(
            Vault::load(dir.path().join("secrets.json")).unwrap(),
        ));
        let credentials = ProviderCredentials::with_env_lookup(Arc::clone(&vault), |name| {
            (name == "OPENAI_API_KEY").then(|| "openai-key".to_string())
        });

        assert_eq!(credentials.configured_providers().await, vec![
            Provider::OpenAi
        ]);
    }

    #[tokio::test]
    async fn configured_providers_includes_vault_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault
            .set(
                "anthropic",
                &serde_json::to_string(&AuthCredential {
                    provider: Provider::Anthropic,
                    details:  AuthDetails::ApiKey {
                        key: "anthropic-key".to_string(),
                    },
                })
                .unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();
        let credentials =
            ProviderCredentials::with_env_lookup(Arc::new(AsyncRwLock::new(vault)), |_| None);

        assert_eq!(credentials.configured_providers().await, vec![
            Provider::Anthropic
        ]);
    }

    #[test]
    fn server_secrets_snapshot_prefers_env_over_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("server.env");
        envfile::write_env_file(
            &env_path,
            &HashMap::from([
                ("SESSION_SECRET".to_string(), "file-value".to_string()),
                (
                    "GITHUB_APP_CLIENT_SECRET".to_string(),
                    "file-client".to_string(),
                ),
            ]),
        )
        .unwrap();

        let secrets = ServerSecrets::load(
            env_path,
            &StubEnv(HashMap::from([(
                "SESSION_SECRET".to_string(),
                "env-value".to_string(),
            )])),
        )
        .unwrap();

        assert_eq!(secrets.get("SESSION_SECRET").as_deref(), Some("env-value"));
        assert_eq!(
            secrets.get("GITHUB_APP_CLIENT_SECRET").as_deref(),
            Some("file-client")
        );
    }

    #[test]
    fn server_secrets_snapshot_is_owned_after_load() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("server.env");
        let mut env = HashMap::from([("SESSION_SECRET".to_string(), "before".to_string())]);

        let secrets = ServerSecrets::load(env_path, &StubEnv(env.clone())).unwrap();
        env.insert("SESSION_SECRET".to_string(), "after".to_string());

        assert_eq!(secrets.get("SESSION_SECRET").as_deref(), Some("before"));
    }
}
