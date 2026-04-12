use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use fabro_config::envfile;
use fabro_llm::client::Client as LlmClient;
use fabro_vault::Vault;
use tokio::sync::RwLock as AsyncRwLock;

type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

const PROVIDER_LOOKUP_NAMES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
    "OPENAI_API_KEY",
    "CHATGPT_ACCOUNT_ID",
    "OPENAI_BASE_URL",
    "OPENAI_ORG_ID",
    "OPENAI_PROJECT_ID",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GEMINI_BASE_URL",
    "KIMI_API_KEY",
    "ZAI_API_KEY",
    "MINIMAX_API_KEY",
    "INCEPTION_API_KEY",
];

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) struct ServerSecrets {
    path:         PathBuf,
    file_entries: HashMap<String, String>,
    env_lookup:   EnvLookup,
}

impl ServerSecrets {
    pub(crate) fn load(path: PathBuf) -> Result<Self, Error> {
        Self::with_env_lookup(path, |name| std::env::var(name).ok())
    }

    pub(crate) fn with_env_lookup<F>(path: PathBuf, env_lookup: F) -> Result<Self, Error>
    where
        F: Fn(&str) -> Option<String> + Send + Sync + 'static,
    {
        Ok(Self {
            file_entries: envfile::read_env_file(&path)?,
            path,
            env_lookup: Arc::new(env_lookup),
        })
    }

    pub(crate) fn get(&self, name: &str) -> Option<String> {
        (self.env_lookup)(name).or_else(|| self.file_entries.get(name).cloned())
    }

    pub(crate) fn persist_updates<I, K, V>(&mut self, updates: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.file_entries = envfile::merge_env_file(&self.path, updates)?;
        Ok(())
    }
}

impl std::fmt::Debug for ServerSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerSecrets")
            .field("path", &self.path)
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
    pub(crate) fn new(vault: Arc<AsyncRwLock<Vault>>) -> Self {
        Self::with_env_lookup(vault, |name| std::env::var(name).ok())
    }

    pub(crate) fn with_env_lookup<F>(vault: Arc<AsyncRwLock<Vault>>, env_lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String> + Send + Sync + 'static,
    {
        Self {
            vault,
            env_lookup: Arc::new(env_lookup),
        }
    }

    pub(crate) async fn get(&self, name: &str) -> Option<String> {
        let env_value = (self.env_lookup)(name);
        if env_value.is_some() {
            return env_value;
        }

        self.vault.read().await.get(name).map(str::to_string)
    }

    pub(crate) async fn has_any(&self, names: &[&str]) -> bool {
        for name in names {
            if self.get(name).await.is_some() {
                return true;
            }
        }
        false
    }

    pub(crate) async fn build_llm_client(&self) -> Result<LlmClient, String> {
        let vault_snapshot = self.vault.read().await.snapshot();
        let lookup = PROVIDER_LOOKUP_NAMES
            .iter()
            .filter_map(|name| {
                (self.env_lookup)(name)
                    .or_else(|| vault_snapshot.get(*name).cloned())
                    .map(|value| ((*name).to_string(), value))
            })
            .collect::<HashMap<_, _>>();

        LlmClient::from_lookup(|name| lookup.get(name).cloned())
            .await
            .map_err(|err| err.to_string())
    }
}

impl std::fmt::Debug for ProviderCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderCredentials")
            .finish_non_exhaustive()
    }
}
